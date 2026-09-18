//! Daemon lifecycle and protocol entry points.

use crate::args::DaemonCmd;
use crate::{fmt, source::Source};
use anyhow::{Result, bail};
use rook_core::Rook;
use std::path::PathBuf;

pub(crate) fn cmd_acp(workspace: Option<PathBuf>) -> Result<()> {
    // stdout carries the protocol, so logs must not: a stray line there is an
    // unparsable message to the editor.
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async move {
        let rook = Rook::open(workspace)?;
        rook_acp::serve_stdio(rook).await?;
        anyhow::Ok(())
    })
}

pub(crate) fn cmd_serve(port: Option<u16>) -> Result<()> {
    let mut config = rook_core::Config::load()?;
    if let Some(p) = port {
        config.server.port = p;
    }
    bail!(
        "the daemon lives in its own binary: run `rookd --port {}`.\n\
         It is separate so an editor integration or a headless box can run the backend \
         without pulling in the TUI.",
        config.server.port
    )
}

/// The daemon is started by opening a window and stopped by killing a process
/// somebody had to go and find. Both halves belong to the program that starts
/// it.
pub(crate) fn cmd_daemon(cmd: DaemonCmd, json: bool) -> Result<()> {
    let running = crate::source::Daemon::running();
    match (cmd, running) {
        (DaemonCmd::Status, None) => {
            if json {
                println!("{}", serde_json::json!({ "running": false }));
            } else {
                println!("nothing is answering — `rook daemon start`, or open `rook tui`");
            }
        }
        (DaemonCmd::Status, Some(daemon)) => {
            let health = daemon.health()?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &serde_json::json!({ "running": true, "base": daemon.base, "health": health })
                    )?
                );
                return Ok(());
            }
            println!("running at {}", daemon.base);
            println!(
                "rook {} · api {} · started {}",
                health.version,
                health.api_version,
                fmt::ago(rook_store::now_unix() - health.uptime_secs as i64)
            );
            println!("store      {}", health.store_root);
            // With how long, because the count alone cannot tell a turn that
            // is thinking from one that has stopped, and both are what a
            // person is looking at this line to find out.
            match health.busy_for_secs {
                Some(secs) => println!(
                    "turns      {} · the oldest has been running {}",
                    health.turns_running,
                    fmt::elapsed(std::time::Duration::from_secs(secs))
                ),
                None => println!("turns      {}", health.turns_running),
            }
            if daemon.replaced {
                println!(
                    "\nthe `rookd` on disk was installed after this one started: it is running the \n\
                     previous build. `rook daemon restart` picks up the installed one, on the same \n\
                     port, so an open window keeps working."
                );
            }
        }
        (DaemonCmd::Start, Some(daemon)) => println!("already running at {}", daemon.base),
        (DaemonCmd::Start, None) => println!("started, at {}", Source::start_a_daemon(None)?),
        (DaemonCmd::Stop { .. }, None) => println!("nothing is answering"),
        (DaemonCmd::Stop { force }, Some(daemon)) => {
            let interrupted = daemon.stop(force)?;
            println!("stopping{}", ended(interrupted));
        }
        (DaemonCmd::Restart { force }, running) => {
            // The port it was on, so the windows already attached — which read
            // the address once, when they attached — find the new one where
            // they are still looking.
            let mut was_on = None;
            if let Some(daemon) = running {
                was_on = daemon.base.rsplit(':').next().and_then(|port| port.parse::<u16>().ok());
                let interrupted = daemon.stop(force)?;
                println!("stopping{}", ended(interrupted));
                // The address file, not the health check: it stops answering
                // the moment it begins shutting down and goes on holding the
                // store for a moment after that, so a daemon started on the
                // answer would fail on the lock rather than the port. The file
                // is removed once the lock is released.
                let address_file = rook_core::paths::daemon_address_file();
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                while address_file.exists() {
                    if std::time::Instant::now() > deadline {
                        bail!("it is still holding the store ten seconds on; stop it by hand");
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
            // Falling back rather than failing: the port may be taken by now,
            // and a daemon somewhere else is better than none. It says so,
            // because that is when a window has to be reopened.
            match was_on.map(|port| Source::start_a_daemon(Some(port))) {
                Some(Ok(base)) => println!("started, at {base} — the same address as before"),
                Some(Err(_)) | None => {
                    let base = Source::start_a_daemon(None)?;
                    println!("started, at {base} — a new address, so reopen any window that was attached");
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn ended(turns: u32) -> String {
    match turns {
        0 => String::new(),
        n => format!(" — {n} turn(s) ended where they were"),
    }
}
