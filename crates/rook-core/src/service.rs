//! The façade both the CLI and the daemon drive. Everything a user can inspect
//! or change goes through here, so the three front ends cannot drift apart.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use rook_skills::{Environment, SkillCard, SkillIndex, SkillSource};
use rook_store::{
    Event, EventKind, GcOptions, GcReport, ObjectId, PruneReport, SessionMeta, Store, StoreStats,
};

use crate::config::Config;
use crate::error::{CoreError, Result};
use crate::fileset::{self, CaptureLimits, Change, FileSet};
use crate::paths;

pub const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");

pub struct Rook {
    pub store: Store,
    pub config: Config,
    pub env: Environment,
    pub skills: SkillIndex,
    pub workspace: PathBuf,
    /// Skills that failed to load, kept so the UIs can show them instead of
    /// silently presenting a shorter catalog.
    pub skill_errors: Vec<String>,
}

impl Rook {
    pub fn open(workspace: Option<PathBuf>) -> Result<Self> {
        paths::ensure_dirs().map_err(|e| CoreError::Io { path: paths::home(), source: e })?;
        let workspace =
            workspace.or_else(|| std::env::current_dir().ok()).unwrap_or_else(|| PathBuf::from("."));
        let config = Config::load()?;
        let mut store = Store::open(paths::store_dir())?;
        store.set_level(config.storage.compression_level);
        let env = Environment::detect(AGENT_VERSION);
        let (skills, skill_errors) = Self::discover_skills(&workspace);
        Ok(Self { store, config, env, skills, workspace, skill_errors })
    }

    /// Assemble a `Rook` from parts instead of from the user's home directory.
    ///
    /// For tests and for embedding Rook in another program. `open` is the normal
    /// path; this exists so a caller can point at a scratch store and a fixed
    /// environment without mutating process-wide state.
    pub fn from_parts(
        store: Store,
        config: Config,
        env: Environment,
        skills: SkillIndex,
        workspace: PathBuf,
    ) -> Self {
        Self { store, config, env, skills, workspace, skill_errors: Vec::new() }
    }

    fn discover_skills(workspace: &Path) -> (SkillIndex, Vec<String>) {
        let mut roots: Vec<(PathBuf, SkillSource)> = Vec::new();
        if let Some(builtin) = paths::builtin_skills_dir() {
            roots.push((builtin, SkillSource::Builtin));
        }
        roots.push((paths::user_skills_dir(), SkillSource::User));
        roots.push((paths::project_skills_dir(workspace), SkillSource::Project));
        let (index, errors) = SkillIndex::discover(&roots);
        (index, errors.into_iter().map(|e| e.to_string()).collect())
    }

    pub fn reload_skills(&mut self) {
        let (index, errors) = Self::discover_skills(&self.workspace);
        self.skills = index;
        self.skill_errors = errors;
    }

    pub fn catalog(&self) -> Vec<SkillCard> {
        self.skills.catalog(&self.env)
    }

    // ------------------------------------------------------- skill versioning

    /// Capture the current on-disk content of a skill into the store and record
    /// it in that skill's history.
    ///
    /// This is what makes `rook skills history` and `rook skills rollback`
    /// possible: every edit the agent or the user makes to a skill becomes an
    /// addressable, restorable version rather than an overwrite.
    pub fn capture_skill(&self, name: &str, note: Option<String>) -> Result<(FileSet, ObjectId)> {
        let resolved = self.skills.resolve(name, &self.env)?;
        let skill = &resolved.skill;
        let version = skill.version().to_string();
        let (set, id) = FileSet::capture(
            &self.store,
            "skill",
            name,
            &version,
            &skill.dir,
            &CaptureLimits::for_skill(),
            note,
        )?;

        self.store.set_ref(&format!("skill/{name}/v/{version}"), &id)?;
        // The object id is part of the history key, not just the timestamp: two
        // captures within the same second are distinct versions, while capturing
        // unchanged content twice is genuinely the same entry and should collapse.
        self.store
            .set_ref(&format!("skill/{name}/h/{:015}-{}", rook_store::now_unix_millis(), id.short()), &id)?;
        Ok((set, id))
    }

