use oxigolem::agent::chat::{ChatEngine, ChatEvent, Message};
use oxigolem::agent::tools::ToolSpec;
use oxigolem::app::{App, Role};
use oxigolem::provider::ollama::Ollama;
use oxigolem::provider::{ChatOptions, Provider, StreamChunk};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A mock provider that emits chunks from a predefined list.
struct MockProvider {
    chunks: Mutex<Vec<StreamChunk>>,
}

impl MockProvider {
    fn content(texts: &[&str]) -> Self {
        Self {
            chunks: Mutex::new(
                texts
                    .iter()
                    .map(|t| StreamChunk::Content(t.to_string()))
                    .collect(),
            ),
        }
    }
}

impl Provider for MockProvider {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
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
        _tools: &[ToolSpec],
        _opts: &ChatOptions,
        on_chunk: &dyn Fn(StreamChunk),
    ) -> Result<(), String> {
        for c in self.chunks.lock().unwrap().iter() {
            on_chunk(c.clone());
        }
        Ok(())
    }
}

/// A scripted provider: each call to chat() pops the next turn of chunks.
struct ScriptedProvider {
    turns: Mutex<VecDeque<Vec<StreamChunk>>>,
}

impl Provider for ScriptedProvider {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
fn name(&self) -> &'static str {
        "scripted"
    }
    fn list_models(&self) -> Result<Vec<String>, String> {
        Ok(vec!["m".to_string()])
    }
    fn chat(
        &self,
        _model: &str,
        _messages: &[Message],
        _tools: &[ToolSpec],
        _opts: &ChatOptions,
        on_chunk: &dyn Fn(StreamChunk),
    ) -> Result<(), String> {
        let turn = self
            .turns
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| vec![StreamChunk::Content("done".to_string())]);
        for c in turn {
            on_chunk(c);
        }
        Ok(())
    }
}

#[test]
fn chat_engine_streams_tokens_and_finishes() {
    let provider = Arc::new(MockProvider::content(&["Hello", " world"]));
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
    assert!(events.iter().any(|e| matches!(e, ChatEvent::Done(_))));
    assert!(!engine.is_busy());
}

#[test]
fn chat_engine_records_appended_messages_on_done() {
    let provider = Arc::new(MockProvider::content(&["response"]));
    let mut engine = ChatEngine::new(provider, "mock-model".to_string());

    engine.send("question".to_string()).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));

    let events = engine.poll();
    let appended = events.iter().find_map(|e| match e {
        ChatEvent::Done(msgs) => Some(msgs.clone()),
        _ => None,
    });
    let appended = appended.expect("Done should carry appended messages");
    engine.record_appended(appended);

    assert_eq!(engine.history().len(), 2);
    assert_eq!(engine.history()[0].role, "user");
    assert_eq!(engine.history()[0].content, "question");
    assert_eq!(engine.history()[1].role, "assistant");
    assert_eq!(engine.history()[1].content, "response");
}

