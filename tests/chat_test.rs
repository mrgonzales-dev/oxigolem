use oxigolem::chat::{ChatEngine, ChatEvent, Message};
use oxigolem::provider::Provider;
use std::sync::{Arc, Mutex};

/// A mock provider that emits tokens from a predefined list.
struct MockProvider {
    tokens: Mutex<Vec<String>>,
}

impl Provider for MockProvider {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn list_models(&self) -> Result<Vec<String>, String> {
        Ok(vec!["mock-model".to_string()])
    }

    fn chat(
        &self,
        _model: &str,
        _messages: &[Message],
        on_token: &dyn Fn(String),
    ) -> Result<(), String> {
        let tokens = self.tokens.lock().unwrap();
        for t in tokens.iter() {
            on_token(t.clone());
        }
        Ok(())
    }
}

#[test]
fn chat_engine_streams_tokens_and_finishes() {
    let provider = Arc::new(MockProvider {
        tokens: Mutex::new(vec!["Hello".to_string(), " world".to_string()]),
    });
    let mut engine = ChatEngine::new(provider, "mock-model".to_string());

    assert!(!engine.is_busy());
    engine.send("hi".to_string()).unwrap();
    assert!(engine.is_busy());

    // Give the background thread time to run.
    std::thread::sleep(std::time::Duration::from_millis(100));

    let events = engine.poll();
    let tokens: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            ChatEvent::Token(t) => Some(t.clone()),
            _ => None,
        })
        .collect();

    assert_eq!(tokens, vec!["Hello", " world"]);
    assert!(events.iter().any(|e| matches!(e, ChatEvent::Done)));
    assert!(!engine.is_busy());
}

#[test]
fn chat_engine_records_assistant_message() {
    let provider = Arc::new(MockProvider {
        tokens: Mutex::new(vec!["response".to_string()]),
    });
    let mut engine = ChatEngine::new(provider, "mock-model".to_string());

    engine.send("question".to_string()).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));

    let events = engine.poll();
    assert!(events.iter().any(|e| matches!(e, ChatEvent::Token(_))));
    assert!(events.iter().any(|e| matches!(e, ChatEvent::Done)));

    engine.record_assistant("response".to_string());
    assert_eq!(engine.history().len(), 2);
    assert_eq!(engine.history()[0].role, "user");
    assert_eq!(engine.history()[0].content, "question");
    assert_eq!(engine.history()[1].role, "assistant");
    assert_eq!(engine.history()[1].content, "response");
}

#[test]
fn chat_engine_rejects_concurrent_send() {
    let provider = Arc::new(MockProvider {
        tokens: Mutex::new(vec!["slow".to_string()]),
    });
    let mut engine = ChatEngine::new(provider, "mock-model".to_string());

    engine.send("first".to_string()).unwrap();
    assert!(engine.is_busy());

    let result = engine.send("second".to_string());
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "generation in progress");
}

#[test]
fn chat_engine_propagates_error() {
    struct ErrorProvider;
    impl Provider for ErrorProvider {
        fn name(&self) -> &'static str {
            "error"
        }
        fn list_models(&self) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        fn chat(
            &self,
            _model: &str,
            _messages: &[Message],
            _on_token: &dyn Fn(String),
        ) -> Result<(), String> {
            Err("provider failed".to_string())
        }
    }

    let provider = Arc::new(ErrorProvider);
    let mut engine = ChatEngine::new(provider, "err-model".to_string());

    engine.send("test".to_string()).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));

    let events = engine.poll();
    assert!(events.iter().any(|e| match e {
        ChatEvent::Error(e) => e.contains("provider failed"),
        _ => false,
    }));
    assert!(!engine.is_busy());
}
