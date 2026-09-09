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
                std::time::Instant::now() < deadline,
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
        format!("{} bytes read, child {alive}", self.seen.len())
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
fn a_model_that_asks_to_run(home: &std::path::Path, command: &str) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let arguments = serde_json::json!({ "command": command, "cwd": "." }).to_string();
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
            // Drained, or the client sees the connection close on its body.
            std::io::copy(&mut reader.take(length as u64), &mut std::io::sink()).ok();

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
    let sent = pty.screen_showing(100, 30, "› /session").join("\n");
    assert!(sent.contains("› /session"), "what was sent goes into the log:\n{sent}");

    pty.send("\u{1b}[A"); // up, for the last thing sent
    // Twice on the screen: once where it was said, once back in the box. They
    // are drawn the same, which is the point — it is there to be edited.
    let recalled = pty.screen(100, 30);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut screen = recalled;
    while screen.iter().filter(|l| l.contains("› /session")).count() < 2 {
        assert!(std::time::Instant::now() < deadline, "never came back:\n{}", screen.join("\n"));
        screen = pty.screen(100, 30);
    }
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