    /// Every captured version of a skill, newest first.
    pub fn skill_history(&self, name: &str) -> Result<Vec<SkillVersionRecord>> {
        let mut out = Vec::new();
        for (refname, id) in self.store.list_refs(&format!("skill/{name}/h/"))? {
            let set = FileSet::load(&self.store, &id)?;
            out.push(SkillVersionRecord {
                reference: refname,
                object: id.to_hex(),
                version: set.version.clone(),
                captured_at: set.captured_at,
                files: set.files.len(),
                bytes: set.total_bytes,
                note: set.note.clone(),
            });
        }
        // Sort by the ref name, not by `captured_at`: the key carries
        // milliseconds and is lexicographically chronological, while the
        // displayed timestamp is only second-resolution and ties.
        out.sort_by(|a, b| b.reference.cmp(&a.reference));
        Ok(out)
    }

    /// What changed between two captures of a skill.
    pub fn skill_diff(&self, a: &ObjectId, b: &ObjectId) -> Result<Vec<(String, Change)>> {
        Ok(FileSet::load(&self.store, a)?.diff(&FileSet::load(&self.store, b)?))
    }

    /// Restore one captured version over the skill's directory.
    pub fn rollback_skill(&self, name: &str, object: &ObjectId) -> Result<Rollback> {
        let set = FileSet::load(&self.store, object)?;
        if set.name != name {
            return Err(CoreError::Other(format!(
                "object {} is a capture of {:?}, not {name:?}",
                object.short(),
                set.name
            )));
        }
        // Capture the current state first, so a rollback is itself undoable.
        let _ = self.capture_skill(name, Some("automatic capture before rollback".into()));
        let dest = self
            .skills
            .resolve(name, &self.env)
            .map(|r| r.skill.dir.clone())
            .unwrap_or_else(|_| paths::user_skills_dir().join(name));
        let restored = set.restore(&self.store, &dest)?;

        // Restoring writes files back; it does not delete. Anything on disk that
        // the capture never knew about survives, and saying so is the difference
        // between a rollback the user can trust and a directory that is quietly
        // a hybrid of two versions.
        let mut left_behind = Vec::new();
        for entry in walk_files(&dest) {
            if let Ok(rel) = entry.strip_prefix(&dest) {
                let rel = rel.to_string_lossy().replace('\\', "/");
                if !set.files.contains_key(&rel) {
                    left_behind.push(rel);
                }
            }
        }
        left_behind.sort();
        Ok(Rollback { restored, left_behind, dir: dest })
    }

    /// Scaffold a new skill on disk and capture v0.1.0 of it.
    pub fn new_skill(&self, name: &str, description: &str) -> Result<PathBuf> {
        let dir = paths::user_skills_dir().join(name);
        if dir.exists() {
            return Err(CoreError::Other(format!("{} already exists", dir.display())));
        }
        std::fs::create_dir_all(dir.join("references"))
            .map_err(|e| CoreError::Io { path: dir.clone(), source: e })?;
        let body = skill_template(name, description, &self.env);
        std::fs::write(dir.join("SKILL.md"), body)
            .map_err(|e| CoreError::Io { path: dir.join("SKILL.md"), source: e })?;
        Ok(dir)
    }

    // ------------------------------------------------------------ checkpoints

    /// Snapshot part of the workspace before a risky edit.
    pub fn checkpoint(&self, name: &str, root: Option<&Path>) -> Result<(FileSet, ObjectId)> {
        let root = root.unwrap_or(&self.workspace);
        let (set, id) =
            FileSet::capture(&self.store, "checkpoint", name, "", root, &CaptureLimits::default(), None)?;
        self.store.set_ref(
            &format!("checkpoint/{name}/{:015}-{}", rook_store::now_unix_millis(), id.short()),
            &id,
        )?;
        Ok((set, id))
    }

    pub fn checkpoints(&self) -> Result<Vec<(String, ObjectId)>> {
        self.store.list_refs("checkpoint/").map_err(Into::into)
    }

