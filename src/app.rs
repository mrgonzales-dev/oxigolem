use std::sync::Arc;

use crossterm::event::KeyCode;

use crate::{
    agent::{
        chat::{ChatEngine, ChatEvent},
        tools::ToolOutcome,
    },
    modal::{Connection, Kind, Modal},
    provider::{self, Provider},
    shell::Shell,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Agent,
    Output,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    User,
    /// Assistant reasoning — its own dimmed block, separate from the reply.
    Thinking,
    Golem,
    Tool,
}

#[derive(Clone)]
pub struct TranscriptEntry {
    pub role: Role,
    pub text: String,
    /// Present on `Role::Tool` entries once the call has finished —
    /// carries the structured `ShellResult` so the UI can render a
    /// cursor-cli-style block (command, stdout, stderr, exit code,
    /// duration). `None` while the call is still streaming or on the
    /// `$ cmd` placeholder line.
    pub outcome: Option<ToolOutcome>,
    /// Whether a long tool block is expanded. `false` collapses output
    /// past a small line budget into a `… (N lines hidden)` row.
    pub expanded: bool,
}

/// One segment of the input buffer: typed text or a paste chip.
#[derive(Clone)]
pub enum InputPart {
    Text(char),
    Paste { id: usize, content: String },
}

pub struct App {
    pub view: View,
    parts: Vec<InputPart>,
    next_paste_id: usize,
    pub term_lines: Vec<String>,
    pub modal: Option<Modal>,
    pub connected: Option<Connection>,
    pub toast: Option<String>,
    pub transcript: Vec<TranscriptEntry>,
    providers: Vec<Arc<dyn Provider>>,
    pub chat: Option<ChatEngine>,
    /// Shared terminal: user commands in Output view and the agent's
    /// `bash` tool run in the same working directory.
    pub shell: Shell,
    /// Frame counter for animations — bumps every loop tick.
    pub tick: u64,
    /// Manual scrollback offset (visual rows up from the bottom).
    /// `None` = follow newest; `Some(n)` = pinned n rows up.
    pub scroll_back: Option<u16>,
    /// Tick when the last generation finished — drives the shimmer→dim
    /// fade so thinking text eases out instead of snapping.
    pub gen_done_tick: Option<u64>,
}

impl App {
    pub fn new() -> Self {
        Self {
            view: View::Agent,
            parts: Vec::new(),
            next_paste_id: 1,
            term_lines: vec!["oxigolem terminal — type a command below".to_string()],
            modal: None,
            connected: None,
            toast: None,
            transcript: Vec::new(),
            providers: provider::registry(),
            chat: None,
            shell: Shell::new(),
            tick: 0,
            scroll_back: None,
            gen_done_tick: None,
        }
    }

    /// Scroll the agent transcript up `n` visual rows (pin position).
    pub fn scroll_up(&mut self, n: u16) {
        self.scroll_back = Some(self.scroll_back.unwrap_or(0).saturating_add(n));
    }

    /// Scroll down; reaching the bottom re-engages auto-follow.
    pub fn scroll_down(&mut self, n: u16) {
        if let Some(v) = self.scroll_back {
            match v.checked_sub(n) {
                Some(0) | None => self.scroll_back = None,
                Some(rest) => self.scroll_back = Some(rest),
            }
        }
    }

    /// True while a generation is still streaming.
    pub fn chat_busy(&self) -> bool {
        self.chat.as_ref().is_some_and(|c| c.is_busy())
    }

    pub fn open_palette(&mut self) {
        self.modal = Some(Modal::new(Kind::Palette));
    }

    /// Handle Ctrl+C. Returns true if the app should quit.
    pub fn ctrl_c(&mut self) -> bool {
        if !self.is_input_empty() {
            self.parts.clear();
            self.toast = None;
            false
        } else if self.toast.is_some() {
            true
        } else {
            self.toast = Some("Press Ctrl+C again to exit".to_string());
            false
        }
    }

    /// Dismiss the toast if it is showing (called on any non-Ctrl+C key).
    pub fn dismiss_toast(&mut self) {
        self.toast = None;
    }

