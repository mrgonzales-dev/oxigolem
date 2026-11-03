use oxigolem::agent::tools;
use oxigolem::shell::{BgStatus, Shell, new_bg_shells};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

fn rig() -> (Shell, oxigolem::shell::BgShells, Arc<AtomicBool>) {
    (
        Shell::new(),
        new_bg_shells(),
        Arc::new(AtomicBool::new(false)),
    )
}

#[test]
fn bash_runs_command_and_captures_stdout() {
    let (sh, shells, demote) = rig();
    let out = tools::execute(
        &sh,
        &shells,
        &demote,
        "bash",
        &serde_json::json!({"command": "echo hello"}),
    );
    assert_eq!(out.result.stdout.trim(), "hello");
    assert_eq!(out.result.code, Some(0));
    assert_eq!(out.command, "echo hello");
}

#[test]
fn bash_captures_stderr_and_exit_code() {
    let (sh, shells, demote) = rig();
    let out = tools::execute(
        &sh,
        &shells,
        &demote,
        "bash",
        &serde_json::json!({"command": "echo oops >&2; exit 3"}),
    );
    assert!(
        out.result.stderr.contains("oops"),
        "stderr captured, got: {:?}",
        out.result
    );
    assert_eq!(out.result.code, Some(3));
    // Flattened form still carries stderr as `! ` lines + `exit N`.
    let flat = out.to_tool_message();
    assert!(flat.contains("! oops"), "got: {flat}");
    assert!(flat.contains("exit 3"), "got: {flat}");
}

#[test]
fn bash_demote_returns_shell_id_not_kill() {
    // Demote (Ctrl+B×2 or block-budget) must leave the process running
    // and hand the caller a shell id + log path — never a kill.
    unsafe {
        std::env::set_var("OXIGOLEM_JOBS_DIR", "/tmp/oxi-test-jobs-nk");
    }
    std::fs::create_dir_all("/tmp/oxi-test-jobs-nk").ok();
    let (sh, shells, demote) = rig();
    let shells2 = Arc::clone(&shells);
    let demote2 = Arc::clone(&demote);
    let sh2 = sh.clone();
    let h = std::thread::spawn(move || {
        tools::execute(
            &sh2,
            &shells2,
            &demote2,
            "bash",
            &serde_json::json!({"command": "sleep 30"}),
        )
    });
    for _ in 0..100 {
        if shells.lock().unwrap().len() == 1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    demote.store(true, Ordering::Relaxed);
    let out = h.join().expect("execute returned");
    assert!(
        out.demoted,
        "outcome must flag demote, got: {:?}",
        out.result
    );
    // Shell survives — still running in the registry, not killed.
    let reg = shells.lock().unwrap();
    assert_eq!(reg.len(), 1);
    assert_eq!(reg[0].status(), BgStatus::Running);
    // Tool message names the shell id + log so the model can poll.
    let msg = out.to_tool_message();
    assert!(msg.contains(&reg[0].id), "got: {msg}");
    assert!(msg.contains(".log"), "got: {msg}");
    reg[0].kill();
}

#[test]
fn bash_missing_command_errors() {
    let (sh, shells, demote) = rig();
    let out = tools::execute(&sh, &shells, &demote, "bash", &serde_json::json!({}));
    assert!(
        out.result.error.as_deref().unwrap_or("").contains("error"),
        "got: {:?}",
        out.result
    );
}

#[test]
fn execute_unknown_tool_errors() {
    let (sh, shells, demote) = rig();
    let out = tools::execute(&sh, &shells, &demote, "nope", &serde_json::json!({}));
    assert!(
        out.result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("unknown tool"),
        "got: {:?}",
        out.result
    );
}

#[test]
fn shell_cd_persists_across_calls() {
    // The user terminal and the model share one shell: a `cd` by either
    // side sticks for the next command.
    let sh = Shell::new();
    sh.run("cd /tmp");
    let out = sh.run("pwd");
    assert_eq!(out.trim(), "/tmp");
}

#[test]
fn shell_relative_cd_tracks_directory() {
    let sh = Shell::new();
    sh.run("cd /tmp");
    sh.run("mkdir -p oxigolem-test-sub && cd oxigolem-test-sub");
    let out = sh.run("pwd");
    assert_eq!(out.trim(), "/tmp/oxigolem-test-sub");
}

#[test]
fn shell_bare_cd_goes_home() {
    let sh = Shell::new();
    sh.run("cd /tmp");
    sh.run("cd");
    let home = std::env::var("HOME").unwrap();
    assert_eq!(sh.run("pwd").trim(), home);
}

#[test]
fn shell_cd_tilde_expands() {
    let sh = Shell::new();
    sh.run("cd ~");
    let home = std::env::var("HOME").unwrap();
    assert_eq!(sh.run("pwd").trim(), home);
}

#[test]
fn shell_cd_quoted_path_with_spaces() {
    let sh = Shell::new();
    sh.run("mkdir -p '/tmp/oxi dir with spaces'");
    sh.run("cd '/tmp/oxi dir with spaces'");
    assert_eq!(sh.run("pwd").trim(), "/tmp/oxi dir with spaces");
}

#[test]
fn shell_compound_cd_via_bash() {
    // `cd /tmp && pwd` is NOT a bare cd — bash runs it; marker still
    // captures the new dir so subsequent commands stay there.
    let sh = Shell::new();
    sh.run("cd /tmp && pwd");
    assert_eq!(sh.run("pwd").trim(), "/tmp");
}

#[test]
fn specs_contains_bash_with_command_param() {
    let specs = tools::specs();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].name, "bash");
    assert!(specs[0].parameters["properties"]["command"].is_object());
    assert_eq!(specs[0].parameters["required"][0], "command");
}

