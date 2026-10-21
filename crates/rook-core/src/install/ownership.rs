//! A cache mutation lease shared by cooperating installer processes.
use std::{fs::File, path::Path, time::Duration};

const MAX_WAIT: Duration = Duration::from_secs(600);

pub(super) async fn acquire(cache: &Path, command: &str) -> Result<File, String> {
    acquire_with_timeout(cache, command, MAX_WAIT).await
}

async fn acquire_with_timeout(cache: &Path, command: &str, limit: Duration) -> Result<File, String> {
    use sha2::{Digest, Sha256};
    let directory = cache.join(".install-locks");
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("could not create install ownership directory: {error}"))?;
    // This namespace is separate from both version tags and current's files.
    // Retain the inode after release: deleting a lock file splits ownership
    // between a still-open inode and a newly created replacement.
    let lock = directory.join(hex::encode(Sha256::digest(command.as_bytes())));
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock)
        .map_err(|error| format!("could not open install ownership for {command}: {error}"))?;
    let until = tokio::time::Instant::now() + limit;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => {
                let now = tokio::time::Instant::now();
                if now >= until {
                    return Err(format!(
                        "another process is still changing the {command} cache; retry when it finishes"
                    ));
                }
                tokio::time::sleep(Duration::from_millis(50).min(until - now)).await;
                if tokio::time::Instant::now() >= until {
                    return Err(format!(
                        "another process is still changing the {command} cache; retry when it finishes"
                    ));
                }
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(format!("could not acquire install ownership for {command}: {error}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{future::Future, task::Poll};

    const WORKER_HOME: &str = "ROOK_INSTALL_OWNERSHIP_TEST_HOME";

    #[test]
    #[ignore = "owned process fixture, explicitly launched by the process-loss test"]
    fn ownership_worker() {
        let Some(home) = std::env::var_os(WORKER_HOME) else { return };
        let home = std::path::PathBuf::from(home);
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let _ownership = runtime.block_on(acquire(&home, "owned-server")).unwrap();
        std::fs::write(home.join("ready"), b"held").unwrap();
        std::thread::sleep(Duration::from_secs(60));
    }

    struct OwnedChild {
        child: std::process::Child,
        reaped: bool,
    }
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            if !self.reaped {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }

    #[tokio::test]
    async fn process_loss_releases_the_actual_kernel_lease_without_deleting_its_file() {
        let cache = tempfile::tempdir().unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--ignored", "--exact", "install::ownership::tests::ownership_worker"])
            .env(WORKER_HOME, cache.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut worker =
            OwnedChild { child: rook_contain::quietly(&mut command).spawn().unwrap(), reaped: false };
        tokio::time::timeout(Duration::from_secs(30), async {
            while !cache.path().join("ready").is_file() {
                assert!(worker.child.try_wait().unwrap().is_none(), "the owned lock worker exited early");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert!(acquire_with_timeout(cache.path(), "owned-server", Duration::ZERO).await.is_err());
        worker.child.kill().unwrap();
        worker.child.wait().unwrap();
        worker.reaped = true;
        let _next = tokio::time::timeout(Duration::from_secs(5), acquire(cache.path(), "owned-server"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(std::fs::read_dir(cache.path().join(".install-locks")).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn another_handle_waits_and_released_ownership_is_reusable() {
        let cache = tempfile::tempdir().unwrap();
        let owner = acquire(cache.path(), "owned-server").await.unwrap();
        let mut waiting = Box::pin(acquire(cache.path(), "owned-server"));
        std::future::poll_fn(|cx| {
            assert!(waiting.as_mut().poll(cx).is_pending(), "the other handle owns mutation");
            Poll::Ready(())
        })
        .await;
        drop(owner);
        let next = tokio::time::timeout(Duration::from_secs(5), waiting).await.unwrap().unwrap();
        assert!(next.metadata().unwrap().is_file());
        drop(next);
        let locks: Vec<_> = std::fs::read_dir(cache.path().join(".install-locks")).unwrap().collect();
        assert_eq!(locks.len(), 1, "release must retain the same coordination inode");
        acquire(cache.path(), "owned-server").await.unwrap();
    }

    #[tokio::test]
    async fn different_commands_and_caches_do_not_wait_for_this_owner() {
        let cache = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let _owner = acquire(cache.path(), "first").await.unwrap();
        let _second = tokio::time::timeout(Duration::from_secs(5), acquire(cache.path(), "second"))
            .await
            .unwrap()
            .unwrap();
        let _third = tokio::time::timeout(Duration::from_secs(5), acquire(other.path(), "first"))
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn cancelling_a_waiter_leaves_the_owner_and_no_late_acquisition() {
        let cache = tempfile::tempdir().unwrap();
        let owner = acquire(cache.path(), "owned-server").await.unwrap();
        let mut waiting = Box::pin(acquire(cache.path(), "owned-server"));
        std::future::poll_fn(|cx| {
            assert!(waiting.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(waiting);
        assert!(acquire_with_timeout(cache.path(), "owned-server", Duration::ZERO).await.is_err());
        drop(owner);
        tokio::time::timeout(Duration::from_secs(5), acquire(cache.path(), "owned-server"))
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn deadline_does_not_steal_another_handle_and_control_io_errors_fail() {
        let cache = tempfile::tempdir().unwrap();
        let owner = acquire(cache.path(), "owned-server").await.unwrap();
        let error = acquire_with_timeout(cache.path(), "owned-server", Duration::ZERO).await.unwrap_err();
        assert!(error.contains("another process"), "{error}");
        drop(owner);
        let blocked = tempfile::tempdir().unwrap();
        std::fs::write(blocked.path().join(".install-locks"), b"owned obstacle").unwrap();
        assert!(acquire(blocked.path(), "owned-server").await.unwrap_err().contains("ownership directory"));
    }
}
