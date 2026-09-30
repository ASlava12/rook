//! The TUI, started for real on a pseudo-terminal.
//!
//! Nothing else can see that it starts: it needs a tty to render at all, and a
//! panic on launch would leave every other test green. Two things make this
//! work — the window size has to be set with `TIOCSWINSZ` or ratatui draws into
//! a zero-sized terminal and emits nothing, and the output has to be replayed
//! into a grid before it can be read, because characters are placed cell by cell
//! and a word the screen plainly shows is never contiguous in the byte stream.

#![cfg(unix)]

use std::io::Read;
use std::os::fd::{FromRawFd, OwnedFd};
use std::process::{Child, Command, Stdio};

/// How long a wait may take before it is a failure rather than a slow machine.
///
/// Not a performance claim: every wait here ends the moment the thing it is
/// waiting for arrives, so this only decides how long a genuinely stuck app
/// hangs the suite. It was twenty seconds, and a FreeBSD VM running nine of
/// these at once starved one of them past that while it was still drawing.
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(60);

struct Pty {
    master: std::fs::File,
    child: Child,
    /// Every byte so far. A redraw after a keypress only emits the cells that
    /// changed, so a frame has to be replayed over the ones before it.
    seen: String,
}

impl Pty {
    fn spawn(program: &std::path::Path, args: &[&str], env: &[(&str, &str)], cols: u16, rows: u16) -> Self {
        let (mut master, mut slave) = (0, 0);
        let mut size = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
        // A raw pointer rather than `&mut`: the last parameter is `*mut winsize`
        // on macOS and `*const winsize` on Linux, and a `&mut` passed to the
        // second is a clippy error under `-D warnings`. `*mut` weakens to
        // `*const`, so one spelling satisfies both.
        let opened = unsafe {
            libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), &raw mut size)
        };
        assert_eq!(opened, 0, "openpty failed");

        let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave));
        for (k, v) in env {
            command.env(k, v);
        }
        let child = command.spawn().unwrap();
        Self { master: std::fs::File::from(master), child, seen: String::new() }
    }

    /// Collect frames for a fixed window, then read the screen off them.
    ///
    /// Not "until it goes quiet": the app redraws on a 60ms tick whether or not
    /// anything changed, so the stream never is.
    /// Waits for a frame, not for a byte: entering the alternate screen writes
    /// before anything is drawn, and whatever the app does in between — opening
    /// a store, probing the machine for what skills apply — lands in that gap.
    fn screen(&mut self, cols: usize, rows: usize) -> Vec<String> {
        let painted = |seen: &str| grid(seen, cols, rows).iter().filter(|line| !line.is_empty()).count();
        let deadline = std::time::Instant::now() + PATIENCE;
        while painted(&self.seen) <= 3 && std::time::Instant::now() < deadline {
            assert!(self.read_more(200), "the pty closed before the app drew a frame");
        }
        // A redraw emits only the cells that changed, so keep accumulating for a
        // moment rather than stopping at the first frame that looks complete.
        let settle = std::time::Instant::now() + std::time::Duration::from_millis(400);
        while std::time::Instant::now() < settle && self.read_more(100) {}
        grid(&self.seen, cols, rows)
    }

    /// The screen once `wanted` is on it. Failing to find it is the failure, so
    /// this panics rather than handing back a screen for the caller to assert
    /// the same thing about twice.
    ///
    /// A settling window is a guess about how long a redraw takes, and under a
    /// full test run that guess is wrong: waiting for the thing being asserted
    /// is the same lesson as waiting for a frame rather than for a byte.
    fn screen_showing(&mut self, cols: usize, rows: usize, wanted: &str) -> Vec<String> {
        let deadline = std::time::Instant::now() + PATIENCE;
        loop {
            let screen = self.screen(cols, rows);
            if screen.iter().any(|line| line.contains(wanted)) {
                return screen;
            }
            assert!(
                self.child.try_wait().unwrap().is_none() && std::time::Instant::now() < deadline,
                "{wanted:?} never appeared. {}\n{}",
                self.diagnosis(),
                screen.join("\n")
            );
        }
    }

    /// Why the screen looks the way it does, for when it looks like nothing.
    /// A blank grid alone cannot be told from an app that exited, one that drew
    /// and was then cleared, or one that never started.
    fn diagnosis(&mut self) -> String {
        let alive = match self.child.try_wait() {
            Ok(None) => "still running".to_string(),
            Ok(Some(status)) => format!("exited with {status}"),
            Err(e) => format!("unknown: {e}"),
        };
        let tail: String = self.seen.chars().rev().take(2000).collect::<Vec<_>>().into_iter().rev().collect();
        format!("{} bytes read, child {alive}; terminal tail: {tail:?}", self.seen.len())
    }

    /// Whether the pty is still open, so a child that exited ends the wait
    /// instead of spinning it out to the deadline. Silence is not closure: a
    /// slow machine has not drawn *yet*.
    fn read_more(&mut self, timeout_ms: i32) -> bool {
        let mut chunk = [0u8; 8192];
        if !readable(&self.master, timeout_ms) {
            return true;
        }
        match self.master.read(&mut chunk) {
            Ok(0) | Err(_) => false,
            Ok(n) => {
                self.seen.push_str(&String::from_utf8_lossy(&chunk[..n]));
                true
            }
        }
    }

    fn send(&mut self, keys: &str) {
        use std::io::Write;
        self.master.write_all(keys.as_bytes()).unwrap();
        self.master.flush().unwrap();
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn readable(file: &std::fs::File, timeout_ms: i32) -> bool {
    use std::os::fd::AsRawFd;
    let mut fds = libc::pollfd { fd: file.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    unsafe { libc::poll(&mut fds, 1, timeout_ms) > 0 }
}

/// Replay the cursor-positioning escapes into a character grid.
///
/// Only the sequences ratatui actually emits for a full redraw are handled:
/// absolute positioning, erase-in-display, and printable text. Everything else
/// is skipped, which is why this reads the screen rather than emulating one.
fn grid(stream: &str, cols: usize, rows: usize) -> Vec<String> {
    let mut cells = vec![vec![' '; cols]; rows];
    let (mut row, mut col) = (0usize, 0usize);
    let mut chars = stream.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            match c {
                '\n' => {
                    row += 1;
                    col = 0;
                }
                '\r' => col = 0,
                c if !c.is_control() => {
                    if row < rows && col < cols {
                        cells[row][col] = c;
                    }
                    col += 1;
                }
                _ => {}
            }
            continue;
        }
        if chars.peek() != Some(&'[') {
            continue;
        }
        chars.next();
        let mut params = String::new();
        let mut final_byte = ' ';
        for c in chars.by_ref() {
            if c.is_ascii_alphabetic() || c == '@' {
                final_byte = c;
                break;
            }
            params.push(c);
        }
        match final_byte {
            'H' => {
                let mut parts = params.split(';');
                row = parts.next().and_then(|p| p.parse::<usize>().ok()).unwrap_or(1).saturating_sub(1);
                col = parts.next().and_then(|p| p.parse::<usize>().ok()).unwrap_or(1).saturating_sub(1);
            }
            // Erase-in-display, by mode. Not all of them clear the screen: `0`
            // — which is what an empty parameter means, and what crossterm
            // emits most — erases only from the cursor down. Treating every
            // `J` as `2J` threw away every frame accumulated before it, and
            // since the whole stream is replayed each time, one of them
            // anywhere left the screen blank however much had been drawn.
            'J' => {
                let (from, to) = match params.as_str() {
                    "" | "0" => ((row, col), (rows, 0)),
                    "1" => ((0, 0), (row, col + 1)),
                    _ => ((0, 0), (rows, 0)),
                };
                for (r, line) in cells.iter_mut().enumerate().take(to.0.min(rows)).skip(from.0) {
                    let start = if r == from.0 { from.1 } else { 0 };
                    let end = if r == to.0 { to.1.min(cols) } else { cols };
                    for cell in line.iter_mut().take(end).skip(start) {
                        *cell = ' ';
                    }
                }
            }
            _ => {}
        }
    }
    cells.into_iter().map(|r| r.into_iter().collect::<String>().trim_end().to_string()).collect()
}

/// One at a time.
///
/// These look independent — each has its own home and workspace — but each also
/// starts a whole `rook` from cold: opening a store, discovering skills and
/// plugins, and building a provider, all before the first byte reaches the
/// terminal. Nine of those at once on the FreeBSD runner, which is a VM, starved
/// one past a minute of having drawn nothing at all, twice, on two different
/// tests. Serially each gets the machine and finishes in seconds.
///
/// The guard is taken for the whole test, so it is released when the `Pty` that
/// holds it is dropped — which is also when the child is killed.
fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    GATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// A window that takes the store for itself, which is what most of these are
/// about. The default shares one through `rookd` — the tests for that start
/// their own, so nothing here leaves a daemon behind a temporary directory.
fn tui(home: &std::path::Path, workspace: &std::path::Path) -> Pty {
    Pty::spawn(
        std::path::Path::new(env!("CARGO_BIN_EXE_rook")),
        &["--workspace", workspace.to_str().unwrap(), "tui", "--alone"],
        &[("ROOK_HOME", home.to_str().unwrap()), ("ROOK_LOG", "error"), ("TERM", "xterm-256color")],
        100,
        30,
    )
}

#[test]
fn the_tui_starts_on_the_conversation() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut pty = tui(home.path(), workspace.path());

    let screen = pty.screen(100, 30);
    let all = screen.join("\n");

    // The window is the conversation, and what it says first is what to type.
    assert!(all.contains("Ask it something"), "the conversation must be drawn:\n{all}");
    assert!(all.contains("^p commands"), "and the one key that opens everything else:\n{all}");
    // The chat's own keys, and not the browsing ones: `j`, `k`, `r` and `q` are
    // characters in the message box here, and a footer promising them had
    // somebody typing `jjkkk` into their next prompt trying to scroll back.
    assert!(!all.contains("j/k"), "and not the ones that type letters:\n{all}");
    assert!(
        screen.iter().filter(|line| !line.is_empty()).count() > 3,
        "a nearly blank screen means it drew into a zero-sized terminal:\n{all}"
    );

    // Where those keys do work is inside a pane, and a pane is a `^p` away.
    pty.send("\u{10}");
    pty.screen_showing(100, 30, "what would you like to do");
    pty.send("sessions\r");
    let browsing = pty.screen_showing(100, 30, "j/k").join("\n");
    assert!(browsing.contains("continue"), "with the keys that pane actually has:\n{browsing}");
}

#[test]
fn a_pane_opens_over_the_conversation_and_closes_again() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut pty = tui(home.path(), workspace.path());
    pty.screen(100, 30);

    pty.send("\u{10}");
    pty.send("skills\r");
    let screen = pty.screen_showing(100, 30, "skills").join("\n");

    assert!(screen.contains("skills"), "the pane must render on an empty store:\n{screen}");
    // Over the conversation rather than instead of it, which is the whole
    // difference from the tabs this replaced: what you were doing is still
    // there, at the edges, and Esc puts you back in it.
    assert!(screen.contains("Ask it something"), "the conversation stays visible underneath:\n{screen}");

    pty.send("\u{1b}");
    let back = pty.screen_showing(100, 30, "^p commands").join("\n");
    assert!(!back.contains("no skills here yet"), "and Esc closes it:\n{back}");
}

