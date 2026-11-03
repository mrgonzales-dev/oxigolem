use std::sync::Arc;

use crossterm::event::KeyCode;

use crate::{
    agent::{
        chat::{ChatEngine, ChatEvent},
        tools::ToolOutcome,
    },
    modal::{Connection, Kind, Modal},
    provider::{self, Provider, Usage},
    shell::{new_bg_shells, BgShells, Shell},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Agent,
    /// Showing the output of the background shell at this index in
    /// `App::shells`. Blue-accented — same look as the old terminal view.
    Shell(usize),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    User,
    /// Assistant reasoning — its own dimmed block, separate from the reply.
    Thinking,
    Golem,
    Tool,
    /// Verbatim multi-line block (e.g. `/doctor` output) — dimmed, no
    /// tool-call chrome. Distinct from `Tool` so a report never picks
    /// up the `● bash · $ cmd` header by accident.
    Report,
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

/// Result of a background `list_models` fetch — provider index plus
/// the live list or the error string.
type ModelsFetch = (usize, Result<Vec<String>, String>);

pub struct App {
    pub view: View,
    parts: Vec<InputPart>,
    next_paste_id: usize,
    pub modal: Option<Modal>,
    pub connected: Option<Connection>,
    pub toast: Option<String>,
    pub transcript: Vec<TranscriptEntry>,
    providers: Vec<Arc<dyn Provider>>,
    pub chat: Option<ChatEngine>,
    /// Shared shell cwd — every bash call the agent makes runs here.
    /// `ChatEngine::with_all` clones this into the worker.
    pub shell: Shell,
    /// Registry of every spawned background/transient shell. The
    /// footer reads it for tabs; `tools::execute` pushes to it.
    pub shells: BgShells,
    /// Frame counter for animations — bumps every loop tick.
    pub tick: u64,
    /// Manual scrollback offset (visual rows up from the bottom).
    /// `None` = follow newest; `Some(n)` = pinned n rows up.
    pub scroll_back: Option<u16>,
    /// Same scroll-pin idea for Shell views — kept separate from
    /// `scroll_back` so each view remembers its own position.
    pub term_scroll: Option<u16>,
    /// Tick when the last generation finished — drives the shimmer→dim
    /// fade so thinking text eases out instead of snapping.
    pub gen_done_tick: Option<u64>,
    /// Token + timing counters from the most recent generation —
    /// the footer reads this for `cached%` and `t/s`. `None` until
    /// the first done chunk arrives.
    pub last_usage: Option<Usage>,
    /// Worker thread that fetches model lists off the UI thread.
    models_rx: Option<std::sync::mpsc::Receiver<ModelsFetch>>,

}

impl App {
    pub fn new() -> Self {
        // Spill files live under the project the TUI was opened in —
        // `<launch-cwd>/.oxigolem/jobs`. Created eagerly; tools::execute
        // picks this up via `init_project_jobs_dir`.
        if let Ok(cwd) = std::env::current_dir() {
            crate::agent::tools::init_project_jobs_dir(&cwd);
        }
        Self {
            view: View::Agent,
            parts: Vec::new(),
            next_paste_id: 1,
            modal: None,
            connected: None,
            toast: None,
            transcript: Vec::new(),
            providers: provider::registry(),
            chat: None,
            shell: Shell::new(),
            shells: new_bg_shells(),
            tick: 0,
            scroll_back: None,
            term_scroll: None,
            gen_done_tick: None,
            last_usage: None,
            models_rx: None,
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

    /// Shell-view scroll — same pin semantics on the bg-shell buffer.
    pub fn term_scroll_up(&mut self, n: u16) {
        self.term_scroll = Some(self.term_scroll.unwrap_or(0).saturating_add(n));
    }

    pub fn term_scroll_down(&mut self, n: u16) {
        if let Some(v) = self.term_scroll {
            match v.checked_sub(n) {
                Some(0) | None => self.term_scroll = None,
                Some(rest) => self.term_scroll = Some(rest),
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
            Kind::Palette => vec![
                "Connect provider".to_string(),
                "/doctor — model + runtime report".to_string(),
            ],
            Kind::Providers => self.providers.iter().map(|p| p.name().to_string()).collect(),
            Kind::ModelsLoading { cached, .. } => {
                cached.clone().unwrap_or_else(|| vec!["fetching…".to_string()])
            }
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
            Kind::Palette if choice.starts_with("/doctor") => {
                self.push_doctor_report();
                None
            }
            Kind::Palette => Some(Modal::new(Kind::Providers)),
            Kind::Providers => {
                let idx = self.providers.iter().position(|p| p.name() == choice);
                idx.map(|i| {
                    // Fetch the model list on a worker thread — a dead
                    // ollama host must not block the UI for 3s. The
                    // cached list (if any) shows immediately as a hint.
                    let cached = provider::read_model_list(self.providers[i].name());
                    let p = Arc::clone(&self.providers[i]);
                    let (tx, rx) = std::sync::mpsc::channel();
                    std::thread::spawn(move || {
                        let _ = tx.send((i, p.list_models()));
                    });
                    self.models_rx = Some(rx);
                    Modal::new(Kind::ModelsLoading { provider: i, cached })
                })
            }
            Kind::ModelsLoading { provider, .. } | Kind::Models { provider, .. } => {
                // Enter on a "fetching…" placeholder is a no-op.
                if choice == "fetching…" {
                    self.modal = Some(m);
                    return;
                }
                self.connect_model(provider, choice);
                None
            }
            Kind::Message(_) => None,
        };
    }

    /// Connect `providers[i]` to `model` — shared by the live Models
    /// picker and the ModelsLoading cache path. Resolves num_ctx +
    /// think from the model card once; every request this session uses
    /// the same options so the prompt prefix (and its KV cache) stays
    /// stable.
    fn connect_model(&mut self, i: usize, model: String) {
        let p = Arc::clone(&self.providers[i]);
        self.connected = Some(Connection {
            provider: self.providers[i].name(),
            model: model.clone(),
        });
        let mut engine = ChatEngine::with_all(
            Arc::clone(&p),
            model.clone(),
            self.shell.clone(),
            Arc::clone(&self.shells),
        );
        if let Some(ollama) = p.as_any().downcast_ref::<crate::provider::ollama::Ollama>() {
            match ollama.inspect(&model) {
                Ok(info) => {
                    if !info.supports("tools") {
                        self.toast =
                            Some(format!("{model}: no tools capability — fenced mode"));
                        engine.set_fenced(true);
                    }
                    let kv = crate::provider::ollama::kv_bytes_per_token(&info.raw);
                    let num_ctx = crate::provider::ollama::resolve_num_ctx(
                        info.context_length,
                        crate::provider::ollama::avail_ram_bytes(),
                        kv,
                    );
                    engine.set_options(crate::provider::ChatOptions {
                        num_ctx,
                        think: info.think_for(&model),
                    });
                }
                Err(e) => {
                    self.toast = Some(format!("inspect failed: {e}"));
                }
            }
        }
        self.chat = Some(engine);
    }

    /// Swap a ModelsLoading modal for the live list once the worker
    /// thread lands; on failure fall back to the cached list or an
    /// error message. Called every loop tick — cheap.
    pub fn poll_models(&mut self) {
        let Some(rx) = &self.models_rx else { return };
        let Ok((i, res)) = rx.try_recv() else { return };
        self.models_rx = None;
        // Only swap if the user is still in the loading modal for
        // this provider — otherwise the result is stale.
        let still_loading = matches!(
            self.modal.as_ref().map(|m| &m.kind),
            Some(Kind::ModelsLoading { provider, .. }) if *provider == i
        );
        if !still_loading {
            // Cache the live list anyway so a dead host next time
            // shows something.
            if let Ok(models) = &res {
                provider::write_model_list(self.providers[i].name(), models);
            }
            return;
        }
        self.modal = Some(match res {
            Ok(models) => {
                provider::write_model_list(self.providers[i].name(), &models);
                Modal::new(Kind::Models { provider: i, models })
            }
            Err(e) => {
                let cached = provider::read_model_list(self.providers[i].name());
                match cached {
                    Some(models) if !models.is_empty() => {
                        self.toast = Some(format!("offline — showing cached list ({e})"));
                        Modal::new(Kind::Models { provider: i, models })
                    }
                    _ => Modal::new(Kind::Message(e)),
                }
            }
        });
    }

    /// Try to run `text` as a slash command. Returns true when handled
    /// — caller should treat the input as consumed. Unknown commands
    /// return false so the model still sees them (the user may be
    /// asking about a literal `/path`).
    fn try_slash_command(&mut self, text: &str) -> bool {
        // Match the command word — `/doctor extra args` still hits.
        match text.split_whitespace().next() {
            Some("/doctor") => {
                self.push_doctor_report();
                true
            }
            _ => false,
        }
    }

    /// Length in chars of the recognized slash-command prefix at the
    /// start of `text`, or 0 when the input doesn't begin with a known
    /// command. Used by the input bar to paint `/doctor` orange while
    /// the user types. The `/` must sit at column 0 — `input_display`
    /// isn't trimmed, so a leading space would offset the highlight.
    pub fn slash_cmd_prefix_len(text: &str) -> usize {
        if !text.starts_with('/') {
            return 0;
        }
        let first = text.split_whitespace().next().unwrap_or("");
        match first {
            "/doctor" => first.chars().count(),
            _ => 0,
        }
    }

    /// Push the /doctor report as a transcript entry.
    fn push_doctor_report(&mut self) {
        self.transcript.push(TranscriptEntry {
            role: Role::Report,
            text: self.doctor_report(),
            outcome: None,
            expanded: true,
        });
    }

    /// /doctor — model + runtime report as a transcript entry. Shows
    /// the connected model's ctx, tool mode, and the last generation's
    /// cache hit rate / prefill / t/s / load time.
    fn doctor_report(&self) -> String {
        let mut out = String::from("── /doctor ──\n");
        match (&self.connected, &self.chat) {
            (Some(c), Some(engine)) => {
                out.push_str(&format!("provider : {}\n", c.provider));
                out.push_str(&format!("model    : {}\n", engine.model()));
                let o = engine.options();
                out.push_str(&format!("num_ctx  : {}\n", o.num_ctx));
                out.push_str(&format!("think    : {}\n", o.think));
                out.push_str(&format!(
                    "tools    : {}\n",
                    if engine.is_fenced() { "fenced repair" } else { "native" }
                ));
            }
            _ => {
                out.push_str("no model connected — pick one via the palette\n");
                return out;
            }
        }
        match &self.last_usage {
            Some(u) => {
                out.push_str(&format!(
                    "last turn: {}% cached ({}/{} prompt tokens)\n",
                    u.cached_pct() as u32,
                    u.cached,
                    u.prompt
                ));
                out.push_str(&format!("prefill  : {} ms\n", u.prefill_ms));
                out.push_str(&format!("t/s      : {:.1}\n", u.tokens_per_sec()));
                out.push_str(&format!("load     : {} ms\n", u.load_ms));
            }
            None => out.push_str("no generation yet this session\n"),
        }
        out
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
                InputPart::Paste { content, .. } => out.push_str(content.as_str()),
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

    /// Cycle the view: Agent → sh1 → sh2 → … → Agent. BackTab.
    /// Shell index is clamped to live entries — a transient shell that
    /// finished and was removed mid-view just falls back to Agent.
    pub fn next_view(&mut self) {
        let count = self.shells.lock().unwrap().len();
        self.view = match self.view {
            View::Agent if count > 0 => View::Shell(0),
            View::Shell(i) if i + 1 < count => View::Shell(i + 1),
            _ => View::Agent,
        };
        self.term_scroll = None;
    }

    /// If the current Shell view's index is past the end of the
    /// registry (shell finished and was removed), drop back to Agent.
    /// Called every tick — cheap.
    pub fn normalize_view(&mut self) {
        if let View::Shell(i) = self.view {
            let count = self.shells.lock().unwrap().len();
            if i >= count {
                self.view = View::Agent;
                self.term_scroll = None;
            }
        }
    }

    /// Ctrl+B pressed — two presses within a second demote the running
    /// foreground bash into a background shell. Returns "first" on the
    /// first press so the UI can hint "press again".
    pub fn ctrl_b(&mut self) -> &'static str {
        use std::time::{Duration, Instant};
        static LAST: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);
        let now = Instant::now();
        let mut last = LAST.lock().unwrap();
        match *last {
            Some(t) if now.duration_since(t) < Duration::from_millis(600) => {
                *last = None;
                if let Some(c) = self.chat.as_ref() {
                    c.demote();
                }
                "demoted"
            }
            _ => {
                *last = Some(now);
                "first"
            }
        }
    }

    /// Scan shells for exits the agent doesn't know about yet. When a
    /// non-transient shell finishes AND the engine is idle, inject a
    /// synthetic user message so the model gets told: "[shN exited C]
    /// log: <path>". Done here (not in the reader thread) so it only
    /// fires while the app is alive to render it.
    pub fn poll_shells(&mut self) {
        let Some(engine) = &mut self.chat else { return };
        if engine.is_busy() {
            return;
        }
        let pokes: Vec<String> = {
            let reg = self.shells.lock().unwrap();
            reg.iter()
                .filter(|s| {
                    !s.transient.load(std::sync::atomic::Ordering::Relaxed)
                        && !s.poked.load(std::sync::atomic::Ordering::Relaxed)
                        && matches!(
                            s.status(),
                            crate::shell::BgStatus::Exited(_) | crate::shell::BgStatus::Killed
                        )
                })
                .map(|s| {
                    s.poked.store(true, std::sync::atomic::Ordering::Relaxed);
                    match s.status() {
                        crate::shell::BgStatus::Exited(c) => format!(
                            "[{} exited {c}]\nlog: {}",
                            s.id, s.log_path
                        ),
                        _ => format!("[{} killed]\nlog: {}", s.id, s.log_path),
                    }
                })
                .collect()
        };
        for p in pokes {
            self.dispatch_chat(p);
        }
    }

    /// Send a prompt straight to the engine. Pushes the
    /// User transcript entry and starts the generation.
    fn dispatch_chat(&mut self, text: String) {
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

    /// Send input to the connected model. Called on Enter in Agent view.
    /// While a generation is running: typed text force-sends — it cancels
    /// the in-flight reply and jumps the queue so this prompt goes next.
    /// Esc is the only key that cancels without a new prompt.
    pub fn send_chat(&mut self) {
        let text = self.input_expanded();
        let text = text.trim().to_string();

        // Slash commands intercept before the provider check — they
        // never reach the model and must work unconnected (`/doctor`
        // reports "no model connected" itself).
        if text.starts_with('/') && self.try_slash_command(&text) {
            self.parts.clear();
            return;
        }

        if self.chat.is_none() {
            self.toast = Some("No provider connected — Ctrl+P to connect".to_string());
            return;
        }

        if self.chat.as_ref().unwrap().is_busy() {
            if text.is_empty() {
                // Enter on empty input does nothing while busy — Esc
                // is the interrupt key.
                return;
            }
            // Force-send: kill the running generation NOW (drop its
            // channel — the worker unwinds on its own) and dispatch
            // the new prompt immediately. No waiting for Cancelled.
            self.parts.clear();
            if let Some(c) = self.chat.as_mut() {
                c.force_abort();
            }
            self.dispatch_chat(text);
            return;
        }

        self.parts.clear();
        if text.is_empty() {
            return;
        }
        self.dispatch_chat(text);
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
                ChatEvent::Usage(u) => {
                    self.last_usage = Some(u);
                }
                ChatEvent::Cancelled => {
                    // Push a small "interrupted" marker so the transcript
                    // shows where the reply was cut off. Partial text
                    // stays visible; the user's question stays in history
                    // so they can ask again without losing context.
                    self.transcript.push(TranscriptEntry {
                        role: Role::Tool,
                        text: "interrupted".to_string(),
                        outcome: None,
                        expanded: false,
                    });
                    self.gen_done_tick = Some(self.tick);
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

    /// Esc pressed. Cancels an in-flight generation if one is running —
    /// that is its only job. Quitting lives on Ctrl+C (double-press),
    /// so Esc always returns false.
    pub fn esc(&mut self) -> bool {
        if let Some(c) = self.chat.as_mut()
            && c.is_busy()
        {
            c.cancel();
        }
        false
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

    /// Route Enter based on the current view. Shell views are
    /// read-only — Enter there does nothing.
    pub fn handle_enter(&mut self) {
        match self.view {
            View::Agent => self.send_chat(),
            View::Shell(_) => {}
        }
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}
