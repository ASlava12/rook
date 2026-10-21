//! A real descendant must stop writing when an installer command is cancelled.
use std::{path::PathBuf, time::Duration};
use tokio::io::AsyncReadExt;

const HOME: &str = "ROOK_INSTALL_COMMAND_TEST_HOME";

/// Runs only in the explicitly launched child test harness, with child-only env.
#[test]
#[ignore = "owned child fixture, launched by the cancellation test"]
fn install_worker() {
    let Some(home) = std::env::var_os(HOME) else { return };
    let home = PathBuf::from(home);
    let address = std::fs::read_to_string(home.join("address")).unwrap();
    let mut channel = std::net::TcpStream::connect(address).unwrap();
    channel.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
    std::fs::write(home.join("ready"), std::process::id().to_string()).unwrap();
    for _ in 0..3000 {
        if home.join("stop").is_file() {
            break;
        }
        if std::io::Write::write_all(&mut channel, b".").is_err() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = std::fs::write(home.join("done"), b"stopped");
}

struct OwnedWorker {
    home: tempfile::TempDir,
    stopped: bool,
}
impl Drop for OwnedWorker {
    fn drop(&mut self) {
        let _ = std::fs::write(self.home.path().join("stop"), b"stop");
        if !self.stopped {
            // Baseline failures stop only this owned worker via its fixture gate.
            for _ in 0..100 {
                if self.home.path().join("done").is_file() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

#[tokio::test]
async fn cancelling_an_install_command_stops_its_actual_descendant() {
    let mut owned = OwnedWorker { home: tempfile::tempdir().unwrap(), stopped: false };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    std::fs::write(owned.home.path().join("address"), listener.local_addr().unwrap().to_string()).unwrap();
    let exe = std::env::current_exe().unwrap();
    let worker =
        format!("\"{}\" --ignored --exact install::command_tests::install_worker --nocapture", exe.display());
    // Keep the shell as the parent, including on shells that otherwise exec
    // their final command. Killing the shell alone must not pass this test.
    let command =
        if cfg!(windows) { format!("{worker} & rem owned fixture") } else { format!("{worker}; :") };
    let env = [(HOME.to_string(), owned.home.path().display().to_string())];
    let mut running = Box::pin(super::run_to_completion(&command, owned.home.path(), &env));
    let ready = async {
        let (mut channel, _) = listener.accept().await.unwrap();
        channel.read_exact(&mut [0; 1]).await.unwrap();
        channel
    };
    let mut channel = tokio::select! {
        result = &mut running => panic!("the command must remain running: {result:?}"),
        ready = tokio::time::timeout(Duration::from_secs(30), ready) => ready.expect("the owned descendant did not start")
    };
    drop(running);
    // A stalled worker is not evidence of termination. Its independent TCP
    // channel closes only on worker exit, not when the shell/pipes are dropped.
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        for _ in 0..4096 {
            match channel.read(&mut [0; 1]).await {
                Ok(0) => return true,
                Ok(_) => {}
                Err(error) => {
                    return matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                    );
                }
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    owned.stopped = closed;
    assert!(closed, "the actual installer descendant survived command cancellation");
}

#[tokio::test]
async fn install_commands_keep_normal_success_and_failure_results() {
    let home = tempfile::tempdir().unwrap();
    super::run_to_completion("echo owned install fixture", home.path(), &[]).await.unwrap();
    let failure = super::run_to_completion("exit 7", home.path(), &[]).await.unwrap_err();
    assert!(failure.contains("failed") && failure.contains('7'), "{failure}");
}
