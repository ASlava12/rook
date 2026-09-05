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
    /// Shared rather than owned: the TUI hands the same engine to every turn
    /// it runs, and a turn outlives the call that started it.
    Local(std::sync::Arc<Rook>),
    Daemon(Daemon),
}

impl Source {
    /// A source for a window that expects other windows.
    ///
    /// The store takes one writer and a `tui` that takes it serves nobody, so
    /// the second window was an error message however many projects somebody
    /// works in. This makes sure there is a daemon — starting one if there is
    /// none — and works through it, which makes every window the same kind of
    /// client and takes nothing from the others when one is closed.
    ///
    /// Falling back to the store itself where no daemon can be had: one window
    /// with no `rookd` beside it is still the ordinary case on a machine where
    /// the binary was installed alone.
    pub fn shared(workspace: Option<std::path::PathBuf>) -> Result<(Self, Option<String>)> {
        if let Some(mut daemon) = Daemon::running() {
            daemon.workspace = asked_about(workspace.clone());
            // An upgrade leaves the running daemon on the old code, and every
            // window keeps working — at the previous version. Said on the way
            // in, because the alternative is finding out from a fix that did
            // not take.
            let note = daemon.replaced.then(|| {
                format!(
                    "the rookd at {} started before this build was installed —                      `rook daemon restart` runs the new one",
                    daemon.base
                )
            });
            return Ok((Self::Daemon(daemon), note));
        }
        let Some((mut child, started)) = start_daemon() else {
            return Ok((Self::open(workspace)?, None));
        };
        if came_up(&mut child)
            && let Some(mut daemon) = Daemon::running()
        {
            daemon.workspace = asked_about(workspace.clone());
            return Ok((Self::Daemon(daemon), Some(started)));
        }
        let _ = child.kill();
        // It did not come up. The direct path says what is actually wrong, and
        // a window that opens is better than a window that explains.
        Ok((Self::open(workspace)?, None))
    }

    /// Start one and wait for it, for `rook daemon start` and the restart that
    /// is a stop and this.
    pub fn start_a_daemon() -> Result<String> {
        let Some((mut child, beside)) = start_daemon() else {
            bail!("no `rookd` next to this binary — install the two together")
        };
        if !came_up(&mut child) {
            let _ = child.kill();
            bail!("`{beside}` did not come up; what it says is in {}", paths::logs_dir().display());
        }
        Daemon::running().map(|d| d.base).context("it answered and then stopped")
    }