#[test]
fn the_footer_shows_the_settings_and_f2_changes_them() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut pty = tui(home.path(), workspace.path());

    let before = pty.screen_showing(100, 30, "assist/high").join("\n");
    assert!(before.contains("assist/high"), "the configured defaults, in the footer:\n{before}");

    // F2 as a VT sequence; crossterm reads both this and SS3, and a pty is not
    // a terminal that will translate one for us.
    pty.send("\u{1b}[12~");
    pty.screen_showing(100, 30, "autonomous/high");
}

#[test]
fn the_memory_tab_shows_what_the_agent_remembers() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();

    let added = std::process::Command::new(env!("CARGO_BIN_EXE_rook"))
        .env("ROOK_HOME", home.path())
        .args(["--workspace", workspace.path().to_str().unwrap()])
        .args(["memory", "add", "prefer tabs in Makefiles", "--tag", "style"])
        .status()
        .unwrap();
    assert!(added.success());

    let mut pty = tui(home.path(), workspace.path());
    pty.screen(100, 30);
    pty.send("\u{10}");
    pty.send("memory\r");
    let screen = pty.screen_showing(100, 30, "memory (").join("\n");

    assert!(screen.contains("prefer tabs in Makefiles"), "and the fact:\n{screen}");
    assert!(screen.contains("style"), "with its tags:\n{screen}");
}

/// Reading memory without being able to correct it sent people to another
/// window for `rook memory add`, which is the thing the tab is for.
#[test]
fn the_memory_tab_adds_and_forgets_what_it_lists() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut pty = tui(home.path(), workspace.path());
    pty.screen(100, 30);
    pty.send("\u{10}");
    pty.send("memory\r");
    pty.screen_showing(100, 30, "memory (");

    pty.send("a");
    pty.screen_showing(100, 30, "enter saves");
    pty.send("the deploy key lives in 1password\r");
    let added = pty.screen_showing(100, 30, "remembered as").join("\n");
    assert!(added.contains("the deploy key lives in 1password"), "and it is listed:\n{added}");

    pty.send("d");
    // The fact is named in the pane's title while it can still be put back, so
    // what says it is gone is the count and the empty list, not its absence.
    let gone = pty.screen_showing(100, 30, "puts it back").join("\n");
    assert!(gone.contains("memory (0)"), "forgetting removes the row it was on:\n{gone}");
    assert!(gone.contains("nothing remembered yet"), "and the pane says so:\n{gone}");

    // Undo, because the row `d` lands on is the one a wrong keystroke costs.
    pty.send("u");
    let back = pty.screen_showing(100, 30, "is back").join("\n");
    assert!(back.contains("the deploy key lives in 1password"), "and it is listed again:\n{back}");
}

#[test]
fn the_tui_chat_answers_the_same_slash_commands_as_the_plain_cli() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut pty = tui(home.path(), workspace.path());
    pty.screen(100, 30);

    pty.send("/goal ship the release\r");
    pty.send("/goal\r");
    let screen = pty.screen_showing(100, 30, "goal set").join("\n");

    assert!(screen.contains("ship the release"), "and read back:\n{screen}");
}

#[test]
fn an_unknown_slash_command_in_the_tui_says_so() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut pty = tui(home.path(), workspace.path());
    pty.screen(100, 30);

    pty.send("/nonsense\r");
    let screen = pty.screen_showing(100, 30, "unknown command").join("\n");

    assert!(!screen.contains("cannot reach"), "it must not have gone to the provider:\n{screen}");
}

/// Nothing is spent before a turn runs, and the footer has to say the settings
/// without a stray separator where the total will go.
#[test]
fn the_footer_shows_no_running_total_before_there_is_one() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut pty = tui(home.path(), workspace.path());

    let screen = pty.screen_showing(100, 30, "assist/high").join("\n");
    assert!(screen.contains("assist/high"), "{screen}");
    assert!(!screen.contains(" in / "), "a total nobody has spent yet:\n{screen}");
}

/// Accepts and then says nothing, so the turn that reaches it stays running for
/// as long as the test needs it to.
fn a_model_that_never_answers(home: &std::path::Path) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        while let Ok((socket, _)) = listener.accept() {
            held.push(socket);
        }
    });
    std::fs::write(
        home.join("config.toml"),
        "[agent]\nmodel = \"openai-compatible/never\"\n\n[sandbox]\nmode = \"auto\"\n",
    )
    .unwrap();
    unsafe { std::env::set_var("ROOK_LLM_BASE_URL", format!("http://{addr}/v1")) };
}

/// Answers every turn with one `run_command` call, streamed the way the loop
/// reads it, so a test can put a real approval on the screen. The turn stops
/// there: an approval blocks until somebody answers, which is the state being
/// looked at.
fn a_model_that_asks_to_run(
    home: &std::path::Path,
    command: &str,
) -> std::sync::mpsc::Receiver<serde_json::Value> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let arguments = serde_json::json!({ "command": command, "cwd": "." }).to_string();
    let (sent, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader, Write};
        while let Ok((mut socket, _)) = listener.accept() {
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut request = String::new();
            let mut length = 0usize;
            while reader.read_line(&mut request).unwrap_or(0) > 0 {
                let line = request.rsplit('\n').nth(1).unwrap_or("").trim().to_string();
                if let Some(said) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = said.trim().parse().unwrap_or(0);
                }
                if request.ends_with("\r\n\r\n") {
                    break;
                }
            }
            assert!(length <= 1024 * 1024, "mock request is unexpectedly large");
            let mut body = Vec::new();
            reader.take(length as u64).read_to_end(&mut body).unwrap();
            if request.contains("chat/completions") {
                let _ = sent.send(serde_json::from_slice(&body).unwrap());
            }

            let body = if request.contains("chat/completions") {
                let call = serde_json::json!({"choices": [{"index": 0, "delta": {"tool_calls": [{
                    "index": 0, "id": "call-1", "type": "function",
                    "function": { "name": "run_command", "arguments": arguments }
                }]}, "finish_reason": null}]});
                let end = serde_json::json!({"choices": [{"index": 0, "delta": {},
                    "finish_reason": "tool_calls"}]});
                format!("data: {call}\n\ndata: {end}\n\ndata: [DONE]\n\n")
            } else {
                r#"{"data":[]}"#.to_string()
            };
            let kind = match request.contains("chat/completions") {
                true => "text/event-stream",
                false => "application/json",
            };
            let _ = write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.flush();
        }
    });
    std::fs::write(
        home.join("config.toml"),
        // `ask` because the approval is the thing being looked at, and it is
        // the stance a person gets by default.
        "[agent]\nmodel = \"openai-compatible/asks\"\nnative_tools = true\n\n\
         [sandbox]\nmode = \"ask\"\n",
    )
    .unwrap();
    unsafe { std::env::set_var("ROOK_LLM_BASE_URL", format!("http://{addr}/v1")) };
    received
}

fn steering_during_a_tool_reaches_the_next_request(through_daemon: bool) {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let requests = a_model_that_asks_to_run(
        home.path(),
        "echo ready > steering-ready; while test ! -f steering-release; do sleep 0.1; done; echo done > steering-finished",
    );
    let config = home.path().join("config.toml");
    let text = std::fs::read_to_string(&config).unwrap().replace("mode = \"ask\"", "mode = \"auto\"");
    std::fs::write(config, text).unwrap();
    let _daemon = through_daemon.then(|| Daemon::start(home.path(), workspace.path()));
    let mut pty = tui(home.path(), workspace.path());
    // Even a failed assertion must release the shell before its workspace goes.
    struct ReleaseTool<'a>(&'a std::path::Path);
    impl Drop for ReleaseTool<'_> {
        fn drop(&mut self) {
            let _ = std::fs::write(self.0.join("steering-release"), "continue");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while self.0.join("steering-ready").exists()
                && !self.0.join("steering-finished").exists()
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
    let _release = ReleaseTool(workspace.path());
    pty.screen(100, 30);
    pty.send("start the waiting command\rEARLY_NOTE\r");
    let first = requests.recv_timeout(PATIENCE).unwrap();
    assert!(first["messages"].to_string().contains("start the waiting command"));

    // A file written by the tool proves it is running; no timing guess about
    // whether Enter arrived before or after the turn finished.
    let deadline = std::time::Instant::now() + PATIENCE;
    while !workspace.path().join("steering-ready").exists() {
        pty.screen(100, 30);
        assert!(std::time::Instant::now() < deadline, "the tool never started: {}", pty.diagnosis());
    }
    pty.send("/mcp\r");
    pty.screen_showing(100, 30, "MCP connections");
    assert!(requests.try_recv().is_err(), "MCP inspection must not start another model request");
    pty.send("\u{1b}");
    pty.screen_showing(100, 30, "^p commands");
    let diagnostic = home.path().join("during-turn.json");
    pty.send(&format!("/diagnostics {}\r", diagnostic.display()));
    pty.screen_showing(100, 30, "Diagnostics saved to");
    let exported = std::fs::read_to_string(&diagnostic).unwrap();
    let report: serde_json::Value = serde_json::from_str(&exported).unwrap();
    assert_eq!(report["execution"]["status"], "running");
    assert_eq!(report["execution"]["pending"], true);
    assert!(!exported.contains("start the waiting command"));
    pty.send("QUOTE_DRAFT\u{6}");
    pty.screen_showing(100, 30, "history ·");
    pty.send("g0\r");
    pty.screen_showing(100, 30, "#0 · byte 0");
    pty.send("q");
    pty.screen_showing(100, 30, "Quote inserted in draft");
    assert!(requests.try_recv().is_err(), "quoting must not start another model request");
    pty.send("\r");
    pty.screen_showing(100, 30, "the turn will see this");
    pty.send("PREFER_BLUE\r");
    pty.screen_showing(100, 30, "the turn will see this");
    pty.send("/schema-retries 1\r");
    pty.screen_showing(100, 30, "done · the turn hears it");
    if through_daemon {
        // Receipt by rookd, rather than only the TUI's optimistic local echo.
        pty.screen_showing(100, 30, "↩ /schema-retries 1");
    }
    std::fs::write(workspace.path().join("steering-release"), "continue").unwrap();

    let next = requests.recv_timeout(PATIENCE).unwrap();
    let messages = next["messages"].as_array().unwrap();
    assert!(
        !next["messages"].to_string().contains("/diagnostics"),
        "local export is not a model instruction"
    );
    for text in ["EARLY_NOTE", "PREFER_BLUE", "/schema-retries 1", "QUOTE_DRAFT"] {
        assert!(
            messages.iter().any(|m| m["role"] == "user"
                && m["content"].as_str().is_some_and(|body| body.lines().any(|line| line == text))),
            "the next model request must contain the steering message {text:?}: {messages:?}"
        );
    }
    let quoted = messages
        .iter()
        .filter(|m| m["role"] == "user")
        .filter_map(|m| m["content"].as_str())
        .flat_map(str::lines)
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|value| value["rook_source"]["kind"] == "transcript_quote")
        .expect("the quote must remain a structured source record inside the steering message");
    assert_eq!(quoted["rook_source"]["authority"], "data");
    assert_eq!(quoted["rook_source"]["content"], "start the waiting command");
    assert_eq!(quoted["rook_source"]["seq"], 0);
    pty.send("\u{3}");
}

