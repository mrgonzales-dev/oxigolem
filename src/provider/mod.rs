pub mod ollama;

use std::sync::Arc;

use crate::chat::Message;
use crate::tools::ToolSpec;

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
