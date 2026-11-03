use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Max time a single command may run before being killed.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Max output chars kept per command — head and tail with a gap marker.
const MAX_OUTPUT: usize = 4000;

/// One shared shell for the whole app: the user's Output-view commands and
/// the model's `bash` tool calls run in the same working directory, so a
/// `cd` by either side sticks for everyone.
#[derive(Clone)]
pub struct Shell {
    cwd: Arc<Mutex<std::path::PathBuf>>,
}

impl Shell {
    pub fn new() -> Self {
        Self {
            cwd: Arc::new(Mutex::new(
                std::env::current_dir().unwrap_or_else(|_| "/".into()),
            )),
        }
    }

    /// Run `cmd` in the shared directory. Returns formatted output:
    /// stdout lines, stderr lines prefixed `! `, `exit N` on failure.
    /// A bare `cd` updates the shared working directory (a spawned bash
    /// dies with its cwd, so we track it here). `cd` inside a longer
    /// compound command affects only that command.
    pub fn run(&self, cmd: &str) -> String {
        // Bare `cd` only — a whole command like `cd /tmp`. Compounds
        // (`cd x && y`, `cd x; y`) go through bash where the marker
        // captures the final dir anyway.
        if let Some(target) = cd_target(cmd) {
            let mut cwd = self.cwd.lock().unwrap();
            let resolved = if target.starts_with('/') {
                std::path::PathBuf::from(&target)
            } else {
                cwd.join(&target)
            };
            if resolved.is_dir() {
                *cwd = resolved.canonicalize().unwrap_or(resolved);
                return String::new();
            }
            return format!("! cd: no such directory: {target}\nexit 1");
        }

        let dir = self.cwd.lock().unwrap().clone();
        // A spawned bash dies with its cwd — but a compound command like
        // `cd x && make` must still move the shared dir. So the child
        // prints its final pwd after a NUL marker; we split it off the
        // real output and store it as the new cwd. The NUL comes from
        // printf's \0 escape — an actual NUL can't go in argv.
        const MARK: &str = "\0__OXIGOLEM_PWD__\n";
        let wrapped = format!("{cmd}; printf '\\0__OXIGOLEM_PWD__\\n%s' \"$PWD\"");
        let mut child = match unsafe {
            Command::new("bash")
                .arg("-c")
                .arg(&wrapped)
                .current_dir(&dir)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                // Own process group so a timeout kills the command AND
                // any children it spawned — not our whole app.
                .pre_exec(|| {
                    libc::setpgid(0, 0);
                    Ok(())
                })
                .spawn()
        } {
            Ok(c) => c,
            Err(e) => return format!("spawn failed: {e}"),
        };
        let pgid = child.id() as i32;

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let status = child.wait();
            let mut out = Vec::new();
            let mut err = Vec::new();
            if let Some(mut so) = child.stdout.take() {
                let _ = so.read_to_end(&mut out);
            }
            if let Some(mut se) = child.stderr.take() {
                let _ = se.read_to_end(&mut err);
            }
            let _ = tx.send((out, err, status.ok()));
        });

        let (out, err, status) = match rx.recv_timeout(TIMEOUT) {
            Ok(v) => v,
            Err(_) => {
                unsafe {
                    // Kill the process GROUP only when the child's own
                    // pgid actually differs from ours — if setpgid failed
                    // in pre_exec, a group kill would take down the app.
                    let child_pgid = libc::getpgid(pgid);
                    if child_pgid != libc::getpgid(0) {
                        libc::kill(-child_pgid, libc::SIGKILL);
                    } else {
                        libc::kill(pgid, libc::SIGKILL);
                    }
                }
                return format!("! timed out after {}s", TIMEOUT.as_secs());
            }
        };

        let raw = String::from_utf8_lossy(&out).to_string();
        let (lines, new_dir) = match raw.rfind(MARK) {
            Some(pos) => {
                let dir = raw[pos + MARK.len()..].trim().to_string();
                (raw[..pos].to_string(), Some(dir))
            }
            None => (raw, None),
        };
        if let Some(d) = new_dir
            && std::path::Path::new(&d).is_dir()
        {
            *self.cwd.lock().unwrap() = d.into();
        }

        let mut lines = lines;
        for l in String::from_utf8_lossy(&err).lines() {
            lines.push_str(&format!("! {l}\n"));
        }
        if let Some(s) = status
            && s.code().unwrap_or(-1) != 0
        {
            lines.push_str(&format!("exit {}", s.code().unwrap_or(-1)));
        }
        truncate(&lines)
    }
}

impl Default for Shell {
    fn default() -> Self {
        Self::new()
    }
}

/// If `cmd` is exactly `cd` or `cd <path>` (nothing else — no `&&`, `;`,
/// `|`, comments), return the resolved target path. `cd` alone → $HOME,
/// `~` and `~/…` expand, surrounding quotes stripped.
fn cd_target(cmd: &str) -> Option<String> {
    let t = cmd.trim();
    let rest = if t == "cd" {
        ""
    } else {
        t.strip_prefix("cd ")?
    };
    let rest = rest.trim();
    // Anything with shell metachars isn't a bare cd — let bash run it.
    if rest.contains(['&', ';', '|', '>', '<', '#', '\n', '`', '$', '(', ')']) {
        return None;
    }
    if rest.is_empty() {
        return std::env::var("HOME").ok();
    }
    // Strip one layer of matching quotes so `cd "my dir"` works.
    let unquoted = rest
        .strip_prefix('"')
        .and_then(|r| r.strip_suffix('"'))
        .or_else(|| rest.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')))
        .unwrap_or(rest);
    if unquoted == "~" {
        return std::env::var("HOME").ok();
    }
    if let Some(tail) = unquoted.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return Some(format!("{home}/{tail}"));
    }
    Some(unquoted.to_string())
}

/// Keep output bounded: first half + last half with a gap line.
fn truncate(s: &str) -> String {
    if s.len() <= MAX_OUTPUT {
        return s.to_string();
    }
    let half = MAX_OUTPUT / 2;
    let head: String = s.chars().take(half).collect();
    let tail: String = s
        .chars()
        .rev()
        .take(half)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{head}\n… output truncated …\n{tail}")
}