#[test]
fn to_json_builds_ollama_tools_array() {
    let v = tools::to_json(&tools::specs());
    assert_eq!(v[0]["type"], "function");
    assert_eq!(v[0]["function"]["name"], "bash");
    assert!(v[0]["function"]["description"].is_string());
}

#[test]
fn bash_output_over_pipe_buffer_does_not_deadlock() {
    // `seq 1 20000` is ~110KB — well past the 64KB pipe buffer. If the
    // shell waits for exit before draining stdout, the child blocks on
    // write and this hits the 30s timeout instead of finishing in ms.
    let (sh, shells, demote) = rig();
    let start = std::time::Instant::now();
    let out = tools::execute(
        &sh,
        &shells,
        &demote,
        "bash",
        &serde_json::json!({"command": "seq 1 20000"}),
    );
    assert!(
        start.elapsed() < std::time::Duration::from_secs(10),
        "big output should not stall near the timeout"
    );
    assert!(!out.result.timed_out, "got: {:?}", out.result);
    assert_eq!(out.result.code, Some(0));
}

#[test]
fn small_output_stays_inline() {
    let (sh, shells, demote) = rig();
    let out = tools::execute(
        &sh,
        &shells,
        &demote,
        "bash",
        &serde_json::json!({"command": "echo hi"}),
    );
    assert!(out.spilled.is_none(), "small output must not spill");
    let msg = out.to_tool_message();
    assert!(msg.contains("hi"), "got: {msg}");
    assert!(!msg.contains(".log"), "inline output carries no path");
}

#[test]
fn large_output_spills_to_file_with_preview() {
    unsafe {
        std::env::set_var("OXIGOLEM_JOBS_DIR", "/tmp/oxi-test-jobs");
    }
    let (sh, shells, demote) = rig();
    let out = tools::execute(
        &sh,
        &shells,
        &demote,
        "bash",
        &serde_json::json!({"command": "seq 1 5000"}), // ~24KB → over the 4KB inline cap
    );
    let path = out
        .spilled
        .clone()
        .expect("large output should spill to a file");
    assert!(
        std::path::Path::new(&path).exists(),
        "spill file must exist: {path}"
    );

    // Full output preserved on disk — nothing lost.
    let disk = std::fs::read_to_string(&path).unwrap();
    assert!(disk.contains("4999"), "tail of output on disk");
    assert!(disk.contains("1\n2\n3"), "head of output on disk");

    // Tool message carries the path + a preview, staying small.
    let msg = out.to_tool_message();
    assert!(msg.contains(&path), "message must name the file: {msg}");
    assert!(msg.contains("output saved"), "got: {msg}");
    assert!(msg.contains("4999"), "preview should include the tail");
    assert!(
        msg.len() < 5000,
        "tool message stays small despite 24KB output, got {} chars",
        msg.len()
    );
}