#[test]
fn a_running_daemon_turn_receives_text_and_slash_commands_from_the_tui() {
    steering_during_a_tool_reaches_the_next_request(true);
}

#[test]
fn a_running_local_turn_still_receives_text_and_slash_commands_from_the_tui() {
    steering_during_a_tool_reaches_the_next_request(false);
}

/// The panel was four rows whatever it held, so the command being approved —
/// its first line — was cut at the panel's width with nothing saying so, and
/// `y` approved a sentence nobody had read the end of.
#[test]
fn an_approval_shows_the_whole_command_it_is_asking_about() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    // Longer than the panel is wide, which is the whole point, and ending in
    // something unmistakable so a clipped screen cannot contain it by accident.
    let command = format!("echo {} the-very-end", "some-long-argument ".repeat(6));
    a_model_that_asks_to_run(home.path(), &command);
    let mut pty = tui(home.path(), workspace.path());

    pty.screen(100, 30);
    pty.send("run it\r");
    let shown = pty.screen_showing(100, 30, "the-very-end").join("\n");

    assert!(shown.contains("approval"), "the approval panel is up:\n{shown}");
    assert!(shown.contains("the-very-end"), "and the command is readable to its end:\n{shown}");
}

/// The chat REPL and the browser could both stop a turn; the TUI could only be
/// killed, taking the browsing state and any approval granted for the run.
/// A window showing only that something is happening cannot say how much room
/// is left, and a turn at step 190 of 200 is about to stop mid-task.
#[test]
fn a_running_turn_says_which_step_of_its_budget_it_is_on() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    a_model_that_never_answers(home.path());

    let mut pty = tui(home.path(), workspace.path());
    pty.screen(100, 30);
    pty.send("hello\r");

    let screen = pty.screen_showing(100, 30, "step 1/").join("\n");
    assert!(screen.contains("working…"), "beside how long it has been going: {screen}");
}

#[test]
fn ctrl_c_stops_a_running_turn_rather_than_the_whole_ui() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    a_model_that_never_answers(home.path());
    let mut pty = tui(home.path(), workspace.path());

    pty.screen(100, 30);
    pty.send("hello\r");
    let running = pty.screen(100, 30).join("\n");
    assert!(!running.contains("[stopped]"), "nothing has been stopped yet:\n{running}");

    pty.send("\u{3}");
    let after = pty.screen_showing(100, 30, "[stopped]").join("\n");
    assert!(after.contains("[stopped]"), "ctrl-c stops the turn:\n{after}");
    assert!(after.contains("^p commands"), "and the window is still up, so it did not quit:\n{after}");
}

/// The store holds every workspace, and the sessions tab named none of them:
/// another project's session read as one of this project's.
#[test]
fn the_sessions_tab_names_the_workspace_only_when_it_is_another_one() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    // Named rather than random, because the pane is a third of the screen and a
    // temporary directory's name does not fit in it.
    let root = tempfile::tempdir().unwrap();
    let here = root.path().join("mine");
    let other = root.path().join("theirs");
    std::fs::create_dir_all(&here).unwrap();
    std::fs::create_dir_all(&other).unwrap();

    for (workspace, title) in [(here.as_path(), "from here"), (other.as_path(), "from elsewhere")] {
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_rook"))
            .env("ROOK_HOME", home.path())
            .env("ROOK_LOG", "error")
            .args(["--workspace", workspace.to_str().unwrap(), "chat"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        use std::io::Write;
        child.stdin.take().unwrap().write_all(format!("{title}\n/quit\n").as_bytes()).unwrap();
        child.wait().unwrap();
    }

    let mut pty = tui(home.path(), &here);
    pty.screen(100, 30);
    pty.send("\u{10}");
    pty.screen_showing(100, 30, "what would you like to do");
    pty.send("sessions\r");
    let screen = pty.screen_showing(100, 30, "events · theirs").join("\n");

    assert!(screen.contains("events · theirs"), "a session from another project says which:\n{screen}");
    assert!(
        !screen.contains("events · mine"),
        "and this one's own is not repeated on every row — the title bar already says it:\n{screen}"
    );
}

/// A second window is the ordinary case — another project, or the same one
/// beside a running daemon — and it was an error message, because opening the
/// store is the first thing the TUI did. Reading routes over the daemon's API
/// the way every other command's does; a turn is what still needs the lock,
/// and the chat tab says so rather than looking broken.
#[test]
fn a_second_window_opens_and_browses_while_the_daemon_holds_the_store() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();

    // Something to read: a session written before the daemon takes the store.
    let mut seed = std::process::Command::new(env!("CARGO_BIN_EXE_rook"))
        .args(["--workspace", workspace.path().to_str().unwrap(), "chat"])
        .env("ROOK_HOME", home.path())
        .env("ROOK_LOG", "error")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    use std::io::Write;
    seed.stdin.take().unwrap().write_all(b"/new held-open\n/quit\n").unwrap();
    seed.wait().unwrap();

    let daemon = Daemon::start(home.path(), workspace.path());

    let mut pty = tui(home.path(), workspace.path());
    pty.screen(100, 30);
    // Wait for the palette before typing into it, rather than for a settling
    // window: a window that reads through a daemon takes longer to come up,
    // and the keys went where the palette was not yet.
    pty.send("\u{10}");
    pty.screen_showing(100, 30, "what would you like to do");
    pty.send("sessions\r");
    let browsing = pty.screen_showing(100, 30, "held-open").join("\n");
    assert!(browsing.contains("held-open"), "the sessions pane reads over the daemon:\n{browsing}");

    // `q` rather than Esc: alone Esc is a key, and followed immediately by
    // text it is the start of an escape sequence, which is what a terminal has
    // to assume — so the pane stayed open and `/context` was typed into it,
    // where `\r` continued a session instead. The pane offers both.
    pty.send("q");
    pty.screen_showing(100, 30, "^p commands");
    pty.send("/context\r");
    let chat = pty.screen_showing(100, 30, "holds the store").join("\n");
    assert!(chat.contains("holds the store"), "a slash command says why it cannot run here:\n{chat}");
    drop(daemon);
}

/// The case somebody actually meets: two projects, two windows, and no daemon
/// started by hand. The first window starts one and works through it, so the
/// second finds it and works too — where before it died on the lock the first
/// had taken for itself.
#[test]
fn two_windows_open_with_no_daemon_started_by_hand() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();

    let shared = |workspace: &std::path::Path| {
        Pty::spawn(
            std::path::Path::new(env!("CARGO_BIN_EXE_rook")),
            &["--workspace", workspace.to_str().unwrap(), "tui"],
            &[
                ("ROOK_HOME", home.path().to_str().unwrap()),
                ("ROOK_LOG", "error"),
                ("TERM", "xterm-256color"),
            ],
            100,
            30,
        )
    };
    // The daemon it starts is the one the test has to clean up, whichever
    // window started it: a temporary directory with a live process in it is
    // not one that goes away.
    let _stop = Stopper(home.path().to_path_buf());

    let mut one = shared(first.path());
    let started = one.screen_showing(100, 30, "via http://").join("\n");
    assert!(started.contains("via http://"), "the first window says it is sharing:\n{started}");

    let mut two = shared(second.path());
    let beside = two.screen_showing(100, 30, "via http://").join("\n");
    assert!(
        beside.contains("Ask it something"),
        "and the second opens rather than dying on the lock:\n{beside}"
    );
    // By the directory's own name rather than the first twenty bytes of its
    // path: a temporary directory is `/tmp/.tmpAbCdEf` on Linux, which is
    // fifteen bytes long, and slicing it panicked on every runner but this
    // one.
    let name = second.path().file_name().unwrap().to_string_lossy().to_string();
    assert!(beside.contains(&name), "on its own project:\n{beside}");
}

/// Kills whatever `rookd` was started against a home, when a test is done with
/// it. Nothing else knows to: a window leaves it running on purpose.
struct Stopper(std::path::PathBuf);

impl Drop for Stopper {
    fn drop(&mut self) {
        let address = self.0.join("rookd.addr");
        for _ in 0..50 {
            if address.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        // By the port it published, because that is the only thing about it on
        // this machine rather than in its environment: the command line is
        // `rookd --port 0` whichever home it was given, so matching on that
        // would take a daemon this test never started.
        let Some(port) = std::fs::read_to_string(&address)
            .ok()
            .and_then(|base| base.trim().rsplit(':').next().map(str::to_string))
        else {
            return;
        };
        let _ = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("kill $(lsof -ti :{port} -sTCP:LISTEN) 2>/dev/null || true"))
            .status();
    }
}

/// And the turn itself: the store takes one writer, so this window cannot run
/// its own loop — the daemon holding it does, and its socket is the same
/// conversation from the other side. The approval that comes back is the proof
/// the whole path is joined: prompt out, engine there, question here.
#[test]
fn a_second_window_runs_its_turn_through_the_daemon() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let command = format!("echo {} the-very-end", "some-long-argument ".repeat(4));
    // Written before the daemon starts, so it is the config the daemon reads.
    a_model_that_asks_to_run(home.path(), &command);

    let daemon = Daemon::start(home.path(), workspace.path());
    let mut pty = tui(home.path(), workspace.path());

    pty.screen(100, 30);
    pty.send("run it\r");
    let asked = pty.screen_showing(100, 30, "the-very-end").join("\n");

    assert!(asked.contains("approval"), "the daemon's approval arrives here:\n{asked}");
    assert!(asked.contains("the-very-end"), "with the command whole:\n{asked}");
    // The connection reports its settings when it opens and again for each one
    // this window sets, and printing each put `stance: assist · effort: high`
    // three times above a turn that had not started.
    assert!(
        asked.matches("effort:").count() <= 1,
        "the settings handshake is not three lines of log:\n{asked}"
    );
    // Answered from this window, which is the other half of the path.
    pty.send("y");
    let ran = pty.screen_showing(100, 30, "run_command").join("\n");
    assert!(ran.contains("run_command"), "and the call goes through:\n{ran}");
    drop(daemon);
}

/// `rookd`, for the test above. Port 0 so two tests can never collide, and the
/// address file is what says it is up.
struct Daemon(std::process::Child);

