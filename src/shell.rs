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

/// One shared shell for the whole app: the model's `bash` tool calls
/// run in this working directory, so a `cd` sticks across calls.
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

    /// Legacy flattened form: the `! stderr` + `exit N` single-string
    /// shape the tool message payload uses.
    pub fn run(&self, cmd: &str) -> String {
        self.run_result(cmd).to_text()
    }
}

impl Default for Shell {
    fn default() -> Self {
        Self::new()
    }
}

/// Max lines kept in a background shell's in-memory ring buffer per
/// stream. Full output always lands in the log file — this bounds RAM
/// for chatty servers running for hours.
pub const BG_RING_LINES: usize = 10_000;

/// Lifecycle of a background shell — read under `BgShell::status`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BgStatus {
    Running,
    Exited(i32),
    Killed,
}

/// A detached shell process. Spawned in its own process group with a
/// snapshot of the shared shell's cwd — it can't move or be moved by
/// the shared shell unless spawned with `foreground` (pwd marker).
/// Stdout/stderr each drain into a bounded ring buffer for the UI and
/// a complete file under the jobs dir (`<id>.log` / `<id>.err`) — the
/// file is what the model greps and what a finished foreground call
/// rebuilds its `ShellResult` from.
pub struct BgShell {
    /// Display id, e.g. "sh3" — also the log filename stem.
    pub id: String,
    pub command: String,
    /// Stdout ring buffer shared with the reader thread.
    pub lines: Arc<Mutex<std::collections::VecDeque<String>>>,
    /// Stderr ring buffer.
    pub err_lines: Arc<Mutex<std::collections::VecDeque<String>>>,
    pub status: Arc<Mutex<BgStatus>>,
    /// Path to the complete stdout log (what the model greps).
    pub log_path: String,
    /// Path to the complete stderr log.
    pub err_path: String,
    /// True while this shell is a foreground bash call in disguise —
    /// `execute` removes it from the registry when it finishes.
    /// Demoting (Ctrl+B×2) flips it false so it stays as a tab.
    pub transient: std::sync::atomic::AtomicBool,
    /// Set once the app has poked the agent about this shell's exit.
    pub poked: std::sync::atomic::AtomicBool,
    /// Child pid — needed to kill the process group.
    pgid: i32,
}

impl BgShell {
    /// Snapshot for the shell view: stdout lines, then stderr as
    /// `! `-prefixed lines (streams aren't interleaved — the log
    /// files keep each stream whole instead).
    pub fn snapshot(&self) -> Vec<String> {
        let mut v: Vec<String> = self.lines.lock().unwrap().iter().cloned().collect();
        v.extend(
            self.err_lines
                .lock()
                .unwrap()
                .iter()
                .map(|l| format!("! {l}")),
        );
        v
    }

    pub fn status(&self) -> BgStatus {
        *self.status.lock().unwrap()
    }

    /// SIGKILL the process group. Idempotent.
    pub fn kill(&self) {
        let mut st = self.status.lock().unwrap();
        if *st == BgStatus::Running {
            unsafe {
                libc::kill(-self.pgid, libc::SIGKILL);
            }
            *st = BgStatus::Killed;
        }
    }
}

/// Shared registry of background shells — the agent tool pushes new
/// ones, the App reads them for the footer tabs and shell view.
pub type BgShells = Arc<Mutex<Vec<Arc<BgShell>>>>;

pub fn new_bg_shells() -> BgShells {
    Arc::new(Mutex::new(Vec::new()))
}

/// Push a line onto a ring buffer, dropping the oldest past the cap.
fn ring_push(q: &Mutex<std::collections::VecDeque<String>>, line: String) {
    let mut q = q.lock().unwrap();
    q.push_back(line);
    while q.len() > BG_RING_LINES {
        q.pop_front();
    }
}

/// Read a whole file into a string, capped at `cap` bytes — used to
/// rebuild a `ShellResult` from a finished shell's logs.
pub fn read_capped(path: &std::path::Path, cap: usize) -> String {
    let mut s = String::new();
    if let Ok(f) = std::fs::File::open(path) {
        let _ = f.take(cap as u64).read_to_string(&mut s);
    }
    s
}

