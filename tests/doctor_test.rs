//! Phase 6: /doctor report + model-list cache tests.

use std::sync::Arc;
use std::time::Duration;

use crossterm::event::KeyCode;
use oxigolem::{
    agent::{chat::ChatEngine, tools::ToolSpec},
    app::{App, Role},
    modal::Kind,
    provider::{ChatOptions, Provider, StreamChunk, Usage},
};

/// Provider that answers instantly — never hits the network.
struct Dummy;

impl Provider for Dummy {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn name(&self) -> &'static str {
        "dummy"
    }
    fn list_models(&self) -> Result<Vec<String>, String> {
        Ok(vec!["m1".to_string(), "m2".to_string()])
    }
    fn chat(
        &self,
        _m: &str,
        _msgs: &[oxigolem::agent::chat::Message],
        _tools: &[ToolSpec],
        _opts: &ChatOptions,
        on_chunk: &dyn Fn(StreamChunk),
    ) -> Result<(), String> {
        on_chunk(StreamChunk::Content("ok".to_string()));
        on_chunk(StreamChunk::Usage(Usage {
            prompt: 1000,
            cached: 994,
            eval: 40,
            prefill_ms: 85,
            eval_ms: 950,
            load_ms: 12,
        }));
        Ok(())
    }
}

fn press(app: &mut App, code: KeyCode) {
    app.modal_key(code);
}

#[test]
fn doctor_without_connection_reports_no_model() {
    let mut app = App::new();
    app.open_palette();
    // "/doctor" is the second palette entry.
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    assert!(app.modal.is_none());
    let last = app.transcript.last().expect("a report entry was pushed");
    assert_eq!(last.role, Role::Report);
    assert!(last.text.contains("/doctor"));
    assert!(last.text.contains("no model connected"));
}

#[test]
fn doctor_via_input_slash_command_works_without_provider() {
    let mut app = App::new();
    // Type `/doctor` into the input and press Enter — no palette.
    for c in "/doctor".chars() {
        app.push_char(c);
    }
    app.handle_enter();
    let last = app.transcript.last().expect("report pushed");
    assert_eq!(last.role, Role::Report);
    assert!(last.text.contains("/doctor"));
    assert!(last.text.contains("no model connected"));
    // Slash command must not flow into chat (there is none anyway).
    assert!(app.chat.is_none());
}

#[test]
fn doctor_via_input_slash_command_works_when_connected() {
    let mut app = App::new();
    app.chat = Some(ChatEngine::new(Arc::new(Dummy), "m".into()));
    for c in "/doctor".chars() {
        app.push_char(c);
    }
    app.handle_enter();
    let last = app.transcript.last().expect("report pushed");
    assert_eq!(last.role, Role::Report);
    // And no generation got kicked off — the engine stays idle.
    assert!(!app.chat.as_ref().unwrap().is_busy());
}

#[test]
fn slash_like_non_command_text_reaches_model() {
    // `/etc/passwd` should be treated as a normal prompt, not a
    // failed slash command — the model must see it verbatim.
    let mut app = App::new();
    app.chat = Some(ChatEngine::new(Arc::new(Dummy), "m".into()));
    for c in "/etc/passwd".chars() {
        app.push_char(c);
    }
    app.handle_enter();
    for _ in 0..50 {
        if !app.chat.as_ref().unwrap().is_busy() {
            break;
        }
        app.poll_chat();
        std::thread::sleep(Duration::from_millis(10));
    }
    app.poll_chat();
    // The user message lives in engine history — report a Report
    // would have been pushed if /etc were misinterpreted as a cmd.
    let report = app
        .transcript
        .iter()
        .find(|e| matches!(e.role, Role::Report));
    assert!(report.is_none(), "/etc/passwd wrongly hit Report path");
}

#[test]
fn doctor_reports_model_ctx_tools_and_usage() {
    let mut app = App::new();
    app.chat = Some(ChatEngine::new(Arc::new(Dummy), "qwen3:1.7b".into()));
    // Simulate a completed generation so last_usage is populated.
    app.chat.as_mut().unwrap().send("hi".into()).unwrap();
    for _ in 0..50 {
        if !app.chat.as_ref().unwrap().is_busy() {
            break;
        }
        app.poll_chat();
        std::thread::sleep(Duration::from_millis(10));
    }
    app.poll_chat();
    // `connected` is normally set via the modal — fake it directly.
    app.connected = Some(oxigolem::modal::Connection {
        provider: "ollama",
        model: "qwen3:1.7b".to_string(),
    });
    app.open_palette();
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    let last = app.transcript.last().expect("report pushed");
    assert!(last.text.contains("qwen3:1.7b"), "{}", last.text);
    assert!(last.text.contains("num_ctx"), "{}", last.text);
    assert!(last.text.contains("cached"), "{}", last.text);
    assert!(last.text.contains("t/s"), "{}", last.text);
    assert!(last.text.contains("load"), "{}", last.text);
}

#[test]
fn model_list_cache_round_trip() {
    let dir = std::env::temp_dir().join(format!("oxi-modellist-{}", std::process::id()));
    unsafe { std::env::set_var("OXIGOLEM_CONFIG_DIR", &dir) };

    let models = vec!["a:latest".to_string(), "b:7b".to_string()];
    oxigolem::provider::write_model_list("ollama", &models);
    let got = oxigolem::provider::read_model_list("ollama").expect("read back");
    assert_eq!(got, models);

    // Unknown provider → None, not an empty vec.
    assert!(oxigolem::provider::read_model_list("other").is_none());
    unsafe { std::env::remove_var("OXIGOLEM_CONFIG_DIR") };
}

#[test]
fn connect_flow_reaches_models_loading_then_models() {
    let mut app = App::new();
    // Open palette → Connect provider → provider list.
    app.open_palette();
    press(&mut app, KeyCode::Enter);
    // First provider selected → ModelsLoading while the fetch runs.
    press(&mut app, KeyCode::Enter);
    match app.modal.as_ref().map(|m| &m.kind) {
        Some(Kind::ModelsLoading { .. }) | Some(Kind::Models { .. }) => {}
        _ => panic!("expected ModelsLoading or Models"),
    }
    // Pump until the worker lands — list_models is instant for the
    // default provider only when ollama is up; the poll must be safe
    // either way (live list, cache, or error message).
    for _ in 0..50 {
        app.poll_models();
        match app.modal.as_ref().map(|m| &m.kind) {
            Some(Kind::ModelsLoading { .. }) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => break,
        }
    }
    assert!(
        !matches!(app.modal.as_ref().map(|m| &m.kind), Some(Kind::ModelsLoading { .. })),
        "loading must resolve within the pump budget"
    );
}
