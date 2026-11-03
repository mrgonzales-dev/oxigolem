use std::sync::mpsc::Receiver;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;

use super::{
    systemprompt::system_prompt,
    tools::{self, ToolOutcome},
};
use crate::provider::{ChatOptions, Provider, StreamChunk, Usage};
use crate::shell::{new_bg_shells, BgShells, Shell};

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
    /// Tool finished — carries the structured outcome so the UI can
    /// render a cursor-cli-style block (cmd + stdout + stderr + code +
    /// duration) instead of a pre-flattened string.
    ToolResult { name: String, outcome: ToolOutcome },
    /// Generation finished. Carries every message appended this round
    /// (assistant replies + tool results) so history stays complete.
    Done(Vec<Message>),
    /// Token + timing counters from the provider's done chunk — one
    /// per generation, used by the footer to show hit rate / t/s.
    Usage(Usage),
    /// User pressed Esc during a generation — the partial reply stays
    /// in the transcript but is not committed to history.
    Cancelled,
    Error(String),
}

/// Max model→tool→model round-trips per user message. Guards against a
/// model that calls tools forever.
/// Max model→tool→model round-trips per user message. Raised from 8
/// when prefix stability landed — a higher cap is safe now that a long
/// session doesn't pay a cold prefill every iteration.
pub const MAX_ITERATIONS: usize = 24;

pub struct ChatEngine {
    provider: Arc<dyn Provider>,
    model: String,
    history: Vec<Message>,
    rx: Option<Receiver<ChatEvent>>,
    /// Cancel flag shared with the worker thread — set by `cancel()` on
    /// Esc; the loop checks it between iterations and inside the chunk
    /// callback so the stream aborts on the next chunk.
    cancel: Arc<AtomicBool>,
    /// Shared shell cwd — every bash call runs in this directory.
    shell: Shell,
    /// Registry of background shells — every bash call (foreground or
    /// `run_in_background`) spawns through this so the UI can show a
    /// live tab per shell.
    shells: BgShells,
    /// Ctrl+B×2 sets this — the foreground bash wait returns early, the
    /// process keeps running as a normal background shell, and the tool
    /// result hands the model the shell id + log path instead of output.
    demote: Arc<AtomicBool>,
    /// Resolved generation options (num_ctx / think) — set once when
    /// the model is picked via `set_options`.
    opts: ChatOptions,
    /// True when inspect() reports no "tools" capability — Phase 4
    /// reads this to enable fenced-block tool-call repair.
    fenced: bool,
    /// Frozen at construction: the byte prefix of every request. Never
    /// rebuilt per turn — rebuilding would invalidate the KV cache.
    system_prompt: String,
    /// Once history crosses the token budget we cut a boundary here and
    /// commit to it. The cut never moves — a moving boundary would
    /// rewrite bytes in the middle of the request.
    compact_from: Option<usize>,
}

impl ChatEngine {
    pub fn new(provider: Arc<dyn Provider>, model: String) -> Self {
        Self::with_all(provider, model, Shell::new(), new_bg_shells())
    }

    /// Full wiring — app passes its shared shell AND the bg-shell
    /// registry the footer tabs render from.
    pub fn with_all(
        provider: Arc<dyn Provider>,
        model: String,
        shell: Shell,
        shells: BgShells,
    ) -> Self {
        Self {
            provider,
            model,
            history: Vec::new(),
            rx: None,
            cancel: Arc::new(AtomicBool::new(false)),
            shell,
            shells,
            demote: Arc::new(AtomicBool::new(false)),
            opts: ChatOptions::default(),
            fenced: false,
            system_prompt: system_prompt(),
            compact_from: None,
        }
    }

    /// Swap in resolved options (num_ctx/think from inspect()).
    /// Called once after model selection — never mid-generation.
    pub fn set_options(&mut self, opts: ChatOptions) {
        self.opts = opts;
    }

