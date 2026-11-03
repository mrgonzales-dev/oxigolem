use std::io::{BufRead, BufReader};
use std::time::Duration;

use super::{Provider, StreamChunk};
use crate::chat::Message;
use crate::tools::{self, ToolSpec};

pub struct Ollama {
    host: String,
    agent: ureq::Agent,
}

impl Ollama {
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            agent: new_agent(),
        }
    }
}

/// Agent for chat: no total deadline (streams can be long), generous
/// per-read timeout so a stalled connection errors out, and a short
/// connect timeout so a dead host fails fast.
fn new_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(300))
        .build()
}

impl Default for Ollama {
    fn default() -> Self {
        let host = std::env::var("OLLAMA_HOST")
            .ok()
            .map(|h| {
                if h.starts_with("http") {
                    h
                } else {
                    format!("http://{h}")
                }
            })
            .unwrap_or_else(|| "http://localhost:11434".to_string());
        Self {
            host,
            agent: new_agent(),
        }
    }
}

impl Provider for Ollama {
    fn name(&self) -> &'static str {
        "ollama"
    }

    fn list_models(&self) -> Result<Vec<String>, String> {
        let url = format!("{}/api/tags", self.host.trim_end_matches('/'));
        let body: serde_json::Value = ureq::get(&url)
            .timeout(Duration::from_secs(3))
            .call()
            .map_err(|e| format!("cannot reach ollama at {}: {e}", self.host))?
            .into_json()
            .map_err(|e| format!("bad response: {e}"))?;

        let models = body["models"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|m| m["name"].as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        if models.is_empty() {
            Err("ollama has no models installed".to_string())
        } else {
            Ok(models)
        }
    }

    fn chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolSpec],
        on_chunk: &dyn Fn(StreamChunk),
    ) -> Result<(), String> {
        let url = format!("{}/api/chat", self.host.trim_end_matches('/'));

        let messages_json: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| {
                let mut v = serde_json::json!({ "role": m.role, "content": m.content });
                if let Some(calls) = &m.tool_calls {
                    v["tool_calls"] = serde_json::Value::Array(
                        calls
                            .iter()
                            .map(|c| {
                                serde_json::json!({
                                    "function": {
                                        "name": c.name,
                                        "arguments": c.arguments,
                                    }
                                })
                            })
                            .collect(),
                    );
                }
                if let Some(name) = &m.name {
                    v["name"] = serde_json::Value::String(name.clone());
                }
                v
            })
            .collect();

        let mut body = serde_json::json!({
            "model": model,
            "messages": messages_json,
            "stream": true,
        });
        if !tools.is_empty() {
            body["tools"] = tools::to_json(tools);
        }

        let response = self
            .agent
            .post(&url)
            .send_json(body)
            .map_err(|e| chat_error(&url, e))?;

        let mut reader = BufReader::new(response.into_reader());
        let mut got_any = false;
        let mut finished = false;

        let mut buf = String::new();
        loop {
            buf.clear();
            match reader.read_line(&mut buf) {
                // Clean EOF: connection closed (with or without done:true).
                Ok(0) => break,
                Ok(_) => {}
                // A mid-stream read/decode error (e.g. "Error while decoding
                // chunks" when the server resets between chunks) is only fatal
                // if we got no usable text yet.
                Err(e) => {
                    if got_any {
                        break;
                    }
                    return Err(format!("stream read error: {e}"));
                }
            }

            for line in buf.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue; // skip partial/garbled line
                };
                // Tool calls can arrive on the same chunk as done:true —
                // parse them BEFORE the done check or they're dropped.
                if let Some(calls) = v["message"]["tool_calls"].as_array() {
                    for c in calls {
                        let name = c["function"]["name"].as_str().unwrap_or("").to_string();
                        // Ollama emits arguments as an object, but some
                        // models send a JSON *string* — parse it back.
                        let args = match &c["function"]["arguments"] {
                            serde_json::Value::String(s) => {
                                serde_json::from_str(s).unwrap_or(serde_json::json!({}))
                            }
                            serde_json::Value::Null => serde_json::json!({}),
                            other => other.clone(),
                        };
                        got_any = true;
                        on_chunk(StreamChunk::ToolCall {
                            name,
                            arguments: args,
                        });
                    }
                }
                if v["done"].as_bool() == Some(true) {
                    finished = true;
                    break;
                }
                // Reasoning lives in `thinking`, the answer in `content`.
                // Emit them as separate chunk types so the UI can render
                // thinking in its own dimmed block above the reply.
                if let Some(t) = v["message"]["thinking"].as_str()
                    && !t.is_empty()
                {
                    got_any = true;
                    on_chunk(StreamChunk::Thinking(t.to_string()));
                }
                if let Some(c) = v["message"]["content"].as_str()
                    && !c.is_empty()
                {
                    got_any = true;
                    on_chunk(StreamChunk::Content(c.to_string()));
                }
            }
            if finished {
                break;
            }
        }

        if !got_any {
            return Err("empty response from model".to_string());
        }
        Ok(())
    }
}

/// Build a useful error string. For HTTP status errors, pull the server's
/// `{"error": "..."}` message out of the body so the user sees the real cause.
fn chat_error(url: &str, e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, resp) => {
            let detail = resp
                .into_json::<serde_json::Value>()
                .ok()
                .and_then(|v| v["error"].as_str().map(str::to_string))
                .unwrap_or_else(|| format!("status code {code}"));
            format!("chat request failed: {url}: {detail}")
        }
        ureq::Error::Transport(t) => format!("chat request failed: {url}: {t}"),
    }
}
