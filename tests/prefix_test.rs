/// The Phase-2 contract: request N's serialized body must be a strict
/// byte-prefix of request N+1's body. If the byte stream ever stops
/// being append-only, Ollama's KV cache is invalidated and every turn
/// pays a full cold prefill.
use oxigolem::agent::chat::{ChatEngine, Message, ToolCall};
use oxigolem::agent::tools::ToolSpec;
use oxigolem::provider::{ChatOptions, Provider, StreamChunk};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Captures every (model, messages) the engine sends it.
struct RecordingProvider {
    seen: Arc<Mutex<Vec<Vec<Message>>>>,
    script: Mutex<std::collections::VecDeque<Vec<StreamChunk>>>,
}

impl RecordingProvider {
    fn scripted(turns: Vec<Vec<StreamChunk>>) -> (Self, Arc<Mutex<Vec<Vec<Message>>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                seen: Arc::clone(&seen),
                script: Mutex::new(turns.into()),
            },
            seen,
        )
    }
}

impl Provider for RecordingProvider {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn name(&self) -> &'static str {
        "rec"
    }
    fn list_models(&self) -> Result<Vec<String>, String> {
        Ok(vec!["m".into()])
    }
    fn chat(
        &self,
        _model: &str,
        messages: &[Message],
        _tools: &[ToolSpec],
        _opts: &ChatOptions,
        on_chunk: &dyn Fn(StreamChunk),
    ) -> Result<(), String> {
        self.seen.lock().unwrap().push(messages.to_vec());
        let turn = self
            .script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| vec![StreamChunk::Content("ok".into())]);
        for c in turn {
            on_chunk(c);
        }
        Ok(())
    }
}

fn serialize(m: &Message) -> String {
    // Mirror ollama.rs's shape closely enough that byte-level prefix
    // comparison is meaningful: role/content/tool_calls/name.
    let mut v = serde_json::json!({ "role": m.role, "content": m.content });
    if let Some(calls) = &m.tool_calls {
        v["tool_calls"] = serde_json::Value::Array(
            calls
                .iter()
                .map(|c| {
                    serde_json::json!({
                        "function": {"name": c.name, "arguments": c.arguments}
                    })
                })
                .collect(),
        );
    }
    if let Some(name) = &m.name {
        v["name"] = serde_json::Value::String(name.clone());
    }
    v.to_string()
}

