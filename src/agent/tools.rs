use crate::shell::{Shell, ShellResult};

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
                      directory and side effects persist.",
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "the bash command to run"
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

/// Where spilled outputs live: `$OXIGOLEM_JOBS_DIR` override (tests),
/// else `~/.cache/oxigolem/jobs`.
fn jobs_dir() -> Option<String> {
    if let Ok(d) = std::env::var("OXIGOLEM_JOBS_DIR") {
        return Some(d);
    }
    let home = std::env::var("HOME").ok()?;
    Some(format!("{home}/.cache/oxigolem/jobs"))
}

/// Run a tool call and return its structured outcome. Unknown tools or a
/// missing `command` still produce an outcome — tagged `error` so the UI
/// can render the failure the same way as a failed exit code.
pub fn execute(shell: &Shell, name: &str, args: &serde_json::Value) -> ToolOutcome {
    match name {
        "bash" => {
            let cmd = args["command"].as_str().unwrap_or("").to_string();
            let result = if cmd.is_empty() {
                ShellResult {
                    stdout: String::new(),
                    stderr: String::new(),
                    code: None,
                    duration: std::time::Duration::ZERO,
                    timed_out: false,
                    error: Some("error: bash call missing 'command' argument".to_string()),
                }
            } else {
                shell.run_result(&cmd)
            };
            // Big outputs go to disk now — the model gets the path +
            // preview via `to_tool_message`, and can grep the file for
            // the rest. Errors/timeouts have nothing worth spilling.
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
            }
        }
        other => ToolOutcome {
            command: String::new(),
            result: ShellResult {
                stdout: String::new(),
                stderr: String::new(),
                code: None,
                duration: std::time::Duration::ZERO,
                timed_out: false,
                error: Some(format!("error: unknown tool '{other}'")),
            },
            spilled: None,
        },
    }
}
