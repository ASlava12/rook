//! Short synchronous probes need a deadline on pipe EOF as well as child exit.

use std::{
    io::{self, Read},
    process::{Child, Command, ExitStatus, Output, Stdio},
    time::{Duration, Instant},
};

struct Owned {
    child: Child,
    group: crate::Group,
    finished: bool,
}

impl Owned {
    fn start(command: &mut Command) -> io::Result<Self> {
        let child = crate::on_its_own(command).spawn()?;
        let group = crate::Group::holding(Some(child.id()));
        Ok(Self { child, group, finished: false })
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.group.end();
        let _ = self.child.kill();
        // Never replace a bounded probe with an unbounded wait to reap it.
        let until = Instant::now() + Duration::from_secs(1);
        while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Ask whether a short command exits within its post-spawn patience.
///
/// `Some` retains the observed exit status; `None` means the direct child was
/// still running when patience expired. Standard input is null; output is discarded.
/// Every outcome requests owned-group/direct-child cleanup, including a normal
/// parent exit that left descendants. Reaping polls for at most one additional
/// second. Process creation/signalling is synchronous, Darwin group cleanup
/// retains its five-second grace and [`crate::Group`]'s ownership limits apply.
pub fn probed_within(command: &mut Command, patience: Duration) -> io::Result<Option<ExitStatus>> {
    let mut owned = Owned::start(command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()))?;
    let deadline = Instant::now().checked_add(patience).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "probe patience exceeds the monotonic clock")
    })?;
    loop {
        // Inspect status before the clock: an overdue observer must retain a
        // completed shim's refusal rather than call it a still-running server.
        if let Some(status) = owned.child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(5).min(deadline.saturating_duration_since(Instant::now())));
    }
}

/// Capture a short owned command with a combined stdout/stderr byte ceiling.
///
/// After spawning, the same patience covers exit and both pipe EOFs. Ready
/// reads drain noisy children and retain at most `maximum` output bytes. A
/// timed-out or oversized result is rejected, including its partial output.
/// Errors trigger best-effort owned-group/direct-child termination and bounded
/// reap polling. Platform process creation/signalling remains synchronous;
/// Darwin group cleanup can additionally spend its existing five-second grace.
/// Job attachment and detached descendants retain [`crate::Group`]'s limits.
pub fn captured_within(command: &mut Command, patience: Duration, maximum: usize) -> io::Result<Output> {
    capture(command, patience, maximum, false).map(|result| result.output)
}

/// A completed capture and the separate facts observed after requesting cleanup.
#[derive(Debug)]
pub struct CapturedCommand {
    pub output: Output,
    /// Whether the platform accepted the owned-group cleanup signal.
    pub cleanup_signalled: bool,
    /// The immediate answer from [`crate::Group::alive`], including its platform
    /// limits. A terminating group can still be present; unknown is not stopped.
    pub observed_group_alive: Option<bool>,
}

/// Capture under the same exit/EOF deadline and byte ceiling as
/// [`captured_within`], requesting group cleanup as soon as the parent exits.
///
/// A checked example can finish while leaving a child or inherited pipe handle.
/// Cleanup happens while the remaining pipe bytes drain, before awaiting EOF.
/// The observed command result stays separate from group cleanup facts. This
/// does not certify the absence of detached processes; [`crate::Group`]'s
/// ownership limits and Darwin's existing cleanup grace still apply.
pub fn captured_with_cleanup(
    command: &mut Command,
    patience: Duration,
    maximum: usize,
) -> io::Result<CapturedCommand> {
    capture(command, patience, maximum, true)
}