    /// Mark the model as lacking native tool support — Phase 4 uses
    /// this to turn on fenced-block repair instead of tools[] calls.
    /// Stored now so the capability toast can land before Phase 4 ships.
    pub fn set_fenced(&mut self, fenced: bool) {
        self.fenced = fenced;
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
        self.cancel.store(false, Ordering::Relaxed);
        self.demote.store(false, Ordering::Relaxed);

        let provider = Arc::clone(&self.provider);
        let model = self.model.clone();
        let shell = self.shell.clone();
        let shells = Arc::clone(&self.shells);
        let demote = Arc::clone(&self.demote);
        let cancel = Arc::clone(&self.cancel);
        let opts = self.opts.clone();
        let fenced = self.fenced;
        // Budget: pick the plan once per send. KeepAll sends history
        // verbatim; Compact sends history[cut..] where `cut` is the
        // committed boundary from a previous send (or a fresh one
        // committed now). Either way the request is append-only.
        let budget_tokens = (self.opts.num_ctx as usize).saturating_mul(3) / 4;
        let plan = super::budget::plan(&self.history, budget_tokens, self.compact_from);
        if let super::budget::Plan::Compact { keep_from } = plan {
            self.compact_from = Some(keep_from);
        }
        let history_slice = match plan {
            super::budget::Plan::KeepAll => &self.history[..],
            super::budget::Plan::Compact { keep_from } => {
                &self.history[keep_from.min(self.history.len())..]
            }
        };
        let mut msgs = Vec::with_capacity(history_slice.len() + 1);
        msgs.push(Message::system(&self.system_prompt));
        msgs.extend(history_slice.iter().cloned());

        thread::spawn(move || {
            let mut appended: Vec<Message> = Vec::new();
            let result = (|| -> Result<(), String> {
                for _ in 0..MAX_ITERATIONS {
                    if cancel.load(Ordering::Relaxed) {
                        return Err("__cancelled__".to_string());
                    }
                    // Run the provider stream on its own thread so the
                    // agent loop polls cancel every ~50ms — a stalled
                    // HTTP read otherwise holds the whole reply hostage
                    // until the next chunk arrives.
                    let (ptx, prx) = std::sync::mpsc::channel::<Result<StreamChunk, String>>();
                    {
                        let provider = Arc::clone(&provider);
                        let model = model.clone();
                        let msgs = msgs.clone();
                        let specs = tools::specs();
                        let opts = opts.clone();
                        thread::spawn(move || {
                            let r = provider.chat(&model, &msgs, &specs, &opts, &|chunk| {
                                let _ = ptx.send(Ok(chunk));
                            });
                            let _ = ptx.send(match r {
                                Ok(()) => Err("__provider_done__".to_string()),
                                Err(e) => Err(e),
                            });
                        });
                    }
                    let mut text = String::new();
                    let mut calls: Vec<ToolCall> = Vec::new();
                    // Pump provider chunks until it signals done or the
                    // cancel flag fires.
                    loop {
                        match prx.recv_timeout(std::time::Duration::from_millis(50)) {
                            Ok(Ok(chunk)) => match chunk {
                                StreamChunk::Content(t) => {
                                    text.push_str(&t);
                                    let _ = tx.send(ChatEvent::Token(t));
                                }
                                StreamChunk::Thinking(t) => {
                                    let _ = tx.send(ChatEvent::Thinking(t));
                                }
                                StreamChunk::ToolCall { name, arguments } => {
                                    calls.push(ToolCall { name, arguments });
                                }
                                StreamChunk::Usage(u) => {
                                    let _ = tx.send(ChatEvent::Usage(u));
                                }
                            },
                            // Provider finished (sent __provider_done__) or hit an error.
                            Ok(Err(e)) if e == "__provider_done__" => break,
                            Ok(Err(e)) => return Err(e),
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                                if cancel.load(Ordering::Relaxed) {
                                    return Err("__cancelled__".to_string());
                                }
                            }
                            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                        }
                    }
                    if cancel.load(Ordering::Relaxed) {
                        return Err("__cancelled__".to_string());
                    }

                    // Fenced-mode repair: a model without native tool
                    // support often writes ```bash blocks or bare JSON
                    // instead of a real tool_call. Recover one from the
                    // text and run it like a normal call — the model
                    // never learns the difference.
                    if fenced
                        && calls.is_empty()
                        && let Some(c) = super::repair::recover(&text, &["bash"])
                    {
                        calls.push(c);
                    }

                    let mut assistant = Message::assistant(text);
                    if !calls.is_empty() {
                        assistant.tool_calls = Some(calls.clone());
                    }
                    msgs.push(assistant.clone());
                    appended.push(assistant);

                    if calls.is_empty() {
                        return Ok(());
                    }
                    let run_one = |call: &ToolCall| -> ToolOutcome {
                        // catch_unwind: a tool panic must not take the
                        // agent thread down — synthesize an error
                        // outcome so the loop keeps going.
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            tools::execute(&shell, &shells, &demote, &call.name, &call.arguments)
                        }))
                        .unwrap_or_else(|_| tools::ToolOutcome {
                            command: call.arguments["command"]
                                .as_str()
                                .unwrap_or("")
                                .to_string(),
                            result: crate::shell::ShellResult {
                                stdout: String::new(),
                                stderr: String::new(),
                                code: None,
                                duration: std::time::Duration::ZERO,
                                timed_out: false,
                                error: Some("error: tool panicked".to_string()),
                            },
                            spilled: None,
                            demoted: false,
                        })
                    };

                    // Parallel batch: 2+ bash calls that are all
                    // provably read-only run on scoped threads (cap
                    // 4). Anything mutating or unrecognized falls back
                    // to sequential — safety beats speed.
                    let all_batchable = calls.len() > 1
                        && calls.iter().all(|c| {
                            c.name == "bash"
                                && tools::batchable(c.arguments["command"].as_str().unwrap_or(""))
                        });
                    let outcomes: Vec<ToolOutcome> = if all_batchable {
                        for call in &calls {
                            let _ = tx.send(ChatEvent::ToolCall {
                                name: call.name.clone(),
                                args: call.arguments.clone(),
                            });
                        }
                        let mut results: Vec<Option<ToolOutcome>> =
                            (0..calls.len()).map(|_| None).collect();
                        std::thread::scope(|s| {
                            let mut slots = results.iter_mut();
                            for chunk in calls.chunks(4) {
                                let mut handles = Vec::new();
                                for call in chunk {
                                    let slot = slots.next().unwrap();
                                    handles.push(s.spawn(move || {
                                        *slot = Some(run_one(call));
                                    }));
                                }
                                for h in handles {
                                    let _ = h.join();
                                }
                            }
                        });
                        results.into_iter().map(|r| r.unwrap()).collect()
                    } else {
                        calls
                            .iter()
                            .map(|call| {
                                let _ = tx.send(ChatEvent::ToolCall {
                                    name: call.name.clone(),
                                    args: call.arguments.clone(),
                                });
                                run_one(call)
                            })
                            .collect()
                    };
                    for (call, outcome) in calls.iter().zip(outcomes) {
                        let text = outcome.to_tool_message();
                        let _ = tx.send(ChatEvent::ToolResult {
                            name: call.name.clone(),
                            outcome,
                        });
                        let tm = Message::tool(&call.name, &text);
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
                Err(e) if e == "__cancelled__" => {
                    let _ = tx.send(ChatEvent::Cancelled);
                }
                Err(e) => {
                    let _ = tx.send(ChatEvent::Error(e));
                }
            }
        });

