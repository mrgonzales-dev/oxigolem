use std::process::Command;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Agent,
    Output,
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
}

impl App {
    pub fn new() -> Self {
        Self {
            view: View::Agent,
            parts: Vec::new(),
            next_paste_id: 1,
            term_lines: vec!["oxigolem terminal — type a command below".to_string()],
        }
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

        let out = Command::new("bash").arg("-c").arg(&cmd).output();

        match out {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                let stderr = String::from_utf8_lossy(&o.stderr);
                for line in stdout.lines() {
                    self.term_lines.push(line.to_string());
                }
                for line in stderr.lines() {
                    self.term_lines.push(format!("! {line}"));
                }
                let code = o.status.code().unwrap_or(-1);
                if code != 0 {
                    self.term_lines.push(format!("exit {code}"));
                }
            }
            Err(e) => self.term_lines.push(format!("spawn failed: {e}")),
        }
    }
}