fn capture(
    command: &mut Command,
    patience: Duration,
    maximum: usize,
    cleanup: bool,
) -> io::Result<CapturedCommand> {
    let mut owned = Owned::start(command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()))?;
    let deadline = Instant::now().checked_add(patience).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "probe patience exceeds the monotonic clock")
    })?;
    let mut stdout = owned.child.stdout.take().ok_or_else(|| io::Error::other("missing probe stdout"))?;
    let mut stderr = owned.child.stderr.take().ok_or_else(|| io::Error::other("missing probe stderr"))?;
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let (mut out_eof, mut err_eof) = (false, false);
    let mut status = None;
    let mut cleanup_signalled = false;
    loop {
        if Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "probe exit or output exceeded patience"));
        }
        let mut progressed = false;
        if !out_eof {
            progressed |= receive(&mut stdout, &mut out, err.len(), maximum, &mut out_eof)?;
        }
        if !err_eof {
            progressed |= receive(&mut stderr, &mut err, out.len(), maximum, &mut err_eof)?;
        }
        if status.is_none() {
            status = owned.child.try_wait()?;
            if cleanup && status.is_some() {
                cleanup_signalled = owned.group.end();
            }
        }
        if let Some(status) = status.filter(|_| out_eof && err_eof) {
            let observed_group_alive = if cleanup { owned.group.alive() } else { None };
            owned.finished = true;
            return Ok(CapturedCommand {
                output: Output { status, stdout: out, stderr: err },
                cleanup_signalled,
                observed_group_alive,
            });
        }
        if !progressed {
            std::thread::sleep(
                Duration::from_millis(5).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }
}

fn receive<P: Pipe>(
    pipe: &mut P,
    bytes: &mut Vec<u8>,
    other: usize,
    maximum: usize,
    eof: &mut bool,
) -> io::Result<bool> {
    let mut buffer = [0; 4096];
    let Some(count) = pipe.ready(&mut buffer)? else { return Ok(false) };
    if count == 0 {
        *eof = true;
    } else {
        let room = maximum.saturating_sub(other).saturating_sub(bytes.len());
        if count > room {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "probe output exceeded capture ceiling"));
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    Ok(true)
}

trait Pipe: Read {
    fn ready(&mut self, buffer: &mut [u8]) -> io::Result<Option<usize>>;
}

#[cfg(unix)]
impl<T: Read + std::os::fd::AsRawFd> Pipe for T {
    fn ready(&mut self, buffer: &mut [u8]) -> io::Result<Option<usize>> {
        let mut descriptor = libc::pollfd { fd: self.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        // Safety: one live borrowed descriptor; zero timeout never waits.
        let ready = unsafe { libc::poll(&mut descriptor, 1, 0) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            return if error.kind() == io::ErrorKind::Interrupted { Ok(None) } else { Err(error) };
        }
        if ready == 0 {
            return Ok(None);
        }
        if descriptor.revents & libc::POLLNVAL != 0 {
            return Err(io::Error::other("probe pipe descriptor closed unexpectedly"));
        }
        // No other reader consumes this owned pipe. Read returns available
        // bytes or EOF after poll reports input/hangup, without filling buffer.
        self.read(buffer).map(Some)
    }
}

#[cfg(windows)]
impl<T: Read + std::os::windows::io::AsRawHandle> Pipe for T {
    fn ready(&mut self, buffer: &mut [u8]) -> io::Result<Option<usize>> {
        use windows_sys::Win32::{
            Foundation::{ERROR_BROKEN_PIPE, ERROR_NO_DATA},
            System::Pipes::PeekNamedPipe,
        };
        let mut available = 0;
        // Safety: the borrowed anonymous-pipe handle remains live; optional
        // buffers are null and available points to a writable DWORD.
        let result = unsafe {
            PeekNamedPipe(
                self.as_raw_handle(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        };
        if result == 0 {
            let error = io::Error::last_os_error();
            return match error.raw_os_error().map(|code| code as u32) {
                Some(ERROR_BROKEN_PIPE | ERROR_NO_DATA) => Ok(Some(0)),
                _ => Err(error),
            };
        }
        if available == 0 {
            return Ok(None);
        }
        let count = buffer.len().min(available as usize);
        self.read(&mut buffer[..count]).map(Some)
    }
}

#[cfg(not(any(unix, windows)))]
impl<T: Read> Pipe for T {
    fn ready(&mut self, _: &mut [u8]) -> io::Result<Option<usize>> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "ready probe-pipe reads are unsupported"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn command(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "capture::tests::fixture_child", "--nocapture"])
            .env("ROOK_OWNED_CAPTURE_TEST", mode);
        command
    }

