//! Short synchronous probes need a deadline on pipe EOF as well as child exit.

use std::{
    io::{self, Read},
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

struct Owned {
    child: Child,
    group: crate::Group,
    finished: bool,
}

impl Drop for Owned {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.group.end();
        let _ = self.child.kill();
        // Never replace a bounded capture with an unbounded wait to reap it.
        let until = Instant::now() + Duration::from_secs(1);
        while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
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
    let child = crate::on_its_own(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let group = crate::Group::holding(Some(child.id()));
    let mut owned = Owned { child, group, finished: false };
    let deadline = Instant::now().checked_add(patience).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "probe patience exceeds the monotonic clock")
    })?;
    let mut stdout = owned.child.stdout.take().ok_or_else(|| io::Error::other("missing probe stdout"))?;
    let mut stderr = owned.child.stderr.take().ok_or_else(|| io::Error::other("missing probe stderr"))?;
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let (mut out_eof, mut err_eof) = (false, false);
    let mut status = None;
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
        }
        if let Some(status) = status.filter(|_| out_eof && err_eof) {
            owned.finished = true;
            return Ok(Output { status, stdout: out, stderr: err });
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
        reason = "the fixture deliberately exits before its pipe holder; the outer capture owns their group"
    )]
    fn fixture_child() {
        let Ok(mode) = std::env::var("ROOK_OWNED_CAPTURE_TEST") else { return };
        match mode.as_str() {
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
    fn a_normal_probe_keeps_both_streams_and_has_no_interactive_stdin() {
        let output = captured_within(&mut command("normal"), Duration::from_secs(15), 2048).unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("owned version 99.0.1; stdin EOF"));
        assert!(String::from_utf8_lossy(&output.stderr).contains("owned stderr"));
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