impl Daemon {
    fn start(home: &std::path::Path, workspace: &std::path::Path) -> Self {
        let rookd = std::path::PathBuf::from(env!("CARGO_BIN_EXE_rook")).with_file_name(if cfg!(windows) {
            "rookd.exe"
        } else {
            "rookd"
        });
        if !rookd.exists() {
            let built = std::process::Command::new(env!("CARGO"))
                .args(["build", "-p", "rookd"])
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .status();
            assert!(built.is_ok_and(|s| s.success()), "could not build rookd");
        }
        let child = std::process::Command::new(&rookd)
            .env("ROOK_HOME", home)
            .env("ROOK_LOG", "error")
            .args(["--workspace", workspace.to_str().unwrap()])
            .args(["--port", "0"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let started = Self(child);
        let address = home.join("rookd.addr");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            if address.exists() {
                std::thread::sleep(std::time::Duration::from_millis(150));
                return started;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // Held in `started`, so the panic below takes the child with it.
        panic!("rookd never published its address");
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Taking a snapshot of the workspace and putting one back were `rook
/// checkpoint create` and `rook checkpoint restore <object> --to <dir>` in
/// another terminal, with the object id read off a third command.
#[test]
fn a_checkpoint_can_be_taken_and_put_back_from_the_window() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("notes.txt"), "as it was\n").unwrap();

    let mut pty = tui(home.path(), workspace.path());
    pty.screen(100, 30);
    pty.send("\u{10}");
    pty.send("checkpoints\r");
    let empty = pty.screen_showing(100, 30, "nothing snapshotted yet").join("\n");
    assert!(empty.contains("`c` takes one"), "and says how one is taken:\n{empty}");

    pty.send("cbefore the risky bit\r");
    let taken = pty.screen_showing(100, 30, "took \"before the risky bit\"").join("\n");
    assert!(taken.contains("file(s)"), "and what went into it:\n{taken}");

    // Changed behind the window, then put back — with the question asked
    // first, because this writes over the workspace.
    std::fs::write(workspace.path().join("notes.txt"), "after the risky bit\n").unwrap();
    pty.send("R");
    pty.screen_showing(100, 30, "y / n");
    pty.send("y");
    pty.screen_showing(100, 30, "restored");
    let back = std::fs::read_to_string(workspace.path().join("notes.txt")).unwrap();
    assert_eq!(back, "as it was\n", "the snapshot is what is on disk again");
}

/// Versioning a skill was two commands in another terminal with an object id
/// read off a third: `skills history`, then `skills capture`, then `skills
/// rollback <name> <object>`. The tab that lists the skills is where both
/// belong, and a rollback nobody can see the versions of is one nobody asks
/// for.
#[test]
fn a_skill_can_be_captured_and_rolled_back_where_it_is_listed() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let skills = home.path().join("skills/tidy-up");
    std::fs::create_dir_all(&skills).unwrap();
    std::fs::write(
        skills.join("SKILL.md"),
        "---\nname: tidy-up\ndescription: How to tidy up.\nversion: 1.0.0\n---\nsweep\n",
    )
    .unwrap();

    let mut pty = tui(home.path(), workspace.path());
    pty.screen(100, 30);
    pty.send("\u{10}");
    pty.send("skills\r");
    let listed = pty.screen_showing(100, 30, "versions").join("\n");
    assert!(listed.contains("none captured"), "an empty history says what to do:\n{listed}");

    pty.send("c");
    let captured = pty.screen_showing(100, 30, "captured tidy-up").join("\n");
    assert!(captured.contains("file(s)"), "and what it took:\n{captured}");
    assert!(captured.contains("versions (1)"), "which is then listed:\n{captured}");

    // Changed on disk behind the window, then put back from the capture.
    std::fs::write(
        skills.join("SKILL.md"),
        "---\nname: tidy-up\ndescription: How to tidy up.\nversion: 1.0.0\n---\nburn it down\n",
    )
    .unwrap();
    pty.send("u");
    pty.screen_showing(100, 30, "rolled tidy-up back");
    let body = std::fs::read_to_string(skills.join("SKILL.md")).unwrap();
    assert!(body.contains("sweep"), "the captured body is back: {body}");
}

/// The Sessions tab is where a session is found, and the id was the only way
/// to take it up: read it there, then type it into the chat.
#[test]
fn the_session_under_the_cursor_can_be_taken_up_where_it_is() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let _ = std::process::Command::new(env!("CARGO_BIN_EXE_rook"))
        .env("ROOK_HOME", home.path())
        .env("ROOK_LOG", "error")
        .args(["--workspace", workspace.path().to_str().unwrap()])
        .args(["run", "yesterday's question"])
        .output();

    let mut pty = tui(home.path(), workspace.path());
    pty.screen(100, 30);
    // The sessions pane, where the cursor starts on the newest.
    pty.send("\u{10}");
    pty.send("sessions\r");
    pty.screen_showing(100, 30, "continue");
    pty.send("\r");

    let screen = pty.screen_showing(100, 30, "continuing").join("\n");
    assert!(screen.contains("Ask it something") || screen.contains("›"), "back in the chat:\n{screen}");
    assert!(screen.contains("yesterday's question"), "with what was said in it:\n{screen}");
}

/// Continuing an older conversation was a choice you could only make before
/// starting — `rook chat --session last` — so a session found in the Sessions
/// tab could be read there and continued only by quitting and starting again
/// with its id. The browser has had a picker since it had a chat.
#[test]
fn a_window_can_continue_a_session_it_finds() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    // A run with no model to answer it still leaves the session behind, named
    // by what it was asked.
    let _ = std::process::Command::new(env!("CARGO_BIN_EXE_rook"))
        .env("ROOK_HOME", home.path())
        .env("ROOK_LOG", "error")
        .args(["--workspace", workspace.path().to_str().unwrap()])
        .args(["run", "the earlier conversation"])
        .output();

    let mut pty = tui(home.path(), workspace.path());
    pty.screen(100, 30);
    pty.send("/session last\r");

    let screen = pty.screen_showing(100, 30, "continuing").join("\n");
    assert!(screen.contains("earlier conversation"), "named by what it was about:\n{screen}");
}

/// The plain chat has kept its history in `~/.rook/history` since it had one;
/// the window did not, so closing it forgot everything typed in it — and the
/// two are one person on one machine.
#[test]
fn a_window_opens_knowing_what_was_typed_in_the_last_one() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    // As the plain chat leaves it: one prompt a line, oldest first.
    std::fs::write(home.path().join("history"), "look at the parser\nrun the tests\n").unwrap();

    let mut pty = tui(home.path(), workspace.path());
    pty.screen(100, 30);
    pty.send("\u{1b}[A"); // up, for the last thing typed — in another window

    let screen = pty.screen_showing(100, 30, "run the tests").join("\n");
    assert!(screen.contains("run the tests"), "the newest comes back first:\n{screen}");

    pty.send("\u{1b}[A");
    let older = pty.screen_showing(100, 30, "look at the parser").join("\n");
    assert!(older.contains("look at the parser"), "and the one before it:\n{older}");
}

/// Up set the box to the last prompt, so a half-written message went at the
/// first press of the key that everywhere else moves within one — and coming
/// back down cleared the box rather than giving it back.
#[test]
fn what_was_being_typed_survives_a_walk_through_the_history() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut pty = tui(home.path(), workspace.path());
    pty.screen(100, 30);

    // Something to walk back to.
    pty.send("/nosuch remembered\r");
    pty.screen_showing(100, 30, "▌ /nosuch remembered");

    // A message of two rows, half written.
    pty.send("\u{1b}[200~alpha line\r\nbeta line\u{1b}[201~");
    pty.screen_showing(100, 30, "beta line");

    // Up moves within it rather than leaving it.
    pty.send("\u{1b}[A");
    let inside = pty.screen_showing(100, 30, "alpha line").join("\n");
    assert!(inside.contains("beta line"), "both rows are still there:\n{inside}");

    // Up from the top row is the history, and Down comes back to what was
    // being written rather than to an empty box.
    pty.send("\u{1b}[A");
    let walked = pty.screen_showing(100, 30, "› /nosuch remembered").join("\n");
    assert!(!walked.contains("› alpha line"), "the history is what is offered now:\n{walked}");
    pty.send("\u{1b}[B");
    let back = pty.screen_showing(100, 30, "› alpha line").join("\n");
    assert!(back.contains("beta line"), "and it comes back whole:\n{back}");
}

/// Pasting a paragraph sent it one line at a time — the first line as a
/// prompt, the rest chasing it as prompts of their own — because a terminal
/// delivers a pasted newline as the Enter key. Bracketed paste tells the two
/// apart, and this sends what a terminal sends when it brackets one.
#[test]
fn a_pasted_paragraph_stays_in_the_box_as_one_message() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut pty = tui(home.path(), workspace.path());
    pty.screen(100, 30);

    // What a terminal writes for a paste, which is the whole reason this
    // works: the same bytes typed by hand are Enter, and Enter sends.
    pty.send("\u{1b}[200~first line\r\nsecond line\r\nthird line\u{1b}[201~");
    let held = pty.screen_showing(100, 30, "third line").join("\n");
    assert!(held.contains("› first line"), "the box takes the first line:\n{held}");
    assert!(held.contains("second line"), "and the rest of them:\n{held}");
    // Nothing was sent: a message that has been sent carries the mark of who
    // said it, and this is still in the box.
    assert!(!held.contains("▌ first line"), "and none of it went as a message:\n{held}");

    // Cleared, so what goes next is only what is pasted next.
    pty.send("\u{15}"); // ctrl-u, kill to the start
    // A command, so no turn runs: an unknown one is answered here rather than
    // by a model. Its second line is what proves the whole paste went at once.
    pty.send("\u{1b}[200~/nosuch alpha\r\nomega\u{1b}[201~\r");
    let sent = pty.screen_showing(100, 30, "▌ /nosuch alpha").join("\n");
    assert!(sent.contains("omega"), "the whole paste went as one message:\n{sent}");
}

/// A conversation shows a call as one line, which is right while a turn runs
/// and not enough afterwards: "it edited `service.toml`" does not say what it
/// wrote there. Reaching the bytes meant the sessions pane, a session to select
/// and a transcript to scroll.
#[test]
fn what_a_call_was_given_and_what_came_back_is_one_key_away() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();

    // Seeded and closed: redb takes one writer and the window opens the same
    // store as it starts.
    {
        // Built from parts rather than opened: `Rook::open` reads `ROOK_HOME`
        // from this process, and the store the window will use is under the
        // home the pty is given.
        let store = rook_store::Store::open(home.path().join("store")).unwrap();
        let (skills, _) = rook_skills::SkillIndex::discover(&[]);
        let rook = rook_core::Rook::from_parts(
            store,
            rook_core::Config::default(),
            rook_skills::Environment::bare("linux", "x86_64", "0.1.0"),
            skills,
            workspace.path().to_path_buf(),
        );
        let session = rook.start_session("calls").unwrap();
        rook.log(session, rook_store::EventKind::UserMessage, "prompt", "what is in it?").unwrap();
        rook.log(
            session,
            rook_store::EventKind::ToolCall,
            "read_file",
            &serde_json::json!({ "path": "service.toml" }).to_string(),
        )
        .unwrap();
        rook.log(session, rook_store::EventKind::ToolResult, "read_file", "port = 8080\n").unwrap();
    }

    let mut pty = tui(home.path(), workspace.path());
    pty.screen(100, 30);

    // A window opens on a new conversation, so the one that made the call is
    // taken up first — which is what a person asking "what did it do" has just
    // done anyway.
    pty.send("\u{10}");
    pty.screen_showing(100, 30, "what would you like to do");
    pty.send("sessions\r");
    pty.screen_showing(100, 30, "j/k");
    pty.send("\r");
    pty.screen_showing(100, 30, "read service.toml");

    pty.send("\u{f}"); // ctrl-o
    let opened = pty.screen_showing(100, 30, "what it was given").join("\n");
    assert!(opened.contains("service.toml"), "the call is named:\n{opened}");
    assert!(opened.contains("port = 8080"), "and what came back is there:\n{opened}");
}

