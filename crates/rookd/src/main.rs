//! `rookd` — the HTTP backend and web UI.
//!
//! Separate from the CLI binary so a headless machine, a container or an editor
//! integration can run the backend without linking a terminal UI into it. Bound
//! to loopback unless told otherwise: an agent's transcript is the most sensitive
//! thing on a developer's machine, and it should not become reachable by
//! accident.

mod api;
mod web;

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

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
    pub rook: RwLock<Rook>,
    pub started: std::time::Instant,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("ROOK_LOG").unwrap_or_else(|_| "info".into()))
        .init();

    let rook = Rook::open(args.workspace).context("opening the store")?;
    let config = rook.config.clone();
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

    let state = Arc::new(AppState { rook: RwLock::new(rook), started: std::time::Instant::now() });
    let app = api::router(state.clone()).merge(web::router());

    let addr = SocketAddr::new(bind, port);
    let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| format!("binding {addr}"))?;
    tracing::info!("rookd listening on http://{addr}");
    println!("rook web UI:  http://{addr}");
    println!("rook API:     http://{addr}/api/health");

    axum::serve(listener, app).with_graceful_shutdown(shutdown()).await.context("serving")?;
    Ok(())
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}