impl Shell {
    /// Spawn `cmd` detached: own pgid, stdin from /dev/null, stdout and
    /// stderr piped to reader threads that fill the ring buffers and
    /// the `<id>.log` / `<id>.err` files. `foreground` also wraps the
    /// command with the pwd marker (compound `cd`s move the shared
    /// shell) and marks the shell transient so `execute` drops it from
    /// the registry when it exits. `id` like "sh1" comes from the
    /// caller; `jobs_dir` is where the logs land.
    pub fn spawn_bg(
        &self,
        id: &str,
        cmd: &str,
        jobs_dir: &std::path::Path,
        foreground: bool,
    ) -> Result<Arc<BgShell>, String> {
        let cwd = self.cwd.lock().unwrap().clone();
        let log_path = jobs_dir.join(format!("{id}.log"));
        let err_path = jobs_dir.join(format!("{id}.err"));
        let wrapped = if foreground {
            format!("{cmd}; printf '\\0__OXIGOLEM_PWD__\\n%s' \"$PWD\"")
        } else {
            cmd.to_string()
        };

        let mut child = unsafe {
            Command::new("bash")
                .arg("-c")
                .arg(&wrapped)
                .current_dir(&cwd)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .pre_exec(|| {
                    libc::setpgid(0, 0);
                    Ok(())
                })
                .spawn()
                .map_err(|e| format!("spawn failed: {e}"))?
        };
        let pgid = child.id() as i32;

        let lines = Arc::new(Mutex::new(std::collections::VecDeque::new()));
        let err_lines = Arc::new(Mutex::new(std::collections::VecDeque::new()));
        let status = Arc::new(Mutex::new(BgStatus::Running));
        let shell = Arc::new(BgShell {
            id: id.to_string(),
            command: cmd.to_string(),
            lines: Arc::clone(&lines),
            err_lines: Arc::clone(&err_lines),
            status: Arc::clone(&status),
            log_path: log_path.to_string_lossy().into_owned(),
            err_path: err_path.to_string_lossy().into_owned(),
            transient: std::sync::atomic::AtomicBool::new(foreground),
            poked: std::sync::atomic::AtomicBool::new(false),
            pgid,
        });

        // Stdout reader: ring buffer + full log. In foreground mode the
        // trailing `\0__OXIGOLEM_PWD__` marker line is intercepted and
        // the next line becomes the shared cwd — never displayed.
        let mut out = child.stdout.take().unwrap();
        let out_lines = Arc::clone(&lines);
        let shared_cwd = Arc::clone(&self.cwd);
        let ot = std::thread::spawn(move || {
            use std::io::{BufRead, BufReader, Write};
            let mut reader = BufReader::new(&mut out);
            let mut log = std::fs::File::create(&log_path).ok();
            let mut line = String::new();
            let mut want_dir = false;
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let t = line.trim_end_matches('\n');
                        if want_dir {
                            let dir = t.trim();
                            if std::path::Path::new(dir).is_dir() {
                                *shared_cwd.lock().unwrap() = dir.into();
                            }
                            want_dir = false;
                            continue;
                        }
                        if t == "\0__OXIGOLEM_PWD__" {
                            want_dir = true;
                            continue;
                        }
                        ring_push(&out_lines, t.to_string());
                        if let Some(f) = log.as_mut() {
                            let _ = writeln!(f, "{t}");
                        }
                    }
                }
            }
        });

        // Stderr reader: same treatment, own ring + own file.
        let mut err = child.stderr.take().unwrap();
        let err_lines2 = Arc::clone(&err_lines);
        let et = std::thread::spawn(move || {
            use std::io::{BufRead, BufReader, Write};
            let mut reader = BufReader::new(&mut err);
            let mut log = std::fs::File::create(&err_path).ok();
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let t = line.trim_end_matches('\n');
                        ring_push(&err_lines2, t.to_string());
                        if let Some(f) = log.as_mut() {
                            let _ = writeln!(f, "{t}");
                        }
                    }
                }
            }
        });

        // Waiter: join the readers first so the log files are complete
        // when status flips to Exited — a finished foreground call reads
        // its result straight off disk.
        std::thread::spawn(move || {
            let code = child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);
            let _ = ot.join();
            let _ = et.join();
            let mut st = status.lock().unwrap();
            if *st == BgStatus::Running {
                *st = BgStatus::Exited(code);
            }
        });

        Ok(shell)
    }

    /// Poll a shell until it exits, times out, or `demote` fires.
    /// Finished → `ShellResult` rebuilt from the log files (full
    /// streams, capped at MAX_CAPTURE). Timeout → process group
    /// killed, `timed_out` result. Demote → `None`; the caller marks
    /// the shell non-transient and tells the model where to find it.
    pub fn wait_bg(
        &self,
        bg: &Arc<BgShell>,
        demote: &std::sync::atomic::AtomicBool,
    ) -> Option<ShellResult> {
        let started = Instant::now();
        loop {
            match bg.status() {
                BgStatus::Running => {}
                BgStatus::Exited(code) => {
                    return Some(ShellResult {
                        stdout: read_capped(
                            std::path::Path::new(&bg.log_path),
                            MAX_CAPTURE,
                        ),
                        stderr: read_capped(
                            std::path::Path::new(&bg.err_path),
                            MAX_CAPTURE,
                        ),
                        code: Some(code),
                        duration: started.elapsed(),
                        timed_out: false,
                        error: None,
                    });
                }
                BgStatus::Killed => {
                    return Some(ShellResult {
                        stdout: read_capped(
                            std::path::Path::new(&bg.log_path),
                            MAX_CAPTURE,
                        ),
                        stderr: read_capped(
                            std::path::Path::new(&bg.err_path),
                            MAX_CAPTURE,
                        ),
                        code: None,
                        duration: started.elapsed(),
                        timed_out: true,
                        error: None,
                    });
                }
            }
            if demote.load(std::sync::atomic::Ordering::Relaxed) {
                return None;
            }
            if started.elapsed() >= TIMEOUT {
                unsafe {
                    let my_pgid = libc::getpgid(0);
                    let child_pgid = libc::getpgid(bg.pgid);
                    if child_pgid != my_pgid {
                        libc::kill(-child_pgid, libc::SIGKILL);
                    } else {
                        libc::kill(bg.pgid, libc::SIGKILL);
                    }
                }
                *bg.status.lock().unwrap() = BgStatus::Killed;
                return Some(ShellResult {
                    stdout: read_capped(std::path::Path::new(&bg.log_path), MAX_CAPTURE),
                    stderr: read_capped(std::path::Path::new(&bg.err_path), MAX_CAPTURE),
                    code: None,
                    duration: started.elapsed(),
                    timed_out: true,
                    error: None,
                });
            }
            std::thread::sleep(Duration::from_millis(20));
        }
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
