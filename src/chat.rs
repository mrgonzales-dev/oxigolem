use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::thread;

use crate::provider::{Provider, StreamChunk};

/// A single chat message in the Ollama API format.
#[derive(Clone)]
pub struct Message {
    pub role: String,
    pub content: String,
}

/// Events streamed from the background thread to the UI.
pub enum ChatEvent {
    /// Answer text for the current reply block.
    Token(String),
    /// Reasoning text, rendered in its own dimmed block.
    Thinking(String),
    Done,
    Error(String),
}

pub struct ChatEngine {
    provider: Arc<dyn Provider>,
    model: String,
    history: Vec<Message>,
    rx: Option<Receiver<ChatEvent>>,
}

impl ChatEngine {
    pub fn new(provider: Arc<dyn Provider>, model: String) -> Self {
        Self {
            provider,
            model,
            history: Vec::new(),
            rx: None,
        }
    }

    pub fn is_busy(&self) -> bool {
        self.rx.is_some()
    }

    /// Start a generation on a background thread.
    pub fn send(&mut self, user_text: String) -> Result<(), String> {
        if self.is_busy() {
            return Err("generation in progress".to_string());
        }

        self.history.push(Message {
            role: "user".to_string(),
            content: user_text,
        });

        let (tx, rx) = std::sync::mpsc::channel::<ChatEvent>();
        self.rx = Some(rx);

        let provider = Arc::clone(&self.provider);
        let model = self.model.clone();
        let messages = self.history.clone();

        thread::spawn(move || {
            let tx_cb = tx.clone();
            let result = provider.chat(&model, &messages, &|chunk: StreamChunk| {
                let ev = match chunk {
                    StreamChunk::Content(t) => ChatEvent::Token(t),
                    StreamChunk::Thinking(t) => ChatEvent::Thinking(t),
                };
                let _ = tx_cb.send(ev);
            });
            match result {
                Ok(()) => { let _ = tx.send(ChatEvent::Done); }
                Err(e) => { let _ = tx.send(ChatEvent::Error(e)); }
            }
        });

        Ok(())
    }

    /// Drain all pending events. Called every event-loop iteration.
    pub fn poll(&mut self) -> Vec<ChatEvent> {
        let Some(rx) = &self.rx else { return Vec::new() };
        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            let terminal = matches!(ev, ChatEvent::Done | ChatEvent::Error(_));
            events.push(ev);
            if terminal {
                break;
            }
        }
        if events.iter().any(|e| matches!(e, ChatEvent::Done | ChatEvent::Error(_))) {
            self.rx = None;
        }
        events
    }

    /// Append the assistant response to history after streaming completes.
    pub fn record_assistant(&mut self, text: String) {
        self.history.push(Message {
            role: "assistant".to_string(),
            content: text,
        });
    }

    pub fn history(&self) -> &[Message] {
        &self.history
    }
}
