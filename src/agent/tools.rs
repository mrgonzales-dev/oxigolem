use crate::shell::{BgShells, Shell, ShellResult};

/// One callable tool, described in the Ollama `tools` request format.
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: serde_json::Value,
}

/// All tools the model may call. Linux build: bash only — the terminal
/// the user types in and the model's commands share one Shell.
pub fn specs() -> Vec<ToolSpec> {
    vec![ToolSpec {
        name: "bash",
        description: "Run a bash command on the user's Linux machine. \
                      Shares one shell session with the user — working \
                      directory and side effects persist. Commands that \
                      outlive the 120s block budget keep running as a \
                      background shell and you get an id + log path to \
                      poll. Prefer run_in_background for servers and \
                      watchers: it returns immediately.",
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "the bash command to run"
                },
                "run_in_background": {
                    "type": "boolean",
                    "description": "run detached and return immediately with a shell id + log path"
                }
            },
            "required": ["command"]
        }),
    }]
}

/// Serialize specs into the Ollama request `"tools"` array.
pub fn to_json(specs: &[ToolSpec]) -> serde_json::Value {
    serde_json::Value::Array(
        specs
            .iter()
            .map(|s| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": s.name,
                        "description": s.description,
                        "parameters": s.parameters,
                    }
                })
            })
            .collect(),
    )
}

/// Combined inline cap — outputs past this spill to a file and the tool
/// message carries the path + a preview instead of the full dump.
const INLINE_CAP: usize = 4000;

/// Preview chars kept inline from head and tail when an output spills.
const PREVIEW_CHARS: usize = 2000;

/// Outcome of a tool call. For `bash` this wraps the `ShellResult`; for
/// unknown/bad calls it carries a synthesized error so the UI can still
/// render it like a finished block.
#[derive(Clone, Debug)]
pub struct ToolOutcome {
    /// Command the tool actually ran (for bash, `args.command`).
    pub command: String,
    pub result: ShellResult,
    /// Set by `to_tool_message` when the combined output exceeded
    /// INLINE_CAP: the path the full output was written to.
    pub spilled: Option<String>,
    /// True when the tool returned early but the process is still
    /// running as a background shell — Ctrl+B×2 demote or block-budget
    /// expiry. UI renders `⏳` instead of `✓/✗`.
    pub demoted: bool,
}

impl ToolOutcome {
    /// Text form for the Ollama `role:"tool"` message — what the model
    /// sees. Small outputs inline as before; big ones (already spilled
    /// to disk by `execute`) carry the path plus a head/tail preview so
    /// the model usually doesn't need to re-read the file.
    pub fn to_tool_message(&self) -> String {
        // Error/timeout results have no output to spill.
        if self.result.error.is_some() || self.result.timed_out {
            return self.result.to_text();
        }
        match &self.spilled {
            Some(path) => {
                let combined_len = self.result.stdout.len() + self.result.stderr.len();
                let head: String = self.result.stdout.chars().take(PREVIEW_CHARS).collect();
                let tail: String = self
                    .result
                    .stdout
                    .chars()
                    .rev()
                    .take(PREVIEW_CHARS / 2)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect();
                let mut msg = format!(
                    "[output saved to {path} — {combined_len} chars]\n{head}\n… {combined_len} chars total …\n{tail}"
                );
                if !self.result.stderr.is_empty() {
                    let stail: String = self
                        .result
                        .stderr
                        .chars()
                        .rev()
                        .take(500)
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect();
                    msg.push_str(&format!("\nstderr (tail): {stail}"));
                }
                if let Some(c) = self.result.code
                    && c != 0
                {
                    msg.push_str(&format!("\nexit {c}"));
                }
                msg
            }
            // Couldn't write the file — fall back to the truncated flat form.
            None => self.result.to_text(),
        }
    }
}

/// Write a result's stdout+stderr to a jobs file; returns the path.
fn spill_output(result: &ShellResult) -> Option<String> {
    let dir = jobs_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    static JOB: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = JOB.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = format!("{}/job-{}-{n}.log", dir, std::process::id());
    let mut body = String::with_capacity(result.stdout.len() + result.stderr.len() + 64);
    body.push_str(&result.stdout);
    if !result.stderr.is_empty() {
        body.push_str("\n─── stderr ───\n");
        body.push_str(&result.stderr);
    }
    std::fs::write(&path, body).ok()?;
    Some(path)
}

