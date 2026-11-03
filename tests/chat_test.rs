use oxigolem::app::{App, Role};
use oxigolem::chat::{ChatEngine, ChatEvent, Message};
use oxigolem::provider::ollama::Ollama;
use oxigolem::provider::Provider;
use std::cell::RefCell;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

/// Spawn a fake Ollama `/api/chat` server on a local port.
/// Returns the base URL (e.g. "http://127.0.0.1:PORT").
/// `serve` is invoked with the connected stream after the request is read.
fn fake_ollama(
    serve: impl FnOnce(&mut std::net::TcpStream) + Send + 'static,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        // Read HTTP request (headers + body) until we see the body.
        let mut buf = vec![0u8; 8192];
        let n = stream.read(&mut buf).unwrap();
        let _ = n;
        serve(&mut stream);
    });
    format!("http://127.0.0.1:{port}")
}

#[test]
fn ollama_chat_streams_tokens() {
    let host = fake_ollama(|stream| {
        let body = "{\"message\":{\"content\":\"Hello\"}}\n\
                    {\"message\":{\"content\":\" world\"}}\n\
                    {\"done\":true}\n";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(resp.as_bytes()).unwrap();
    });

    let provider = Ollama::new(host);
    let tokens = RefCell::new(Vec::new());
    provider
        .chat("m", &[Message { role: "user".into(), content: "hi".into() }], &|t| {
            tokens.borrow_mut().push(t)
        })
        .unwrap();
    assert_eq!(*tokens.borrow(), vec!["Hello", " world"]);
}

#[test]
fn ollama_chat_no_total_stream_timeout() {
    // The server pauses mid-stream. Total stream duration exceeds the
    // pause but tokens still arrive — proving any timeout is per-read,
    // not a cap on the whole stream.
    let host = fake_ollama(|stream| {
        let chunk = |c: &str| format!("{{\"message\":{{\"content\":\"{c}\"}}}}\n");
        let tail = "{\"done\":true}\n";
        let body = format!("{}{}{}", chunk("a"), chunk("b"), tail);
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(chunk("a").as_bytes()).unwrap();
        stream.flush().unwrap();
        std::thread::sleep(Duration::from_millis(300));
        stream
            .write_all(format!("{}{}", chunk("b"), tail).as_bytes())
            .unwrap();
    });

    let provider = Ollama::new(host);
    let tokens = RefCell::new(Vec::new());
    let start = std::time::Instant::now();
    provider
        .chat("m", &[Message { role: "user".into(), content: "hi".into() }], &|t| {
            tokens.borrow_mut().push(t)
        })
        .unwrap();
    // The stream took >300ms total but still completed — no total timeout.
    assert!(start.elapsed() >= Duration::from_millis(300));
    assert_eq!(*tokens.borrow(), vec!["a", "b"]);
}

#[test]
fn ollama_chat_error_surfaces_server_message() {
    let host = fake_ollama(|stream| {
        let body = "{\"error\":\"model 'x' not found\"}";
        let resp = format!(
            "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(resp.as_bytes()).unwrap();
    });

    let provider = Ollama::new(host);
    let err = provider
        .chat("x", &[Message { role: "user".into(), content: "hi".into() }], &|_| {})
        .unwrap_err();
    assert!(
        err.contains("model 'x' not found"),
        "error should include the server's message, got: {err}"
    );
}

#[test]
fn ollama_chat_tolerates_abrupt_close_mid_stream() {
    // Server sends tokens then closes WITHOUT a done:true chunk and
    // without a proper chunked terminator. We already received usable
    // content, so the abrupt close should be treated as end-of-stream,
    // not a "decoding chunks" error.
    let host = fake_ollama(|stream| {
        let body = "{\"message\":{\"content\":\"partial answer\"}}\n";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{}\r\n",
            body.len(),
            body
        );
        stream.write_all(resp.as_bytes()).unwrap();
        stream.shutdown(std::net::Shutdown::Both).ok();
    });

    let provider = Ollama::new(host);
    let tokens = RefCell::new(Vec::new());
    let res = provider.chat(
        "m",
        &[Message { role: "user".into(), content: "hi".into() }],
        &|t| tokens.borrow_mut().push(t),
    );
    assert!(res.is_ok(), "expected Ok, got {res:?}");
    assert_eq!(*tokens.borrow(), vec!["partial answer"]);
}

#[test]
fn ollama_chat_surfaces_thinking() {
    // Thinking models emit chunks where content is empty but thinking is
    // set. We should surface the thinking text so the transcript isn't
    // silent while the model reasons.
    let host = fake_ollama(|stream| {
        let body = "{\"message\":{\"content\":\"\",\"thinking\":\"hmm\"}}\n\
                    {\"message\":{\"content\":\"answer\"}}\n{\"done\":true}\n";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(resp.as_bytes()).unwrap();
    });

    let provider = Ollama::new(host);
    let tokens = RefCell::new(Vec::new());
    provider
        .chat("m", &[Message { role: "user".into(), content: "hi".into() }], &|t| {
            tokens.borrow_mut().push(t)
        })
        .unwrap();
    assert_eq!(*tokens.borrow(), vec!["hmm", "answer"]);
}

#[test]
fn app_chat_error_goes_to_transcript() {
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
            Err("chat request failed: http://localhost:11434/api/chat: connection refused".into())
        }
    }

    let mut app = App::new();
    app.chat = Some(ChatEngine::new(Arc::new(ErrorProvider), "m".into()));
    // Simulate a user message already sent + placeholder.
    app.transcript.push(oxigolem::app::TranscriptEntry {
        role: Role::User,
        text: "hi".into(),
    });
    app.transcript.push(oxigolem::app::TranscriptEntry {
        role: Role::Golem,
        text: String::new(),
    });
    // Manually kick a send so an error is pending.
    app.chat.as_mut().unwrap().send("hi".into()).unwrap();
    std::thread::sleep(Duration::from_millis(150));
    app.poll_chat();

    // The empty Golem placeholder should be replaced by a Tool error entry.
    let last = app.transcript.last().unwrap();
    assert!(
        matches!(last.role, Role::Tool),
        "expected a Tool entry with the error, got role text: {}",
        last.text
    );
    assert!(
        last.text.contains("connection refused"),
        "error text should be visible, got: {}",
        last.text
    );
}