/// Naming a file meant knowing where it was: the short way to ask about one
/// was to leave the window, find the path, and paste it back.
#[test]
fn a_file_is_named_by_a_few_letters_of_it() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(workspace.path().join("src")).unwrap();
    std::fs::write(workspace.path().join("src/service.rs"), "fn main() {}\n").unwrap();
    let mut pty = tui(home.path(), workspace.path());
    pty.screen(100, 30);

    pty.send("why does @serv");
    let offered = pty.screen_showing(100, 30, "src/service.rs").join("\n");
    assert!(offered.contains("tab completes"), "the pane says what to press:\n{offered}");

    pty.send("\t");
    let taken = pty.screen_showing(100, 30, "@src/service.rs").join("\n");
    assert!(taken.contains("why does @src/service.rs"), "and the path lands in the sentence:\n{taken}");
}

/// The box you type in was a `String` with `push` and `pop`: no cursor, no
/// history, so a typo in the middle of a long prompt cost every character
/// after it and running the last thing again meant retyping it.
#[test]
fn the_prompt_box_edits_and_remembers_like_a_terminal() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut pty = tui(home.path(), workspace.path());
    pty.screen(100, 30);

    // Typed wrong, then fixed in the middle rather than from the end.
    pty.send("chekc the port");
    pty.send("\u{1}"); // ctrl-a, to the start
    pty.send("\u{1b}[C\u{1b}[C\u{1b}[C"); // right three, to after `che`
    pty.send("\u{8}"); // ctrl-h, which is the backspace key on some terminals
    let typed = pty.screen_showing(100, 30, "chkc the port").join("\n");
    assert!(typed.contains("chkc the port"), "the cursor edits where it is:\n{typed}");

    // Not Esc: alone it is a key, and followed immediately by text it is the
    // start of an escape sequence — which is what a terminal has to assume.
    pty.send("\u{1}\u{b}"); // ctrl-a then ctrl-k, clearing the line
    // A command rather than a prompt, so no turn runs: what the box says while
    // one does is `working…`, and this is about what comes back into the box.
    pty.send("/session\r");
    // In the log it carries the mark of who said it; in the box it carries the
    // prompt. Two marks, one text — asserting the text alone would pass on
    // either half of that.
    let sent = pty.screen_showing(100, 30, "▌ /session").join("\n");
    assert!(sent.contains("▌ /session"), "what was sent goes into the log:\n{sent}");

    pty.send("\u{1b}[A"); // up, for the last thing sent
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut screen = pty.screen(100, 30);
    while !screen.iter().any(|l| l.contains("› /session")) {
        assert!(std::time::Instant::now() < deadline, "never came back:\n{}", screen.join("\n"));
        screen = pty.screen(100, 30);
    }
    assert!(
        screen.iter().any(|l| l.contains("▌ /session")),
        "and it is still where it was said:\n{}",
        screen.join("\n")
    );
}

/// Documentation the agent gathered was readable only by asking the agent
/// about it again. What a person wants off it is the addresses — the local copy
/// an answer was made of, and the page it came from — and both are on this pane.
#[test]
fn documentation_is_listed_with_both_addresses_and_can_be_dropped() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();

    // Seeded and then closed: redb takes one writer, and the window opens the
    // same store the moment it starts.
    {
        let store = rook_store::Store::open(home.path().join("store")).unwrap();
        let (skills, _) = rook_skills::SkillIndex::discover(&[]);
        let rook = rook_core::Rook::from_parts(
            store,
            rook_core::Config::default(),
            rook_skills::Environment::bare("linux", "x86_64", "0.1.0"),
            skills,
            workspace.path().to_path_buf(),
        );
        rook.keep_docs(&rook_core::docs::DocSet::new(
            "redis",
            rook_core::docs::LATEST,
            vec![rook_core::docs::Page {
                url: "https://redis.io/docs/persistence".into(),
                title: "Persistence".into(),
                text: "An append only file, rewritten in the background as it grows.".into(),
                ..Default::default()
            }],
        ))
        .unwrap();
    }

    let mut pty = tui(home.path(), workspace.path());
    pty.screen(100, 30);
    pty.send("\u{10}");
    pty.send("docs\r");
    let listed = pty.screen_showing(100, 30, "redis").join("\n");
    assert!(listed.contains("latest"), "the version is part of what is kept:\n{listed}");
    assert!(listed.contains("docs/redis/latest"), "the local copy, by its reference:\n{listed}");
    assert!(listed.contains("redis.io/docs/persistence"), "and the page it was read from:\n{listed}");

    pty.send("d");
    let dropped = pty.screen_showing(100, 30, "no documentation gathered yet").join("\n");
    assert!(dropped.contains("/docs <topic>"), "and says how to gather one:\n{dropped}");
}

/// Both halves of the bargain, and the window says which one it is holding.
///
/// Capture is how the wheel scrolls back through a turn, and while it is on the
/// terminal never sees the drag that selects a line — so what the agent wrote
/// could be read and not copied. Asserted on the escape sequences themselves,
/// because that is the contract with the terminal: the footer is how a person
/// finds the key, and `?1000` is what actually decides whether they can select.
#[test]
fn the_mouse_can_be_handed_to_the_terminal_so_a_line_can_be_selected() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut pty = tui(home.path(), workspace.path());

    // The precondition: it took the mouse on the way in, and offers to give it
    // back.
    let before = pty.screen_showing(100, 30, "^s").join("\n");
    assert!(before.contains("select"), "the footer offers what pressing it gives:\n{before}");
    assert!(pty.seen.contains("?1000h"), "and the terminal was actually asked for the mouse");

    pty.send("\u{13}");
    let handed = pty.screen_showing(100, 30, "wheel").join("\n");
    assert!(handed.contains("wheel"), "and now offers the other one back:\n{handed}");
    assert!(
        pty.seen.contains("?1000l"),
        "the mouse has to actually go back to the terminal, or nothing can be selected"
    );

    // And again, because a toggle that only goes one way is a setting.
    pty.send("\u{13}");
    let taken = pty.screen_showing(100, 30, "select").join("\n");
    assert!(taken.contains("select"), "back to holding it:\n{taken}");
}

/// Reopening a session shows what the session says happened to it.
///
/// The window drew a recalled conversation from `user`, `assistant` and
/// `tool-call` and dropped everything else — so the notes that say a turn ran
/// out of steps, that the goal check disagreed, or that the process running a
/// turn died before the turn did were invisible in the one place somebody would
/// go to ask. A session that had crashed read as a session that simply stopped.
/// The browser had been showing them all along.
#[test]
fn reopening_a_session_shows_the_notes_that_say_what_happened_to_it() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();

    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_rook"))
        .env("ROOK_HOME", home.path())
        .env("ROOK_LOG", "error")
        .args(["--workspace", workspace.path().to_str().unwrap(), "chat"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(b"audit the three projects\n/quit\n").unwrap();
    child.wait().unwrap();

    // A note, written the way anything else writes one.
    let noted = std::process::Command::new(env!("CARGO_BIN_EXE_rook"))
        .env("ROOK_HOME", home.path())
        .env("ROOK_LOG", "error")
        .args(["--workspace", workspace.path().to_str().unwrap()])
        .args(["session", "goal", "last", "finish the report"])
        .status()
        .unwrap();
    assert!(noted.success(), "the session has to carry a note or this proves nothing");

    let mut pty = tui(home.path(), workspace.path());
    pty.screen(100, 30);
    pty.send("\u{10}");
    pty.screen_showing(100, 30, "what would you like to do");
    pty.send("sessions\r");
    pty.screen_showing(100, 30, "events");
    pty.send("\r");
    let screen = pty.screen_showing(100, 30, "finish the report").join("\n");

    assert!(screen.contains("finish the report"), "the note is on the screen:\n{screen}");
}

#[test]
fn attachments_can_be_selected_and_cleared_from_the_tui() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let file = workspace.path().join("context.txt");
    std::fs::write(&file, "this is source context").unwrap();
    let mut pty = tui(home.path(), workspace.path());
    pty.screen(100, 30);
    pty.send(&format!("/attach-context {}\r", file.display()));
    pty.screen_showing(100, 30, "attachments for next turn: 1");
    pty.send("/attachments clear\r");
    pty.screen_showing(100, 30, "attachments for next turn: 0");
}

/// The key that writes a second line has to be one this terminal sends, and
/// twice now the named one was not: `Alt+⏎` on Windows, which the terminal
/// keeps for itself, and `⌥⏎` on macOS, which reaches nothing until iTerm2 is
/// told to send Option as Meta — off by default, and the reason somebody with
/// a fresh install could not write a second line.
///
/// `^J` is the line feed. In raw mode the terminal stops folding `\r` into
/// `\n`, so it is a key of its own on every terminal and needs nothing
/// configured. That is what this asserts: the byte goes in, the message does
/// not go out.
#[test]
fn ctrl_j_writes_a_second_line_instead_of_sending_the_first() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut pty = tui(home.path(), workspace.path());
    pty.screen(100, 30);

    // A command, so nothing needs a model: if it were sent, the window would
    // answer it and the mark of a said message would appear.
    pty.send("/nosuch alpha");
    pty.send("\n"); // ^J — the line feed itself, not the Enter key
    pty.send("omega");
    let held = pty.screen_showing(100, 30, "omega").join("\n");

    assert!(held.contains("alpha"), "the first line is still in the box:\n{held}");
    assert!(
        !held.contains("▌ /nosuch alpha"),
        "and ^J did not send it — a said message carries that mark:\n{held}"
    );

    // And Enter after it sends the pair, so the newline went in rather than
    // being swallowed: what comes back is one message with both lines.
    pty.send("\r");
    let sent = pty.screen_showing(100, 30, "▌ /nosuch alpha").join("\n");
    assert!(sent.contains("omega"), "both lines went as one message:\n{sent}");
}