fn wait_done(engine: &mut ChatEngine) {
    for _ in 0..100 {
        let evs = engine.poll();
        if evs
            .iter()
            .any(|e| matches!(e, oxigolem::agent::chat::ChatEvent::Done(_) | oxigolem::agent::chat::ChatEvent::Error(_)))
        {
            // Commit appended messages like the real app does.
            if let Some(msgs) = evs.iter().find_map(|e| match e {
                oxigolem::agent::chat::ChatEvent::Done(m) => Some(m.clone()),
                _ => None,
            }) {
                engine.record_appended(msgs);
            }
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("engine never finished");
}

#[test]
fn request_body_is_append_only_across_turns() {
    // Turn 1: plain answer. Turn 2: tool call + answer.
    let script = vec![
        vec![StreamChunk::Content("first answer".into())],
        vec![
            StreamChunk::ToolCall {
                name: "bash".into(),
                arguments: serde_json::json!({"command": "ls"}),
            },
        ],
        vec![StreamChunk::Content("second answer".into())],
    ];
    let (provider, seen) = RecordingProvider::scripted(script);
    let mut engine = ChatEngine::new(Arc::new(provider), "m".into());

    engine.send("q1".into()).unwrap();
    wait_done(&mut engine);
    engine.send("q2".into()).unwrap();
    wait_done(&mut engine);

    let calls = seen.lock().unwrap();
    assert!(calls.len() >= 3, "expected 3 provider calls, got {}", calls.len());
    // calls[0] = [system, user:q1]
    // calls[1] = [system, user:q1, asst, user:q2]
    // calls[2] = [system, user:q1, asst, user:q2, asst(tool_calls), tool]
    let serialized: Vec<Vec<String>> = calls
        .iter()
        .map(|turn| turn.iter().map(serialize).collect())
        .collect();

    // The first turn is a strict prefix of every later turn.
    for later in &serialized[1..] {
        assert!(
            later.len() >= serialized[0].len(),
            "later request must be at least as long"
        );
        for (i, msg) in serialized[0].iter().enumerate() {
            assert_eq!(
                &later[i], msg,
                "message {i} diverged — prefix cache broken\nprev: {msg}\nnext: {}",
                later[i]
            );
        }
    }
    // And each consecutive pair is append-only too.
    for w in serialized.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        assert!(b.len() > a.len(), "turn must grow");
        for (i, msg) in a.iter().enumerate() {
            assert_eq!(&b[i], msg, "turn grew but prefix diverged at {i}");
        }
    }
}

#[test]
fn system_prompt_is_identical_between_requests() {
    // The same frozen string is reused — nothing is rebuilt per turn.
    let (provider, seen) = RecordingProvider::scripted(vec![
        vec![StreamChunk::Content("a".into())],
        vec![StreamChunk::Content("b".into())],
    ]);
    let mut engine = ChatEngine::new(Arc::new(provider), "m".into());
    engine.send("q1".into()).unwrap();
    wait_done(&mut engine);
    engine.send("q2".into()).unwrap();
    wait_done(&mut engine);
    let calls = seen.lock().unwrap();
    let s0 = calls[0][0].content.clone();
    let s1 = calls[1][0].content.clone();
    assert_eq!(s0, s1, "system prompt must be frozen");
}

#[test]
fn tool_specs_do_not_appear_in_system_prompt() {
    // Tool list lives in the top-level `tools` array. Duplicating it
    // in the prompt wastes tokens and breaks the byte-prefix contract
    // whenever the registry changes shape.
    let prompt = oxigolem::agent::systemprompt::system_prompt();
    assert!(
        !prompt.contains("Here are your tools"),
        "system prompt must not inline the tool registry, got: {prompt}"
    );
}

#[test]
fn budget_plan_compacts_once_then_stops() {
    use oxigolem::agent::budget::{plan, Plan};
    let mut history: Vec<Message> = Vec::new();
    history.push(Message::user("seed"));
    for i in 0..50 {
        let mut a = Message::assistant(format!("a{i}"));
        a.tool_calls = Some(vec![ToolCall {
            name: "bash".into(),
            arguments: serde_json::json!({"command": format!("echo {i}")}),
        }]);
        history.push(a);
        history.push(Message::tool("bash", &"x".repeat(200)));
    }
    history.push(Message::user("last"));

    // First call on a tight budget → Compact with some boundary.
    let p1 = plan(&history, 1024, None);
    let keep_from_1 = match p1 {
        Plan::Compact { keep_from } => keep_from,
        Plan::KeepAll => panic!("tight budget must compact"),
    };
    // Caller commits the boundary. Every later call reuses it verbatim
    // even though history has grown — that's the append-only contract.
    history.push(Message::assistant("extra".into()));
    history.push(Message::user("next"));
    let p2 = plan(&history, 1024, Some(keep_from_1));
    let keep_from_2 = match p2 {
        Plan::Compact { keep_from } => keep_from,
        Plan::KeepAll => panic!("still tight"),
    };
    assert_eq!(
        keep_from_1, keep_from_2,
        "committed boundary must be reused verbatim — recomputing breaks the prefix"
    );
}

#[test]
fn budget_plan_keepall_when_under_budget() {
    use oxigolem::agent::budget::{plan, Plan};
    let history = vec![Message::user("hi"), Message::assistant("hello".into())];
    assert!(matches!(plan(&history, 1_000_000, None), Plan::KeepAll));
}

#[test]
fn iteration_cap_is_24() {
    assert_eq!(oxigolem::agent::chat::MAX_ITERATIONS, 24);
}
