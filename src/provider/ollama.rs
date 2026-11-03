use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::sync::Mutex;
use std::time::Duration;

use super::{ChatOptions, Provider, StreamChunk};
use crate::agent::chat::Message;
use crate::agent::tools::{self, ToolSpec};

/// What `/api/show` tells us about a model — cached in memory and on
/// disk (`.oxigolem/models.json`) so every turn doesn't re-ask.
#[derive(Clone, Debug)]
pub struct ModelInfo {
    /// `model_info.*.context_length` — the trained context window.
    pub context_length: u64,
    /// `capabilities` — e.g. ["completion","tools","thinking"].
    pub capabilities: Vec<String>,
    /// `model_info.*.parameter_count` (0 when absent).
    pub parameter_count: u64,
    /// Raw `model_info` map — `kv_bytes_per_token` digs the geometry out.
    pub raw: serde_json::Value,
}

impl ModelInfo {
    pub fn supports(&self, cap: &str) -> bool {
        self.capabilities.iter().any(|c| c == cap)
    }

    /// Whether reasoning should be sent as `think:true`. Default is
    /// off — thinking eats context a small model can't spare. A
    /// per-model override in `.oxigolem/config.json`
    /// (`models.<name>.think`) flips it back on.
    pub fn think_for(&self, model: &str) -> bool {
        if let Some(v) = config_model(model, "think") {
            return v;
        }
        self.think_default()
    }

    /// Default think flag: always off (we keep reasoning visible via
    /// the stream but don't pay its prefill unless the user asks).
    pub fn think_default(&self) -> bool {
        false
    }
}

/// Read a per-model knob out of `.oxigolem/config.json`.
/// `$OXIGOLEM_CONFIG_DIR` overrides the directory (tests).
fn config_model(model: &str, key: &str) -> Option<bool> {
    let dir = config_dir()?;
    let text = std::fs::read_to_string(dir.join("config.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v["models"][model][key].as_bool()
}

fn config_dir() -> Option<std::path::PathBuf> {
    if let Ok(d) = std::env::var("OXIGOLEM_CONFIG_DIR") {
        return Some(d.into());
    }
    std::env::current_dir().ok().map(|d| d.join(".oxigolem"))
}

/// KV-cache footprint of one token: 2 (K+V) × layers × kv_heads ×
/// head_dim × 2 bytes. Used to budget num_ctx against free RAM.
/// Returns None when the geometry isn't in model_info.
pub fn kv_bytes_per_token(model_info: &serde_json::Value) -> Option<u64> {
    // Find the arch-prefixed namespace (e.g. "qwen3.*") by locating
    // any key that ends with ".block_count".
    let obj = model_info.as_object()?;
    let prefix = obj.keys().find_map(|k| {
        k.strip_suffix(".block_count").map(|p| p.to_string())
    })?;
    let get = |suffix: &str| obj.get(&format!("{prefix}{suffix}"))?.as_u64();
    let layers = get(".block_count")?;
    let kv_heads = get(".attention.head_count_kv")?;
    let heads = get(".attention.head_count")?;
    let emb = get(".embedding_length")?;
    if heads == 0 {
        return None;
    }
    let head_dim = emb / heads;
    Some(2 * layers * kv_heads * head_dim * 2)
}

/// Pick the context window: `OXIGOLEM_NUM_CTX` env beats everything;
/// otherwise the smaller of the model's trained context and what the
/// RAM budget can hold (¼ of available bytes for KV), never under 8192.
pub fn resolve_num_ctx(model_ctx: u64, avail_ram: u64, kv_per_token: Option<u64>) -> u64 {
    const FLOOR: u64 = 8192;
    if let Ok(v) = std::env::var("OXIGOLEM_NUM_CTX")
        && let Ok(n) = v.trim().parse::<u64>() {
            return n.max(FLOOR.min(n)); // env wins outright, even below floor
        }
    let budgeted = kv_per_token
        .and_then(|b| (avail_ram / 4).checked_div(b))
        .unwrap_or(0);
    model_ctx.min(budgeted).max(FLOOR)
}

/// Free RAM for the KV budget — MemAvailable from /proc/meminfo,
/// 8 GiB fallback when unreadable.
pub fn avail_ram_bytes() -> u64 {
    let text = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemAvailable:")
            && let Some(kb) = rest.split_whitespace().next().and_then(|v| v.parse::<u64>().ok()) {
                return kb * 1024;
            }
    }
    8 << 30
}

pub struct Ollama {
    host: String,
    agent: ureq::Agent,
    /// inspect() memoization — model name → ModelInfo.
    info_cache: Mutex<HashMap<String, ModelInfo>>,
}

impl Ollama {
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            agent: new_agent(),
            info_cache: Mutex::new(HashMap::new()),
        }
    }

    /// Fetch `context_length`/`capabilities`/geometry via `/api/show`.
    /// Memoized in memory and persisted to `.oxigolem/models.json` —
    /// model cards don't change between turns.
    pub fn inspect(&self, model: &str) -> Result<ModelInfo, String> {
        if let Some(info) = self.info_cache.lock().unwrap().get(model) {
            return Ok(info.clone());
        }
        if let Some(info) = read_model_cache(model) {
            self.info_cache
                .lock()
                .unwrap()
                .insert(model.to_string(), info.clone());
            return Ok(info);
        }

        let url = format!("{}/api/show", self.host.trim_end_matches('/'));
        let body: serde_json::Value = self
            .agent
            .post(&url)
            .send_json(serde_json::json!({ "model": model }))
            .map_err(|e| chat_error(&url, e))?
            .into_json()
            .map_err(|e| format!("bad /api/show response: {e}"))?;

        let raw = body["model_info"].clone();
        let obj = raw.as_object().cloned().unwrap_or_default();
        // Arch-prefixed lookup — find "<arch>.context_length".
        let get = |suffix: &str| -> u64 {
            obj.iter()
                .find(|(k, _)| k.ends_with(suffix))
                .and_then(|(_, v)| v.as_u64())
                .unwrap_or(0)
        };
        let info = ModelInfo {
            context_length: get(".context_length"),
            capabilities: body["capabilities"]
                .as_array()
                .map(|a| a.iter().filter_map(|c| c.as_str().map(str::to_string)).collect())
                .unwrap_or_default(),
            parameter_count: get(".parameter_count"),
            raw,
        };
        write_model_cache(model, &info);
        self.info_cache
            .lock()
            .unwrap()
            .insert(model.to_string(), info.clone());
        Ok(info)
    }
}