    /// Items of the open modal, narrowed by its filter.
    pub fn modal_items(&self) -> Vec<String> {
        let Some(m) = &self.modal else { return Vec::new() };
        let all: Vec<String> = match &m.kind {
            Kind::Palette => vec!["Connect provider".to_string()],
            Kind::Providers => self.providers.iter().map(|p| p.name().to_string()).collect(),
            Kind::Models { models, .. } => models.clone(),
            Kind::Message(_) => Vec::new(),
        };
        let f = m.filter.to_lowercase();
        all.into_iter().filter(|i| i.to_lowercase().contains(&f)).collect()
    }

    pub fn modal_key(&mut self, code: KeyCode) {
        let items = self.modal_items();
        let Some(m) = self.modal.as_mut() else { return };
        match code {
            KeyCode::Esc => self.modal = None,
            KeyCode::Up => m.sel = m.sel.saturating_sub(1),
            KeyCode::Down => {
                if m.sel + 1 < items.len() {
                    m.sel += 1;
                }
            }
            KeyCode::Backspace => {
                m.filter.pop();
                m.sel = 0;
            }
            KeyCode::Char(c) => {
                m.filter.push(c);
                m.sel = 0;
            }
            KeyCode::Enter => self.modal_select(items),
            _ => {}
        }
    }

    fn modal_select(&mut self, items: Vec<String>) {
        let Some(m) = self.modal.take() else { return };
        if matches!(m.kind, Kind::Message(_)) {
            return;
        }
        let Some(choice) = items.get(m.sel).cloned() else {
            self.modal = Some(m);
            return;
        };
        self.modal = match m.kind {
            Kind::Palette => Some(Modal::new(Kind::Providers)),
            Kind::Providers => {
                let idx = self.providers.iter().position(|p| p.name() == choice);
                idx.map(|i| match self.providers[i].list_models() {
                    Ok(models) => Modal::new(Kind::Models { provider: i, models }),
                    Err(e) => Modal::new(Kind::Message(e)),
                })
            }
            Kind::Models { provider, .. } => {
                let p = Arc::clone(&self.providers[provider]);
                self.connected = Some(Connection {
                    provider: self.providers[provider].name(),
                    model: choice.clone(),
                });
                self.chat = Some(ChatEngine::with_shell(p, choice, self.shell.clone()));
                None
            }
            Kind::Message(_) => None,
        };
    }

    pub fn toggle_view(&mut self) {
        self.view = match self.view {
            View::Agent => View::Output,
            View::Output => View::Agent,
        };
    }

    /// Render the input for display: paste segments become `[Pasted text #N +K lines]` chips.
    pub fn input_display(&self) -> String {
        let mut out = String::new();
        for part in &self.parts {
            match part {
                InputPart::Text(c) => out.push(*c),
                InputPart::Paste { id, content } => {
                    let lines = content.lines().count();
                    let bytes = content.len();
                    out.push_str(&format!("[Pasted text #{id} +{lines} lines {bytes}B]"));
                }
            }
        }
        out
    }

    /// Expand chips into the actual command string for execution.
    fn input_expanded(&self) -> String {
        let mut out = String::new();
        for part in &self.parts {
            match part {
                InputPart::Text(c) => out.push(*c),
                InputPart::Paste { content, .. } => out.push_str(content),
            }
        }
        out
    }

    pub fn is_input_empty(&self) -> bool {
        self.parts.is_empty()
    }

    pub fn push_char(&mut self, c: char) {
        self.parts.push(InputPart::Text(c));
    }

    pub fn pop_input(&mut self) {
        self.parts.pop();
    }

    pub fn push_paste(&mut self, content: String) {
        let id = self.next_paste_id;
        self.next_paste_id += 1;
        self.parts.push(InputPart::Paste { id, content });
    }

    pub fn run_command(&mut self) {
        let cmd = self.input_expanded();
        self.parts.clear();
        let cmd = cmd.trim().to_string();
        if cmd.is_empty() {
            return;
        }

        self.term_lines.push(format!("$ {cmd}"));

        if cmd == "clear" {
            self.term_lines.clear();
            return;
        }

        // Shared shell — a `cd` here also moves the agent's bash tool.
        let out = self.shell.run(&cmd);
        for line in out.lines() {
            self.term_lines.push(line.to_string());
        }
    }

