use std::process::Command;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Agent,
    Output,
}

pub struct App {
    pub view: View,
    pub input: String,
    pub term_lines: Vec<String>,
}

impl App {
    pub fn new() -> Self {
        Self {
            view: View::Agent,
            input: String::new(),
            term_lines: vec!["oxigolem terminal — type a command below".to_string()],
        }
    }

    pub fn toggle_view(&mut self) {
        self.view = match self.view {
            View::Agent => View::Output,
            View::Output => View::Agent,
        };
    }

    pub fn run_command(&mut self) {
        let cmd = self.input.trim().to_string();
        self.input.clear();
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
