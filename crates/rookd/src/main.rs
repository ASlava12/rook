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
use tokio::sync::{OnceCell, RwLock};

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
    /// One engine per project, all sharing the store this daemon holds.
    ///
    /// A workspace is per project and the store takes one writer, so binding the
    /// two together made a second project a second process — and the second
    /// process the one that could not open the store. Kept apart here: several
    /// projects run at once against one history, one memory and one search.
    pub elsewhere: RwLock<std::collections::HashMap<std::path::PathBuf, Project>>,
    /// Per project, and outliving every connection to it: see [`chat::Shared`].
    equipment: RwLock<std::collections::HashMap<std::path::PathBuf, Arc<OnceCell<chat::Shared>>>>,
    pub started: std::time::Instant,
    /// Read once at startup rather than through the lock on every request: none
    /// of it changes while the process runs, and `/api/health` must be able to
    /// answer while a turn holds the store — a liveness check that waits reports
    /// a working daemon as a dead one.
    pub about: About,
    /// The ceiling on `elsewhere`, from `[server] max_projects`.
    pub max_projects: usize,
    /// When `config.toml` was last read. The daemon read it once at start, so
    /// changing `[agent] model` — the setting people change most — took a
    /// restart, and the restart was something a person had to be told to do
    /// rather than something that happened.
    config_read: std::sync::Mutex<Option<std::time::SystemTime>>,
    /// Resolved once, with the rest of what is read at startup: where the file
    /// is does not change while the process runs.
    pub config_path: std::path::PathBuf,
    /// When this process started, against which the binary's own timestamp is
    /// read: an upgrade leaves the running daemon on the old code, and until
    /// something says so the only symptom is a fix that did not take.
    pub started_at: std::time::SystemTime,
    /// Turns in flight, so stopping can say what it would interrupt.
    turns: std::sync::atomic::AtomicU32,
    /// Asked to stop over the API rather than by a signal. Stopping a daemon
    /// meant finding its process id, which is not something a person should
    /// have to do to a program they started by opening a window.
    pub stopping: tokio::sync::Notify,
}

