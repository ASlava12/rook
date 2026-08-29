//! Where the CLI reads from.
//!
//! The index allows one writer, and `rookd` holds it while it runs. Rather than
//! telling the user to stop the daemon, a read goes over its API and prints the
//! same thing — the difference should not be visible unless something fails.

use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;

use rook_core::search::Found;
use rook_core::{Rook, SessionSummary, TranscriptEntry, paths};
use rook_proto::Page;
use rook_skills::{SkillCard, SkillDetail};
use rook_store::{ObjectRow, RefRow, StoreStats};

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

    pub fn objects(&self, kind: Option<rook_store::Kind>, limit: usize) -> Result<Vec<ObjectRow>> {
        match self {
            Self::Local(rook) => Ok(rook.store.object_rows(kind, limit)?),
            Self::Daemon(d) => {
                let kind = kind.map(|k| format!("&kind={}", k.as_str())).unwrap_or_default();
                Ok(d.get::<Page<ObjectRow>>(&format!("/api/store/objects?limit={limit}{kind}"))?.items)
            }
        }
    }

    pub fn refs(&self, prefix: &str) -> Result<Vec<RefRow>> {
        match self {
            Self::Local(rook) => Ok(rook.store.ref_rows(prefix)?),
            Self::Daemon(d) => {
                Ok(d.get::<Page<RefRow>>(&format!("/api/store/refs?prefix={}", escaped(prefix)))?.items)
            }
        }
    }

    /// One object's bytes. `max_bytes` is what the caller can take: the daemon
    /// windows a large payload rather than sending all of it, and says so.
    pub fn object(&self, id: &str, max_bytes: usize) -> Result<(Vec<u8>, bool)> {
        match self {
            Self::Local(rook) => {
                let object = rook
                    .store
                    .resolve_prefix(id)?
                    .with_context(|| format!("no object matches {id:?} (or the prefix is ambiguous)"))?;
                Ok((rook.store.get(&object)?, false))
            }
            Self::Daemon(d) => {
                let got: serde_json::Value =
                    d.get(&format!("/api/store/objects/{}?max_bytes={max_bytes}", escaped(id)))?;
                let body = got["body"].as_str().unwrap_or_default().as_bytes().to_vec();
                Ok((body, got["truncated"].as_bool().unwrap_or(false)))
            }
        }
    }

    pub fn sessions(&self) -> Result<Vec<SessionSummary>> {
        match self {
            Self::Local(rook) => Ok(rook.session_summaries()?),
            Self::Daemon(d) => Ok(d.get::<Page<SessionSummary>>("/api/sessions")?.items),
        }
    }

    pub fn catalog(&self, workspace: &std::path::Path) -> Result<Vec<SkillCard>> {
        match self {
            Self::Local(rook) => Ok(rook.catalog()),
            Self::Daemon(d) => {
                Ok(d.get::<Page<SkillCard>>(&format!("/api/skills?workspace={}", here(workspace)))?.items)
            }
        }
    }

    pub fn skill(&self, name: &str, workspace: &std::path::Path) -> Result<SkillDetail> {
        match self {
            Self::Local(rook) => Ok(rook.skills().resolve(name, rook.env())?.detail()),
            Self::Daemon(d) => d.get(&format!("/api/skills/{}?workspace={}", escaped(name), here(workspace))),
        }
    }

    /// Resolve `last`, a prefix, or a whole id, wherever the sessions come from.
    pub fn session_named(&self, spec: &str, workspace: &std::path::Path) -> Result<u128> {
        Ok(rook_core::session_named(spec, workspace, &self.sessions()?)?)
    }

    pub fn search(&self, query: &str, options: &rook_core::search::Search) -> Result<Found> {
        match self {
            Self::Local(rook) => Ok(rook.search(query, options)?),
            Self::Daemon(d) => {
                let mut path = format!("/api/search?q={}&limit={}", escaped(query), options.limit);
                if let Some(session) = options.session {
                    path.push_str(&format!("&session={}", rook_store::format_session_id(session)));
                }
                if options.conversation_only {
                    path.push_str("&conversation=true");
                }
                d.get(&path)
            }
        }
    }

    /// What a session costs in context. The workspace travels with the request:
    /// the window is decided by the model that project configured.
    pub fn context_usage(
        &self,
        session: u128,
        window: Option<usize>,
        workspace: &std::path::Path,
    ) -> Result<rook_core::ContextUsage> {
        match self {
            Self::Local(rook) => Ok(rook.context_usage(session, window)?),
            Self::Daemon(d) => {
                let mut path = format!(
                    "/api/sessions/{}/context?workspace={}",
                    rook_store::format_session_id(session),
                    here(workspace)
                );
                if let Some(window) = window {
                    path.push_str(&format!("&window={window}"));
                }
                d.get(&path)
            }
        }
    }

    pub fn changes(&self, session: u128, with_diff: bool) -> Result<rook_core::changes::Changes> {
        match self {
            Self::Local(rook) => Ok(rook.changes(session, with_diff)?),
            Self::Daemon(d) => d.get(&format!(
                "/api/sessions/{}/changes?diff={with_diff}",
                rook_store::format_session_id(session)
            )),
        }
    }

    /// Every fact the agent holds, unscoped.
    ///
    /// Scoping is left to the caller so that both paths do it with the same
    /// code: a listing that narrowed differently depending on where it read from
    /// would be a difference the user can see, which is the one thing routing is
    /// not allowed to be.
    pub fn memory(&self) -> Result<Vec<rook_core::Fact>> {
        match self {
            Self::Local(rook) => Ok(rook.memory()?.facts.clone()),
            Self::Daemon(d) => Ok(d.get::<Page<rook_core::Fact>>("/api/memory?all=true")?.items),
        }
    }

    pub fn transcript(
        &self,
        session: u128,
        from: u64,
        limit: usize,
        max_body: usize,
    ) -> Result<Vec<TranscriptEntry>> {
        match self {
            Self::Local(rook) => Ok(rook.transcript(session, from, limit, max_body)?),
            Self::Daemon(d) => Ok(d
                .get::<Page<TranscriptEntry>>(&format!(
                    "/api/sessions/{}/transcript?from={from}&limit={limit}&max_body={max_body}",
                    rook_store::format_session_id(session)
                ))?
                .items),
        }
    }
}

/// The project being asked about, as a query value.
fn here(workspace: &std::path::Path) -> String {
    escaped(&workspace.display().to_string())
}

/// A query safe to paste into a url. Written out for the same reason as the one
/// in `rook-tools`: one rule, and the crate that does it properly is a
/// dependency for ten lines.
fn escaped(query: &str) -> String {
    query
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            other => format!("%{other:02X}"),
        })
        .collect()
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