/// The other half: where the terminal *can* tell Shift+Enter from Enter, it is
/// taken as a newline. A unix terminal sends the same byte for both until it is
/// asked to disambiguate, which is what the window asks for at startup through
/// the keyboard protocol iTerm2, Ghostty, Kitty, WezTerm and foot all speak.
/// This sends what such a terminal sends — Enter is key 13, and modifier 2 is
/// Shift — so the binding is proved without needing one of them to run the test.
#[test]
fn shift_enter_writes_a_second_line_where_the_terminal_can_say_it_was_shift() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut pty = tui(home.path(), workspace.path());
    pty.screen(100, 30);

    pty.send("/nosuch alpha");
    pty.send("\u{1b}[13;2u"); // Shift+Enter, as a disambiguating terminal writes it
    pty.send("omega");
    let held = pty.screen_showing(100, 30, "omega").join("\n");

    assert!(held.contains("alpha"), "the first line is still in the box:\n{held}");
    assert!(
        !held.contains("▌ /nosuch alpha"),
        "and Shift+⏎ did not send it — a said message carries that mark:\n{held}"
    );

    pty.send("\r");
    let sent = pty.screen_showing(100, 30, "▌ /nosuch alpha").join("\n");
    assert!(sent.contains("omega"), "both lines went as one message:\n{sent}");
}

#[test]
fn scheduled_tasks_can_be_created_disabled_and_deleted_without_commands_or_ids() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[work]\nmax_parallel_runs = 0\n").unwrap();
    let _daemon = Daemon::start(home.path(), workspace.path());
    let mut pty = tui(home.path(), workspace.path());
    pty.screen(100, 30);
    pty.send("\u{10}");
    pty.screen_showing(100, 30, "what would you like to do");
    pty.send("tasks\r");
    pty.screen_showing(100, 30, "No tasks yet");
    pty.send("n");
    pty.screen_showing(100, 30, "Goal (1/8)");
    pty.send("Inspect this project");
    for _ in 0..3 {
        pty.screen(100, 30);
    }
    pty.screen_showing(100, 30, "Inspect this project");
    pty.send("\r");
    pty.screen_showing(100, 30, "Schedule (2/8)");
    pty.send("\u{15}weekly fri 09:00\r");
    pty.screen_showing(100, 30, "Timezone (3/8)");
    pty.send("\u{15}Europe/Moscow\r");
    pty.screen_showing(100, 30, "Workspace (4/8)");
    pty.send("\r");
    pty.screen_showing(100, 30, "Permissions (5/8)");
    pty.send("\r");
    pty.screen_showing(100, 30, "Seconds per run (6/8)");
    pty.send("\r");
    pty.screen_showing(100, 30, "Tokens per run (7/8)");
    pty.send("\r");
    pty.screen_showing(100, 30, "Iterations per run (8/8)");
    pty.send("\r");
    pty.screen_showing(100, 30, "Schedule saved");
    pty.screen_showing(100, 30, "Europe/Moscow");
    pty.send("p");
    pty.screen_showing(100, 30, "Disabled; existing session continues");
    pty.send("e");
    pty.screen_showing(100, 30, "Enabled");
    pty.send("r");
    pty.screen_showing(100, 30, "Session queued");
    pty.send("\r");
    pty.screen_showing(100, 30, "Waiting for a worker");
    pty.send("x");
    pty.screen_showing(100, 30, "Cancel the latest session?");
    pty.send("y");
    pty.screen_showing(100, 30, "Latest session cancelled");
    pty.send("d");
    pty.screen_showing(100, 30, "No tasks yet");
    pty.send("q");
    pty.screen_showing(100, 30, "F4");
    pty.send("\u{1b}OS");
    pty.screen_showing(100, 30, "No tasks yet");
}

#[test]
fn goal_runs_in_the_current_session_and_can_be_left_corrected_and_resumed() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let (session, idle) = {
        let store = rook_store::Store::open(home.path().join("store")).unwrap();
        let rook = rook_core::Rook::from_parts(
            store,
            rook_core::Config::default(),
            rook_skills::Environment::bare("linux", "x86_64", "0.1.0"),
            rook_skills::SkillIndex::discover(&[]).0,
            workspace.path().into(),
        );
        let idle = rook.start_session("idle conversation").unwrap();
        let session = rook.start_session("existing goal conversation").unwrap();
        rook.log(session, rook_store::EventKind::UserMessage, "", "EARLIER_REQUIREMENT: preserve the API")
            .unwrap();
        (rook_store::format_session_id(session), rook_store::format_session_id(idle))
    };
    let requests = a_model_that_asks_to_run(
        home.path(),
        "echo ready > goal-ready; while test ! -f goal-release; do sleep 0.1; done; if test -f goal-finished; then while test ! -f goal-resume-release; do sleep 0.1; done; fi; echo done > goal-finished",
    );
    let config = home.path().join("config.toml");
    let text = std::fs::read_to_string(&config).unwrap().replace("mode = \"ask\"", "mode = \"auto\"");
    std::fs::write(config, text).unwrap();
    let _daemon = Daemon::start(home.path(), workspace.path());
    struct Release<'a>(&'a std::path::Path);
    impl Drop for Release<'_> {
        fn drop(&mut self) {
            let _ = std::fs::write(self.0.join("goal-release"), "continue");
            let _ = std::fs::write(self.0.join("goal-resume-release"), "continue");
        }
    }
    let _release = Release(workspace.path());
    let base = std::fs::read_to_string(home.path().join("rookd.addr")).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    rook_llm::init_tls();
    let client = reqwest::Client::builder().no_proxy().timeout(PATIENCE).build().unwrap();
    let goal = || {
        runtime.block_on(async {
            client
                .get(format!("{}/api/work/{session}", base.trim()))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json::<serde_json::Value>()
                .await
                .unwrap()
        })
    };
    let mut pty = tui(home.path(), workspace.path());
    pty.screen(100, 30);
    pty.send(&format!("/session {session}\r"));
    pty.screen_showing(100, 30, "nothing is running in this session");
    pty.send("/goal Finish the requested work\r");
    pty.screen_showing(100, 30, "Goal started in this session");
    let first = requests.recv_timeout(PATIENCE).unwrap();
    assert!(first["messages"].to_string().contains("EARLIER_REQUIREMENT"));
    let deadline = std::time::Instant::now() + PATIENCE;
    while !workspace.path().join("goal-ready").exists() {
        pty.screen(100, 30);
        assert!(std::time::Instant::now() < deadline, "tool never started: {}", pty.diagnosis());
    }
    let run = goal();
    assert_eq!(run["id"], session);
    assert_eq!(run["session"], session);
    for limit in ["max_iterations", "max_seconds", "max_tokens"] {
        assert_eq!(run[limit], 0);
    }
    pty.send("\u{10}");
    pty.screen_showing(100, 30, "what would you like to do");
    pty.send("sessions\r");
    pty.screen_showing(100, 30, "j/k");
    pty.send("j\r");
    pty.screen_showing(100, 30, "nothing is running in this session");
    assert_eq!(goal()["status"], "running", "the session picker leaves the goal running");
    pty.send(&format!("/session {session}\r"));
    pty.screen_showing(100, 30, "joined a turn already running here");
    pty.send("/new\r");
    pty.screen_showing(100, 30, "Other sessions keep working");
    pty.send(&format!("/session {idle}\r"));
    pty.screen_showing(100, 30, "nothing is running in this session");
    assert_eq!(goal()["status"], "running", "switching must not cancel the goal");
    pty.send(&format!("/session {session}\r"));
    pty.screen_showing(100, 30, "joined a turn already running here");
    pty.send("PREFER_BLUE\r");
    pty.screen_showing(100, 30, "↩ PREFER_BLUE");
    assert!(goal()["instructions"][0]["applied_at"].is_null());
    pty.send("\u{3}");
    pty.screen_showing(100, 30, "Pausing goal");
    assert_eq!(goal()["status"], "paused");
    drop(pty);
    std::fs::write(workspace.path().join("goal-release"), "continue").unwrap();
    let deadline = std::time::Instant::now() + PATIENCE;
    loop {
        let health = runtime.block_on(async {
            client
                .get(format!("{}/api/health", base.trim()))
                .send()
                .await
                .unwrap()
                .json::<serde_json::Value>()
                .await
                .unwrap()
        });
        if health["turns_running"] == 0 {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "pause did not finish: {health}");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let mut pty = tui(home.path(), workspace.path());
    pty.screen(100, 30);
    pty.send(&format!("/session {session}\r"));
    pty.screen_showing(100, 30, "nothing is running in this session");
    pty.send("/continue\r");
    pty.screen_showing(100, 30, "taken up");
    let next = requests.recv_timeout(PATIENCE).unwrap();
    assert!(next["messages"].to_string().contains("PREFER_BLUE"));
    assert!(goal()["instructions"][0]["applied_at"].is_u64());
    runtime.block_on(async {
        client
            .post(format!("{}/api/work/{session}/control", base.trim()))
            .json(&"cancel")
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    });
}

#[test]
fn external_editor_round_trips_the_draft_and_remembers_the_choice() {
    use std::os::unix::fs::PermissionsExt;
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let editor = home.path().join("console editor");
    std::fs::write(
        &editor,
        r##"#!/bin/sh
set -eu
[ "$1" = '--literal;$HOME' ]
[ -t 0 ] && [ -t 1 ] && [ -t 2 ]
case "$(/bin/stty -a)" in *-icanon*) exit 31;; esac
printf '%s' "$2" > "$ROOK_HOME/edited-path"
/bin/cat "$2" > "$ROOK_HOME/before"
printf '\nEDITOR_READY\n'
read -r action
[ "$action" = save ]
/bin/cat "$ROOK_HOME/replacement" > "$2"
if [ -f "$ROOK_HOME/fail" ]; then exit 7; fi
"##,
    )
    .unwrap();
    std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o700)).unwrap();
    let command = format!("'{}' '--literal;$HOME'", editor.display());
    let spawn = |command: &str| {
        Pty::spawn(
            std::path::Path::new(env!("CARGO_BIN_EXE_rook")),
            &["--workspace", workspace.path().to_str().unwrap(), "tui", "--alone"],
            &[
                ("ROOK_HOME", home.path().to_str().unwrap()),
                ("ROOK_LOG", "error"),
                ("TERM", "xterm-256color"),
                ("VISUAL", ""),
                ("EDITOR", command),
            ],
            100,
            30,
        )
    };
    let mut pty = spawn(&command);
    pty.screen(100, 30);
    let original = "original\nПривет";
    pty.send(&format!("\x1b[200~{original}\x1b[201~"));
    pty.screen_showing(100, 30, "Привет");
    pty.send("\x05");
    pty.screen_showing(100, 30, "external editor");
    pty.send("\x1b");
    pty.screen_showing(100, 30, "Привет");
    assert!(!home.path().join("before").exists(), "Esc must not start an editor");
    let replacement = "updated draft\nЮникод\n";
    std::fs::write(home.path().join("replacement"), replacement).unwrap();
    pty.send("\x05");
    let choices = pty.screen_showing(100, 30, "console editor").join("\n");
    assert!(choices.contains("› console editor"), "environment editor should be selected:\n{choices}");
    pty.send("\r");
    pty.screen_showing(100, 30, "EDITOR_READY");
    assert_eq!(std::fs::read_to_string(home.path().join("before")).unwrap(), original);
    pty.send("save\n");
    let screen = pty.screen_showing(100, 30, "updated draft").join("\n");
    assert!(screen.contains("Юникод"), "multiline Unicode must survive:\n{screen}");
    assert!(!screen.contains("▌ updated draft"), "the editor must not submit the draft:\n{screen}");
    pty.send("\x1a");
    pty.screen_showing(100, 30, "Привет");
    pty.send("\x1bz");
    pty.screen_showing(100, 30, "updated draft");
    let edited_path = std::fs::read_to_string(home.path().join("edited-path")).unwrap();
    assert!(!std::path::Path::new(&edited_path).exists(), "successful drafts are temporary");
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.path().join("tui-editor.json")).unwrap()).unwrap();
    assert_eq!(saved["program"], editor.to_str().unwrap());
    drop(pty);

    // A different environment preference cannot displace the last used one.
    let mut pty = spawn("vi");
    pty.screen(100, 30);
    pty.send("surviving draft");
    pty.screen_showing(100, 30, "surviving draft");
    pty.send("\x05");
    let choices = pty.screen_showing(100, 30, "console editor").join("\n");
    assert!(choices.contains("› console editor"), "last used stays first after restart:\n{choices}");
    std::fs::write(home.path().join("fail"), "").unwrap();
    pty.send("\r");
    pty.screen_showing(100, 30, "EDITOR_READY");
    pty.send("save\n");
    let screen = pty.screen_showing(100, 30, "original draft kept").join("\n");
    assert!(screen.contains("surviving draft"), "failed editor must keep the input:\n{screen}");
    let recovery =
        std::path::PathBuf::from(std::fs::read_to_string(home.path().join("edited-path")).unwrap());
    assert_eq!(std::fs::read_to_string(&recovery).unwrap(), replacement);
    std::fs::remove_dir_all(recovery.parent().unwrap()).unwrap();
    // Further typing proves raw-mode input and the screen were restored.
    pty.send(" plus");
    pty.screen_showing(100, 30, "surviving draft plus");
}