    // -------------------------------------------------------------- sessions

    pub fn sessions(&self) -> Result<Vec<SessionMeta>> {
        let mut list = self.store.list_sessions()?;
        list.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
        Ok(list)
    }

    pub fn start_session(&self, title: &str) -> Result<u128> {
        let id = rook_store::new_session_id();
        let mut meta =
            SessionMeta::new(id, title, self.workspace.display().to_string(), rook_store::now_unix());
        meta.model = self.config.agent.model.clone();
        meta.agent = format!("rook {AGENT_VERSION}");
        self.store.create_session(&meta)?;
        Ok(id)
    }

    /// Render a session's log with bodies resolved. `max_body` bounds how much of
    /// each payload is materialised, so viewing a session with a 200 MB tool
    /// result does not itself become the problem.
    pub fn transcript(
        &self,
        session: u128,
        from: u64,
        limit: usize,
        max_body: usize,
    ) -> Result<Vec<TranscriptEntry>> {
        let events: Vec<Event> = self.store.events(session, from, limit)?;
        let mut out = Vec::with_capacity(events.len());
        for e in events {
            let meta = self.store.stat_object(&e.record.body)?;
            let (body, truncated) = match self.store.get(&e.record.body) {
                Ok(raw) => {
                    let (windowed, truncated) = crate::context::window_bytes(&raw, max_body);
                    (String::from_utf8_lossy(&windowed).into_owned(), truncated)
                }
                Err(err) => (format!("<unreadable: {err}>"), false),
            };
            out.push(TranscriptEntry {
                seq: e.seq,
                ts: e.record.ts,
                kind: e.record.kind.as_str().to_string(),
                label: e.record.label.clone(),
                object: e.record.body.to_hex(),
                bytes: meta.as_ref().map(|m| m.size_raw).unwrap_or(0),
                stored_bytes: meta.as_ref().map(|m| m.size_stored).unwrap_or(0),
                tokens_in: e.record.tokens_in,
                tokens_out: e.record.tokens_out,
                truncated,
                body,
            });
        }
        Ok(out)
    }

    /// What a session is costing in context, broken down by what is in it.
    ///
    /// Answers the question every agent gets asked and few can: why is this
    /// conversation nearly full, and of what.
    pub fn context_usage(&self, session: u128, window: usize) -> Result<ContextUsage> {
        let budget = crate::context::ContextBudget::new(window, self.config.agent.compact_at);
        let mut by_kind: BTreeMap<String, KindUsage> = BTreeMap::new();
        let mut compactions = 0;

        for event in self.store.events(session, 0, usize::MAX)? {
            let kind = event.record.kind;
            if kind == EventKind::Compaction {
                compactions += 1;
            }
            let bytes = self.store.stat_object(&event.record.body)?.map(|m| m.size_raw).unwrap_or(0);
            let entry = by_kind.entry(kind.as_str().to_string()).or_default();
            entry.events += 1;
            entry.bytes += bytes;
            entry.tokens += (bytes as usize).div_ceil(4);
        }

        let live: usize =
            by_kind.iter().filter(|(k, _)| k.as_str() != "checkpoint").map(|(_, v)| v.tokens).sum();

        Ok(ContextUsage {
            window,
            usable: budget.usable(),
            compact_at: budget.threshold(),
            logged_tokens: by_kind.values().map(|v| v.tokens).sum(),
            live_tokens: live,
            needs_compaction: budget.needs_compaction(live),
            compactions,
            by_kind: by_kind.into_iter().collect(),
        })
    }

