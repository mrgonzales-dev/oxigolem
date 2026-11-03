pub mod ollama;

/// A model backend oxigolem can connect to.
pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;
    fn list_models(&self) -> Result<Vec<String>, String>;
}

/// Providers the user can pick in the connect modal.
pub fn registry() -> Vec<Box<dyn Provider>> {
    vec![Box::new(ollama::Ollama::default())]
}
