use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Max time a single command may run before being killed.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Max output chars kept per command — head and tail with a gap marker.
/// Used by `to_text` (the flat form for the terminal view + tool message
/// preview). The full `stdout`/`stderr` fields keep up to MAX_CAPTURE.
const MAX_OUTPUT: usize = 4000;

/// Max bytes captured per stream from the child — bounds memory/disk for
/// pathological dumps while keeping far more than the inline cap so
/// large outputs can spill to a file intact.
const MAX_CAPTURE: usize = 256 * 1024;

/// Result of one command: raw streams plus exit/timeout metadata. The
/// agent tool turns this into a `tool` message; the UI renders it as a
/// structured block (header/output/footer), so we keep streams split
/// rather than pre-formatted.
#[derive(Clone, Debug)]
pub struct ShellResult {
    pub stdout: String,
    pub stderr: String,
    /// Exit code if the process finished; `None` when killed on timeout
    /// or when spawn failed before a status could be read.
    pub code: Option<i32>,
    pub duration: Duration,
    pub timed_out: bool,
    /// Spawn-level error (e.g. `bash` missing). When set, stdout/stderr
    /// are empty and `code` is None.
    pub error: Option<String>,
}

impl ShellResult {
    /// Flatten to the single-string form the Ollama `tool` message and
    /// the Output-view terminal both expect: stdout lines, stderr lines
    /// prefixed `! `, `exit N` on nonzero or `timed out` on kill.
    pub fn to_text(&self) -> String {
        let mut s = String::new();
        if let Some(e) = &self.error {
            s.push_str(e);
            return s;
        }
        if self.timed_out {
            s.push_str(&format!("! timed out after {}s", TIMEOUT.as_secs()));
            return s;
        }
        s.push_str(&self.stdout);
        for l in self.stderr.lines() {
            s.push_str(&format!("! {l}\n"));
        }
        if let Some(c) = self.code
            && c != 0
        {
            s.push_str(&format!("exit {c}"));
        }
        truncate(&s)
    }
}

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

    /// Run `cmd` in the shared directory and return a structured result.
    /// A bare `cd` updates the shared working directory without spawning.
    pub fn run_result(&self, cmd: &str) -> ShellResult {
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
                return ShellResult {
                    stdout: String::new(),
                    stderr: String::new(),
                    code: Some(0),
                    duration: Duration::ZERO,
                    timed_out: false,
                    error: None,
                };
            }
            return ShellResult {
                stdout: String::new(),
                stderr: format!("cd: no such directory: {target}"),
                code: Some(1),
                duration: Duration::ZERO,
                timed_out: false,
                error: None,
            };
        }

        let dir = self.cwd.lock().unwrap().clone();
        // A spawned bash dies with its cwd — but a compound command like
        // `cd x && make` must still move the shared dir. So the child
        // prints its final pwd after a NUL marker; we split it off the
        // real output and store it as the new cwd. The NUL comes from
        // printf's \0 escape — an actual NUL can't go in argv.
        const MARK: &str = "\0__OXIGOLEM_PWD__\n";
        let wrapped = format!("{cmd}; printf '\\0__OXIGOLEM_PWD__\\n%s' \"$PWD\"");
        let started = Instant::now();
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
            Err(e) => {
                return ShellResult {
                    stdout: String::new(),
                    stderr: String::new(),
                    code: None,
                    duration: started.elapsed(),
                    timed_out: false,
                    error: Some(format!("spawn failed: {e}")),
                }
            }
        };
        let pgid = child.id() as i32;

        // Drain stdout/stderr on their own threads while the child runs.
        // Waiting on exit BEFORE draining deadlocks past the 64KB pipe
        // buffer: the child blocks on write, we block on wait, the 30s
        // timeout kills it. Read-capped threads keep big outputs alive.
        let (dtx, drx) = std::sync::mpsc::channel();
        let mut so = child.stdout.take().unwrap();
        let mut se = child.stderr.take().unwrap();
        let ot = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = so.by_ref().take(MAX_CAPTURE as u64).read_to_end(&mut buf);
            buf
        });
        let et = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = se.by_ref().take(MAX_CAPTURE as u64).read_to_end(&mut buf);
            buf
        });
        std::thread::spawn(move || {
            let status = child.wait();
            let out = ot.join().unwrap_or_default();
            let err = et.join().unwrap_or_default();
            let _ = dtx.send((out, err, status.ok()));
        });
        let rx = drx;

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
                return ShellResult {
                    stdout: String::new(),
                    stderr: String::new(),
                    code: None,
                    duration: started.elapsed(),
                    timed_out: true,
                    error: None,
                };
            }
        };

        let raw = String::from_utf8_lossy(&out).to_string();
        let (stdout, new_dir) = match raw.rfind(MARK) {
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

        ShellResult {
            stdout,
            stderr: String::from_utf8_lossy(&err).to_string(),
            code: status.and_then(|s| s.code()),
            duration: started.elapsed(),
            timed_out: false,
            error: None,
        }
    }

    /// Legacy flattened form: same output the tool message and terminal
    /// view have always used. Kept so existing callers (app Output view,
    /// tool message payload) stay one-line.
    pub fn run(&self, cmd: &str) -> String {
        self.run_result(cmd).to_text()
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