    /// Fork a session at `to_seq` and put the workspace back the way it was.
    ///
    /// The original session is left intact — the fork carries the kept prefix
    /// and the rewound-past turns stay readable in the parent. File state comes
    /// from the checkpoints the loop takes before each mutating tool call: for
    /// every path, the earliest capture at or after `to_seq` holds its
    /// pre-edit content, and a path recorded as absent is deleted.
    pub fn rewind(&self, session: u128, to_seq: u64, restore_files: bool) -> Result<Rewind> {
        let meta = self
            .store
            .get_session(session)?
            .ok_or_else(|| CoreError::NoSession(rook_store::format_session_id(session)))?;

        let forked = self.store.fork_session(
            session,
            rook_store::new_session_id(),
            to_seq,
            &format!("{} @{to_seq}", meta.title),
        )?;

        let mut restore: BTreeMap<PathBuf, ObjectId> = BTreeMap::new();
        let mut remove: Vec<PathBuf> = Vec::new();
        let mut checkpoints = 0;

        if restore_files {
            for event in self.store.events(session, to_seq, usize::MAX)? {
                if event.record.kind != EventKind::Checkpoint {
                    continue;
                }
                let set = FileSet::load(&self.store, &event.record.body)?;
                checkpoints += 1;
                let root = PathBuf::from(&set.root);
                for (rel, hex) in &set.files {
                    if let Some(id) = ObjectId::from_hex(hex) {
                        restore.entry(root.join(rel)).or_insert(id);
                    }
                }
                for rel in &set.absent {
                    let path = root.join(rel);
                    if !restore.contains_key(&path) && !remove.contains(&path) {
                        remove.push(path);
                    }
                }
            }
        }

        let mut restored = 0;
        for (path, id) in &restore {
            let data = self.store.get(id)?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| CoreError::Io { path: parent.to_path_buf(), source: e })?;
            }
            std::fs::write(path, data).map_err(|e| CoreError::Io { path: path.clone(), source: e })?;
            restored += 1;
        }
        let removed = remove.iter().filter(|p| std::fs::remove_file(p).is_ok()).count();

        Ok(Rewind {
            session: rook_store::format_session_id(forked.id),
            parent: rook_store::format_session_id(session),
            events_kept: forked.event_count,
            checkpoints_applied: checkpoints,
            files_restored: restored,
            files_removed: removed,
        })
    }

    /// Capture `paths` before something modifies them, and record it in the log.
    pub fn checkpoint_paths(&self, session: u128, label: &str, paths: &[PathBuf]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let (set, _) = fileset::capture_paths(
            &self.store,
            "checkpoint",
            label,
            &self.workspace,
            paths,
            &CaptureLimits::for_skill(),
        )?;
        self.store.append_event(
            session,
            rook_store::NewEvent::new(
                EventKind::Checkpoint,
                rook_store::Kind::Snapshot,
                &serde_json::to_vec(&set)?,
            )
            .label(label),
        )?;
        Ok(())
    }

    pub fn log(&self, session: u128, kind: EventKind, label: &str, body: &str) -> Result<u64> {
        let body_kind = match kind {
            EventKind::ToolResult => rook_store::Kind::ToolResult,
            EventKind::SkillLoaded => rook_store::Kind::Skill,
            EventKind::Checkpoint => rook_store::Kind::Snapshot,
            _ => rook_store::Kind::Message,
        };
        Ok(self.store.append_event(
            session,
            rook_store::NewEvent::new(kind, body_kind, body.as_bytes()).label(label),
        )?)
    }

    /// Connect every enabled MCP server and collect what they offer.
    ///
    /// Servers are connected concurrently and failures are collected rather than
    /// propagated: one misconfigured server must not stop the agent from
    /// starting with the tools that do work.
    pub async fn connect_mcp(&self) -> McpSession {
        let enabled: Vec<_> = self.config.mcp.iter().filter(|c| c.enabled).collect();
        let connections = enabled.iter().map(|config| async move {
            match rook_mcp::Server::connect(config).await {
                Ok(server) => {
                    let server = std::sync::Arc::new(server);
                    match server.list_tools().await {
                        Ok(tools) => Ok((server, tools)),
                        Err(e) => Err((config.name.clone(), e.to_string())),
                    }
                }
                Err(e) => Err((config.name.clone(), e.to_string())),
            }
        });

        let mut session = McpSession::default();
        for outcome in futures_util::future::join_all(connections).await {
            match outcome {
                Ok((server, tools)) => session.servers.push((server, tools)),
                Err(failure) => session.failures.push(failure),
            }
        }
        session
    }

    // ------------------------------------------------------------ maintenance

    pub fn stats(&self) -> Result<StoreStats> {
        Ok(self.store.stats()?)
    }

    /// Prune to the retention policy, then collect what that freed.
    pub fn maintenance(&self, dry_run: bool) -> Result<MaintenanceReport> {
        let prune = self.store.prune(&self.config.storage.retention, dry_run)?;
        let gc = self.store.gc(&GcOptions {
            expand: Some(&fileset::gc_expander),
            dry_run,
            ..Default::default()
        })?;
        let trained = if dry_run {
            Vec::new()
        } else {
            self.store.train_dictionaries(
                self.config.storage.train_dictionaries_after,
                self.config.storage.dictionary_bytes,
            )?
        };
        Ok(MaintenanceReport { prune, gc, dictionaries_trained: trained })
    }
}

