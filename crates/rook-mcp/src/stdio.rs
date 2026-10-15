//! The stdio transport: a subprocess speaking newline-delimited JSON-RPC.

use std::collections::{HashMap, VecDeque};
use std::process::Stdio as ProcessStdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{Mutex, oneshot};

use crate::protocol::{Incoming, Notification, Request};
use crate::transport::Transport;
use crate::{McpError, Result, ServerConfig};

struct Waiting {
    reply: oneshot::Sender<Incoming>,
    owner: Option<Arc<crate::elicitation::Invocation>>,
}
type Pending = Arc<std::sync::Mutex<HashMap<u64, Waiting>>>;

struct Registered<'a> {
    pending: &'a Pending,
    id: u64,
    owner: Option<Arc<crate::elicitation::Invocation>>,
    finished: &'a AtomicBool,
    completed: bool,
}
impl Drop for Registered<'_> {
    fn drop(&mut self) {
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.id);
        if let Some(owner) = &self.owner {
            owner.close();
            if !self.completed {
                self.finished.store(true, Ordering::Release);
            }
        }
    }
}

/// The tail of the server's stderr, kept so a failure can say what the server
/// said about it. Bounded in lines and in each line, because a server dying in
/// a loop writes as much as it likes on the way down.
#[derive(Clone, Default)]
struct LastWords(Arc<std::sync::Mutex<VecDeque<String>>>);

const MOST_LAST_WORDS: usize = 5;
const MOST_LAST_WORD_CHARS: usize = 400;

impl LastWords {
    fn heard(&self, line: &str) {
        let line: String = line.trim().chars().take(MOST_LAST_WORD_CHARS).collect();
        if line.is_empty() {
            return;
        }
        let Ok(mut kept) = self.0.lock() else { return };
        kept.push_back(line);
        while kept.len() > MOST_LAST_WORDS {
            kept.pop_front();
        }
    }

    fn said(&self) -> String {
        let Ok(kept) = self.0.lock() else { return String::new() };
        kept.iter().cloned().collect::<Vec<_>>().join(" / ")
    }
}

/// The same cap the HTTP transport puts on one event, for the same reason:
/// `lines()` grows a single line until the machine runs out of memory, and how
/// long a line a server sends is decided by the server. Not a trust boundary —
/// a stdio server is a subprocess with the user's own privileges and needs no
/// trick to do harm — but a broken one should cost its own connection rather
/// than the agent's memory.
const MAX_LINE_BYTES: u64 = 8 << 20;

pub(crate) struct Stdio {
    name: String,
    heard: LastWords,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Pending,
    /// Set once the server's stdout ends, which is the last thing that can
    /// release a waiter. A request registered after that would wait out the
    /// whole call timeout for a reply nobody is left to deliver — thirty
    /// seconds of a turn, per call, for a server that is already gone.
    finished: Arc<AtomicBool>,
    next_id: AtomicU64,
    connection: u64,
    child: Mutex<Option<Child>>,
    group: std::sync::Mutex<Option<rook_contain::Group>>,
    lease: std::sync::Mutex<Option<crate::reap::Lease<'static>>>,
}

impl Drop for Stdio {
    fn drop(&mut self) {
        // npm launchers and browser drivers start children of their own. Killing
        // just the launcher leaves those children holding pipes and profiles.
        if let Some(group) = self.group.get_mut().unwrap_or_else(|e| e.into_inner()).take()
            && let Some(child) = self.child.get_mut().take()
            && let Some(lease) = self.lease.get_mut().unwrap_or_else(|e| e.into_inner()).take()
        {
            crate::reap::later(child, group, lease);
        }
    }
}

#[derive(PartialEq, Eq)]
enum Line {
    Read,
    Ended,
}

/// One line into `line`, or [`Line::Ended`] when the stream closed or a line
/// passed [`MAX_LINE_BYTES`].
///
/// A line over the cap ends the connection rather than being resynchronised
/// past: a server that sends one is broken, the waiters are released by the
/// same path that handles a closed pipe, and [`crate::Server::restart`] is what
/// brings a broken server back.
async fn read_bounded<R>(reader: &mut R, line: &mut Vec<u8>) -> Line
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    line.clear();
    match reader.take(MAX_LINE_BYTES).read_until(b'\n', line).await {
        Ok(0) => Line::Ended,
        Ok(_) if line.last() != Some(&b'\n') => Line::Ended,
        Ok(_) => Line::Read,
        Err(_) => Line::Ended,
    }
}

