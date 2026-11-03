use std::io::{BufRead, BufReader};
use std::time::Duration;

use super::Provider;
use crate::chat::Message;

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
        on_token: &dyn Fn(String),
    ) -> Result<(), String> {
        let url = format!("{}/api/chat", self.host.trim_end_matches('/'));

        let messages_json: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| serde_json::json!({ "role": m.role, "content": m.content }))
            .collect();

        let body = serde_json::json!({
            "model": model,
            "messages": messages_json,
            "stream": true,
        });

        let response = self
            .agent
            .post(&url)
            .send_json(body)
            .map_err(|e| chat_error(&url, e))?;

        let reader = BufReader::new(response.into_reader());
        let mut got_any = false;

        for line in reader.lines() {
            let line = line.map_err(|e| format!("stream read error: {e}"))?;
            if line.is_empty() {
                continue;
            }
            let v: serde_json::Value =
                serde_json::from_str(&line).map_err(|e| format!("bad NDJSON line: {e}"))?;

            if v["done"].as_bool() == Some(true) {
                break;
            }
            if let Some(content) = v["message"]["content"].as_str()
                && !content.is_empty()
            {
                got_any = true;
                on_token(content.to_string());
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