#[test]
fn run_in_background_returns_immediately_with_id_and_log() {
    let (sh, shells, demote) = rig();
    let start = std::time::Instant::now();
    let out = tools::execute(
        &sh,
        &shells,
        &demote,
        "bash",
        &serde_json::json!({"command": "sleep 2; echo done", "run_in_background": true}),
    );
    assert!(
        start.elapsed() < Duration::from_millis(500),
        "bg spawn should return immediately"
    );
    assert_eq!(out.result.code, Some(0));
    assert!(
        out.result.stdout.contains(".log"),
        "got: {}",
        out.result.stdout
    );
    // Shell registered.
    let reg = shells.lock().unwrap();
    assert_eq!(reg.len(), 1);
    assert!(reg[0].log_path.ends_with(&format!("{}.log", reg[0].id)));
    assert!(
        out.result.stdout.contains(&reg[0].id),
        "got: {}",
        out.result.stdout
    );
    // Still running.
    assert_eq!(reg[0].status(), BgStatus::Running);
}

#[test]
fn run_in_background_shell_records_lines_and_exits() {
    unsafe {
        std::env::set_var("OXIGOLEM_JOBS_DIR", "/tmp/oxi-test-jobs-bg");
    }
    std::fs::create_dir_all("/tmp/oxi-test-jobs-bg").ok();
    let (sh, shells, demote) = rig();
    tools::execute(
        &sh,
        &shells,
        &demote,
        "bash",
        &serde_json::json!({"command": "echo one; echo two; echo three", "run_in_background": true}),
    );
    // Wait for exit.
    for _ in 0..100 {
        let st = {
            let reg = shells.lock().unwrap();
            reg.first().map(|s| s.status())
        };
        if matches!(st, Some(BgStatus::Exited(_))) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let reg = shells.lock().unwrap();
    let bg = reg.first().expect("bg shell registered");
    assert_eq!(bg.status(), BgStatus::Exited(0));
    let snap = bg.snapshot();
    assert!(snap.iter().any(|l| l == "one"), "got: {snap:?}");
    assert!(snap.iter().any(|l| l == "three"), "got: {snap:?}");
    // Full output also on disk.
    let disk = std::fs::read_to_string(&bg.log_path).unwrap();
    assert!(disk.contains("two"), "got: {disk}");
}

#[test]
fn demote_turns_foreground_call_into_bg_shell() {
    unsafe {
        std::env::set_var("OXIGOLEM_JOBS_DIR", "/tmp/oxi-test-jobs-dm");
    }
    std::fs::create_dir_all("/tmp/oxi-test-jobs-dm").ok();
    let (sh, shells, demote) = rig();
    // Spawn on a thread; it will block in wait_bg until demote fires.
    let shells2 = Arc::clone(&shells);
    let demote2 = Arc::clone(&demote);
    let sh2 = sh.clone();
    let h = std::thread::spawn(move || {
        tools::execute(
            &sh2,
            &shells2,
            &demote2,
            "bash",
            &serde_json::json!({"command": "sleep 30"}),
        )
    });
    // Wait for the shell to register, then demote.
    for _ in 0..100 {
        if shells.lock().unwrap().len() == 1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(shells.lock().unwrap().len(), 1, "shell registered");
    demote.store(true, Ordering::Relaxed);
    let out = h.join().expect("execute returned");
    // Demoted → early return carrying the shell id + log path.
    assert_eq!(out.result.code, Some(0));
    assert!(out.demoted, "outcome must flag demote");
    assert!(
        out.result.stdout.contains("still running"),
        "got: {}",
        out.result.stdout
    );
    let id = shells.lock().unwrap()[0].id.clone();
    assert!(
        out.result.stdout.contains(&id),
        "got: {}",
        out.result.stdout
    );
    // Shell stays in the registry, no longer transient, still running.
    let reg = shells.lock().unwrap();
    assert_eq!(reg.len(), 1);
    assert!(!reg[0].transient.load(Ordering::Relaxed));
    assert_eq!(reg[0].status(), BgStatus::Running);
    // Clean up the sleeper.
    reg[0].kill();
}

#[test]
fn foreground_shell_removed_from_registry_on_exit() {
    let (sh, shells, demote) = rig();
    let _ = tools::execute(
        &sh,
        &shells,
        &demote,
        "bash",
        &serde_json::json!({"command": "echo x"}),
    );
    // Foreground (transient) shells are removed once execute finishes.
    assert_eq!(shells.lock().unwrap().len(), 0);
}