fn model_cache_path() -> Option<std::path::PathBuf> {
    config_dir().map(|d| d.join("models.json"))
}

fn read_model_cache(model: &str) -> Option<ModelInfo> {
    let text = std::fs::read_to_string(model_cache_path()?).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let e = &v[model];
    if e.is_null() {
        return None;
    }
    Some(ModelInfo {
        context_length: e["context_length"].as_u64()?,
        capabilities: e["capabilities"]
            .as_array()
            .map(|a| a.iter().filter_map(|c| c.as_str().map(str::to_string)).collect())
            .unwrap_or_default(),
        parameter_count: e["parameter_count"].as_u64().unwrap_or(0),
        raw: e["model_info"].clone(),
    })
}

fn write_model_cache(model: &str, info: &ModelInfo) {
    let Some(path) = model_cache_path() else { return };
    let mut v: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    v[model] = serde_json::json!({
        "context_length": info.context_length,
        "capabilities": info.capabilities,
        "parameter_count": info.parameter_count,
        "model_info": info.raw,
    });
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(s) = serde_json::to_string_pretty(&v) {
        let _ = std::fs::write(&path, s);
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
            info_cache: Mutex::new(HashMap::new()),
        }
    }
}

impl Provider for Ollama {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

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
        opts: &ChatOptions,
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
            "keep_alive": "30m",
            "think": opts.think,
            "options": {
                "num_ctx": opts.num_ctx,
                "num_predict": 2048,
                "temperature": 0.1,
                "top_p": 0.9,
            },
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
                    // The done chunk carries the counters — emit Usage
                    // before breaking so the footer can show hit rate
                    // and tokens/sec for the turn.
                    got_any = true;
                    on_chunk(StreamChunk::Usage(super::Usage {
                        prompt: v["prompt_eval_count"].as_u64().unwrap_or(0),
                        cached: v["prompt_eval_cached_count"].as_u64().unwrap_or(0),
                        eval: v["eval_count"].as_u64().unwrap_or(0),
                        prefill_ms: v["prompt_eval_duration"].as_u64().unwrap_or(0) / 1_000_000,
                        eval_ms: v["eval_duration"].as_u64().unwrap_or(0) / 1_000_000,
                        load_ms: v["load_duration"].as_u64().unwrap_or(0) / 1_000_000,
                    }));
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
