use oxigolem::shell::Shell;
use oxigolem::agent::tools;

#[test]
fn bash_runs_command_and_captures_stdout() {
    let sh = Shell::new();
    let out = tools::execute(&sh, "bash", &serde_json::json!({"command": "echo hello"}));
    assert_eq!(out.trim(), "hello");
}

#[test]
fn bash_captures_stderr_and_exit_code() {
    let sh = Shell::new();
    let out = tools::execute(
        &sh,
        "bash",
        &serde_json::json!({"command": "echo oops >&2; exit 3"}),
    );
    assert!(out.contains("! oops"), "stderr should be prefixed, got: {out}");
    assert!(out.contains("exit 3"), "exit code appended, got: {out}");
}

#[test]
fn bash_times_out_on_runaway() {
    let sh = Shell::new();
    let start = std::time::Instant::now();
    let out = tools::execute(&sh, "bash", &serde_json::json!({"command": "sleep 60"}));
    assert!(
        start.elapsed() < std::time::Duration::from_secs(45),
        "should not wait the full sleep"
    );
    assert!(out.contains("timed out"), "got: {out}");
}

#[test]
fn bash_missing_command_errors() {
    let sh = Shell::new();
    let out = tools::execute(&sh, "bash", &serde_json::json!({}));
    assert!(out.contains("error"), "got: {out}");
}

#[test]
fn execute_unknown_tool_errors() {
    let sh = Shell::new();
    let out = tools::execute(&sh, "nope", &serde_json::json!({}));
    assert!(out.contains("unknown tool"), "got: {out}");
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