#[test]
fn external_editor_picker_without_editors_can_be_cancelled() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut pty = Pty::spawn(
        std::path::Path::new(env!("CARGO_BIN_EXE_rook")),
        &["--workspace", workspace.path().to_str().unwrap(), "tui", "--alone"],
        &[
            ("ROOK_HOME", home.path().to_str().unwrap()),
            ("ROOK_LOG", "error"),
            ("TERM", "xterm-256color"),
            ("VISUAL", ""),
            ("EDITOR", ""),
            ("PATH", workspace.path().to_str().unwrap()),
        ],
        100,
        30,
    );
    pty.screen(100, 30);
    pty.send("keep this draft");
    pty.screen_showing(100, 30, "keep this draft");
    pty.send("\x05");
    pty.screen_showing(100, 30, "No console editor found");
    pty.send("\r");
    pty.screen_showing(100, 30, "No console editor found");
    pty.send("\x1b");
    pty.screen(100, 30);
    pty.send(" here");
    pty.screen_showing(100, 30, "keep this draft here");
}

#[test]
fn remapped_prompt_undo_and_palette_help_agree_without_submitting() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[tui.keys]\n'prompt.undo'=['ctrl+x']\n").unwrap();
    let mut pty = Pty::spawn(
        std::path::Path::new(env!("CARGO_BIN_EXE_rook")),
        &["--workspace", workspace.path().to_str().unwrap(), "tui", "--alone"],
        &[("ROOK_HOME", home.path().to_str().unwrap()), ("ROOK_LOG", "error"), ("TERM", "xterm-256color")],
        100,
        30,
    );
    pty.screen(100, 30);
    pty.send("\x1b[200~Привет\x1b[201~");
    pty.screen_showing(100, 30, "Привет");
    pty.send("\x15\x1asentinel");
    let screen = pty.screen_showing(100, 30, "sentinel").join("\n");
    assert!(!screen.contains("Привет"), "the former undo shortcut must be inactive: {screen}");
    pty.send("\x18\x18");
    pty.screen_showing(100, 30, "Привет");
    pty.send("\x1b[200~\nВторая строка\x1b[201~");
    pty.screen_showing(100, 30, "Вторая строка");
    pty.send("\x18\x10prompt.undo");
    let screen = pty.screen_showing(100, 30, "action: prompt.undo").join("\n");
    assert!(screen.contains("ctrl+x"), "the palette shows the active binding: {screen}");
    pty.send("\x1b");
    let screen = pty.screen_showing(100, 30, "Привет").join("\n");
    assert!(!screen.contains("Вторая строка"), "one undo removes the entire paste: {screen}");
    assert!(!screen.contains("▌ Привет"), "the draft was never submitted: {screen}");
}

fn config_editor(home: &std::path::Path) -> Pty {
    Pty::spawn(
        std::path::Path::new(env!("CARGO_BIN_EXE_rook")),
        &["config", "edit"],
        &[("ROOK_HOME", home.to_str().unwrap()), ("ROOK_LOG", "error"), ("TERM", "xterm-256color")],
        100,
        30,
    )
}

#[test]
fn config_editor_explains_edits_saves_resets_and_discards_without_the_store() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("config.toml");
    let original = "# personal notes\n[agent]\nmax_steps = 40 # limited on purpose\n";
    std::fs::write(&file, original).unwrap();
    let _locked = rook_store::Store::open(home.path().join("store")).unwrap();
    let mut pty = config_editor(home.path());
    pty.screen_showing(100, 30, "rook config edit");
    pty.send("\r");
    pty.screen_showing(100, 30, "agent  [saved]");
    pty.send("/max_steps\r");
    pty.screen_showing(100, 30, "Maximum tool/model steps");
    pty.send("\r");
    pty.screen_showing(100, 30, "Edit value");
    pty.send("\x15banana\r");
    pty.screen_showing(100, 30, "enter a whole number");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
    pty.send("\x1563\r");
    pty.screen_showing(100, 30, "updated in draft");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), original, "Enter only updates the draft");
    pty.send("\x13");
    pty.screen_showing(100, 30, "Saved.");
    assert_eq!(rook_core::Config::load_from(file.clone()).unwrap().agent.max_steps, 63);
    assert!(std::fs::read_to_string(&file).unwrap().contains("# limited on purpose"));
    pty.send("d");
    pty.screen_showing(100, 30, "Remove from configuration?");
    pty.send("y");
    pty.screen_showing(100, 30, "Removed from draft");
    pty.send("\x13");
    pty.screen_showing(100, 30, "Saved.");
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(!saved.contains("max_steps"), "removing an override restores the default: {saved}");
    assert!(saved.contains("# personal notes"));
    pty.send("\r");
    pty.screen_showing(100, 30, "Edit value");
    pty.send("\x1588\r");
    pty.screen_showing(100, 30, "updated in draft");
    pty.send("q");
    pty.screen_showing(100, 30, "Unsaved changes");
    pty.send("d");
    assert!(pty.child.wait().unwrap().success());
    assert_eq!(std::fs::read_to_string(file).unwrap(), saved);
}

#[test]
fn config_editor_adds_and_removes_an_mcp_server_without_starting_it() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("config.toml");
    let mut pty = config_editor(home.path());
    pty.screen_showing(100, 30, "rook config edit");
    assert!(!file.exists(), "opening the editor does not create a config");
    pty.send("/mcp\r\r");
    pty.screen_showing(100, 30, "No entries.");
    pty.send("a");
    pty.screen_showing(100, 30, "Add entry");
    pty.send("docs\r");
    pty.screen_showing(100, 30, "Entry added to draft");
    pty.send("\x13");
    pty.screen_showing(100, 30, "set command or url");
    assert!(!file.exists(), "incomplete server must not be saved");
    pty.send("/command\r\r");
    pty.screen_showing(100, 30, "Edit value");
    pty.send("not-an-installed-command\r");
    pty.screen_showing(100, 30, "updated in draft");
    pty.send("\x13");
    pty.screen_showing(100, 30, "Saved.");
    let config = rook_core::Config::load_from(file.clone()).unwrap();
    assert_eq!(config.mcp.len(), 1);
    assert_eq!(config.mcp[0].name, "docs");
    assert_eq!(config.mcp[0].command, "not-an-installed-command");
    pty.send("\x1b");
    pty.screen(100, 30); // clear search
    pty.send("\x1b");
    pty.screen_showing(100, 30, "mcp  [saved]");
    pty.send("d");
    pty.screen_showing(100, 30, "Remove from configuration?");
    pty.send("y");
    pty.screen_showing(100, 30, "Removed from draft");
    pty.send("\x13");
    pty.screen_showing(100, 30, "Saved.");
    assert!(rook_core::Config::load_from(file).unwrap().mcp.is_empty());
}

#[test]
fn config_editor_keeps_credentials_out_of_the_screen_including_the_edit_field() {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "[endpoints.private]\napi='openai'\nkey='DO_NOT_SHOW_THIS_KEY'\n",
    )
    .unwrap();
    let mut pty = config_editor(home.path());
    pty.screen_showing(100, 30, "rook config edit");
    pty.send("/endpoints\r\r");
    pty.screen_showing(100, 30, "endpoints  [saved]");
    pty.send("\r");
    pty.screen_showing(100, 30, "private  [saved]");
    pty.send("/key\r");
    pty.screen_showing(100, 30, "<hidden>");
    pty.send("\r");
    pty.screen_showing(100, 30, "Edit value");
    assert!(!pty.seen.contains("DO_NOT_SHOW_THIS_KEY"), "secrets must not appear in terminal output");
}

fn history_browsing_preserves_the_session(through_daemon: bool) {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[transcript]\npage_entries=3\nsearch_events=256\n")
        .unwrap();
    let session = rook_store::new_session_id();
    {
        let store = rook_store::Store::open(home.path().join("store")).unwrap();
        store
            .create_session(&rook_store::SessionMeta::new(
                session,
                "history fixture",
                workspace.path().display().to_string(),
                rook_store::now_unix(),
            ))
            .unwrap();
        for n in 0..80 {
            store
                .append_event(
                    session,
                    rook_store::NewEvent::new(
                        rook_store::EventKind::UserMessage,
                        rook_store::Kind::Message,
                        format!("HISTORY_EVENT_{n}").as_bytes(),
                    ),
                )
                .unwrap();
        }
    }
    let daemon = through_daemon.then(|| Daemon::start(home.path(), workspace.path()));
    let mut pty = tui(home.path(), workspace.path());
    pty.screen(100, 30);
    pty.send(&format!("/session {}\r", rook_store::format_session_id(session)));
    pty.screen_showing(100, 30, "continuing");
    pty.send("PRESERVED_DRAFT\u{6}");
    pty.screen_showing(100, 30, "HISTORY_EVENT_79");
    pty.send("p");
    pty.screen_showing(100, 30, "HISTORY_EVENT_76");
    pty.send("/HISTORY_EVENT_12\r");
    pty.screen_showing(100, 30, "Search complete.");
    pty.screen_showing(100, 30, "HISTORY_EVENT_12");
    pty.send("g0\r");
    pty.screen_showing(100, 30, "#0 · byte 0");
    pty.screen_showing(100, 30, "HISTORY_EVENT_0");
    pty.send("\u{1b}");
    pty.screen_showing(100, 30, "PRESERVED_DRAFT");
    drop(pty);
    drop(daemon);
    let store = rook_store::Store::open(home.path().join("store")).unwrap();
    assert_eq!(store.get_session(session).unwrap().unwrap().next_seq, 80);
}

