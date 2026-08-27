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
        let opened = unsafe {
            libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), &mut size)
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
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while painted(&self.seen) <= 3 && std::time::Instant::now() < deadline {
            assert!(self.read_more(200), "the app drew nothing at all");
        }
        // A redraw emits only the cells that changed, so keep accumulating for a
        // moment rather than stopping at the first frame that looks complete.
        let settle = std::time::Instant::now() + std::time::Duration::from_millis(400);
        while std::time::Instant::now() < settle && self.read_more(100) {}
        grid(&self.seen, cols, rows)
    }

    /// The screen once `wanted` is on it, or the last one before giving up.
    ///
    /// A settling window is a guess about how long a redraw takes, and under a
    /// full test run that guess is wrong: waiting for the thing being asserted
    /// is the same lesson as waiting for a frame rather than for a byte.
    fn screen_showing(&mut self, cols: usize, rows: usize, wanted: &str) -> Vec<String> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let screen = self.screen(cols, rows);
            if screen.iter().any(|line| line.contains(wanted)) || std::time::Instant::now() >= deadline {
                return screen;
            }
        }
    }

    /// Whether anything arrived, so a closed pty ends the wait instead of
    /// spinning it out to the deadline.
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
            'J' => cells = vec![vec![' '; cols]; rows],
            _ => {}
        }
    }
    cells.into_iter().map(|r| r.into_iter().collect::<String>().trim_end().to_string()).collect()
}

fn tui(home: &std::path::Path, workspace: &std::path::Path) -> Pty {
    Pty::spawn(
        std::path::Path::new(env!("CARGO_BIN_EXE_rook")),
        &["--workspace", workspace.to_str().unwrap(), "tui"],
        &[("ROOK_HOME", home.to_str().unwrap()), ("ROOK_LOG", "error"), ("TERM", "xterm-256color")],
        100,
        30,
    )
}

#[test]
fn the_tui_starts_and_draws_its_tabs() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut pty = tui(home.path(), workspace.path());

    let screen = pty.screen(100, 30);
    let all = screen.join("\n");

    assert!(all.contains("Chat"), "the first tab must be drawn:\n{all}");
    assert!(all.contains("quit"), "the footer names the keys:\n{all}");
    assert!(
        screen.iter().filter(|line| !line.is_empty()).count() > 3,
        "a nearly blank screen means it drew into a zero-sized terminal:\n{all}"
    );
}

#[test]
fn the_browsing_tabs_render_without_a_model() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut pty = tui(home.path(), workspace.path());
    pty.screen(100, 30);

    // Tab, not "3": on the chat tab a digit is a character in the message.
    pty.send("\t\t");
    let screen = pty.screen_showing(100, 30, "Skills").join("\n");

    assert!(screen.contains("Skills"), "the skills tab must render on an empty store:\n{screen}");
    assert!(
        !screen.contains("Ask it something"),
        "the chat pane should be gone once another tab is selected:\n{screen}"
    );
}

#[test]
fn the_footer_shows_the_settings_and_f2_changes_them() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut pty = tui(home.path(), workspace.path());

    let before = pty.screen_showing(100, 30, "ask/high").join("\n");
    assert!(before.contains("ask/high"), "the configured defaults, in the footer:\n{before}");

    // F2 as a VT sequence; crossterm reads both this and SS3, and a pty is not
    // a terminal that will translate one for us.
    pty.send("\u{1b}[12~");
    let after = pty.screen_showing(100, 30, "readonly/high").join("\n");

    assert!(after.contains("readonly/high"), "F2 cycles approvals:\n{after}");
}

#[test]
fn the_memory_tab_shows_what_the_agent_remembers() {
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
    // Tabs, not "3": on the chat tab a digit is a character in the message.
    pty.send("\t\t");
    let screen = pty.screen_showing(100, 30, "Memory").join("\n");

    assert!(screen.contains("Memory"), "the tab must be there:\n{screen}");
    assert!(screen.contains("prefer tabs in Makefiles"), "and the fact:\n{screen}");
    assert!(screen.contains("style"), "with its tags:\n{screen}");
}

#[test]
fn the_tui_chat_answers_the_same_slash_commands_as_the_plain_cli() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut pty = tui(home.path(), workspace.path());
    pty.screen(100, 30);

    pty.send("/goal ship the release\r");
    pty.send("/goal\r");
    let screen = pty.screen_showing(100, 30, "goal set").join("\n");

    assert!(screen.contains("goal set"), "the command must run, not be sent to a model:\n{screen}");
    assert!(screen.contains("ship the release"), "and read back:\n{screen}");
}

#[test]
fn an_unknown_slash_command_in_the_tui_says_so() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut pty = tui(home.path(), workspace.path());
    pty.screen(100, 30);

    pty.send("/nonsense\r");
    let screen = pty.screen_showing(100, 30, "unknown command").join("\n");

    assert!(screen.contains("unknown command"), "{screen}");
    assert!(!screen.contains("cannot reach"), "it must not have gone to the provider:\n{screen}");
}

/// Nothing is spent before a turn runs, and the footer has to say the settings
/// without a stray separator where the total will go.
#[test]
fn the_footer_shows_no_running_total_before_there_is_one() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut pty = tui(home.path(), workspace.path());

    let screen = pty.screen_showing(100, 30, "ask/high").join("\n");
    assert!(screen.contains("ask/high"), "{screen}");
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

/// The chat REPL and the browser could both stop a turn; the TUI could only be
/// killed, taking the browsing state and any approval granted for the run.
#[test]
fn ctrl_c_stops_a_running_turn_rather_than_the_whole_ui() {
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
    assert!(after.contains("Chat"), "and the tabs are still drawn, so it did not quit:\n{after}");
}

/// The store holds every workspace, and the sessions tab named none of them:
/// another project's session read as one of this project's.
#[test]
fn the_sessions_tab_names_the_workspace_only_when_it_is_another_one() {
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
    // Digits are characters in the message box on the chat tab, so the way to
    // the next tab is the key the footer names.
    pty.send("\t");
    let screen = pty.screen_showing(100, 30, "events · theirs").join("\n");

    assert!(screen.contains("events · theirs"), "a session from another project says which:\n{screen}");
    assert!(
        !screen.contains("events · mine"),
        "and this one's own is not repeated on every row — the title bar already says it:\n{screen}"
    );
}