/// The program to start, looked up the way a shell would.
///
/// Windows searches `PATH` for `foo.exe` and consults `PATHEXT` only in a
/// shell, so a program installed as `npx.cmd` — which is how npm, uv and bun
/// install theirs — is "program not found" there while working everywhere
/// else. Everything a README tells someone to configure goes through this.
///
/// Copied rather than shared: `rook-mcp`, `rook-lsp` and `rook-skills` each
/// start a program somebody named in configuration, and the three sit on one
/// layer with nothing beneath them to hold it.
fn program(command: &str, cwd: Option<&std::path::Path>) -> std::path::PathBuf {
    let named = std::path::Path::new(command);
    // A relative program *and* a working directory: resolved against the
    // parent's on some platforms and the child's on others, which Rust's own
    // documentation calls unreliable. Resolved here it is neither. A bare name
    // is not this case — it belongs to the PATH search below.
    let has_directory = named.parent().is_some_and(|at| at != std::path::Path::new(""));
    if let Some(cwd) = cwd.filter(|_| named.is_relative() && has_directory) {
        return cwd.join(named);
    }
    match cfg!(windows) {
        true => resolved(
            command,
            &std::env::var_os("PATH").unwrap_or_default(),
            &std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".into()),
        ),
        false => std::path::PathBuf::from(command),
    }
}

/// What Windows would start, given its two variables.
///
/// Apart from [`program`] so that all of it is reachable from a test: which
/// machine the test runs on is not what decides whether this is right.
fn resolved(command: &str, path: &std::ffi::OsStr, exts: &str) -> std::path::PathBuf {
    let named = std::path::Path::new(command);
    if named.extension().is_some() || named.parent() != Some(std::path::Path::new("")) {
        return named.to_path_buf();
    }
    let exts: Vec<&str> = exts.split(';').filter(|e| !e.is_empty()).collect();
    std::env::split_paths(path)
        .find_map(|dir| exts.iter().map(|ext| dir.join(format!("{command}{ext}"))).find(|c| c.is_file()))
        .unwrap_or_else(|| named.to_path_buf())
}