    #[test]
    #[allow(
        clippy::zombie_processes,
        reason = "the fixture deliberately exits before its pipe/socket holder; the outer probe owns their group"
    )]
    fn fixture_child() {
        let Ok(mode) = std::env::var("ROOK_OWNED_CAPTURE_TEST") else { return };
        match mode.as_str() {
            "refused" => std::process::exit(7),
            "pending" => {
                std::fs::write(std::env::var("ROOK_OWNED_CAPTURE_MARKER").unwrap(), b"ready").unwrap();
                std::thread::sleep(Duration::from_secs(30));
            }
            "socket-holder" => {
                let mut socket =
                    std::net::TcpStream::connect(std::env::var("ROOK_OWNED_CAPTURE_ENDPOINT").unwrap())
                        .unwrap();
                socket.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
                socket.write_all(b"ready").unwrap();
                let mut ack = [0];
                socket.read_exact(&mut ack).unwrap();
                std::fs::write(std::env::var("ROOK_OWNED_CAPTURE_MARKER").unwrap(), b"ready").unwrap();
                let _ = socket.read(&mut ack);
            }
            "probe-parent" => {
                let mut holder = command("socket-holder");
                let _holder = crate::quietly(&mut holder)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap();
                let marker = std::env::var("ROOK_OWNED_CAPTURE_MARKER").unwrap();
                let deadline = Instant::now() + Duration::from_secs(10);
                while !std::path::Path::new(&marker).is_file() {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            "pipe-holder" => std::thread::sleep(Duration::from_secs(30)),
            "parent" => {
                let mut child = command("pipe-holder");
                let child = crate::quietly(&mut child)
                    .stdin(Stdio::null())
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .spawn()
                    .unwrap();
                std::fs::write(std::env::var("ROOK_OWNED_CAPTURE_MARKER").unwrap(), child.id().to_string())
                    .unwrap();
                println!("parent finished");
            }
            "combined" | "flood" => {
                let count = if mode == "combined" { 1024 } else { 128 * 1024 };
                std::io::stdout().write_all(&vec![0xfe; count]).unwrap();
                std::io::stderr().write_all(&vec![0xfd; count]).unwrap();
            }
            "normal" => {
                let mut byte = [0];
                assert_eq!(std::io::stdin().read(&mut byte).unwrap(), 0);
                println!("owned version 99.0.1; stdin EOF");
                eprintln!("owned stderr");
            }
            _ => panic!("unknown owned fixture mode"),
        }
        std::process::exit(0);
    }

    #[test]
    fn status_probes_retain_success_and_refusal_without_using_cleanup_as_a_verdict() {
        let accepted = probed_within(&mut command("normal"), Duration::from_secs(15)).unwrap().unwrap();
        assert!(accepted.success());
        let refused = probed_within(&mut command("refused"), Duration::from_secs(15)).unwrap().unwrap();
        assert_eq!(refused.code(), Some(7));
    }

    #[test]
    fn status_probe_timeout_is_distinct_from_a_command_refusal() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("started");
        let mut pending = command("pending");
        pending.env("ROOK_OWNED_CAPTURE_MARKER", &marker);
        let started = Instant::now();
        assert!(probed_within(&mut pending, Duration::from_secs(5)).unwrap().is_none());
        assert!(marker.is_file(), "the fixture must actually start before timeout");
        assert!(started.elapsed() < Duration::from_secs(20));
    }

    #[test]
    fn status_probe_spawn_failure_remains_an_error() {
        let directory = tempfile::tempdir().unwrap();
        let error = probed_within(&mut Command::new(directory.path().join("absent")), Duration::from_secs(5))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn a_successful_status_probe_retires_its_owned_descendant_connection() {
        assert_descendant_connection_retires(false);
    }

    #[test]
    fn a_successful_capture_retires_its_owned_descendant_connection() {
        assert_descendant_connection_retires(true);
    }

    fn assert_descendant_connection_retires(capture: bool) {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("connected");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let (sent, received) = std::sync::mpsc::channel();
        let observer = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    _ => {
                        let _ = sent.send((false, "no owned connection accepted".to_string()));
                        return;
                    }
                }
            };
            // Windows accepts the listener's nonblocking mode with the socket.
            // This observer waits for EOF under its own finite read deadline.
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
            let mut hello = [0; 5];
            stream.read_exact(&mut hello).unwrap();
            assert_eq!(&hello, b"ready");
            stream.write_all(b"!").unwrap();
            let ending = stream.read(&mut [0]);
            let detail = format!("{ending:?}");
            let retired = match ending {
                Ok(0) => true,
                Err(error) => error.kind() == io::ErrorKind::ConnectionReset,
                _ => false,
            };
            let _ = sent.send((retired, detail));
        });
        let mut parent = command("probe-parent");
        parent
            .env("ROOK_OWNED_CAPTURE_MARKER", &marker)
            .env("ROOK_OWNED_CAPTURE_ENDPOINT", address.to_string());
        if capture {
            let result = captured_with_cleanup(&mut parent, Duration::from_secs(15), 4096).unwrap();
            assert!(result.output.status.success());
            assert!(result.cleanup_signalled, "the descendant must receive the cleanup request");
        } else {
            assert!(probed_within(&mut parent, Duration::from_secs(15)).unwrap().unwrap().success());
        }
        assert!(marker.is_file(), "a real descendant held the connection before its parent exited");
        let (retired, detail) = received.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(retired, "owned connection did not report retirement: {detail}");
        observer.join().unwrap();
    }

    #[test]
    fn a_normal_probe_keeps_both_streams_and_has_no_interactive_stdin() {
        let output = captured_within(&mut command("normal"), Duration::from_secs(15), 2048).unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("owned version 99.0.1; stdin EOF"));
        assert!(String::from_utf8_lossy(&output.stderr).contains("owned stderr"));
    }

    #[test]
    fn capture_cleanup_does_not_replace_exit_or_stream_facts() {
        let result = captured_with_cleanup(&mut command("normal"), Duration::from_secs(15), 4096).unwrap();
        assert!(result.output.status.success());
        assert!(String::from_utf8_lossy(&result.output.stdout).contains("stdin EOF"));
        assert!(String::from_utf8_lossy(&result.output.stderr).contains("owned stderr"));
        let refused = captured_with_cleanup(&mut command("refused"), Duration::from_secs(15), 4096).unwrap();
        assert_eq!(refused.output.status.code(), Some(7));
    }

    #[test]
    fn large_stdout_and_stderr_are_captured_without_losing_bytes() {
        let output = captured_within(&mut command("flood"), Duration::from_secs(15), 300 * 1024).unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout.iter().filter(|byte| **byte == 0xfe).count(), 128 * 1024);
        assert_eq!(output.stderr.iter().filter(|byte| **byte == 0xfd).count(), 128 * 1024);
    }

    #[test]
    fn individually_small_streams_still_share_one_capture_ceiling() {
        let error = captured_within(&mut command("combined"), Duration::from_secs(15), 1500).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("capture ceiling"));
    }

    #[test]
    fn parent_exit_cannot_remove_the_deadline_on_a_descendant_held_pipe() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("owned-child.pid");
        let mut parent = command("parent");
        parent.env("ROOK_OWNED_CAPTURE_MARKER", &marker);
        let started = Instant::now();
        let error = captured_within(&mut parent, Duration::from_secs(5), 2048).unwrap_err();
        assert!(marker.is_file(), "the owned parent must actually launch its pipe-holding descendant");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the thirty-second descendant cannot govern capture completion"
        );
    }
}
