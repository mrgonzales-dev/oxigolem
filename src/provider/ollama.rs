use std::time::Duration;

use super::Provider;

pub struct Ollama {
    host: String,
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
        Self { host }
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
}
