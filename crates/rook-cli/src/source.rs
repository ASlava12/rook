//! Where the CLI reads from.
//!
//! The index allows one writer, and `rookd` holds it while it runs. Rather than
//! telling the user to stop the daemon, a read goes over its API and prints the
//! same thing — the difference should not be visible unless something fails.

use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;

use rook_core::{Rook, SessionSummary, paths};
use rook_proto::Page;
use rook_skills::SkillCard;
use rook_store::StoreStats;

pub enum Source {
    Local(Box<Rook>),
    Daemon(Daemon),
}

impl Source {
    /// Local unless the store is locked and a daemon is reachable, which is the
    /// only case the fallback is for: any other failure is the user's to see.
    pub fn open(workspace: Option<std::path::PathBuf>) -> Result<Self> {
        match Rook::open(workspace) {
            Ok(rook) => Ok(Self::Local(Box::new(rook))),
            Err(e) if is_locked(&e) => match Daemon::running() {
                Some(daemon) => {
                    // On stderr, so `--json` output stays machine-readable, and
                    // said at all because the answer may be a moment stale.
                    eprintln!("using the running rookd at {}", daemon.base);
                    Ok(Self::Daemon(daemon))
                }
                None => Err(e.into()),
            },
            Err(e) => Err(e.into()),
        }
    }

    /// For the commands that write, which the daemon does not expose.
    pub fn local(&self) -> Result<&Rook> {
        match self {
            Self::Local(rook) => Ok(rook),
            Self::Daemon(daemon) => bail!(
                "`rookd` is running at {} and holds the store's single write lock. \
                 Stop it to run this command.",
                daemon.base
            ),
        }
    }

    pub fn stats(&self) -> Result<StoreStats> {
        match self {
            Self::Local(rook) => Ok(rook.stats()?),
            Self::Daemon(d) => d.get("/api/store/stats"),
        }
    }

    pub fn sessions(&self) -> Result<Vec<SessionSummary>> {
        match self {
            Self::Local(rook) => Ok(rook.session_summaries()?),
            Self::Daemon(d) => Ok(d.get::<Page<SessionSummary>>("/api/sessions")?.items),
        }
    }

    pub fn catalog(&self) -> Result<Vec<SkillCard>> {
        match self {
            Self::Local(rook) => Ok(rook.catalog()),
            Self::Daemon(d) => Ok(d.get::<Page<SkillCard>>("/api/skills")?.items),
        }
    }
}

fn is_locked(e: &rook_core::CoreError) -> bool {
    matches!(e, rook_core::CoreError::Store(rook_store::StoreError::Locked { .. }))
}

pub struct Daemon {
    base: String,
    runtime: tokio::runtime::Runtime,
    http: reqwest::Client,
}

impl Daemon {
    /// The address `rookd` wrote when it started, if something still answers
    /// there: a file left behind by a crash must not send every command into a
    /// connection error.
    pub fn running() -> Option<Self> {
        Self::at(&paths::daemon_address_file())
    }

    fn at(address_file: &std::path::Path) -> Option<Self> {
        let base = std::fs::read_to_string(address_file).ok()?.trim().to_string();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;
        // reqwest panics on build without one, even for plain HTTP to loopback.
        rook_llm::init_tls();
        let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(10)).build().ok()?;
        let daemon = Self { base, runtime, http };
        daemon.get::<serde_json::Value>("/api/health").ok()?;
        Some(daemon)
    }

    fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = format!("{}{path}", self.base);
        self.runtime.block_on(async {
            let response = self.http.get(&url).send().await.with_context(|| format!("GET {url}"))?;
            let status = response.status();
            let body = response.text().await.with_context(|| format!("reading {url}"))?;
            if !status.is_success() {
                bail!("{url} answered {status}: {body}");
            }
            serde_json::from_str(&body).with_context(|| format!("decoding {url}"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address_file(dir: &tempfile::TempDir, contents: Option<&str>) -> std::path::PathBuf {
        let path = dir.path().join("rookd.addr");
        if let Some(contents) = contents {
            std::fs::write(&path, contents).unwrap();
        }
        path
    }

    #[test]
    fn no_address_file_means_no_daemon() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Daemon::at(&address_file(&dir, None)).is_none());
    }

    #[test]
    fn an_address_nothing_answers_is_not_a_daemon() {
        // Port 1 is reserved and never listening. A file left behind by a crash
        // must not turn every command into a connection error.
        let dir = tempfile::tempdir().unwrap();
        assert!(Daemon::at(&address_file(&dir, Some("http://127.0.0.1:1"))).is_none());
    }

    #[test]
    fn an_address_that_is_not_a_url_is_not_a_daemon() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Daemon::at(&address_file(&dir, Some("nonsense"))).is_none());
    }
}