#[test]
fn chat_engine_rejects_concurrent_send() {
    let provider = Arc::new(MockProvider::content(&["slow"]));
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
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
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
            _tools: &[ToolSpec],
            _opts: &ChatOptions,
            _on_chunk: &dyn Fn(StreamChunk),
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

#[test]
fn engine_executes_tool_call_and_continues() {
    let provider = Arc::new(ScriptedProvider {
        turns: Mutex::new(VecDeque::from(vec![
            vec![StreamChunk::ToolCall {
                name: "bash".to_string(),
                arguments: serde_json::json!({"command": "echo from-tool"}),
            }],
            vec![StreamChunk::Content("the answer".to_string())],
        ])),
    });
    let mut engine = ChatEngine::new(provider, "m".to_string());
    engine.send("run something".to_string()).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let events = engine.poll();

    // Tool call + result events surfaced.
    assert!(events.iter().any(|e| matches!(e, ChatEvent::ToolCall { name, .. } if name == "bash")));
    let result = events.iter().find_map(|e| match e {
        ChatEvent::ToolResult { outcome, .. } => Some(outcome.result.stdout.clone()),
        _ => None,
    });
    assert!(
        result.as_deref().is_some_and(|o| o.contains("from-tool")),
        "tool output should reach the event stream, events: {events:?}"
    );

    // History: user + assistant(tool_calls) + tool + assistant(reply).
    let appended = events.iter().find_map(|e| match e {
        ChatEvent::Done(msgs) => Some(msgs.clone()),
        _ => None,
    });
    engine.record_appended(appended.expect("Done"));
    let roles: Vec<&str> = engine.history().iter().map(|m| m.role.as_str()).collect();
    assert_eq!(roles, vec!["user", "assistant", "tool", "assistant"]);
    assert!(engine.history()[1].tool_calls.is_some());
    assert_eq!(engine.history()[2].content.trim(), "from-tool");
    assert_eq!(engine.history()[3].content, "the answer");
}

#[test]
fn engine_stops_at_iteration_cap() {
    // Provider always returns a tool call — the loop must not run forever.
    struct AlwaysTool;
    impl Provider for AlwaysTool {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
fn name(&self) -> &'static str {
            "always"
        }
        fn list_models(&self) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        fn chat(
            &self,
            _m: &str,
            _msgs: &[Message],
            _tools: &[ToolSpec],
            _opts: &ChatOptions,
            on_chunk: &dyn Fn(StreamChunk),
        ) -> Result<(), String> {
            on_chunk(StreamChunk::ToolCall {
                name: "bash".to_string(),
                arguments: serde_json::json!({"command": "true"}),
            });
            Ok(())
        }
    }
    let mut engine = ChatEngine::new(Arc::new(AlwaysTool), "m".to_string());
    engine.send("loop".to_string()).unwrap();
    // Generous wait; loop of 8 should finish quickly.
    std::thread::sleep(Duration::from_millis(2000));
    let events = engine.poll();
    assert!(
        events.iter().any(|e| matches!(e, ChatEvent::Done(_) | ChatEvent::Error(_))),
        "engine must terminate at the iteration cap"
    );
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

fn test_messages() -> Vec<Message> {
    vec![Message::user("hi")]
}

/// Read a full HTTP request: headers until \r\n\r\n, then body per
/// Content-Length. A single `read()` often returns headers only.
fn read_request(stream: &mut std::net::TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = stream.read(&mut chunk).unwrap();
        buf.extend_from_slice(&chunk[..n]);
        let s = String::from_utf8_lossy(&buf).to_string();
        if let Some(pos) = s.find("\r\n\r\n") {
            let want = s[..pos]
                .to_lowercase()
                .split("content-length:")
                .nth(1)
                .and_then(|v| v.lines().next())
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if s.len() - (pos + 4) >= want {
                return s;
            }
        }
    }
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
        .chat("m", &test_messages(), &[], &ChatOptions::default(), &|c| {
            if !matches!(c, StreamChunk::Usage(_)) {
                tokens.borrow_mut().push(c)
            }
        })
        .unwrap();
    assert_eq!(
        *tokens.borrow(),
        vec![
            StreamChunk::Content("Hello".into()),
            StreamChunk::Content(" world".into())
        ]
    );
}

#[test]
fn ollama_sends_tools_in_request() {
    use std::sync::mpsc;
    let (tx, rx) = mpsc::channel::<String>();
    // fake_ollama discards the request, so use a raw listener that captures it.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let req = read_request(&mut stream);
        let body = "{\"message\":{\"content\":\"ok\"}}\n{\"done\":true}\n";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(resp.as_bytes()).unwrap();
        tx.send(req).unwrap();
    });
    let provider = Ollama::new(format!("http://127.0.0.1:{port}"));
    provider
        .chat("m", &test_messages(), &oxigolem::agent::tools::specs(), &ChatOptions::default(), &|_| {})
        .unwrap();
    let req = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let json_start = req.find('{').unwrap();
    let v: serde_json::Value = serde_json::from_str(&req[json_start..]).unwrap();
    assert_eq!(v["tools"][0]["function"]["name"], "bash", "req: {req}");
}

