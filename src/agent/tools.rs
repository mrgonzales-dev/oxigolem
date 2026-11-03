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

/// Outcome of a tool call. For `bash` this wraps the `ShellResult`; for
/// unknown/bad calls it carries a synthesized error so the UI can still
/// render it like a finished block.
#[derive(Clone, Debug)]
pub struct ToolOutcome {
    /// Command the tool actually ran (for bash, `args.command`).
    pub command: String,
    pub result: ShellResult,
}

impl ToolOutcome {
    /// Text form for the Ollama `role:"tool"` message — what the model
    /// sees. Identical to the old flattened `Shell::run` output.
    pub fn to_tool_message(&self) -> String {
        self.result.to_text()
    }
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
            ToolOutcome {
                command: cmd,
                result,
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
        },
    }
}