impl Stdio {
    pub(crate) fn spawn(config: &ServerConfig) -> Result<Self> {
        let lease = crate::reap::reserve(config.stdio_max_processes)
            .map_err(|source| McpError::Spawn { command: config.command.clone(), source })?;
        let mut command = tokio::process::Command::new(program(
            &config.command,
            config.cwd.as_ref().map(std::path::Path::new),
        ));
        #[cfg(windows)]
        command.creation_flags(rook_contain::NO_WINDOW);
        #[cfg(unix)]
        command.process_group(0);
        command
            .args(&config.args)
            .envs(&config.env)
            .stdin(ProcessStdio::piped())
            .stdout(ProcessStdio::piped())
            .stderr(ProcessStdio::piped())
            .kill_on_drop(true);
        if let Some(cwd) = &config.cwd {
            command.current_dir(cwd);
        }

        let mut child =
            command.spawn().map_err(|e| McpError::Spawn { command: config.command.clone(), source: e })?;
        let group = rook_contain::Group::holding(child.id());
        let stdin = child.stdin.take().expect("stdin was piped");
        let stdin = Arc::new(Mutex::new(stdin));
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");

        // An undrained stderr pipe fills and blocks the server mid-write, which
        // presents as a hang with no output anywhere.
        let name = config.name.clone();
        let heard = LastWords::default();
        let keeping = heard.clone();
        let listening = tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut line = Vec::new();
            while read_bounded(&mut reader, &mut line).await == Line::Read {
                let said = String::from_utf8_lossy(&line);
                tracing::debug!(server = %name, "{}", said.trim_end());
                keeping.heard(&said);
            }
        });

        let pending: Pending = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let reader_pending = pending.clone();
        let finished = Arc::new(AtomicBool::new(false));
        let reader_finished = finished.clone();
        let answering = stdin.clone();
        let requests = crate::elicitation::Seen::default();
        let slots = Arc::new(tokio::sync::Semaphore::new(4));
        let name = config.name.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut line = Vec::new();
            while read_bounded(&mut reader, &mut line).await == Line::Read {
                let Ok(message) = serde_json::from_slice::<Incoming>(&line) else {
                    tracing::debug!(server = %name, "unparsable JSON-RPC frame");
                    continue;
                };
                if message.method.is_some() && message.id.is_some() {
                    if !message.id.as_ref().is_some_and(|id| requests.admit(id)) {
                        reader_finished.store(true, Ordering::Release);
                        break;
                    }
                    let Ok(permit) = slots.clone().try_acquire_owned() else {
                        reader_finished.store(true, Ordering::Release);
                        break;
                    };
                    let owner = {
                        let waiting = reader_pending.lock().unwrap_or_else(|e| e.into_inner());
                        if waiting.len() == 1 {
                            waiting
                                .values()
                                .next()
                                .filter(|w| !w.reply.is_closed())
                                .and_then(|w| w.owner.clone())
                        } else {
                            None
                        }
                    };
                    let running = owner.clone();
                    let writer = answering.clone();
                    let closed = reader_finished.clone();
                    let task = tokio::spawn(async move {
                        let _permit = permit;
                        let response = crate::elicitation::answer(running.clone(), message).await;
                        let Ok(mut line) = serde_json::to_vec(&response) else { return };
                        line.push(b'\n');
                        let _ = tokio::time::timeout(Duration::from_secs(5), async {
                            let mut writer = writer.lock().await;
                            if closed.load(Ordering::Acquire)
                                || running.as_ref().is_some_and(|o| !o.alive.load(Ordering::Acquire))
                            {
                                return;
                            }
                            if writer.write_all(&line).await.is_err() || writer.flush().await.is_err() {
                                closed.store(true, Ordering::Release);
                            }
                        })
                        .await;
                    });
                    if let Some(owner) = owner {
                        owner.track(task.abort_handle());
                    }
                    continue;
                }
                match message.id.as_ref().and_then(|id| id.as_u64()) {
                    Some(id) => {
                        if let Some(tx) = reader_pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id)
                        {
                            let _ = tx.reply.send(message);
                        }
                    }
                    None => tracing::debug!(server = %name, method = ?message.method, "notification"),
                }
            }
            // The two pipes are drained by separate tasks, and the waiters are
            // released from this one: without waiting for the other to finish,
            // whether the error carries the server's explanation depends on
            // which task the scheduler reached first. Bounded, because stdout
            // can close while the process is alive and stderr still open.
            let _ = tokio::time::timeout(Duration::from_secs(1), listening).await;
            // Marked before the waiters are released, not after: a request that
            // registers in the gap would otherwise be neither cleared here nor
            // stopped there, and would wait for the timeout.
            reader_finished.store(true, Ordering::Relaxed);
            // The pipe closed: waiters would otherwise hang until their timeout.
            reader_pending.lock().unwrap_or_else(|e| e.into_inner()).clear();
        });

        Ok(Self {
            name: config.name.clone(),
            heard,
            stdin,
            pending,
            finished,
            next_id: AtomicU64::new(1),
            connection: crate::elicitation::connection(),
            child: Mutex::new(Some(child)),
            group: std::sync::Mutex::new(Some(group)),
            lease: std::sync::Mutex::new(Some(lease)),
        })
    }

    async fn request_inner(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        timeout: Duration,
        handler: Option<Arc<dyn crate::elicitation::Elicitor>>,
    ) -> Result<Incoming> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        let owner = handler.map(|handler| {
            crate::elicitation::Invocation::new(&self.name, self.connection, id, method, handler)
        });
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, Waiting { reply: tx, owner: owner.clone() });
        let mut registered =
            Registered { pending: &self.pending, id, owner, finished: &self.finished, completed: false };
        // After registering, so the two orders are both covered: an insert
        // before the reader's sweep is released by it, and one after sees this.
        if self.finished.load(Ordering::Relaxed) {
            self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
            return Err(McpError::Closed { server: self.name.clone(), said: self.heard.said() });
        }

        let line = serde_json::to_string(&Request { jsonrpc: "2.0", id, method, params }).map_err(|e| {
            McpError::Decode { server: self.name.clone(), method: method.into(), message: e.to_string() }
        })?;
        let result = tokio::time::timeout(timeout, async {
            self.write_line(&line).await?;
            rx.await.map_err(|_| McpError::Closed { server: self.name.clone(), said: self.heard.said() })
        })
        .await
        .map_err(|_| McpError::Timeout {
            server: self.name.clone(),
            method: method.into(),
            timeout,
            said: self.heard.said(),
        })?;
        registered.completed = result.is_ok();
        result
    }

    async fn write_line(&self, line: &str) -> Result<()> {
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(format!("{line}\n").as_bytes())
            .await
            .map_err(|e| McpError::Transport { server: self.name.clone(), message: e.to_string() })?;
        stdin
            .flush()
            .await
            .map_err(|e| McpError::Transport { server: self.name.clone(), message: e.to_string() })
    }
}