#[test]
fn ollama_parses_tool_calls_chunk() {
    let host = fake_ollama(|stream| {
        let body = "{\"message\":{\"tool_calls\":[{\"function\":{\"name\":\"bash\",\"arguments\":{\"command\":\"ls\"}}}]}}\n\
                    {\"done\":true}\n";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(resp.as_bytes()).unwrap();
    });
    let provider = Ollama::new(host);
    let chunks = RefCell::new(Vec::new());
    provider
        .chat("m", &test_messages(), &oxigolem::agent::tools::specs(), &ChatOptions::default(), &|c| {
            if !matches!(c, StreamChunk::Usage(_)) {
                chunks.borrow_mut().push(c)
            }
        })
        .unwrap();
    assert_eq!(
        *chunks.borrow(),
        vec![StreamChunk::ToolCall {
            name: "bash".to_string(),
            arguments: serde_json::json!({"command": "ls"}),
        }]
    );
}

#[test]
fn ollama_parses_tool_calls_on_done_chunk() {
    // Ollama can emit tool_calls in the same chunk that has done:true.
    let host = fake_ollama(|stream| {
        let body = "{\"message\":{\"tool_calls\":[{\"function\":{\"name\":\"bash\",\"arguments\":{\"command\":\"pwd\"}}}]},\"done\":true}\n";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(resp.as_bytes()).unwrap();
    });
    let provider = Ollama::new(host);
    let chunks = RefCell::new(Vec::new());
    provider
        .chat("m", &test_messages(), &oxigolem::agent::tools::specs(), &ChatOptions::default(), &|c| {
            if !matches!(c, StreamChunk::Usage(_)) {
                chunks.borrow_mut().push(c)
            }
        })
        .unwrap();
    assert_eq!(
        *chunks.borrow(),
        vec![StreamChunk::ToolCall {
            name: "bash".to_string(),
            arguments: serde_json::json!({"command": "pwd"}),
        }]
    );
}

#[test]
fn ollama_serializes_tool_messages_back() {
    use std::sync::mpsc;
    let (tx, rx) = mpsc::channel::<String>();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let req = read_request(&mut stream);
        let body = "{\"message\":{\"content\":\"ok\"}}\n{\"done\":true}\n";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(resp.as_bytes()).unwrap();
        tx.send(req).unwrap();
    });
    let provider = Ollama::new(format!("http://127.0.0.1:{port}"));
    let mut assistant = Message::assistant("".to_string());
    assistant.tool_calls = Some(vec![oxigolem::agent::chat::ToolCall {
        name: "bash".to_string(),
        arguments: serde_json::json!({"command": "ls"}),
    }]);
    let msgs = vec![
        Message::user("run ls"),
        assistant,
        Message::tool("bash", "file.rs"),
    ];
    provider.chat("m", &msgs, &[], &ChatOptions::default(), &|_| {}).unwrap();
    let req = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let json_start = req.find('{').unwrap();
    let v: serde_json::Value = serde_json::from_str(&req[json_start..]).unwrap();
    assert_eq!(v["messages"][1]["tool_calls"][0]["function"]["name"], "bash");
    assert_eq!(v["messages"][2]["role"], "tool");
    assert_eq!(v["messages"][2]["name"], "bash");
    assert_eq!(v["messages"][2]["content"], "file.rs");
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
        .chat("m", &test_messages(), &[], &ChatOptions::default(), &|c| {
            if !matches!(c, StreamChunk::Usage(_)) {
                tokens.borrow_mut().push(c)
            }
        })
        .unwrap();
    // The stream took >300ms total but still completed — no total timeout.
    assert!(start.elapsed() >= Duration::from_millis(300));
    assert_eq!(
        *tokens.borrow(),
        vec![
            StreamChunk::Content("a".into()),
            StreamChunk::Content("b".into())
        ]
    );
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
        .chat("x", &test_messages(), &[], &ChatOptions::default(), &|_| {})
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
        &test_messages(),
        &[],
        &ChatOptions::default(),
        &|c| tokens.borrow_mut().push(c),
    );
    assert!(res.is_ok(), "expected Ok, got {res:?}");
    assert_eq!(
        *tokens.borrow(),
        vec![StreamChunk::Content("partial answer".into())]
    );
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
        .chat("m", &test_messages(), &[], &ChatOptions::default(), &|c| {
            if !matches!(c, StreamChunk::Usage(_)) {
                tokens.borrow_mut().push(c)
            }
        })
        .unwrap();
    assert_eq!(
        *tokens.borrow(),
        vec![
            StreamChunk::Thinking("hmm".into()),
            StreamChunk::Content("answer".into())
        ]
    );
}

