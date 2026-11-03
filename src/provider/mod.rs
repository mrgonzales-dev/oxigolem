pub mod ollama;

use std::sync::Arc;

use crate::agent::chat::Message;
use crate::agent::tools::ToolSpec;

/// Per-request token + timing counters, parsed from Ollama's `done`
/// chunk. Derived metrics (hit rate, tokens/sec) live on the struct so
/// the UI never re-derives them.
#[derive(Clone, Debug, PartialEq)]
pub struct Usage {
    /// Tokens fed to the model this turn (`prompt_eval_count`).
    pub prompt: u64,
    /// Prompt tokens served from the KV cache (`prompt_eval_cached_count`,
    /// 0 on servers that don't report it).
    pub cached: u64,
    /// Tokens generated (`eval_count`).
    pub eval: u64,
    /// Time spent in prefill (`prompt_eval_duration`, ns → ms).
    pub prefill_ms: u64,
    /// Time spent generating (`eval_duration`, ns → ms).
    pub eval_ms: u64,
    /// Model load time (`load_duration`, ns → ms).
    pub load_ms: u64,
}

impl Usage {
    /// Cache hit rate as a percentage 0-100. Zero prompt → 0.
    pub fn cached_pct(&self) -> f64 {
        if self.prompt == 0 {
            0.0
        } else {
            self.cached as f64 / self.prompt as f64 * 100.0
        }
    }

    /// Generation throughput in tokens/sec. Zero eval time → 0.
    pub fn tokens_per_sec(&self) -> f64 {
        if self.eval_ms == 0 {
            0.0
        } else {
            self.eval as f64 / (self.eval_ms as f64 / 1000.0)
        }
    }
}

/// A piece of a streamed reply: the visible answer, the model's reasoning,
/// or a request to run a tool.
#[derive(Clone, Debug, PartialEq)]
pub enum StreamChunk {
    /// Answer text shown to the user.
    Content(String),
    /// Reasoning text (thinking models) shown dimmed, separate from the answer.
    Thinking(String),
    /// The model asked to run a tool (Ollama `message.tool_calls`).
    ToolCall {
        name: String,
        arguments: serde_json::Value,
    },
    /// Token + timing counters, emitted once on the `done` chunk.
    Usage(Usage),
}

/// Per-request generation options resolved once when a model is
/// selected (Phase 1: inspect → num_ctx → think). Passed through to
/// `Provider::chat` so every turn uses the same knobs.
#[derive(Clone, Debug, PartialEq)]
pub struct ChatOptions {
    /// Context window sent as `options.num_ctx`.
    pub num_ctx: u64,
    /// `think` field — false disables reasoning on thinking models.
    pub think: bool,
}

impl Default for ChatOptions {
    fn default() -> Self {
        Self {
            num_ctx: 8192,
            think: true,
        }
    }
}

/// A model backend oxigolem can connect to.
pub trait Provider: Send + Sync + std::any::Any + 'static {
    /// Lets the app reach provider-specific extras (e.g. Ollama::inspect).
    fn as_any(&self) -> &dyn std::any::Any;
    fn name(&self) -> &'static str;
    fn list_models(&self) -> Result<Vec<String>, String>;

    /// Stream a chat completion. `on_chunk` is called for each chunk, tagged
    /// by whether it's answer text, reasoning, or a tool call. `tools` is
    /// the registry of tools the model may invoke (empty = none offered).
    /// `opts` carries the resolved generation options.
    fn chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolSpec],
        opts: &ChatOptions,
        on_chunk: &dyn Fn(StreamChunk),
    ) -> Result<(), String>;
}

/// Providers the user can pick in the connect modal.
pub fn registry() -> Vec<Arc<dyn Provider>> {
    vec![Arc::new(ollama::Ollama::default())]
}

/// Last model lists per provider, kept at `.oxigolem/model-list.json`.
/// The connect flow reads it so a dead provider can still show the
/// picker instantly; a successful live list overwrites the cache.
fn model_list_path() -> Option<std::path::PathBuf> {
    ollama::config_dir().map(|d| d.join("model-list.json"))
}

pub fn read_model_list(provider: &str) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(model_list_path()?).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v[provider]
        .as_array()
        .map(|a| a.iter().filter_map(|m| m.as_str().map(str::to_string)).collect())
}

pub fn write_model_list(provider: &str, models: &[String]) {
    let Some(path) = model_list_path() else { return };
    let mut v: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    v[provider] = serde_json::json!(models);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(s) = serde_json::to_string_pretty(&v) {
        let _ = std::fs::write(&path, s);
    }
}
