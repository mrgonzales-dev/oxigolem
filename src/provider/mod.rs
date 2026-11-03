pub mod ollama;

use std::sync::Arc;

use crate::chat::Message;

/// A piece of a streamed reply: the visible answer or the model's reasoning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamChunk {
    /// Answer text shown to the user.
    Content(String),
    /// Reasoning text (thinking models) shown dimmed, separate from the answer.
    Thinking(String),
}

/// A model backend oxigolem can connect to.
pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;
    fn list_models(&self) -> Result<Vec<String>, String>;

    /// Stream a chat completion. `on_chunk` is called for each chunk, tagged
    /// by whether it's answer text or reasoning.
    fn chat(
        &self,
        model: &str,
        messages: &[Message],
        on_chunk: &dyn Fn(StreamChunk),
    ) -> Result<(), String>;
}

/// Providers the user can pick in the connect modal.
pub fn registry() -> Vec<Arc<dyn Provider>> {
    vec![Arc::new(ollama::Ollama::default())]
}