#[test]
fn app_thinking_and_reply_are_separate_entries() {
    let provider = Arc::new(MockProvider {
        chunks: Mutex::new(vec![
            StreamChunk::Thinking("hmm".into()),
            StreamChunk::Thinking(" hmm".into()),
            StreamChunk::Content("answer".into()),
        ]),
    });
    let mut app = App::new();
    app.chat = Some(ChatEngine::new(provider, "m".into()));
    app.transcript.push(oxigolem::app::TranscriptEntry {
        role: Role::User,
        text: "hi".into(),
        outcome: None,
        expanded: false,
    });
    app.chat.as_mut().unwrap().send("hi".into()).unwrap();
    std::thread::sleep(Duration::from_millis(150));
    app.poll_chat();

    let roles: Vec<Role> = app.transcript.iter().map(|e| e.role).collect();
    assert_eq!(roles, vec![Role::User, Role::Thinking, Role::Golem]);
    assert_eq!(app.transcript[1].text, "hmm hmm");
    assert_eq!(app.transcript[2].text, "answer");
}

#[test]
fn app_tool_events_render_as_tool_entries() {
    let provider = Arc::new(ScriptedProvider {
        turns: Mutex::new(VecDeque::from(vec![
            vec![StreamChunk::ToolCall {
                name: "bash".to_string(),
                arguments: serde_json::json!({"command": "echo hi"}),
            }],
            vec![StreamChunk::Content("done answer".to_string())],
        ])),
    });
    let mut app = App::new();
    app.chat = Some(ChatEngine::new(provider, "m".into()));
    app.transcript.push(oxigolem::app::TranscriptEntry {
        role: Role::User,
        text: "run it".into(),
        outcome: None,
        expanded: false,
    });
    app.chat.as_mut().unwrap().send("run it".into()).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    app.poll_chat();

    let roles: Vec<Role> = app.transcript.iter().map(|e| e.role).collect();
    assert_eq!(
        roles,
        vec![Role::User, Role::Tool, Role::Golem],
        "transcript roles: {roles:?}"
    );
    assert_eq!(app.transcript[1].text, "echo hi");
    let out = app.transcript[1]
        .outcome
        .as_ref()
        .expect("tool entry has outcome");
    assert!(out.result.stdout.contains("hi"));
    assert_eq!(out.result.code, Some(0));
}