    /// Send input to the connected model. Called on Enter in Agent view.
    pub fn send_chat(&mut self) {
        if self.chat.is_none() {
            self.toast = Some("No provider connected — Ctrl+P to connect".to_string());
            return;
        }
        if self.chat.as_ref().unwrap().is_busy() {
            self.toast = Some("Generation in progress…".to_string());
            return;
        }
        let text = self.input_expanded();
        self.parts.clear();
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.transcript.push(TranscriptEntry {
            role: Role::User,
            text: text.clone(),
            outcome: None,
            expanded: false,
        });
        self.gen_done_tick = None;
        if let Err(e) = self.chat.as_mut().unwrap().send(text) {
            self.toast = Some(e);
            self.transcript.pop();
        }
    }

    /// Drain chat events from the background thread. Called every loop tick.
    pub fn poll_chat(&mut self) {
        let Some(engine) = &mut self.chat else { return };
        let events = engine.poll();
        for ev in events {
            match ev {
                ChatEvent::Thinking(t) => {
                    Self::append_or_push(&mut self.transcript, Role::Thinking, t);
                }
                ChatEvent::Token(token) => {
                    // First reply token after a thinking block starts the
                    // shimmer→dim fade immediately, not at Done.
                    if self.transcript.last().is_some_and(|e| matches!(e.role, Role::Thinking)) {
                        self.gen_done_tick = Some(self.tick);
                    }
                    Self::append_or_push(&mut self.transcript, Role::Golem, token);
                }
                ChatEvent::ToolCall { name, args } => {
                    // Push a header entry now so the block has somewhere
                    // to live while the command runs. The follow-up
                    // `ToolResult` attaches the structured outcome to
                    // this same entry.
                    let command = if name == "bash" {
                        args["command"].as_str().unwrap_or("").to_string()
                    } else {
                        args.to_string()
                    };
                    self.transcript.push(TranscriptEntry {
                        role: Role::Tool,
                        text: command,
                        outcome: None,
                        expanded: false,
                    });
                }
                ChatEvent::ToolResult { name, outcome } => {
                    // Attach the outcome to the most recent in-flight
                    // tool entry; if we somehow missed the ToolCall
                    // (shouldn't happen), push a fresh one.
                    let entry = self
                        .transcript
                        .iter_mut()
                        .rev()
                        .find(|e| matches!(e.role, Role::Tool) && e.outcome.is_none());
                    match entry {
                        Some(e) => {
                            if e.text.is_empty() {
                                e.text = outcome.command.clone();
                            }
                            e.outcome = Some(outcome);
                        }
                        None => self.transcript.push(TranscriptEntry {
                            role: Role::Tool,
                            text: format!("[{name}]"),
                            outcome: Some(outcome),
                            expanded: false,
                        }),
                    }
                }
                ChatEvent::Done(msgs) => {
                    engine.record_appended(msgs);
                    // Don't re-stamp — the thinking→reply transition
                    // already started the fade; a fresh stamp would
                    // re-light the thinking block at end of generation.
                    if self.gen_done_tick.is_none() {
                        self.gen_done_tick = Some(self.tick);
                    }
                }
                ChatEvent::Error(e) => {
                    while let Some(last) = self.transcript.last()
                        && matches!(last.role, Role::Thinking | Role::Golem)
                        && last.text.is_empty()
                    {
                        self.transcript.pop();
                    }
                    self.transcript.push(TranscriptEntry {
                        role: Role::Tool,
                        text: format!("chat error: {e}"),
                        outcome: None,
                        expanded: false,
                    });
                    self.toast = Some("chat failed — see transcript".to_string());
                }
            }
        }
    }

    /// Append text to the last entry if it has the same role, else push a new one.
    fn append_or_push(transcript: &mut Vec<TranscriptEntry>, role: Role, text: String) {
        if let Some(last) = transcript.last_mut()
            && last.role == role
        {
            last.text.push_str(&text);
            return;
        }
        transcript.push(TranscriptEntry {
            role,
            text,
            outcome: None,
            expanded: false,
        });
    }

    /// Toggle the most recent tool block between collapsed and expanded.
    /// Used by `Ctrl+E` in the Agent view.
    pub fn toggle_tool_expand(&mut self) {
        if let Some(e) = self
            .transcript
            .iter_mut()
            .rev()
            .find(|e| matches!(e.role, Role::Tool) && e.outcome.is_some())
        {
            e.expanded = !e.expanded;
        }
    }

    /// Route Enter based on the current view.
    pub fn handle_enter(&mut self) {
        match self.view {
            View::Agent => self.send_chat(),
            View::Output => self.run_command(),
        }
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}
