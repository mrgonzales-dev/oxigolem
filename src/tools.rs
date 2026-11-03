use crate::shell::Shell;

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

/// Run a tool call and return its output as the tool message content.
pub fn execute(shell: &Shell, name: &str, args: &serde_json::Value) -> String {
    match name {
        "bash" => {
            let Some(cmd) = args["command"].as_str() else {
                return "error: bash call missing 'command' argument".to_string();
            };
            shell.run(cmd)
        }
        other => format!("error: unknown tool '{other}'"),
    }
}