#[test]
fn app_chat_error_goes_to_transcript() {
    struct ErrorProvider;
    impl Provider for ErrorProvider {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
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
            _tools: &[ToolSpec],
            _opts: &ChatOptions,
            _on_chunk: &dyn Fn(StreamChunk),
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
        outcome: None,
        expanded: false,
    });
    app.transcript.push(oxigolem::app::TranscriptEntry {
        role: Role::Golem,
        text: String::new(),
        outcome: None,
        expanded: false,
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

#[test]
fn enter_while_busy_queues_prompt() {
    // Provider that streams slowly so we can enqueue mid-generation.
    struct SlowProvider;
    impl Provider for SlowProvider {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
fn name(&self) -> &'static str {
            "slow"
        }
        fn list_models(&self) -> Result<Vec<String>, String> {
            Ok(vec!["m".into()])
        }
        fn chat(
            &self,
            _model: &str,
            _messages: &[Message],
            _tools: &[ToolSpec],
            _opts: &ChatOptions,
            on_chunk: &dyn Fn(StreamChunk),
        ) -> Result<(), String> {
            // Emit one chunk then linger — gives the test time to queue.
            on_chunk(StreamChunk::Content("hi".into()));
            std::thread::sleep(Duration::from_millis(300));
            on_chunk(StreamChunk::Content("done".into()));
            Ok(())
        }
    }

    let mut app = App::new();
    app.chat = Some(ChatEngine::new(Arc::new(SlowProvider), "m".into()));
    app.chat.as_mut().unwrap().send("first".into()).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    assert!(app.chat_busy());

    // Type + hit Enter while busy — force-sends: aborts the in-flight
    // generation and dispatches this prompt immediately as a User entry.
    app.push_char('q');
    app.handle_enter();
    let users = app
        .transcript
        .iter()
        .filter(|e| matches!(e.role, Role::User))
        .count();
    assert_eq!(users, 1, "force-send pushes the prompt immediately");
}

#[test]
fn second_enter_force_sends_immediately() {
    struct SlowProvider;
    impl Provider for SlowProvider {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
fn name(&self) -> &'static str {
            "slow"
        }
        fn list_models(&self) -> Result<Vec<String>, String> {
            Ok(vec!["m".into()])
        }
        fn chat(
            &self,
            _model: &str,
            _messages: &[Message],
            _tools: &[ToolSpec],
            _opts: &ChatOptions,
            on_chunk: &dyn Fn(StreamChunk),
        ) -> Result<(), String> {
            on_chunk(StreamChunk::Content("hi".into()));
            std::thread::sleep(Duration::from_millis(300));
            on_chunk(StreamChunk::Content("done".into()));
            Ok(())
        }
    }

    let mut app = App::new();
    app.chat = Some(ChatEngine::new(Arc::new(SlowProvider), "m".into()));
    // First prompt goes through the real input path so it lands as a
    // User transcript entry.
    for c in "first".chars() {
        app.push_char(c);
    }
    app.handle_enter();
    std::thread::sleep(Duration::from_millis(50));

    // Second Enter with text — interrupts the running generation and
    // dispatches this prompt immediately (no waiting for Cancelled).
    app.push_char('a');
    app.handle_enter();
    // Two User entries right away — original + force-sent.
    let users = app
        .transcript
        .iter()
        .filter(|e| matches!(e.role, Role::User))
        .count();
    assert_eq!(users, 2, "force-sent prompt lands as a User entry now");
    // A new generation for 'a' is running.
    assert!(app.chat_busy());
    std::thread::sleep(Duration::from_millis(450));
    app.poll_chat();
}

#[test]
fn esc_cancels_in_flight_generation() {
    struct SlowProvider;
    impl Provider for SlowProvider {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
fn name(&self) -> &'static str {
            "slow"
        }
        fn list_models(&self) -> Result<Vec<String>, String> {
            Ok(vec!["m".into()])
        }
        fn chat(
            &self,
            _model: &str,
            _messages: &[Message],
            _tools: &[ToolSpec],
            _opts: &ChatOptions,
            on_chunk: &dyn Fn(StreamChunk),
        ) -> Result<(), String> {
            on_chunk(StreamChunk::Content("hi".into()));
            std::thread::sleep(Duration::from_millis(300));
            on_chunk(StreamChunk::Content("done".into()));
            Ok(())
        }
    }

    let mut app = App::new();
    app.chat = Some(ChatEngine::new(Arc::new(SlowProvider), "m".into()));
    app.chat.as_mut().unwrap().send("first".into()).unwrap();
    std::thread::sleep(Duration::from_millis(50));

    // Esc while busy → cancel, NOT quit.
    let should_quit = app.esc();
    assert!(!should_quit, "esc should cancel, not quit");

    // Poll until Cancelled arrives.
    std::thread::sleep(Duration::from_millis(400));
    app.poll_chat();
    let interrupted = app
        .transcript
        .iter()
        .any(|e| matches!(e.role, Role::Tool) && e.text.contains("interrupted"));
    assert!(interrupted, "transcript should show an interrupted marker");
}