/// Result of restoring a captured version over a live directory.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rollback {
    pub restored: usize,
    /// Files present on disk that this capture does not contain. They were left
    /// alone; delete them by hand if the rollback should be exact.
    pub left_behind: Vec<String>,
    pub dir: PathBuf,
}

fn walk_files(root: &Path) -> Vec<PathBuf> {
    ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(false)
        .build()
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.path().to_path_buf())
        .collect()
}

#[derive(Default)]
pub struct McpSession {
    pub servers: Vec<(std::sync::Arc<rook_mcp::Server>, Vec<rook_mcp::ToolDescriptor>)>,
    /// Server name and why it could not be used.
    pub failures: Vec<(String, String)>,
}

impl McpSession {
    pub fn tool_count(&self) -> usize {
        self.servers.iter().map(|(_, tools)| tools.len()).sum()
    }

    pub async fn shutdown(&self) {
        for (server, _) in &self.servers {
            server.shutdown().await;
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct KindUsage {
    pub events: u64,
    pub bytes: u64,
    pub tokens: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextUsage {
    pub window: usize,
    pub usable: usize,
    pub compact_at: usize,
    /// Everything the session has ever logged.
    pub logged_tokens: usize,
    /// What a fresh turn would actually carry: checkpoints are storage, not context.
    pub live_tokens: usize,
    pub needs_compaction: bool,
    pub compactions: u32,
    pub by_kind: Vec<(String, KindUsage)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rewind {
    pub session: String,
    pub parent: String,
    pub events_kept: u64,
    pub checkpoints_applied: usize,
    pub files_restored: usize,
    pub files_removed: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillVersionRecord {
    pub reference: String,
    pub object: String,
    pub version: String,
    pub captured_at: i64,
    pub files: usize,
    pub bytes: u64,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TranscriptEntry {
    pub seq: u64,
    pub ts: i64,
    pub kind: String,
    pub label: String,
    pub object: String,
    pub bytes: u64,
    pub stored_bytes: u64,
    pub tokens_in: u32,
    pub tokens_out: u32,
    pub truncated: bool,
    pub body: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MaintenanceReport {
    pub prune: PruneReport,
    pub gc: GcReport,
    pub dictionaries_trained: Vec<(String, usize)>,
}

fn skill_template(name: &str, description: &str, env: &Environment) -> String {
    format!(
        "---\n\
         name: {name}\n\
         description: {description}\n\
         version: 0.1.0\n\
         keywords: []\n\
         # Everything below is optional. `requires` gates the whole skill;\n\
         # `variants` swaps only the body. Delete what you do not need.\n\
         requires:\n\
         \x20 os: [{os}]\n\
         \x20 # language:\n\
         \x20 #   rust: \">=1.85\"\n\
         \x20 # tool:\n\
         \x20 #   git: \">=2.30\"\n\
         # variants:\n\
         #   - when: {{ userland: [bsd] }}\n\
         #     body: variants/bsd.md\n\
         ---\n\n\
         # {name}\n\n\
         {description}\n\n\
         ## When to use this\n\n\
         Describe the trigger precisely — this is what the agent matches on.\n\n\
         ## Steps\n\n\
         1. ...\n",
        os = env.os,
    )
}