/// Launch-time project dir set by `App::new` — spill files land in
/// `<launch-cwd>/.oxigolem/jobs` so they stay with the project the
/// session was opened in (independent of the shared shell's cwd).
static PROJECT_JOBS_DIR: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Capture the project jobs dir once, at app start. Creates the folder
/// eagerly so it exists even if no tool ever spills.
pub fn init_project_jobs_dir(launch_cwd: &std::path::Path) {
    let dir = launch_cwd.join(".oxigolem").join("jobs");
    let _ = std::fs::create_dir_all(&dir);
    let _ = PROJECT_JOBS_DIR.set(dir.to_string_lossy().into_owned());
}

/// Where spilled outputs + bg-shell logs live: `$OXIGOLEM_JOBS_DIR`
/// override (tests), else the project `.oxigolem/jobs` when
/// `init_project_jobs_dir` ran, else a unique tmp dir per process —
/// keeps parallel test runs from colliding on `sh1.log`.
pub fn jobs_dir_path() -> Option<std::path::PathBuf> {
    if let Ok(d) = std::env::var("OXIGOLEM_JOBS_DIR") {
        return Some(d.into());
    }
    if let Some(d) = PROJECT_JOBS_DIR.get() {
        return Some(d.clone().into());
    }
    // Per-process fallback under the system temp dir so concurrent
    // test binaries can't clobber each other's `shN.log`.
    static FALLBACK: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let dir = FALLBACK.get_or_init(|| {
        let p = std::env::temp_dir().join(format!("oxigolem-jobs-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&p);
        p.to_string_lossy().into_owned()
    });
    Some(dir.clone().into())
}

fn jobs_dir() -> Option<String> {
    jobs_dir_path().map(|p| p.to_string_lossy().into_owned())
}

/// Read-only allowlist for the parallel-batch path. A command is
/// batchable only when EVERY pipeline stage is one of these verbs and
/// the whole string carries no redirection/subshell metachars. The
/// first word of each `|`-separated stage is checked; flags are free.
const BATCHABLE_VERBS: &[&str] = &[
    "grep", "rg", "cat", "ls", "sed", "head", "tail", "wc", "find", "git", "cargo", "pwd", "echo",
    "printf", "env", "uname", "df", "du", "free", "ps", "date", "which", "whoami", "hostname",
    "file", "stat", "tree",
];

/// Git subcommands that don't mutate the repo or the index.
const BATCHABLE_GIT: &[&str] = &[
    "status",
    "log",
    "diff",
    "grep",
    "show",
    "blame",
    "ls-files",
    "branch",
    "tag",
    "rev-parse",
    "describe",
    "shortlog",
    "stash",
    "remote",
];

/// Cargo subcommands that only read/build — `check`/`test`/`metadata`
/// write target/ but never touch source; the parallelism is safe.
const BATCHABLE_CARGO: &[&str] = &[
    "check",
    "test",
    "metadata",
    "tree",
    "search",
    "version",
    "pkgid",
    "verify-project",
];

/// Metachars that make a command unsafe to batch: redirection, command
/// substitution, backgrounding, sequencing. `|` is handled separately
/// by splitting into stages. `sed` is allowlisted but only with `-n`
/// (in-place `-i` would mutate) — checked below.
const UNSAFE_METACHARS: &[char] = &['>', '<', '&', ';', '$', '`', '\n'];

/// True when `cmd` is safe to run concurrently with other read-only
/// commands on the same shell cwd. Deliberately conservative — anything
/// unrecognized falls back to sequential execution.
pub fn batchable(cmd: &str) -> bool {
    if cmd.chars().any(|c| UNSAFE_METACHARS.contains(&c)) {
        return false;
    }
    for stage in cmd.split('|') {
        let mut words = stage.split_whitespace();
        let Some(verb) = words.next() else {
            return false;
        };
        // Strip path prefixes ("/bin/ls" → "ls") and env assignments
        // ("FOO=1 ls" handled by checking for '=' before the verb).
        if verb.contains('=') {
            return false;
        }
        let verb = verb.rsplit('/').next().unwrap_or(verb);
        if !BATCHABLE_VERBS.contains(&verb) {
            return false;
        }
        match verb {
            "sed" => {
                // Only `sed -n` (print) — bare `sed s/…/` streams edits
                // to stdout so it's also safe, but `-i` writes in place.
                if stage
                    .split_whitespace()
                    .any(|w| w == "-i" || w.starts_with("-i"))
                {
                    return false;
                }
            }
            "git" => {
                let sub = words.next().unwrap_or("");
                if !BATCHABLE_GIT.contains(&sub) {
                    return false;
                }
            }
            "cargo" => {
                let sub = words.next().unwrap_or("");
                if !BATCHABLE_CARGO.contains(&sub) {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

/// Every bash call — foreground or `run_in_background` — spawns through
/// `Shell::spawn_bg` so the process lives in the shared registry while
/// it runs (a shell tab appears live). Foreground shells are marked
/// transient and removed once the result is read; a demote (Ctrl+B×2,
/// which sets `demote`) clears the flag so the shell stays running as
/// a normal background tab.
///
/// Unknown tools or a missing `command` still produce an outcome —
/// tagged `error` so the UI renders the failure like a failed exit.
pub fn execute(
    shell: &Shell,
    shells: &BgShells,
    demote: &std::sync::atomic::AtomicBool,
    name: &str,
    args: &serde_json::Value,
) -> ToolOutcome {
    fn err(command: String, msg: String) -> ToolOutcome {
        ToolOutcome {
            command,
            result: ShellResult {
                stdout: String::new(),
                stderr: String::new(),
                code: None,
                duration: std::time::Duration::ZERO,
                timed_out: false,
                error: Some(msg),
            },
            spilled: None,
            demoted: false,
        }
    }

    match name {
        "bash" => {
            let cmd = args["command"].as_str().unwrap_or("").to_string();
            if cmd.is_empty() {
                return err(
                    cmd,
                    "error: bash call missing 'command' argument".to_string(),
                );
            }
            let Some(jobs) = jobs_dir_path() else {
                return err(cmd, "error: no jobs dir available".to_string());
            };
            let bg = args["run_in_background"].as_bool().unwrap_or(false);

            // Ids come from a process-wide counter — `sh1`, `sh2`, …
            // — so concurrent execute calls can never collide on a
            // filename in a shared jobs dir.
            let id = {
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
                format!(
                    "sh{}",
                    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                )
            };
            let spawned = shell.spawn_bg(&id, &cmd, &jobs, !bg);
            let bgshell = match spawned {
                Ok(b) => b,
                Err(e) => return err(cmd, format!("spawn failed: {e}")),
            };
            shells.lock().unwrap().push(bgshell.clone());

            if bg {
                return ToolOutcome {
                    command: cmd,
                    result: ShellResult {
                        stdout: format!("bg shell {id} started — log: {}", bgshell.log_path),
                        stderr: String::new(),
                        code: Some(0),
                        duration: std::time::Duration::ZERO,
                        timed_out: false,
                        error: None,
                    },
                    spilled: None,
                    demoted: false,
                };
            }

            // Foreground: wait for exit / demote / block-budget expiry.
            match shell.wait_bg(&bgshell, demote) {
                Some(result) => {
                    shells.lock().unwrap().retain(|s| s.id != bgshell.id);
                    let spilled = if result.error.is_none()
                        && !result.timed_out
                        && result.stdout.len() + result.stderr.len() > INLINE_CAP
                    {
                        spill_output(&result)
                    } else {
                        None
                    };
                    ToolOutcome {
                        command: cmd,
                        result,
                        spilled,
                        demoted: false,
                    }
                }
                None => {
                    // Demoted (Ctrl+B×2 or block budget) — keep it
                    // running as a normal bg shell and hand the model
                    // the id + log path it can poll.
                    bgshell
                        .transient
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                    ToolOutcome {
                        command: cmd,
                        result: ShellResult {
                            stdout: format!(
                                "still running as bg shell {id} — log: {}\n\
                                 Poll it with `tail {}` or check back later.",
                                bgshell.log_path, bgshell.log_path
                            ),
                            stderr: String::new(),
                            code: Some(0),
                            duration: std::time::Duration::ZERO,
                            timed_out: false,
                            error: None,
                        },
                        spilled: None,
                        demoted: true,
                    }
                }
            }
        }
        other => err(String::new(), format!("error: unknown tool '{other}'")),
    }
}