// ---- Phase 0: usage instrumentation ----

#[test]
fn ollama_done_chunk_parses_usage() {
    let host = fake_ollama(|stream| {
        let body = "{\"message\":{\"content\":\"hi\"}}\n\
                    {\"done\":true,\"prompt_eval_count\":2400,\"prompt_eval_cached_count\":2380,\"eval_count\":42,\"prompt_eval_duration\":90000000,\"eval_duration\":1000000000,\"load_duration\":500000000}\n";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(resp.as_bytes()).unwrap();
    });
    let provider = Ollama::new(host);
    let chunks = RefCell::new(Vec::new());
    provider
        .chat("m", &test_messages(), &[], &ChatOptions::default(), &|c| {
            chunks.borrow_mut().push(c)
        })
        .unwrap();
    let usage = chunks.borrow().iter().find_map(|c| match c {
        StreamChunk::Usage(u) => Some(u.clone()),
        _ => None,
    });
    let u = usage.expect("done chunk should emit a Usage chunk");
    assert_eq!(u.prompt, 2400);
    assert_eq!(u.cached, 2380);
    assert_eq!(u.eval, 42);
    assert_eq!(u.prefill_ms, 90);
    assert_eq!(u.eval_ms, 1000);
    assert_eq!(u.load_ms, 500);
}

#[test]
fn ollama_done_chunk_missing_cached_counts_as_zero() {
    // Older servers don't emit prompt_eval_cached_count — it must
    // degrade to 0, never None/absent.
    let host = fake_ollama(|stream| {
        let body = "{\"message\":{\"content\":\"hi\"}}\n\
                    {\"done\":true,\"prompt_eval_count\":100,\"eval_count\":10,\"eval_duration\":250000000}\n";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(resp.as_bytes()).unwrap();
    });
    let provider = Ollama::new(host);
    let chunks = RefCell::new(Vec::new());
    provider
        .chat("m", &test_messages(), &[], &ChatOptions::default(), &|c| {
            chunks.borrow_mut().push(c)
        })
        .unwrap();
    let u = chunks.borrow().iter().find_map(|c| match c {
        StreamChunk::Usage(u) => Some(u.clone()),
        _ => None,
    });
    let u = u.expect("usage chunk");
    assert_eq!(u.cached, 0);
    assert_eq!(u.cached_pct(), 0.0);
}

#[test]
fn usage_derived_metrics_no_divide_by_zero() {
    let u = oxigolem::provider::Usage {
        prompt: 0,
        cached: 0,
        eval: 0,
        prefill_ms: 0,
        eval_ms: 0,
        load_ms: 0,
    };
    assert_eq!(u.cached_pct(), 0.0);
    assert_eq!(u.tokens_per_sec(), 0.0);
}

#[test]
fn usage_math_hit_rate_and_speed() {
    let u = oxigolem::provider::Usage {
        prompt: 2000,
        cached: 1990,
        eval: 41,
        prefill_ms: 90,
        eval_ms: 1000,
        load_ms: 0,
    };
    assert!((u.cached_pct() - 99.5).abs() < 0.01);
    assert!((u.tokens_per_sec() - 41.0).abs() < 0.01);
}

#[test]
fn usage_event_reaches_app() {
    let provider = Arc::new(MockProvider {
        chunks: Mutex::new(vec![
            StreamChunk::Content("hi".into()),
            StreamChunk::Usage(oxigolem::provider::Usage {
                prompt: 100,
                cached: 99,
                eval: 10,
                prefill_ms: 5,
                eval_ms: 200,
                load_ms: 0,
            }),
        ]),
    });
    let mut app = App::new();
    app.chat = Some(ChatEngine::new(provider, "m".into()));
    app.chat.as_mut().unwrap().send("hi".into()).unwrap();
    std::thread::sleep(Duration::from_millis(150));
    app.poll_chat();
    let u = app.last_usage.expect("last_usage populated");
    assert_eq!(u.prompt, 100);
    assert_eq!(u.cached, 99);
}

