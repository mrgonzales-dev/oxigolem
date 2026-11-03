pub mod ollama;

use std::sync::Arc;

use crate::chat::Message;

/// A model backend oxigolem can connect to.
pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;
    fn list_models(&self) -> Result<Vec<String>, String>;

    /// Stream a chat completion. `on_token` is called for each text chunk.
    fn chat(
        &self,
        model: &str,
        messages: &[Message],
        on_token: &dyn Fn(String),
    ) -> Result<(), String>;
}

/// Providers the user can pick in the connect modal.
pub fn registry() -> Vec<Arc<dyn Provider>> {
    vec![Arc::new(ollama::Ollama::default())]
}