    /// Local unless the store is locked and a daemon is reachable, which is the
    /// only case the fallback is for: any other failure is the user's to see.
    pub fn open(workspace: Option<std::path::PathBuf>) -> Result<Self> {
        let here = asked_about(workspace.clone());
        match Rook::open(workspace) {
            Ok(rook) => Ok(Self::Local(std::sync::Arc::new(rook))),
            Err(e) if is_locked(&e) => match Daemon::running() {
                Some(mut daemon) => {
                    daemon.workspace = here;
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

    /// The workspace being worked in, which both kinds know: a routed source
    /// was opened against a path even though it does not hold the store.
    pub fn workspace(&self) -> &std::path::Path {
        match self {
            Self::Local(rook) => &rook.workspace,
            Self::Daemon(daemon) => &daemon.workspace,
        }
    }

    /// Where the daemon answers, when one is holding the store: the address a
    /// second window opens its chat socket to.
    pub fn daemon_base(&self) -> Option<&str> {
        match self {
            Self::Local(_) => None,
            Self::Daemon(daemon) => Some(&daemon.base),
        }
    }

    /// The engine in this process, when the store is held here — and `None`
    /// when a daemon holds it, which is what a front end has to branch on
    /// before it can run a turn.
    pub fn here(&self) -> Option<&std::sync::Arc<Rook>> {
        match self {
            Self::Local(rook) => Some(rook),
            Self::Daemon(_) => None,
        }
    }

    /// The engine itself, when there is one here.
    ///
    /// No command needs this to do its work any more — every one of them
    /// routes. What is left is the detail `store maintain` prints beside the
    /// report, which counts sessions in a store this process may not have:
    /// routed, it says the number and stops rather than guessing, which is the
    /// difference between a shorter answer and a wrong one.
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
    /// The writes the daemon's API already serves. Routed for the same reason
    /// the reads are: the store takes one writer, and a person who has `rookd`
    /// running should not have to stop it to set a goal.
    ///
    /// Each is the same call the daemon would make on its own store, so there
    /// is one implementation and two ways in — which is what keeps the direct
    /// path and the routed one from drifting.
    /// Reading a goal has no endpoint of its own: it is a field of the session
    /// the listing already carries, which is one round trip rather than two
    /// and one thing to keep in step rather than two.
    pub fn goal(&self, session: u128) -> Result<Option<String>> {
        match self {
            Source::Local(rook) => Ok(rook.goal(session)?),
            Source::Daemon(_) => {
                Ok(self.sessions()?.into_iter().find(|s| s.meta.id == session).and_then(|s| s.goal))
            }
        }
    }

    pub fn set_goal(&self, session: u128, goal: &str) -> Result<()> {
        match self {
            Source::Local(rook) => Ok(rook.set_goal(session, goal)?),
            Source::Daemon(daemon) => {
                let id = rook_store::format_session_id(session);
                daemon
                    .post::<serde_json::Value>(
                        &format!("/api/sessions/{id}/goal"),
                        &serde_json::json!({ "goal": goal }),
                    )
                    .map(|_| ())
            }
        }
    }

    pub fn rewind(&self, session: u128, to_seq: u64, restore_files: bool) -> Result<rook_core::Rewind> {
        match self {
            Source::Local(rook) => Ok(rook.rewind(session, to_seq, restore_files)?),
            Source::Daemon(daemon) => {
                let id = rook_store::format_session_id(session);
                daemon.post(
                    &format!("/api/sessions/{id}/rewind"),
                    &serde_json::json!({ "to_seq": to_seq, "restore_files": restore_files }),
                )
            }
        }
    }

    pub fn forget(&self, id: &str) -> Result<Option<rook_core::Fact>> {
        match self {
            Source::Local(rook) => Ok(rook.forget(id, Some("removed from the command line".into()))?),
            // The endpoint answers `{"forgot": …}` and 404s for a fact that
            // was not there; both are the same two answers the direct call
            // gives, said differently.
            Source::Daemon(daemon) => {
                match daemon.post::<serde_json::Value>("/api/memory", &serde_json::json!({ "id": id })) {
                    Ok(said) => Ok(serde_json::from_value(said["forgot"].clone()).ok()),
                    Err(e) if e.to_string().contains("404") => Ok(None),
                    Err(e) => Err(e),
                }
            }
        }
    }

    pub fn maintenance(&self, dry_run: bool) -> Result<rook_core::MaintenanceReport> {
        match self {
            Source::Local(rook) => Ok(rook.maintenance(dry_run)?),
            Source::Daemon(daemon) => {
                daemon.post("/api/maintenance", &serde_json::json!({ "dry_run": dry_run }))
            }
        }
    }

    pub fn memory(&self) -> Result<Vec<rook_core::Fact>> {
        match self {
            Self::Local(rook) => Ok(rook.memory()?.facts.clone()),
            Self::Daemon(d) => Ok(d.get::<Page<rook_core::Fact>>("/api/memory?all=true")?.items),
        }
    }

    /// Scored search, which is `recall`'s question asked differently — so the
    /// routed call is the same core function and not the listing endpoint
    /// filtered down.
    pub fn memory_search(
        &self,
        query: &str,
        workspace: &std::path::Path,
    ) -> Result<Vec<rook_core::memory::Hit>> {
        match self {
            Self::Local(rook) => Ok(rook.memory_search(query)?),
            Self::Daemon(d) => Ok(d
                .get::<Page<rook_core::memory::Hit>>(&format!(
                    "/api/memory/search?q={}&workspace={}",
                    escaped(query),
                    here(workspace)
                ))?
                .items),
        }
    }

    pub fn remember(
        &self,
        fact: rook_core::Fact,
        workspace: &std::path::Path,
    ) -> Result<rook_core::memory::Learned> {
        match self {
            Self::Local(rook) => Ok(rook.remember(fact, Some("added from the command line".into()))?),
            // The scope travels as the workspace it is scoped to, because the
            // daemon's own project is not necessarily the one being asked
            // about — a fact filed against the wrong one is not a smaller
            // answer but a different fact.
            Self::Daemon(d) => {
                let said: serde_json::Value = d.post(
                    "/api/memory/add",
                    &serde_json::json!({
                        "text": fact.text,
                        "tags": fact.tags,
                        "global": fact.scope == rook_core::Scope::Global,
                        "pinned": fact.pinned,
                        "workspace": workspace,
                    }),
                )?;
                Ok(serde_json::from_value(said["learned"].clone())?)
            }
        }
    }

    pub fn memory_history(&self) -> Result<Vec<rook_core::MemoryVersion>> {
        match self {
            Self::Local(rook) => Ok(rook.memory_history()?),
            Self::Daemon(d) => Ok(d.get::<Page<rook_core::MemoryVersion>>("/api/memory/history")?.items),
        }
    }

    pub fn memory_diff(&self, a: &str, b: &str) -> Result<Vec<(rook_core::memory::Change, rook_core::Fact)>> {
        match self {
            Self::Local(rook) => Ok(rook.memory_diff(&rook.object_named(a)?, &rook.object_named(b)?)?),
            Self::Daemon(d) => {
                Ok(d.get::<Page<_>>(&format!("/api/memory/diff?a={}&b={}", escaped(a), escaped(b)))?.items)
            }
        }
    }

    pub fn memory_since(&self, days: i64) -> Result<Vec<(rook_core::memory::Change, rook_core::Fact)>> {
        match self {
            Self::Local(rook) => Ok(rook.memory_since(rook_store::now_unix() - days * 86_400)?),
            Self::Daemon(d) => Ok(d.get::<Page<_>>(&format!("/api/memory/since?days={days}"))?.items),
        }
    }

    pub fn why_skill(&self, name: &str, workspace: &std::path::Path) -> Result<rook_core::SkillWhy> {
        match self {
            Self::Local(rook) => Ok(rook.why_skill(name)?),
            Self::Daemon(d) => {
                d.get(&format!("/api/skills/{}/why?workspace={}", escaped(name), here(workspace)))
            }
        }
    }

    /// What the sources offer, and what could not be reached. The errors come
    /// back with the list because a source that is down and a source with
    /// nothing to offer are the same short list otherwise.
    pub fn skills_offered(
        &self,
        query: &str,
        refresh: bool,
    ) -> Result<(Vec<rook_core::catalog::Offered>, Vec<String>)> {
        match self {
            Self::Local(rook) => Ok(rook.skills_offered(query, refresh)),
            Self::Daemon(d) => {
                let said: serde_json::Value =
                    d.get(&format!("/api/skills/offered?q={}&refresh={refresh}", escaped(query)))?;
                Ok((
                    serde_json::from_value(said["items"].clone())?,
                    serde_json::from_value(said["errors"].clone())?,
                ))
            }
        }
    }

    pub fn skill_history(&self, name: &str) -> Result<Vec<rook_core::SkillVersionRecord>> {
        match self {
            Self::Local(rook) => Ok(rook.skill_history(name)?),
            Self::Daemon(d) => Ok(d
                .get::<Page<rook_core::SkillVersionRecord>>(&format!(
                    "/api/skills/{}/history",
                    escaped(name)
                ))?
                .items),
        }
    }

    pub fn skill_diff(&self, a: &str, b: &str) -> Result<Vec<(String, rook_core::fileset::Change)>> {
        match self {
            Self::Local(rook) => Ok(rook.skill_diff(&rook.object_named(a)?, &rook.object_named(b)?)?),
            Self::Daemon(d) => {
                Ok(d.get::<Page<_>>(&format!("/api/skills/diff?a={}&b={}", escaped(a), escaped(b)))?.items)
            }
        }
    }

    pub fn install_skill(&self, name: &str) -> Result<std::path::PathBuf> {
        match self {
            Self::Local(rook) => Ok(rook.install_skill(name)?),
            Self::Daemon(d) => {
                let said: serde_json::Value =
                    d.post("/api/skills/install", &serde_json::json!({ "name": name }))?;
                Ok(serde_json::from_value(said["path"].clone())?)
            }
        }
    }

    pub fn new_skill(&self, name: &str, description: &str) -> Result<std::path::PathBuf> {
        match self {
            Self::Local(rook) => Ok(rook.new_skill(name, description)?),
            Self::Daemon(d) => {
                let said: serde_json::Value = d.post(
                    "/api/skills/new",
                    &serde_json::json!({ "name": name, "description": description }),
                )?;
                Ok(serde_json::from_value(said["dir"].clone())?)
            }
        }
    }

    pub fn capture_skill(
        &self,
        name: &str,
        message: Option<String>,
    ) -> Result<(rook_core::fileset::FileSet, String)> {
        match self {
            Self::Local(rook) => {
                let (set, id) = rook.capture_skill(name, message)?;
                Ok((set, id.to_hex()))
            }
            Self::Daemon(d) => {
                let said: serde_json::Value = d.post(
                    &format!("/api/skills/{}/capture", escaped(name)),
                    &serde_json::json!({ "message": message }),
                )?;
                Ok((
                    serde_json::from_value(said["set"].clone())?,
                    serde_json::from_value(said["object"].clone())?,
                ))
            }
        }
    }

    pub fn rollback_skill(&self, name: &str, object: &str) -> Result<rook_core::Rollback> {
        match self {
            Self::Local(rook) => Ok(rook.rollback_skill(name, &rook.object_named(object)?)?),
            Self::Daemon(d) => d.post(
                &format!("/api/skills/{}/rollback", escaped(name)),
                &serde_json::json!({ "object": object }),
            ),
        }
    }

    pub fn collect_garbage(&self, dry_run: bool) -> Result<rook_store::GcReport> {
        match self {
            Self::Local(rook) => Ok(rook.collect_garbage(dry_run)?),
            Self::Daemon(d) => d.post("/api/store/gc", &serde_json::json!({ "dry_run": dry_run })),
        }
    }

    pub fn prune(&self, dry_run: bool) -> Result<rook_store::PruneReport> {
        match self {
            Self::Local(rook) => Ok(rook.prune(dry_run)?),
            Self::Daemon(d) => d.post("/api/store/prune", &serde_json::json!({ "dry_run": dry_run })),
        }
    }

    pub fn verify(&self) -> Result<Vec<(String, String)>> {
        match self {
            Self::Local(rook) => Ok(rook.verify()?.into_iter().map(|(id, why)| (id.to_hex(), why)).collect()),
            Self::Daemon(d) => {
                let said: serde_json::Value = d.post("/api/store/verify", &serde_json::json!({}))?;
                Ok(said["failed"]
                    .as_array()
                    .map(|rows| {
                        rows.iter()
                            .map(|r| {
                                (
                                    r["object"].as_str().unwrap_or_default().to_string(),
                                    r["why"].as_str().unwrap_or_default().to_string(),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default())
            }
        }
    }

    pub fn train_dictionaries(&self) -> Result<Vec<(String, usize)>> {
        match self {
            Self::Local(rook) => Ok(rook.train_dictionaries()?),
            Self::Daemon(d) => {
                let said: serde_json::Value = d.post("/api/store/train", &serde_json::json!({}))?;
                Ok(serde_json::from_value(said["trained"].clone())?)
            }
        }
    }

    /// The new session's id and how many events it carries.
    pub fn fork_session(&self, session: u128, at: u64) -> Result<(String, u64)> {
        match self {
            Self::Local(rook) => {
                let forked = rook.fork_session(session, at)?;
                Ok((rook_store::format_session_id(forked.id), forked.event_count))
            }
            Self::Daemon(d) => {
                let id = rook_store::format_session_id(session);
                let said: serde_json::Value =
                    d.post(&format!("/api/sessions/{id}/fork"), &serde_json::json!({ "at": at }))?;
                Ok((
                    said["id"].as_str().unwrap_or_default().to_string(),
                    said["event_count"].as_u64().unwrap_or(0),
                ))
            }
        }
    }

    pub fn delete_session(&self, session: u128) -> Result<u64> {
        match self {
            Self::Local(rook) => Ok(rook.delete_session(session)?),
            Self::Daemon(d) => {
                let id = rook_store::format_session_id(session);
                let said: serde_json::Value = d.delete(&format!("/api/sessions/{id}"))?;
                Ok(said["events"].as_u64().unwrap_or(0))
            }
        }
    }

    pub fn checkpoint(
        &self,
        name: &str,
        path: Option<&std::path::Path>,
    ) -> Result<(rook_core::fileset::FileSet, String)> {
        match self {
            Self::Local(rook) => {
                let (set, id) = rook.checkpoint(name, path)?;
                Ok((set, id.to_hex()))
            }
            Self::Daemon(d) => {
                let said: serde_json::Value =
                    d.post("/api/checkpoints", &serde_json::json!({ "name": name, "path": path }))?;
                Ok((
                    serde_json::from_value(said["set"].clone())?,
                    serde_json::from_value(said["object"].clone())?,
                ))
            }
        }
    }

    pub fn checkpoints(&self) -> Result<Vec<(String, String)>> {
        match self {
            Self::Local(rook) => {
                Ok(rook.checkpoints()?.into_iter().map(|(n, id)| (n, id.to_hex())).collect())
            }
            Self::Daemon(d) => Ok(d
                .get::<Page<serde_json::Value>>("/api/checkpoints")?
                .items
                .iter()
                .map(|row| {
                    (
                        row["ref"].as_str().unwrap_or_default().to_string(),
                        row["object"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()),
        }
    }

    pub fn restore_checkpoint(&self, object: &str, to: &std::path::Path) -> Result<usize> {
        match self {
            Self::Local(rook) => Ok(rook.restore_checkpoint(&rook.object_named(object)?, to)?),
            Self::Daemon(d) => {
                let said: serde_json::Value =
                    d.post("/api/checkpoints/restore", &serde_json::json!({ "object": object, "to": to }))?;
                Ok(said["restored"].as_u64().unwrap_or(0) as usize)
            }
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

/// The project a command is about: what was asked for, else where it was run.
fn asked_about(workspace: Option<std::path::PathBuf>) -> std::path::PathBuf {
    workspace.or_else(|| std::env::current_dir().ok()).unwrap_or_else(|| std::path::PathBuf::from("."))
}

/// Start `rookd` beside this binary, detached, and say where it came from.
///
/// Beside rather than on the `PATH`: the two are installed together and a
/// second `rookd` from somewhere else would open a different store. Its own
/// output goes nowhere — it logs to `$ROOK_HOME/logs`, and a line printed into
/// a terminal a TUI is drawing in is a line that corrupts the screen.
fn start_daemon() -> Option<(std::process::Child, String)> {
    let beside =
        std::env::current_exe().ok()?.parent()?.join(if cfg!(windows) { "rookd.exe" } else { "rookd" });
    if !beside.is_file() {
        return None;
    }
    let mut command = std::process::Command::new(&beside);
    command
        // A port the system picks, not the default one: the address file is how
        // anyone finds it, and a fixed port collides with whatever else is on
        // it — including a daemon somebody started for a different `ROOK_HOME`.
        .args(["--port", "0"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Its own process group, so ctrl-c in this terminal is this window's to
    // handle and does not take the engine every other window is using.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    Some((command.spawn().ok()?, beside.display().to_string()))
}

/// Its own address file is the only "it is up" there is, and it writes one
/// after opening the store, discovering skills and binding a port.
///
/// Watched rather than waited out: a daemon that cannot start says so in a
/// moment, and thirty seconds of nothing before a window opens is
/// indistinguishable from a hang.
fn came_up(child: &mut std::process::Child) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        if Daemon::running().is_some() {
            return true;
        }
        if matches!(child.try_wait(), Ok(Some(_)) | Err(_)) {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
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
    pub base: String,
    /// From the health check that proved it was answering: whether the `rookd`
    /// on disk has changed since that process started.
    pub replaced: bool,
    /// The project this window is about, which the daemon serves several of:
    /// every routed read names it, and a front end asking about the daemon's
    /// own workspace instead would be answering a different question.
    workspace: std::path::PathBuf,
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
        let mut daemon =
            Self { base, replaced: false, workspace: std::path::PathBuf::from("."), runtime, http };
        // The same request that proves it is alive answers what it is running,
        // so knowing costs nothing beyond what was already asked.
        daemon.replaced = daemon.health().ok()?.binary_replaced;
        Some(daemon)
    }

    pub fn health(&self) -> Result<rook_proto::Health> {
        self.get("/api/health")
    }

    /// Ask it to stop. It refuses while a turn is running unless told to end
    /// them, which is a decision to make with the number in front of you.
    pub fn stop(&self, force: bool) -> Result<u32> {
        let said: serde_json::Value = self.post("/api/shutdown", &serde_json::json!({ "force": force }))?;
        Ok(said["turns_interrupted"].as_u64().unwrap_or(0) as u32)
    }

    fn post<T: DeserializeOwned>(&self, path: &str, body: &serde_json::Value) -> Result<T> {
        let url = format!("{}{path}", self.base);
        self.runtime.block_on(async {
            let response =
                self.http.post(&url).json(body).send().await.with_context(|| format!("POST {url}"))?;
            let status = response.status();
            let said = response.text().await.with_context(|| format!("reading {url}"))?;
            if !status.is_success() {
                bail!("{url} answered {status}: {said}");
            }
            serde_json::from_str(&said).with_context(|| format!("decoding {url}"))
        })
    }

    fn delete<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = format!("{}{path}", self.base);
        self.runtime.block_on(async {
            let response = self.http.delete(&url).send().await.with_context(|| format!("DELETE {url}"))?;
            let status = response.status();
            let said = response.text().await.with_context(|| format!("reading {url}"))?;
            if !status.is_success() {
                bail!("{url} answered {status}: {said}");
            }
            serde_json::from_str(&said).with_context(|| format!("decoding {url}"))
        })
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
