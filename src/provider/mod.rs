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

/// A model backend oxigolem can connect to.
pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;
    fn list_models(&self) -> Result<Vec<String>, String>;

    /// Stream a chat completion. `on_chunk` is called for each chunk, tagged
    /// by whether it's answer text, reasoning, or a tool call. `tools` is
    /// the registry of tools the model may invoke (empty = none offered).
    fn chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolSpec],
        on_chunk: &dyn Fn(StreamChunk),
    ) -> Result<(), String>;
}

/// Providers the user can pick in the connect modal.
pub fn registry() -> Vec<Arc<dyn Provider>> {
    vec![Arc::new(ollama::Ollama::default())]
}