#[test]
fn local_history_search_jump_and_pages_preserve_the_draft() {
    history_browsing_preserves_the_session(false);
}
#[test]
fn daemon_history_search_jump_and_pages_preserve_the_draft() {
    history_browsing_preserves_the_session(true);
}

fn mcp_panel_reconnects_from_current_config(through_daemon: bool) {
    let _one = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let file = home.path().join("config.toml");
    std::fs::write(&file, "[[mcp]]\nname='docs'\nenabled=false\n").unwrap();
    let daemon = through_daemon.then(|| Daemon::start(home.path(), workspace.path()));
    let mut pty = tui(home.path(), workspace.path());
    pty.screen(100, 30);
    pty.send("/mcp\r");
    pty.screen_showing(100, 30, "docs — disabled");
    let binary = serde_json::to_string(env!("CARGO_BIN_EXE_rook")).unwrap();
    let args = serde_json::json!(["--workspace", workspace.path(), "mcp", "serve", "--yes"]);
    std::fs::write(&file, format!("[[mcp]]\nname='docs'\ncommand={binary}\nargs={args}\n")).unwrap();
    pty.send("r");
    pty.screen_showing(100, 30, "docs — connected");
    std::fs::write(&file, "[[mcp]]\nname='docs'\ncommand='/missing/mcp-command'\n").unwrap();
    pty.send("r");
    pty.screen_showing(100, 30, "could not start server");
    // Leaving the pane still works after a failed reconnect.
    pty.send("\u{1b}");
    pty.screen_showing(100, 30, "^p commands");
    pty.send("PRESERVED_MCP_DRAFT");
    pty.screen_showing(100, 30, "PRESERVED_MCP_DRAFT");
    drop(pty);
    drop(daemon);
}

#[test]
fn local_mcp_panel_reconnects_from_current_config() {
    mcp_panel_reconnects_from_current_config(false);
}
#[test]
fn daemon_mcp_panel_reconnects_from_current_config() {
    mcp_panel_reconnects_from_current_config(true);
}

struct OAuthFixture {
    runtime: tokio::runtime::Runtime,
    task: tokio::task::JoinHandle<()>,
    base: String,
    grants: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
impl Drop for OAuthFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl OAuthFixture {
    fn new() -> Self {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        rook_llm::init_tls();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let listener = runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let grants = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (address, count) = (base.clone(), grants.clone());
        let task = runtime.spawn(async move {
            let mut clients = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (mut socket, _) = accepted.unwrap();
                        let base = address.clone(); let grants = count.clone();
                        clients.spawn(async move {
                            let mut bytes = Vec::new(); let mut chunk = [0u8;4096];
                            let (end,length) = loop {
                                let n = socket.read(&mut chunk).await.unwrap(); if n == 0 { return; }
                                assert!(bytes.len()+n <= 65536); bytes.extend_from_slice(&chunk[..n]);
                                if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                                    let headers = String::from_utf8_lossy(&bytes[..end]);
                                    let length = headers.lines().find_map(|line| line.to_lowercase().strip_prefix("content-length:").map(|s|s.trim().parse::<usize>().unwrap())).unwrap_or(0);
                                    assert!(end+4+length <= 65536); break (end+4,length);
                                }
                            };
                            while bytes.len() < end+length {
                                let n = socket.read(&mut chunk).await.unwrap(); if n == 0 { return; }
                                assert!(bytes.len()+n <= 65536); bytes.extend_from_slice(&chunk[..n]);
                            }
                            let headers = String::from_utf8_lossy(&bytes[..end]);
                            let path = headers.lines().next().unwrap().split(' ').nth(1).unwrap();
                            let (status, extra, body) = match path {
                                "/resource" => (200,String::new(),serde_json::json!({"resource":format!("{base}/mcp"),"authorization_servers":[format!("{base}/issuer")]})),
                                "/.well-known/oauth-authorization-server/issuer" => (200,String::new(),serde_json::json!({"issuer":format!("{base}/issuer"),"authorization_endpoint":format!("{base}/authorize"),"token_endpoint":format!("{base}/token"),"code_challenge_methods_supported":["S256"],"token_endpoint_auth_methods_supported":["none"]})),
                                "/token" => {
                                    grants.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
                                    (200,String::new(),serde_json::json!({"access_token":"PRIVATE_PTY_ACCESS","token_type":"Bearer","expires_in":3600}))
                                }
                                "/mcp" if !headers.contains("Bearer PRIVATE_PTY_ACCESS") => (401,format!("WWW-Authenticate: Bearer resource_metadata=\"{base}/resource\"\r\n"),serde_json::json!({})),
                                "/mcp" => {
                                    let request: serde_json::Value = serde_json::from_slice(&bytes[end..end+length]).unwrap();
                                    let result = match request["method"].as_str().unwrap() {
                                        "initialize" => serde_json::json!({"protocolVersion":"2025-06-18","serverInfo":{"name":"pty","version":"1"},"capabilities":{"tools":{}}}),
                                        "tools/list" => serde_json::json!({"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}),
                                        "notifications/initialized" => serde_json::Value::Null,
                                        other => panic!("unexpected method {other}"),
                                    };
                                    (if request.get("id").is_some() {200} else {202},String::new(),serde_json::json!({"jsonrpc":"2.0","id":request["id"],"result":result}))
                                }
                                _ => (404,String::new(),serde_json::json!({})),
                            };
                            let body = body.to_string();
                            let response = format!("HTTP/1.1 {status} Reply\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}",body.len());
                            let _ = socket.write_all(response.as_bytes()).await;
                        });
                    }
                    Some(result) = clients.join_next() => { result.unwrap(); }
                }
            }
        });
        Self { runtime, task, base, grants }
    }
}

fn mcp_sign_in_keeps_input_live_and_can_cancel_before_completing(through_daemon: bool) {
    let _one = one_at_a_time();
    let fixture = OAuthFixture::new();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"),format!("[agent]\ninstall_servers=false\n[mcp_connections]\noauth_max_pending=1\n[[mcp]]\nname='private'\nurl='{}/mcp'\n[mcp.oauth]\nclient_id='pty-client'\n",fixture.base)).unwrap();
    let daemon = through_daemon.then(|| Daemon::start(home.path(), workspace.path()));
    let mut pty = tui(home.path(), workspace.path());
    pty.screen(100, 30);
    pty.send("/mcp login private\r");
    pty.screen_showing(100, 30, "o opens the sign-in URL");
    if through_daemon {
        fixture.runtime.block_on(async {
            let base = std::fs::read_to_string(home.path().join("rookd.addr")).unwrap();
            let base = base.trim();
            let client = reqwest::Client::builder().timeout(PATIENCE).build().unwrap();
            let response = client.get(format!("{base}/api/mcp/oauth")).send().await.unwrap();
            assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
            let attempts: serde_json::Value = response.json().await.unwrap();
            assert_eq!(attempts.as_array().unwrap().len(), 1, "configured pending limit is reached");
            let response = client
                .post(format!("{base}/api/mcp/private/login"))
                .json(&serde_json::json!({"redirect_uri":format!("{base}/mcp-oauth-callback.html")}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
            let error: serde_json::Value = response.json().await.unwrap();
            assert!(error["error"].as_str().unwrap().contains("too many"), "{error}");
        });
    }
    pty.send("\u{1b}");
    pty.screen_showing(100, 30, "^p commands");
    pty.send("PRESERVED_AUTH_DRAFT");
    pty.screen_showing(100, 30, "PRESERVED_AUTH_DRAFT");
    pty.send("\u{10}mcp\r");
    pty.screen_showing(100, 30, "o opens the sign-in URL");
    pty.send("c");
    pty.screen_showing(100, 30, "Sign-in cancelled.");
    assert!(!home.path().join("mcp-auth/credentials.json").exists());
    assert_eq!(fixture.grants.load(std::sync::atomic::Ordering::SeqCst), 0);
    // Returning through the palette populated status and selected the server.
    pty.send("l");
    let screen = pty.screen_showing(100, 30, "o opens the sign-in URL");
    let text = screen.iter().filter_map(|line| line.split('│').nth(2)).map(str::trim).collect::<String>();
    let at = text.find(&format!("{}/authorize?", fixture.base)).unwrap();
    let url = text[at..].split(|c: char| !c.is_ascii() || c.is_whitespace()).next().unwrap();
    let url = reqwest::Url::parse(url).unwrap();
    let fields: std::collections::BTreeMap<_, _> =
        url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    let redirect =
        fields.get("redirect_uri").unwrap_or_else(|| panic!("missing callback in {url}; screen {screen:?}"));
    let mut callback = reqwest::Url::parse(redirect)
        .unwrap_or_else(|e| panic!("bad callback {e}: {url}; screen {screen:?}"));
    callback
        .query_pairs_mut()
        .append_pair("state", &fields["state"])
        .append_pair("code", "test-code")
        .append_pair("iss", &format!("{}/issuer", fixture.base));
    fixture.runtime.block_on(async {
        let client = reqwest::Client::builder().timeout(PATIENCE).build().unwrap();
        let response = if through_daemon {
            let base = std::fs::read_to_string(home.path().join("rookd.addr")).unwrap();
            client
                .post(format!("{}/api/mcp/oauth/complete", base.trim()))
                .json(&serde_json::json!({"callback":callback.as_str()}))
                .send()
                .await
                .unwrap()
        } else {
            client.get(callback).send().await.unwrap()
        };
        assert!(response.status().is_success(), "{}", response.text().await.unwrap());
    });
    pty.screen_showing(100, 30, "Signed in and reconnected.");
    pty.screen_showing(100, 30, "private — connected");
    assert_eq!(fixture.grants.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(!pty.seen.contains("PRIVATE_PTY_ACCESS"));
    pty.send("x");
    pty.screen_showing(100, 30, "Saved credentials removed.");
    pty.send("\u{1b}");
    pty.screen_showing(100, 30, "PRESERVED_AUTH_DRAFT");
    let stored: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.path().join("mcp-auth/credentials.json")).unwrap())
            .unwrap();
    assert_eq!(stored["entries"], serde_json::json!([]));
    drop(pty);
    drop(daemon);
}
#[test]
fn local_mcp_sign_in_keeps_input_live_and_can_cancel_before_completing() {
    mcp_sign_in_keeps_input_live_and_can_cancel_before_completing(false);
}
#[test]
fn daemon_mcp_sign_in_keeps_input_live_and_can_cancel_before_completing() {
    mcp_sign_in_keeps_input_live_and_can_cancel_before_completing(true);
}