        Ok(())
    }

    /// Ctrl+B×2: the in-flight foreground bash keeps running but the
    /// tool returns early — it becomes a normal background shell tab.
    pub fn demote(&self) {
        self.demote.store(true, Ordering::Relaxed);
    }

    /// Cancel an in-flight generation. The worker thread observes the
    /// flag at the next chunk or loop boundary and emits `Cancelled`.
    /// The user's message stays in history — the model still saw it.
    pub fn cancel(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Cancel AND drop the event channel: the worker unwinds on its own
    /// (its sends fail silently on the dead channel) but `is_busy()`
    /// clears right now so the next `send` doesn't wait for the worker.
    /// Used by force-send — the interrupt must feel instant.
    pub fn force_abort(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.rx = None;
    }

    /// Drain all pending events. Called every event-loop iteration.
    pub fn poll(&mut self) -> Vec<ChatEvent> {
        let Some(rx) = &self.rx else { return Vec::new() };
        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            let terminal = matches!(
                ev,
                ChatEvent::Done(_) | ChatEvent::Error(_) | ChatEvent::Cancelled
            );
            events.push(ev);
            if terminal {
                break;
            }
        }
        if events.iter().any(|e| {
            matches!(
                e,
                ChatEvent::Done(_) | ChatEvent::Error(_) | ChatEvent::Cancelled
            )
        }) {
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