#[test]
fn fenced_mode_recovers_bash_block_into_tool_call() {
    // A model without native tools writes ```bash fenced text — the
    // engine must execute it like a real call and continue the loop.
    let provider = Arc::new(ScriptedProvider {
        turns: Mutex::new(
            vec![
                vec![StreamChunk::Content(
                    "Let me check:\n```bash\necho fenced-ok\n```".into(),
                )],
                vec![StreamChunk::Content("done after tool".into())],
            ]
            .into(),
        ),
    });
    let mut engine = ChatEngine::new(provider, "m".into());
    engine.set_fenced(true);
    engine.send("run it".into()).unwrap();
    let mut calls = 0;
    let mut results = 0;
    for _ in 0..200 {
        for ev in engine.poll() {
            match ev {
                ChatEvent::ToolCall { name, .. } => {
                    assert_eq!(name, "bash");
                    calls += 1;
                }
                ChatEvent::ToolResult { outcome, .. } => {
                    assert!(outcome.result.stdout.contains("fenced-ok"));
                    results += 1;
                }
                ChatEvent::Done(_) | ChatEvent::Error(_) => {
                    assert_eq!(calls, 1, "recovered call fired");
                    assert_eq!(results, 1, "result delivered");
                    return;
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("engine never finished");
}

#[test]
fn fenced_mode_passes_plain_text_unchanged() {
    // No fenced block, no JSON → no phantom tool call.
    let provider = Arc::new(MockProvider::content(&["just an answer"]));
    let mut engine = ChatEngine::new(provider, "m".into());
    engine.set_fenced(true);
    engine.send("hi".into()).unwrap();
    for _ in 0..200 {
        let done = engine.poll().iter().any(|e| matches!(e, ChatEvent::Done(_)));
        if done {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("engine never finished");
}

#[test]
fn parallel_batch_preserves_result_order() {
    // Two read-only calls in one turn — both execute, and ToolResult
    // order must match ToolCall order so history lines up.
    let provider = Arc::new(ScriptedProvider {
        turns: Mutex::new(
            vec![
                vec![
                    StreamChunk::ToolCall {
                        name: "bash".into(),
                        arguments: serde_json::json!({"command": "echo first"}),
                    },
                    StreamChunk::ToolCall {
                        name: "bash".into(),
                        arguments: serde_json::json!({"command": "echo second"}),
                    },
                ],
                vec![StreamChunk::Content("done".into())],
            ]
            .into(),
        ),
    });
    let mut engine = ChatEngine::new(provider, "m".into());
    engine.send("go".into()).unwrap();
    let mut results: Vec<String> = Vec::new();
    for _ in 0..300 {
        for ev in engine.poll() {
            match ev {
                ChatEvent::ToolResult { outcome, .. } => {
                    results.push(outcome.result.stdout.trim().to_string());
                }
                ChatEvent::Done(_) => {
                    assert_eq!(results, vec!["first", "second"]);
                    return;
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("never done");
}

#[test]
fn batchable_allowlist_filters_mutations() {
    use oxigolem::agent::tools::batchable;
    assert!(batchable("ls -la"));
    assert!(batchable("cat foo.txt | grep bar"));
    assert!(batchable("rg -n pattern src/"));
    assert!(batchable("git status"));
    assert!(batchable("git log --oneline -10"));
    assert!(batchable("cargo check"));
    assert!(batchable("find . -name '*.rs'"));
    assert!(batchable("sed -n '1,10p' file"));
    assert!(!batchable("rm -rf x"));
    assert!(!batchable("echo hi > f.txt"));
    assert!(!batchable("ls && rm x"));
    assert!(!batchable("cat $(evil)"));
    assert!(!batchable("git push"));
    assert!(!batchable("git checkout ."));
    assert!(!batchable("cargo build"));
    assert!(!batchable("sed -i 's/a/b/' f"));
    assert!(!batchable("sleep 1"));
    assert!(!batchable("FOO=1 ls"));
}
