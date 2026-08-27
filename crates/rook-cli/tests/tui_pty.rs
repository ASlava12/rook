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
    fn screen(&mut self, cols: usize, rows: usize) -> Vec<String> {
        let mut chunk = [0u8; 8192];
        // Generous for the first byte — starting up opens a store and connects
        // whatever is configured — then a short window to collect the frame.
        assert!(readable(&self.master, 20_000), "the app drew nothing at all");
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(800);
        while std::time::Instant::now() < deadline && readable(&self.master, 100) {
            match self.master.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => self.seen.push_str(&String::from_utf8_lossy(&chunk[..n])),
                Err(_) => break,
            }
        }
        grid(&self.seen, cols, rows)
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
    let screen = pty.screen(100, 30).join("\n");

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

    let before = pty.screen(100, 30).join("\n");
    assert!(before.contains("ask/high"), "the configured defaults, in the footer:\n{before}");

    // F2 as a VT sequence; crossterm reads both this and SS3, and a pty is not
    // a terminal that will translate one for us.
    pty.send("\u{1b}[12~");
    let after = pty.screen(100, 30).join("\n");

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
    let screen = pty.screen(100, 30).join("\n");

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
    let screen = pty.screen(100, 30).join("\n");

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
    let screen = pty.screen(100, 30).join("\n");

    assert!(screen.contains("unknown command"), "{screen}");
    assert!(!screen.contains("cannot reach"), "it must not have gone to the provider:\n{screen}");
}
