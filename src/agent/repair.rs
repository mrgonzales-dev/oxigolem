//! Small-model tool-call repair. Models without the "tools" capability
//! often still *try* to call tools — they emit fenced ```bash blocks,
//! bare `{"command": "..."}` JSON, or prose containing `{"name":"bash"…}`.
//! When `fenced` mode is on, the engine runs `recover` over the text of
//! a reply that carried no native `tool_calls`; a recovered call is
//! executed like a real one and the loop continues.
//!
//! Repair is best-effort: a malformed candidate is skipped, never an
//! error. Unknown tool names get one retry (the model sees "unknown
//! tool" as the tool result and usually answers in text next turn).

use super::chat::ToolCall;

/// Scan a reply's text for an implied tool call. Tries, in order:
/// 1. Fenced ```bash / ```sh block → `bash {"command": <contents>}`
/// 2. Bare JSON object with a `"command"` key → `bash`
/// 3. `{"name": "<tool>", "arguments"/"args": {...}}` anywhere in text
///
/// Returns the first plausible call; `None` means ordinary prose.
pub fn recover(content: &str, valid_tools: &[&str]) -> Option<ToolCall> {
    recover_fenced(content).or_else(|| recover_bare_json(content, valid_tools))
}

/// ```bash\n<cmd>\n``` or ```sh\n<cmd>\n``` → bash tool call.
fn recover_fenced(content: &str) -> Option<ToolCall> {
    for lang in ["```bash", "```sh", "```shell"] {
        if let Some(start) = content.find(lang) {
            let body_start = start + lang.len();
            let rest = &content[body_start..];
            // Skip the newline right after the language tag.
            let rest = rest.strip_prefix('\n').unwrap_or(rest);
            if let Some(end) = rest.find("```") {
                let cmd = rest[..end].trim();
                if !cmd.is_empty() {
                    return Some(ToolCall {
                        name: "bash".to_string(),
                        arguments: serde_json::json!({"command": cmd}),
                    });
                }
            }
        }
    }
    None
}

/// A standalone JSON object in the text: either a direct args object
/// (`{"command": "ls"}`) or a call envelope (`{"name": "bash",
/// "arguments": {"command": "ls"}}`). `valid_tools` filters envelopes
/// so `{"name": "get_weather"…}` doesn't fabricate a call we can't run.
fn recover_bare_json(content: &str, valid_tools: &[&str]) -> Option<ToolCall> {
    // Find balanced {...} spans and try each — cheaper than a parser
    // and good enough for single-line model JSON.
    let bytes = content.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{'
            && let Some(span) = balanced(&content[i..])
        {
            let candidate = &content[i..i + span];
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(candidate)
                && let Some(call) = json_to_call(&v, valid_tools)
            {
                return Some(call);
            }
            i += span;
            continue;
        }
        i += 1;
    }
    None
}

/// Length of the balanced `{...}` span starting at s[0]=='{', or None.
/// String-aware so braces inside quotes don't count.
fn balanced(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    for (idx, c) in s.char_indices() {
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(idx + 1);
                }
            }
            _ => {}
        }
    }
    None
}

fn json_to_call(v: &serde_json::Value, valid_tools: &[&str]) -> Option<ToolCall> {
    // Envelope: {"name": "…", "arguments": {...}} or {"args": {...}}.
    if let Some(name) = v["name"].as_str() {
        if !valid_tools.iter().any(|t| t == &name) {
            return None;
        }
        let args = if v["arguments"].is_null() {
            v["args"].clone()
        } else {
            v["arguments"].clone()
        };
        if args.is_object() || args.is_null() {
            return Some(ToolCall {
                name: name.to_string(),
                arguments: if args.is_null() {
                    serde_json::json!({})
                } else {
                    args
                },
            });
        }
        return None;
    }
    // Direct args for the single bash tool: {"command": "ls"}.
    if let Some(cmd) = v["command"].as_str() {
        return Some(ToolCall {
            name: "bash".to_string(),
            arguments: serde_json::json!({"command": cmd}),
        });
    }
    None
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn fenced_bash_recovers_command() {
        let c = recover("Let me check:\n```bash\nls -la\n```\nDone.", &["bash"]).unwrap();
        assert_eq!(c.name, "bash");
        assert_eq!(c.arguments["command"], "ls -la");
    }

    #[test]
    fn bare_command_json_recovers() {
        let c = recover("run this {\"command\": \"pwd\"} now", &["bash"]).unwrap();
        assert_eq!(c.name, "bash");
        assert_eq!(c.arguments["command"], "pwd");
    }

    #[test]
    fn envelope_json_recovers() {
        let c = recover(
            "{\"name\": \"bash\", \"arguments\": {\"command\": \"ls\"}}",
            &["bash"],
        )
        .unwrap();
        assert_eq!(c.name, "bash");
        assert_eq!(c.arguments["command"], "ls");
    }

    #[test]
    fn unknown_tool_envelope_rejected() {
        assert!(
            recover("{\"name\": \"get_weather\", \"arguments\": {}}", &["bash"]).is_none()
        );
    }

    #[test]
    fn prose_without_call_passes() {
        assert!(recover("The answer is 42.", &["bash"]).is_none());
        assert!(recover("Use `ls` to list files.", &["bash"]).is_none());
    }

    #[test]
    fn braces_inside_strings_dont_break_balance() {
        let c = recover("{\"command\": \"echo '}'\"}", &["bash"]).unwrap();
        assert_eq!(c.arguments["command"], "echo '}'");
    }
}