#[async_trait]
impl Transport for Stdio {
    async fn request(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        timeout: Duration,
    ) -> Result<Incoming> {
        self.request_inner(method, params, timeout, None).await
    }
    async fn request_interactive(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        timeout: Duration,
        handler: Arc<dyn crate::elicitation::Elicitor>,
    ) -> Result<Incoming> {
        self.request_inner(method, params, timeout, Some(handler)).await
    }
    fn reusable(&self) -> bool {
        !self.finished.load(Ordering::Acquire)
    }

    async fn notify(&self, method: &str, params: Option<serde_json::Value>) -> Result<()> {
        let line = serde_json::to_string(&Notification { jsonrpc: "2.0", method, params }).map_err(|e| {
            McpError::Decode { server: self.name.clone(), method: method.into(), message: e.to_string() }
        })?;
        self.write_line(&line).await
    }

    fn child_pid(&self) -> Option<u32> {
        self.child.try_lock().ok()?.as_ref()?.id()
    }

    async fn shutdown(&self) {
        let mut child = self.child.lock().await;
        // Keep ownership here through the cancellable grace wait. If this
        // future is dropped, Stdio::drop still sends the group to the reaper.
        if let Some(group) = self.group.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            group.terminate();
        }
        if let Some(child) = child.as_mut() {
            tokio::time::sleep(crate::reap::GRACE).await;
            if let Some(group) = self.group.lock().unwrap_or_else(|e| e.into_inner()).take() {
                group.end();
            }
            let _ = child.kill().await;
        }
        child.take();
        self.lease.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
}

#[cfg(test)]
mod tests {
    use super::{program, resolved};

    fn windows(command: &str, path: &std::path::Path) -> std::path::PathBuf {
        resolved(command, &std::env::join_paths([path]).unwrap(), ".EXE;.CMD;.BAT")
    }

    /// A server given as `./bin/server` with a `cwd` set: some platforms resolve
    /// that against the parent's directory and some against the child's, which
    /// Rust's own documentation says not to rely on.
    #[test]
    fn a_relative_program_is_resolved_against_the_directory_it_will_run_in() {
        let cwd = std::path::Path::new("/srv/project");
        assert_eq!(program("./bin/server", Some(cwd)), cwd.join("./bin/server"));
        assert_eq!(program("bin/server", Some(cwd)), cwd.join("bin/server"));
        assert_eq!(
            program("/usr/bin/server", Some(cwd)),
            std::path::PathBuf::from("/usr/bin/server"),
            "an absolute one is already answered"
        );
        // A name nowhere on any PATH, so the answer is the same on every
        // platform: on a runner with node installed, `npx` came back as
        // `C:\Program Files\nodejs\npx.CMD` — the lookup working, and the
        // test asserting a macOS reading of it.
        let nowhere = "rook-no-such-program";
        assert_eq!(
            program(nowhere, Some(cwd)),
            std::path::PathBuf::from(nowhere),
            "a bare name is a PATH lookup, not a file in the working directory"
        );
    }

    /// Every MCP README configures a server as `npx …`, and npm installs npx as
    /// `npx.cmd`. `Command::new("npx")` looks for `npx.exe`, finds nothing, and
    /// reports a server that is installed as one that is not.
    #[test]
    fn a_program_installed_under_an_extension_is_found_by_its_bare_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("npx.CMD"), "@echo off").unwrap();

        assert_eq!(windows("npx", dir.path()), dir.path().join("npx.CMD"));
        assert_eq!(
            resolved("npx", &std::env::join_paths([dir.path()]).unwrap(), ".EXE"),
            std::path::PathBuf::from("npx"),
            "and only through an extension PATHEXT names"
        );
    }

    /// A name that is nowhere must come back as it went in: the failure that
    /// follows should say what was asked for, not a path nothing is at.
    #[test]
    fn a_name_that_needs_no_looking_up_is_passed_through_untouched() {
        let dir = tempfile::tempdir().unwrap();
        for command in ["/usr/local/bin/server", "./server", "server.exe", "rook-no-such-program"] {
            assert_eq!(windows(command, dir.path()), std::path::PathBuf::from(command), "{command}");
        }
    }
}