/// A turn in flight. Counted for as long as this lives, which is the turn's
/// task — so a cancelled turn stops counting when its future is dropped,
/// rather than leaving a daemon that believes it is forever busy.
pub struct Running(Arc<AppState>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.turns.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

impl AppState {
    /// The engine for `workspace`, built once and kept.
    ///
    /// `None` is the daemon's own, which is what a client naming no project
    /// gets. A path that is not a directory is refused rather than created: the
    /// name arrives from a request, and a typo should not quietly become an
    /// empty project.
    /// Re-read the configuration if the file has changed, before a turn uses
    /// it. Says what changed, or nothing when nothing did.
    ///
    /// Asked once per turn rather than watched: a turn is when it matters, and
    /// a watcher is a thread and a dependency for a question that costs one
    /// `stat`.
    pub fn turn_started(self: &Arc<Self>) -> Running {
        self.turns.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Running(self.clone())
    }

    pub fn turns_running(&self) -> u32 {
        self.turns.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Whether the `rookd` on disk is not the one this process is running.
    /// Read at the question rather than at startup: the replacement happens
    /// while it runs, which is the whole point of asking.
    pub fn binary_replaced(&self) -> bool {
        replaced_since(self.started_at)
    }

    pub async fn config_if_changed(&self) -> Option<String> {
        let touched = std::fs::metadata(&self.config_path).and_then(|m| m.modified()).ok();
        {
            let mut last = self.config_read.lock().unwrap_or_else(|e| e.into_inner());
            if *last == touched {
                return None;
            }
            *last = touched;
        }
        let Ok(config) = rook_core::Config::load_from(self.config_path.clone()) else { return None };
        let model = config.agent.model.clone();
        let was = {
            let mut rook = self.rook.write().await;
            let was = rook.config.agent.model.clone();
            rook.config = config.clone();
            was
        };
        // Every project, because they share the file as they share the store.
        for project in self.elsewhere.write().await.values() {
            project.engine.write().await.config = config.clone();
        }
        (was != model).then(|| format!("the model is `{model}` now, from config.toml"))
    }

    pub async fn engine_for(
        &self,
        workspace: Option<&std::path::Path>,
    ) -> std::result::Result<Arc<RwLock<Rook>>, String> {
        let Some(asked) = workspace else { return Ok(self.rook.clone()) };
        let here = asked.canonicalize().map_err(|e| format!("{}: {e}", asked.display()))?;
        if !here.is_dir() {
            return Err(format!("{} is not a directory", here.display()));
        }
        // Naming the daemon's own project has to reach the daemon's own engine.
        // A second one for the same directory is a second registry of who is
        // writing what, and two agents that cannot see each other's claims are
        // exactly what the claims exist to prevent.
        if self.rook.read().await.workspace.canonicalize().is_ok_and(|own| own == here) {
            return Ok(self.rook.clone());
        }
        let mut kept = self.elsewhere.write().await;
        if let Some(known) = kept.get_mut(&here) {
            known.last_used = std::time::Instant::now();
            return Ok(known.engine.clone());
        }

        let built = Arc::new(RwLock::new(self.rook.read().await.for_workspace(here.clone())));
        kept.insert(here, Project { engine: built.clone(), last_used: std::time::Instant::now() });
        // How many projects there are is decided by whoever connects. Dropping
        // the one nobody has asked for in longest costs a rediscovery of its
        // skills; a connection still holding it keeps working either way.
        while kept.len() > self.max_projects {
            let Some(stale) = kept.iter().min_by_key(|(_, p)| p.last_used).map(|(path, _)| path.clone())
            else {
                break;
            };
            kept.remove(&stale);
            // The project is gone, so its servers and its background commands
            // go with it: keeping them alive would be keeping processes for a
            // workspace nothing can reach any more.
            self.equipment.write().await.remove(&stale);
        }
        Ok(built)
    }

    /// The shared equipment for whatever project `engine` is, made on first ask.
    ///
    /// Keyed by workspace rather than held on the connection: two browser tabs
    /// on one project are one set of language servers, one MCP session and one
    /// list of background commands, and closing a tab is not the end of any of
    /// them.
    pub async fn equipment_for(&self, engine: &Arc<RwLock<Rook>>) -> Arc<OnceCell<chat::Shared>> {
        let workspace = engine.read().await.workspace.clone();
        if let Some(kept) = self.equipment.read().await.get(&workspace) {
            return kept.clone();
        }
        self.equipment.write().await.entry(workspace).or_default().clone()
    }
}

/// An engine the daemon is keeping, and when it was last wanted.
pub struct Project {
    pub engine: Arc<RwLock<Rook>>,
    last_used: std::time::Instant,
}

pub struct About {
    pub store_root: String,
    pub workspace: String,
    pub os: String,
    pub arch: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    rook_contain::launcher_entry();
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

    let about = About {
        store_root: rook.store.root().display().to_string(),
        workspace: rook.workspace.display().to_string(),
        os: rook.env().os.clone(),
        arch: rook.env().arch.clone(),
    };
    let state = Arc::new(AppState {
        rook: Arc::new(RwLock::new(rook)),
        elsewhere: RwLock::new(std::collections::HashMap::new()),
        equipment: RwLock::new(std::collections::HashMap::new()),
        max_projects: config.server.max_projects.max(1),
        // Read at start, so the first turn compares against this rather than
        // reloading a file nothing has touched.
        config_read: std::sync::Mutex::new(
            std::fs::metadata(rook_core::paths::config_file()).and_then(|m| m.modified()).ok(),
        ),
        config_path: rook_core::paths::config_file(),
        started_at: std::time::SystemTime::now(),
        turns: std::sync::atomic::AtomicU32::new(0),
        stopping: tokio::sync::Notify::new(),
        started: std::time::Instant::now(),
        about,
    });
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

    axum::serve(listener, app).with_graceful_shutdown(shutdown(state.clone())).await.context("serving")?;
    maintenance.abort();
    std::fs::remove_file(&address_file).ok();
    Ok(())
}

/// Prune, collect and enforce the size budget on a schedule, because a daemon
/// left running is exactly where an unbounded store grows unnoticed.
///
/// It needs the store to itself, which is the single-writer store showing
/// through, and it is why this is hourly at its most frequent rather than
/// continuous. It asks rather than waits: this lock is fair, so a writer that
/// queues behind a turn puts every reader behind it too — and a turn holds its
/// read guard for as long as the turn runs, which is minutes. Waiting for the
/// lock would make the whole daemon, `/api/health` included, unanswerable for
/// that long. Postponing the work by a minute costs nothing by comparison.
async fn maintain(state: Arc<AppState>, every_hours: u32) {
    let period = Duration::from_secs(u64::from(every_hours.max(1)) * 3600);
    const WHEN_BUSY: Duration = Duration::from_secs(60);
    let mut waiting = period;
    loop {
        tokio::time::sleep(waiting).await;
        let Ok(rook) = state.rook.clone().try_write_owned() else {
            tracing::debug!("maintenance postponed: a turn is holding the store");
            waiting = WHEN_BUSY;
            continue;
        };
        // As in the handler: off the runtime, so the hours-long timer does not
        // become a stall the moment it fires.
        let done = tokio::task::spawn_blocking(move || rook.maintenance(false)).await;
        match done.unwrap_or_else(|e| Err(rook_core::CoreError::Other(e.to_string()))) {
            Ok(report) => tracing::info!(
                sessions = report.prune.sessions_deleted,
                collected = report.gc.collected,
                freed = report.gc.bytes_freed,
                over_budget_by = report.over_budget_by,
                "maintenance"
            ),
            Err(e) => tracing::warn!("maintenance failed: {e}"),
        }
        waiting = period;
    }
}

/// Whether the binary running this process has been written since `started`.
pub fn replaced_since(started: std::time::SystemTime) -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    std::fs::metadata(exe).and_then(|m| m.modified()).is_ok_and(|built| built > started)
}

/// Both signals, because the address file and the store lock are released on
/// the way out and a service manager sends SIGTERM, not SIGINT — and the API,
/// because `rook daemon stop` should not mean going to find a process id.
async fn shutdown(state: Arc<AppState>) {
    let asked = async { state.stopping.notified().await };
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
            _ = asked => {}
        }
    }
    #[cfg(not(unix))]
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = asked => {}
    }
    tracing::info!("shutting down");
    // A turn holds its websocket open and a graceful shutdown waits for every
    // connection, so a stop that was asked for while one runs would hang
    // instead of stopping. Whoever asked was told what was running and said so
    // anyway; the store is crash-safe by design, and the address file is the
    // one thing that would be left behind.
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        tracing::warn!("a connection is still open five seconds on; exiting anyway");
        std::fs::remove_file(rook_core::paths::daemon_address_file()).ok();
        std::process::exit(0);
    });
}
