//! Drop cannot await a driver's shutdown. Keep its process owned for a bounded
//! grace period on one worker, including when the caller's runtime is ending.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{OnceLock, mpsc};
use std::time::{Duration, Instant};

pub(crate) const GRACE: Duration = Duration::from_millis(600);
const MAX_PENDING: usize = 1024;

#[derive(Default)]
struct Budget(AtomicUsize);
impl Budget {
    fn reserve(&self, maximum: usize) -> std::io::Result<Lease<'_>> {
        // try_update is newer than the supported Rust 1.90 toolchain.
        #[allow(deprecated)]
        self.0.fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
            (used < maximum.clamp(1, MAX_PENDING)).then_some(used + 1)
        }).map_err(|_| std::io::Error::new(std::io::ErrorKind::WouldBlock,
            "MCP stdio process budget reached; wait for closing servers or increase stdio_max_processes"))?;
        Ok(Lease(self))
    }
}
pub(crate) struct Lease<'a>(&'a Budget);
impl Drop for Lease<'_> {
    fn drop(&mut self) {
        self.0.0.fetch_sub(1, Ordering::AcqRel);
    }
}
pub(crate) fn reserve(maximum: usize) -> std::io::Result<Lease<'static>> {
    static BUDGET: Budget = Budget(AtomicUsize::new(0));
    BUDGET.reserve(maximum)
}

struct Owned {
    child: tokio::process::Child,
    group: rook_contain::Group,
    by: Instant,
    // Reserved before spawn and retained through cleanup: queue saturation
    // cannot take away an admitted driver's graceful shutdown.
    _lease: Lease<'static>,
}
impl Drop for Owned {
    fn drop(&mut self) {
        self.group.end();
        let _ = self.child.start_kill();
        let _ = self.child.try_wait();
    }
}

pub(crate) fn later(mut child: tokio::process::Child, group: rook_contain::Group, lease: Lease<'static>) {
    group.terminate();
    static QUEUE: OnceLock<Option<mpsc::SyncSender<Owned>>> = OnceLock::new();
    let queue = QUEUE.get_or_init(|| {
        let (send, receive) = mpsc::sync_channel::<Owned>(MAX_PENDING);
        std::thread::Builder::new()
            .name("rook-mcp-reap".into())
            .spawn(move || {
                let mut pending = Vec::<Owned>::new();
                loop {
                    // A launcher can exit before its driver finishes closing a
                    // detached browser. Keep the root PID unreaped through the
                    // grace period, both for the children and to prevent PID reuse.
                    pending.retain(|process| Instant::now() < process.by);
                    if pending.len() == MAX_PENDING {
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    match receive.recv_timeout(Duration::from_millis(10)) {
                        Ok(process) => pending.push(process),
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .ok()
            .map(|_| send)
    });
    if let Some(queue) = queue {
        // A full queue fails closed rather than growing another worker/backlog.
        let _ = queue.try_send(Owned { child, group, by: Instant::now() + GRACE, _lease: lease });
    } else {
        group.end();
        let _ = child.start_kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn process_admission_reserves_cleanup_capacity_before_spawning() {
        let budget = Budget::default();
        let first = budget.reserve(2).unwrap();
        let second = budget.reserve(2).unwrap();
        assert_eq!(budget.0.load(Ordering::Acquire), 2, "the configured admission bound was reached");
        assert!(budget.reserve(2).is_err());
        assert_eq!(budget.0.load(Ordering::Acquire), 2);
        drop(first);
        let third = budget.reserve(2).unwrap();
        assert_eq!(budget.0.load(Ordering::Acquire), 2);
        drop((second, third));
        assert_eq!(budget.0.load(Ordering::Acquire), 0);
    }
}
