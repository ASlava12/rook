use super::*;
use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
struct Admission;
impl Drop for Admission {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::AcqRel);
    }
}
pub(super) struct Output {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}
pub(super) fn run(
    mut command: Command,
    input: Option<Arc<Vec<u8>>>,
    cap: usize,
    timeout: u64,
) -> Result<Output> {
    let deadline = Instant::now() + Duration::from_secs(timeout);
    loop {
        if ACTIVE.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < 4).then_some(n + 1)).is_ok() {
            break;
        }
        if Instant::now() >= deadline {
            return Err(invalid(
                "SSH control I/O capacity remained occupied through its deadline; inspect the retained operation",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let admission = Arc::new(Admission);
    command
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = rook_contain::quietly(&mut command)
        .spawn()
        .map_err(|e| invalid(format!("SSH launcher unavailable: {e}")))?;
    let group = rook_contain::Group::holding(Some(child.id()));
    let out = reader(child.stdout.take(), cap, admission.clone());
    let err = reader(child.stderr.take(), 8192, admission.clone());
    if let (Some(mut stdin), Some(input)) = (child.stdin.take(), input) {
        let held = admission.clone();
        std::thread::spawn(move || {
            let _held = held;
            let _ = stdin.write_all(&input);
        });
    }
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                group.end();
                let _ = child.kill();
                // Reap without extending the caller's deadline. A driver that cannot
                // exit continues to own admission instead of accumulating waiters.
                let held = admission.clone();
                std::thread::spawn(move || {
                    let _held = held;
                    let _ = child.wait();
                });
                return Err(ToolError::ExecutionUnknown(
                    "SSH control request did not finish within its deadline; inspect retained state".into(),
                ));
            }
        }
    };
    group.end();
    let receive = |rx: std::sync::mpsc::Receiver<(Vec<u8>, bool)>| -> Result<Vec<u8>> {
        let (bytes, overflow) =
            rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).map_err(|_| {
                ToolError::ExecutionUnknown("SSH control output did not close; inspect its resource".into())
            })?;
        if overflow {
            return Err(invalid("SSH control response exceeded its byte limit"));
        }
        Ok(bytes)
    };
    Ok(Output { code: status.code().unwrap_or(-1), stdout: receive(out)?, stderr: receive(err)? })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn control_response_bound_drains_an_actual_oversized_pipe_and_reports_the_reached_limit() {
        let mut command = Command::new("sh");
        command.args(["-c", "head -c 262144 /dev/zero"]);
        let error = run(command, None, 1024, 30).err().unwrap();
        assert!(error.to_string().contains("byte limit"), "{error}");
    }
    #[test]
    fn control_deadline_terminates_and_reaps_its_owned_process_without_waiting_for_the_full_command() {
        let root = tempfile::tempdir().unwrap();
        let pid = root.path().join("pid");
        let mut command = Command::new("sh");
        command.args(["-c", "echo $$ > \"$1\"; exec sleep 60", "fixture"]).arg(&pid);
        assert!(matches!(run(command, None, 1024, 1), Err(ToolError::ExecutionUnknown(_))));
        let pid = std::fs::read_to_string(pid).unwrap().trim().parse::<i32>().unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let alive = unsafe { libc::kill(pid, 0) } == 0;
            if !alive {
                break;
            }
            assert!(Instant::now() < deadline, "owned driver still exists after its deadline");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
fn reader<T: Read + Send + 'static>(
    stream: Option<T>,
    cap: usize,
    held: Arc<Admission>,
) -> std::sync::mpsc::Receiver<(Vec<u8>, bool)> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _held = held;
        let mut bytes = Vec::new();
        let mut overflow = false;
        if let Some(mut stream) = stream {
            let mut chunk = [0u8; 16384];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        let take = n.min(cap.saturating_sub(bytes.len()));
                        bytes.extend_from_slice(&chunk[..take]);
                        overflow |= take < n;
                    }
                    Err(_) => {
                        overflow = true;
                        break;
                    }
                }
            }
        }
        let _ = tx.send((bytes, overflow));
    });
    rx
}
