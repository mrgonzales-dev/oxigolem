use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::thread;

use super::{systemprompt::system_prompt, tools};
use crate::provider::{Provider, StreamChunk};
use crate::shell::Shell;

/// A tool call requested by the model (Ollama `message.tool_calls` shape).
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    pub name: String,
    pub arguments: serde_json::Value,
}

/// A single chat message in the Ollama API format.
#[derive(Clone, Debug)]
pub struct Message {
    pub role: String,
    pub content: String,
    /// Present on assistant messages that requested tool calls.
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Tool name — required on `role: "tool"` messages.
    pub name: Option<String>,
}

impl Message {
    fn bare(role: &str, content: String) -> Self {
        Self {
            role: role.to_string(),
            content,
            tool_calls: None,
            name: None,
        }
    }
    pub fn system(content: &str) -> Self {
        Self::bare("system", content.to_string())
    }
    pub fn user(content: &str) -> Self {
        Self::bare("user", content.to_string())
    }
    pub fn assistant(content: String) -> Self {
        Self::bare("assistant", content)
    }
    pub fn tool(name: &str, content: &str) -> Self {
        let mut m = Self::bare("tool", content.to_string());
        m.name = Some(name.to_string());
        m
    }
}

/// Events streamed from the background thread to the UI.
#[derive(Debug)]
pub enum ChatEvent {
    /// Answer text for the current reply block.
    Token(String),
    /// Reasoning text, rendered in its own dimmed block.
    Thinking(String),
    /// The model asked to run a tool — UI shows it as a `⎿ $ cmd` line.
    ToolCall { name: String, args: serde_json::Value },
    /// Tool finished — UI shows its output dimmed.
    ToolResult { name: String, output: String },
    /// Generation finished. Carries every message appended this round
    /// (assistant replies + tool results) so history stays complete.
    Done(Vec<Message>),
    Error(String),
}

/// Max model→tool→model round-trips per user message. Guards against a
/// model that calls tools forever.
const MAX_ITERATIONS: usize = 8;

pub struct ChatEngine {
    provider: Arc<dyn Provider>,
    model: String,
    history: Vec<Message>,
    rx: Option<Receiver<ChatEvent>>,
    /// Shared with the user's Output-view terminal — one shell, one cwd.
    shell: Shell,
}

impl ChatEngine {
    pub fn new(provider: Arc<dyn Provider>, model: String) -> Self {
        Self {
            provider,
            model,
            history: Vec::new(),
            rx: None,
            shell: Shell::new(),
        }
    }

    /// Use an existing shared shell (the app's terminal shell) so agent
    /// commands and user commands share one working directory.
    pub fn with_shell(provider: Arc<dyn Provider>, model: String, shell: Shell) -> Self {
        Self {
            provider,
            model,
            history: Vec::new(),
            rx: None,
            shell,
        }
    }

    pub fn is_busy(&self) -> bool {
        self.rx.is_some()
    }

    /// Start a generation on a background thread. The thread runs the
    /// agent loop: chat → if tool_calls, execute them and chat again —
    /// until the model answers with plain text or we hit the cap.
    pub fn send(&mut self, user_text: String) -> Result<(), String> {
        if self.is_busy() {
            return Err("generation in progress".to_string());
        }

        self.history.push(Message::user(&user_text));

        let (tx, rx) = std::sync::mpsc::channel::<ChatEvent>();
        self.rx = Some(rx);

        let provider = Arc::clone(&self.provider);
        let model = self.model.clone();
        let shell = self.shell.clone();
        let mut msgs = Vec::with_capacity(self.history.len() + 1);
        msgs.push(Message::system(&system_prompt()));
        msgs.extend(self.history.iter().cloned());

        thread::spawn(move || {
            let mut appended: Vec<Message> = Vec::new();
            let tool_specs = tools::specs();
            let result = (|| -> Result<(), String> {
                for _ in 0..MAX_ITERATIONS {
                    let text = std::cell::RefCell::new(String::new());
                    let calls = std::cell::RefCell::new(Vec::<ToolCall>::new());
                    provider.chat(&model, &msgs, &tool_specs, &|chunk| match chunk {
                        StreamChunk::Content(t) => {
                            text.borrow_mut().push_str(&t);
                            let _ = tx.send(ChatEvent::Token(t));
                        }
                        StreamChunk::Thinking(t) => {
                            let _ = tx.send(ChatEvent::Thinking(t));
                        }
                        StreamChunk::ToolCall { name, arguments } => {
                            calls.borrow_mut().push(ToolCall { name, arguments });
                        }
                    })?;
                    let text = text.into_inner();
                    let calls = calls.into_inner();

                    let mut assistant = Message::assistant(text);
                    if !calls.is_empty() {
                        assistant.tool_calls = Some(calls.clone());
                    }
                    msgs.push(assistant.clone());
                    appended.push(assistant);

                    if calls.is_empty() {
                        return Ok(());
                    }
                    for call in calls {
                        let _ = tx.send(ChatEvent::ToolCall {
                            name: call.name.clone(),
                            args: call.arguments.clone(),
                        });
                        let output = tools::execute(&shell, &call.name, &call.arguments);
                        let _ = tx.send(ChatEvent::ToolResult {
                            name: call.name.clone(),
                            output: output.clone(),
                        });
                        let tm = Message::tool(&call.name, &output);
                        msgs.push(tm.clone());
                        appended.push(tm);
                    }
                }
                Err(format!(
                    "agent loop hit the {MAX_ITERATIONS}-iteration cap"
                ))
            })();
            match result {
                Ok(()) => {
                    let _ = tx.send(ChatEvent::Done(appended));
                }
                Err(e) => {
                    let _ = tx.send(ChatEvent::Error(e));
                }
            }
        });

        Ok(())
    }

    /// Drain all pending events. Called every event-loop iteration.
    pub fn poll(&mut self) -> Vec<ChatEvent> {
        let Some(rx) = &self.rx else { return Vec::new() };
        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            let terminal = matches!(ev, ChatEvent::Done(_) | ChatEvent::Error(_));
            events.push(ev);
            if terminal {
                break;
            }
        }
        if events
            .iter()
            .any(|e| matches!(e, ChatEvent::Done(_) | ChatEvent::Error(_)))
        {
            self.rx = None;
        }
        events
    }

    /// Extend history with the messages produced during a generation
    /// (assistant replies and tool results). Called on `Done`.
    pub fn record_appended(&mut self, msgs: Vec<Message>) {
        self.history.extend(msgs);
    }

    pub fn history(&self) -> &[Message] {
        &self.history
    }
}
