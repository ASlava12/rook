//! `rookd` — the HTTP backend and web UI.
//!
//! Separate from the CLI binary so a headless machine, a container or an editor
//! integration can run the backend without linking a terminal UI into it. Bound
//! to loopback unless told otherwise: an agent's transcript is the most sensitive
//! thing on a developer's machine, and it should not become reachable by
//! accident.

mod api;
mod chat;
mod web;

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use tokio::sync::RwLock;

use rook_core::Rook;

#[derive(Parser)]
#[command(name = "rookd", version = rook_core::AGENT_VERSION, about = "Rook backend and web UI")]
struct Args {
    #[arg(long)]
    port: Option<u16>,
    #[arg(long)]
    bind: Option<String>,
    #[arg(long, short = 'C')]
    workspace: Option<std::path::PathBuf>,
    /// Serve on a non-loopback address. Requires an explicit --bind.
    #[arg(long)]
    allow_remote: bool,
}

pub struct AppState {
    /// An `Arc` so a websocket turn can take an owned read guard and outlive the
    /// request that spawned it.
    pub rook: Arc<RwLock<Rook>>,
    pub started: std::time::Instant,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let rook = Rook::open(args.workspace).context("opening the store")?;
    let config = rook.config.clone();
    rook_core::telemetry::init(&config.telemetry);
    let port = args.port.unwrap_or(config.server.port);
    let bind: IpAddr =
        args.bind.unwrap_or(config.server.bind.clone()).parse().context("--bind must be an IP address")?;

    if !bind.is_loopback() && !(args.allow_remote || config.server.allow_remote) {
        anyhow::bail!(
            "refusing to bind {bind}: it is not loopback. Pass --allow-remote if you really mean \
             to expose the agent's transcripts on the network, and put an authenticating proxy \
             in front of it."
        );
    }

    let state = Arc::new(AppState { rook: Arc::new(RwLock::new(rook)), started: std::time::Instant::now() });
    let app = api::router(state.clone()).merge(web::router());

    let maintenance = tokio::spawn(maintain(state.clone(), config.storage.maintenance_interval_hours));

    let addr = SocketAddr::new(bind, port);
    let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| format!("binding {addr}"))?;
    // Where it landed, not where it asked: `--port 0` means the OS chooses, and
    // an address file naming port 0 is one nothing can reach.
    let addr = listener.local_addr().unwrap_or(addr);
    tracing::info!("rookd listening on http://{addr}");
    let address_file = rook_core::paths::daemon_address_file();
    std::fs::write(&address_file, format!("http://{addr}")).ok();
    println!("rook web UI:  http://{addr}");
    println!("rook API:     http://{addr}/api/health");

    axum::serve(listener, app).with_graceful_shutdown(shutdown()).await.context("serving")?;
    maintenance.abort();
    std::fs::remove_file(&address_file).ok();
    Ok(())
}

/// Prune, collect and enforce the size budget on a schedule, because a daemon
/// left running is exactly where an unbounded store grows unnoticed.
///
/// Takes the write lock for as long as it runs, so a turn started meanwhile
/// waits. That is the single-writer store showing through, and it is why this
/// is hourly at its most frequent rather than continuous.
async fn maintain(state: Arc<AppState>, every_hours: u32) {
    let period = Duration::from_secs(u64::from(every_hours.max(1)) * 3600);
    loop {
        match state.rook.write().await.maintenance(false) {
            Ok(report) => tracing::info!(
                sessions = report.prune.sessions_deleted,
                collected = report.gc.collected,
                freed = report.gc.bytes_freed,
                over_budget_by = report.over_budget_by,
                "maintenance"
            ),
            Err(e) => tracing::warn!("maintenance failed: {e}"),
        }
        tokio::time::sleep(period).await;
    }
}

/// Both signals, because the address file and the store lock are released on
/// the way out and a service manager sends SIGTERM, not SIGINT.
async fn shutdown() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}
