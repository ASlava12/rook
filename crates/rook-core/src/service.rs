//! The façade both the CLI and the daemon drive. Everything a user can inspect
//! or change goes through here, so the three front ends cannot drift apart.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};

use rook_skills::{Environment, SkillCard, SkillIndex, SkillSource};

use crate::plugins::Plugin;
use rook_store::{
    Event, EventKind, GcOptions, GcReport, ObjectId, PruneReport, SessionMeta, Store, StoreStats,
};

use crate::config::Config;
use crate::error::{CoreError, Result};
use crate::fileset::{self, CaptureLimits, Change, FileSet};
use crate::memory::{self, Fact, MemoryBook};
use crate::paths;

pub const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");

const MEMORY_HEAD: &str = "memory/head";
/// Key prefixes for the two positions a session carries besides its events.
const COMPACTED: &str = "compacted";
const FORK_AT: &str = "fork-at";
const OFFERED_SERVER: &str = "offered-server";
const OFFERED_UPDATE: &str = "offered-update";
const MEMORY_LOG: &str = "memory/h/";

pub struct Rook {
    /// Shared, because one process may hold several of these. The store is one
    /// per `ROOK_HOME` and takes a single writer; a workspace is one per
    /// project. Binding the two together is what made a second project a second
    /// process, and a second process the one that could not open the store.
    pub store: Arc<Store>,
    pub config: Config,
    /// Detected on first use, not on open. Probing sixteen toolchains costs
    /// about a third of a second warm and over a second cold — more than the
    /// whole of `session ls`, which never asks what version of Java is here.
    env: OnceLock<Environment>,
    /// Behind a lock so a skill written mid-run is found by the next turn
    /// without restarting: the front ends hold `Rook` shared and cannot take it
    /// mutably.
    skills: std::sync::RwLock<SkillIndex>,
    pub workspace: PathBuf,
    /// Skills and plugins that failed to load, kept so the UIs can show them
    /// instead of silently presenting a shorter catalog.
    pub skill_errors: Vec<String>,
    pub plugins: Vec<Plugin>,
    /// Paths a running turn is part-way through writing, and whose turn.
    ///
    /// Several conversations can share one project now, and nothing else stops
    /// two of them writing the same file at the same moment. `edit_file` refuses
    /// on its own — it replaces exact text, and text another turn has changed is
    /// not there to replace — but `write_file` overwrites whole, so the loser of
    /// that race silently loses its work.
    writing: std::sync::Mutex<BTreeMap<PathBuf, Held>>,
    /// Where the whole of a runaway command's output is kept.
    ///
    /// A field rather than [`paths::output_dir`] because `from_parts` exists so
    /// a caller can point at a scratch store without touching process-wide
    /// state, and `maintenance` *deletes* from this directory: reading it from
    /// the environment would let a test with no `ROOK_HOME` sweep the files of
    /// whoever ran it.
    pub output_dir: PathBuf,
    /// Who last read or wrote each file.
    ///
    /// The claim above stops two turns writing at the same instant. It says
    /// nothing about the slower race: one turn reads a file, another rewrites
    /// it, and the first writes back what it read. This is what tells them
    /// apart — an overwrite by somebody who is not the last to have looked is a
    /// turn writing over something it never saw.
    touched: std::sync::Mutex<BTreeMap<PathBuf, Held>>,
}

/// Who is writing a path, and since when.
#[derive(Clone, Copy, Debug)]
pub struct Held {
    pub session: u128,
    pub since: i64,
}

/// How long a claim is believed.
///
/// The guard releases on drop, which covers a call that returns, one that
/// panics while unwinding, and a turn whose task is aborted. What it does not
/// cover is a call that never returns at all — `run_command` takes its timeout
/// from the model, so "for as long as the call takes" is not by itself a bound.
/// Past this the holder is treated as gone, because a file no one can ever write
/// again is worse than two turns racing for it.
const HELD_FOR_AT_MOST: i64 = 3_600;

/// A turn's hold on the paths it is about to write, released when dropped.
pub struct Writing<'a> {
    rook: &'a Rook,
    paths: Vec<PathBuf>,
}

impl Drop for Writing<'_> {
    fn drop(&mut self) {
        let mut held = self.rook.writing.lock().unwrap_or_else(|e| e.into_inner());
        for path in &self.paths {
            held.remove(path);
        }
    }
}

impl Rook {
    /// The environment skills are resolved against.
    pub fn env(&self) -> &Environment {
        self.env.get_or_init(|| Environment::detect(AGENT_VERSION))
    }

    pub fn open(workspace: Option<PathBuf>) -> Result<Self> {
        paths::ensure_dirs().map_err(|e| CoreError::Io { path: paths::home(), source: e })?;
        let workspace =
            workspace.or_else(|| std::env::current_dir().ok()).unwrap_or_else(|| PathBuf::from("."));
        let config = Config::load()?;
        let mut store = Store::open(paths::store_dir())?;
        store.set_level(config.storage.compression_level);
        let (plugins, plugin_errors) = crate::plugins::discover(&workspace);
        let (skills, mut skill_errors) = Self::discover_skills(&workspace, &plugins);
        skill_errors.extend(plugin_errors);
        Ok(Self {
            store: Arc::new(store),
            config,
            env: OnceLock::new(),
            skills: skills.into(),
            workspace,
            skill_errors,
            plugins,
            output_dir: paths::output_dir(),
            writing: Default::default(),
            touched: Default::default(),
        })
    }

    /// Assemble a `Rook` from parts instead of from the user's home directory.
    ///
    /// For tests and for embedding Rook in another program. `open` is the normal
    /// path; this exists so a caller can point at a scratch store and a fixed
    /// environment without mutating process-wide state.
    #[doc(hidden)]
    pub fn from_parts(
        store: Store,
        config: Config,
        env: Environment,
        skills: SkillIndex,
        workspace: PathBuf,
    ) -> Self {
        let root = store.root().to_path_buf();
        Self {
            store: Arc::new(store),
            config,
            env: OnceLock::from(env),
            skills: skills.into(),
            workspace,
            skill_errors: Vec::new(),
            plugins: Vec::new(),
            // Beside the scratch store rather than in the user's home, for the
            // reason the field carries.
            output_dir: root.join("output"),
            writing: Default::default(),
            touched: Default::default(),
        }
    }

    /// The same store and the same machine, looking at another project.
    ///
    /// Skills and plugins are rediscovered because both are partly the
    /// workspace's own; everything else is shared, which is the point — one
    /// memory, one history, one search across every project rather than a store
    /// per directory.
    pub fn for_workspace(&self, workspace: PathBuf) -> Self {
        let (plugins, plugin_errors) = crate::plugins::discover(&workspace);
        let (skills, mut skill_errors) = Self::discover_skills(&workspace, &plugins);
        skill_errors.extend(plugin_errors);
        Self {
            store: self.store.clone(),
            config: self.config.clone(),
            env: match self.env.get() {
                Some(env) => OnceLock::from(env.clone()),
                None => OnceLock::new(),
            },
            skills: skills.into(),
            workspace,
            skill_errors,
            plugins,
            output_dir: self.output_dir.clone(),
            writing: Default::default(),
            touched: Default::default(),
        }
    }

    /// The skills a workspace has, and what failed to load. Public because
    /// `rook doctor` asks it without a store — a diagnostic that needs the
    /// lock is unavailable exactly when it is wanted.
    pub fn discover_skills(workspace: &Path, plugins: &[Plugin]) -> (SkillIndex, Vec<String>) {
        let mut roots: Vec<(PathBuf, SkillSource)> = Vec::new();
        if let Some(builtin) = paths::builtin_skills_dir() {
            roots.push((builtin, SkillSource::Builtin));
        }
        for plugin in plugins {
            roots.push((plugin.skills_dir(), SkillSource::Plugin(plugin.name.clone())));
        }
        roots.push((paths::user_skills_dir(), SkillSource::User));
        roots.push((paths::project_skills_dir(workspace), SkillSource::Project));
        let (index, errors) = SkillIndex::discover(&roots);
        (index, errors.into_iter().map(|e| e.to_string()).collect())
    }

    pub fn skills(&self) -> std::sync::RwLockReadGuard<'_, SkillIndex> {
        self.skills.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Rediscover from disk. Errors are kept rather than returned so a UI can
    /// show a broken skill instead of quietly presenting a shorter catalog.
    pub fn reload_skills(&self) -> Vec<String> {
        let (index, errors) = Self::discover_skills(&self.workspace, &self.plugins);
        *self.skills.write().unwrap_or_else(|e| e.into_inner()) = index;
        errors
    }

    pub fn catalog(&self) -> Vec<SkillCard> {
        self.skills().catalog(self.env())
    }

    /// Why a name resolves to the skill it does: every version carrying that
    /// name, what this environment fails to satisfy for each, and the winner.
    ///
    /// A value rather than printed lines, because the question is asked from
    /// the command line and over the API and the answer must not depend on
    /// which — and because "why did it pick that one" is exactly the question
    /// a `requires:` gets wrong in a way nobody can see.
    pub fn why_skill(&self, name: &str) -> Result<SkillWhy> {
        let skills = self.skills();
        let versions = skills.versions_of(name);
        if versions.is_empty() {
            return Err(CoreError::Other(format!("no skill named {name:?}")));
        }
        let candidates = versions
            .iter()
            .map(|skill| SkillCandidate {
                id: skill.id(),
                source: skill.source.label().to_string(),
                mismatches: skill.manifest.requires.check(self.env()),
            })
            .collect();
        let (chosen, why_not) = match skills.resolve(name, self.env()) {
            Ok(r) => (Some(format!("{} [{}]", r.skill.id(), r.skill.source.label())), None),
            Err(e) => (None, Some(e.to_string())),
        };
        Ok(SkillWhy { environment: self.env().clone(), versions: candidates, chosen, why_not })
    }

    // ------------------------------------------------------- skill versioning

    /// Capture the current on-disk content of a skill into the store and record
    /// it in that skill's history.
    ///
    /// This is what makes `rook skills history` and `rook skills rollback`
    /// possible: every edit the agent or the user makes to a skill becomes an
    /// addressable, restorable version rather than an overwrite.
    /// Found by name rather than resolved, because a skill is versioned from
    /// wherever it is edited: a FreeBSD-only skill has to be capturable from a
    /// Linux machine, which is the whole point of `requires`.
    pub fn capture_skill(&self, name: &str, note: Option<String>) -> Result<(FileSet, ObjectId)> {
        let skills = self.skills();
        let skill = *skills
            .versions_of(name)
            .first()
            .ok_or_else(|| CoreError::Other(format!("no skill named {name:?}")))?;
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
        self.store.set_ref(&format!("skill/{name}/h/{}", rook_store::history_key()), &id)?;
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
        // A skill that no longer resolves has nothing on disk to lose and is
        // being recreated from a capture, so there is nothing to take first —
        // but when there is, a failed capture means the rollback would be the
        // one nobody can get back from, and it does not happen.
        let resolved = self.skills().resolve(name, self.env()).ok();
        let undo = match &resolved {
            Some(_) => Some(self.capture_skill(name, Some("automatic capture before rollback".into()))?.1),
            None => None,
        };
        let dest = resolved.map(|r| r.skill.dir).unwrap_or_else(|| paths::user_skills_dir().join(name));
        // Writing files stops where it fails, so the directory is then part one
        // version and part the other. Naming the capture that holds what was
        // there is the difference between that and losing it.
        let restored = set.restore(&self.store, &dest).map_err(|e| match &undo {
            Some(id) => CoreError::Other(format!(
                "{e} — {} is now part {} and part what was there before; restore it \
                 with `rook skills rollback {name} {}`",
                dest.display(),
                object.short(),
                id.short()
            )),
            None => e,
        })?;

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
        Ok(Rollback { restored, left_behind, dir: dest, undo })
    }

    /// Scaffold a new skill on disk and capture v0.1.0 of it.
    /// Write a whole skill and record its first version, which is what the
    /// agent does when it learns something worth keeping.
    ///
    /// Unlike [`Rook::new_skill`], which scaffolds a file for a person to edit,
    /// this one takes the finished body. Rewriting an existing skill is allowed
    /// and becomes another captured version, so nothing is lost — but only for
    /// a skill in the user directory: one that ships with the project or the
    /// system is not the agent's to overwrite.
    pub fn write_skill(&self, skill: &AuthoredSkill) -> Result<PathBuf> {
        let name = skill.name.trim();
        if !rook_skills::usable_name(name) {
            return Err(CoreError::Other(format!(
                "{name:?} is not a usable skill name — letters, digits, hyphens and underscores only"
            )));
        }
        if let Ok(existing) = self.skills().resolve(name, self.env())
            && existing.skill.source != SkillSource::User
        {
            return Err(CoreError::Other(format!(
                "{name:?} is a {} skill, not yours to rewrite — pick another name",
                existing.skill.source.label()
            )));
        }

        let dir = paths::user_skills_dir().join(name);
        std::fs::create_dir_all(&dir).map_err(|e| CoreError::Io { path: dir.clone(), source: e })?;
        let path = dir.join("SKILL.md");
        std::fs::write(&path, skill.to_skill_md()?)
            .map_err(|e| CoreError::Io { path: path.clone(), source: e })?;
        for (rel, contents) in &skill.files {
            let file = dir.join(Self::safe_relative(rel)?);
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| CoreError::Io { path: parent.to_path_buf(), source: e })?;
            }
            std::fs::write(&file, contents).map_err(|e| CoreError::Io { path: file.clone(), source: e })?;
            make_runnable(&file, contents);
        }

        // Validated by reading it back rather than by trusting the writer: a
        // skill that does not parse is one the next session silently lacks.
        // Parsing, not resolving — a skill whose `requires` excludes this
        // machine is doing its job, not failing.
        let errors = self.reload_skills();
        if self.skills().versions_of(name).is_empty() {
            let reason = errors
                .iter()
                .find(|e| e.contains(name))
                .cloned()
                .unwrap_or_else(|| "it did not parse".into());
            return Err(CoreError::Other(format!("wrote {} but it does not load: {reason}", path.display())));
        }
        self.capture_skill(name, Some(format!("written by the agent: {}", skill.description)))?;
        Ok(path)
    }

    /// A path inside the skill's own directory, or an error naming why not.
    /// These come from the model, and `../` in one of them would write wherever
    /// it liked with the skill directory's permission.
    fn safe_relative(rel: &str) -> Result<PathBuf> {
        let path = PathBuf::from(rel);
        let sane = !rel.trim().is_empty()
            && path.is_relative()
            && path.components().all(|c| matches!(c, std::path::Component::Normal(_)));
        match sane {
            true => Ok(path),
            false => Err(CoreError::Other(format!(
                "{rel:?} is not a name inside the skill — no absolute paths, no `..`"
            ))),
        }
    }

    /// What the configured sources offer, best match first.
    ///
    /// An empty query lists everything they have. `refresh` is what decides
    /// whether the network is touched.
    pub fn skills_offered(&self, query: &str, refresh: bool) -> (Vec<crate::catalog::Offered>, Vec<String>) {
        let (offered, errors) = crate::catalog::offered(&self.config.skill_sources, refresh);
        let matched: Vec<_> = crate::catalog::matching(&offered, query).into_iter().cloned().collect();
        (matched, errors)
    }

    /// Install one by name, and prove it loads before saying so.
    pub fn install_skill(&self, name: &str) -> Result<PathBuf> {
        let (offered, errors) = crate::catalog::offered(&self.config.skill_sources, false);
        let Some(skill) = offered.iter().find(|o| o.name == name) else {
            let near = crate::catalog::matching(&offered, name);
            let suggestion = match near.first() {
                Some(o) => format!(" — closest is {:?} from {}", o.name, o.source),
                None if errors.is_empty() => String::new(),
                None => format!(" ({})", errors.join("; ")),
            };
            return Err(CoreError::Other(format!("no source offers a skill called {name:?}{suggestion}")));
        };
        let path = crate::catalog::install(skill, &paths::user_skills_dir())?;
        let load_errors = self.reload_skills();
        if self.skills().versions_of(name).is_empty() {
            let reason = load_errors
                .iter()
                .find(|e| e.contains(name))
                .cloned()
                .unwrap_or_else(|| "it did not parse".into());
            return Err(CoreError::Other(format!(
                "installed {} but it does not load: {reason}",
                path.display()
            )));
        }
        self.capture_skill(name, Some(format!("installed from {}", skill.source)))?;
        Ok(path)
    }

    pub fn new_skill(&self, name: &str, description: &str) -> Result<PathBuf> {
        if !rook_skills::usable_name(name) {
            return Err(CoreError::Other(format!(
                "{name:?} is not a usable skill name — letters, digits, hyphens and underscores only"
            )));
        }
        let dir = paths::user_skills_dir().join(name);
        if dir.exists() {
            return Err(CoreError::Other(format!("{} already exists", dir.display())));
        }
        std::fs::create_dir_all(dir.join("references"))
            .map_err(|e| CoreError::Io { path: dir.clone(), source: e })?;
        let body = skill_template(name, description, self.env());
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
        self.store.set_ref(&format!("checkpoint/{name}/{}", rook_store::history_key()), &id)?;
        Ok((set, id))
    }

    /// Write a capture's files back out under `to`, and say how many.
    pub fn restore_checkpoint(&self, object: &ObjectId, to: &Path) -> Result<usize> {
        FileSet::load(&self.store, object)?.restore(&self.store, to)
    }

    pub fn checkpoints(&self) -> Result<Vec<(String, ObjectId)>> {
        self.store.list_refs("checkpoint/").map_err(Into::into)
    }

    // -------------------------------------------------------------- sessions

    pub fn sessions(&self) -> Result<Vec<SessionMeta>> {
        let mut list = self.store.list_sessions()?;
        // The id breaks ties: `updated_at` is whole seconds, and two sessions
        // started in the same one would otherwise come back in whichever order
        // the index happened to hold them. A ULID is time-ordered, so this is
        // the same sort at a finer grain.
        list.sort_by_key(|s| std::cmp::Reverse((s.updated_at, s.id)));
        Ok(list)
    }

    /// Name a session after what was first asked of it, if nothing named it.
    ///
    /// Every front end had a placeholder of its own — `chat`, `tui`, `web`,
    /// `acp <cwd>` — and two of the four already used the first line of the
    /// prompt instead. Twenty sessions called `chat` is a list you have to open
    /// one at a time.
    pub fn name_session_from(&self, session: u128, prompt: &str) -> Result<()> {
        let title: String = prompt
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or_default()
            .chars()
            .take(72)
            .collect();
        self.store.update_session(session, |meta| {
            if meta.title.trim().is_empty() {
                meta.title = title;
            }
        })?;
        Ok(())
    }

    pub fn session_named(&self, spec: &str) -> Result<u128> {
        session_named(spec, &self.workspace, &self.session_summaries()?)
    }

    /// Sessions as every front end lists them: the stored record joined with the
    /// two things kept beside it rather than in it.
    pub fn session_summaries(&self) -> Result<Vec<SessionSummary>> {
        self.sessions()?
            .into_iter()
            .map(|meta| {
                Ok(SessionSummary { goal: self.goal(meta.id)?, forked_at: self.forked_at(meta.id)?, meta })
            })
            .collect()
    }

    /// The engine looking at the project this session belongs to, when that is
    /// not the one this engine is looking at.
    ///
    /// A session is bound to a project: its transcript names that project's
    /// files, its checkpoints restore into it, and its memory is scoped to it.
    /// Resuming one from another directory read the old conversation and edited
    /// the new directory, and said nothing about either.
    ///
    /// `None` when it is the same project, or when the recorded one is no longer
    /// a directory — a project that has been moved is not a reason to refuse to
    /// go on in this one.
    pub fn following(&self, session: u128) -> Result<Option<Self>> {
        let Some(meta) = self.store.get_session(session)? else { return Ok(None) };
        let theirs = PathBuf::from(&meta.workspace);
        // Through symlinks, because the same directory arrives spelled two ways:
        // `--workspace` keeps what was typed and an omitted one is the resolved
        // current directory, which on macOS is `/private/var` for `/var`.
        let same = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        let moved = same(&theirs) != same(&self.workspace) && theirs.is_dir();
        Ok(moved.then(|| self.for_workspace(theirs)))
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

    /// A child session for a delegated task, linked to its parent.
    ///
    /// A fresh session rather than a fork of this one: the point of delegating
    /// is that the sub-agent starts with an empty context.
    pub fn fork_for_subtask(&self, parent: u128, task: &str) -> Result<u128> {
        let id = rook_store::new_session_id();
        let title: String = task.lines().next().unwrap_or("sub-task").chars().take(72).collect();
        let mut meta =
            SessionMeta::new(id, title, self.workspace.display().to_string(), rook_store::now_unix());
        meta.model = self.config.agent.model.clone();
        meta.agent = format!("rook {AGENT_VERSION}");
        meta.parent = Some(parent);
        meta.tags.push("subtask".into());
        self.store.create_session(&meta)?;
        // Where in the parent this errand was handed out. Rewinding the parent
        // past that point has to undo the child's work as well as its own, and
        // without the seq there is no way to tell which children are past it.
        let at = self.store.events(parent, 0, usize::MAX)?.last().map(|e| e.seq).unwrap_or(0);
        self.set_mark(FORK_AT, id, at)?;
        Ok(id)
    }

    /// Where replay should start, and the summary standing in for what came
    /// before it.
    pub fn last_compaction(&self, session: u128) -> Result<(u64, Option<String>)> {
        if let Some(seq) = self.read_mark(COMPACTED, session)?
            && let Some(found) = self.compaction_at(session, seq)?
        {
            return Ok(found);
        }
        // No mark, or one left dangling by a fork that cut past it: read the log
        // once and record what it says, so the next turn does not read it again.
        let mut found = None;
        for event in self.store.events(session, 0, usize::MAX)? {
            if event.record.kind == EventKind::Compaction
                && let Some(parsed) = self.compaction_body(&event)
            {
                found = Some((event.seq, parsed));
            }
        }
        match found {
            Some((seq, parsed)) => {
                self.set_mark(COMPACTED, session, seq)?;
                Ok(parsed)
            }
            None => Ok((0, None)),
        }
    }

    fn compaction_at(&self, session: u128, seq: u64) -> Result<Option<(u64, Option<String>)>> {
        let Some(event) = self.store.events(session, seq, 1)?.into_iter().next() else {
            return Ok(None);
        };
        if event.seq != seq || event.record.kind != EventKind::Compaction {
            return Ok(None);
        }
        Ok(self.compaction_body(&event))
    }

    /// Where replay resumes, and the summary standing in for what came before.
    fn compaction_body(&self, event: &Event) -> Option<(u64, Option<String>)> {
        let body = self.store.get(&event.record.body).ok()?;
        let parsed = serde_json::from_slice::<serde_json::Value>(&body).ok()?;
        let through = parsed.get("through_seq")?.as_u64()?;
        Some((through + 1, parsed.get("summary").and_then(|s| s.as_str()).map(String::from)))
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

    /// How much context a turn in this project actually has.
    ///
    /// Configured, else what the provider says about the configured model, else
    /// a guess. Here rather than in a front end because everything that reports
    /// a percentage has to agree about the denominator: the chat REPL guessed
    /// 128k while the same command in the CLI asked the provider, so the same
    /// session read as two different fractions depending on where it was asked.
    pub fn context_window(&self) -> usize {
        self.config.agent.context_window.unwrap_or_else(|| {
            rook_llm::from_spec_with(&self.config.agent.model, self.config.agent.stream_idle(), None)
                .map(|p| p.context_window())
                .unwrap_or(128_000)
        })
    }

    /// What a session is costing in context, broken down by what is in it.
    ///
    /// Answers the question every agent gets asked and few can: why is this
    /// conversation nearly full, and of what. `window` overrides what this
    /// project's configuration says, for asking how the same session would sit
    /// in a different model.
    pub fn context_usage(&self, session: u128, window: Option<usize>) -> Result<ContextUsage> {
        let window = window.unwrap_or_else(|| self.context_window());
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

        // What a fresh turn would carry: everything after the last compaction
        // that becomes a message, plus its summary. Checkpoints are storage;
        // asides, errors and the rest never reach the model either, and counting
        // them made this overstate the very number it exists to explain.
        let (from_seq, summary) = self.last_compaction(session)?;
        let mut live = summary.as_deref().map(crate::context::estimate_tokens).unwrap_or(0);
        for event in self.store.events(session, from_seq, usize::MAX)? {
            if !crate::context::reaches_the_model(event.record.kind) {
                continue;
            }
            let bytes = self.store.stat_object(&event.record.body)?.map(|m| m.size_raw as usize).unwrap_or(0);
            live += bytes.div_ceil(4);
        }

        Ok(ContextUsage {
            window,
            usable: budget.usable(),
            compact_at: budget.threshold(),
            logged_tokens: by_kind.values().map(|v| v.tokens).sum(),
            live_tokens: live,
            needs_compaction: budget.needs_compaction(live),
            compactions,
            replay_from: self.last_compaction(session)?.0,
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
        self.set_mark(FORK_AT, forked.id, to_seq)?;

        let mut restore: BTreeMap<PathBuf, ObjectId> = BTreeMap::new();
        let mut remove: Vec<PathBuf> = Vec::new();
        let mut checkpoints = 0;

        if restore_files {
            // The turn's own checkpoints and its children's. Reversibility is a
            // property of the workspace, not of one log: a turn that delegated
            // the writing is a turn whose rewind has to undo it, and a sub-task
            // keeps its checkpoints in a session of its own.
            let mut logs = vec![self.store.events(session, to_seq, usize::MAX)?];
            for child in self.delegated_after(session, to_seq)? {
                logs.push(self.store.events(child, 0, usize::MAX)?);
            }
            for event in logs.into_iter().flatten() {
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

        // Before writing over them. The checkpoints hold what the agent found;
        // what is on disk now is whatever happened since, and an edit made by
        // hand is in no checkpoint at all — so without this the restore is the
        // one operation here that destroys something with no copy kept. It is
        // logged on the fork, past the prefix it inherited, which makes
        // rewinding the fork to that point the way back.
        let touched: Vec<PathBuf> = restore.keys().cloned().chain(remove.iter().cloned()).collect();
        let kept = match touched.is_empty() {
            true => 0,
            false => {
                self.checkpoint_paths(forked.id, "before rewind", &touched, &CaptureLimits::default())?;
                touched.len()
            }
        };

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
            files_kept: kept,
        })
    }

    /// Claim `paths` for `session` until the returned guard is dropped.
    ///
    /// Refuses rather than waits, and refuses rather than merges: the other turn
    /// is mid-write, and what a turn wants is to be told so it can do something
    /// else, not to be blocked until it can overwrite the result.
    pub fn writing(&self, session: u128, paths: &[PathBuf]) -> Result<Writing<'_>> {
        let now = rook_store::now_unix();
        let mut held = self.writing.lock().unwrap_or_else(|e| e.into_inner());
        held.retain(|_, by| now.saturating_sub(by.since) < HELD_FOR_AT_MOST);

        if let Some((path, by)) = paths.iter().find_map(|p| held.get(p).map(|by| (p, *by)))
            && by.session != session
        {
            return Err(CoreError::Other(format!(
                "{} is being written by session {} right now — wait for it or work on \
                 something else",
                path.display(),
                rook_store::format_session_id(by.session)
            )));
        }
        for path in paths {
            held.insert(path.clone(), Held { session, since: now });
        }
        Ok(Writing { rook: self, paths: paths.to_vec() })
    }

    /// Move every claim `secs` further into the past.
    ///
    /// Public so a test can reach the expiry without sleeping for an hour, which
    /// is the alternative and is not a test anybody runs.
    #[doc(hidden)]
    pub fn age_claims_for_test(&self, secs: i64) {
        let mut held = self.writing.lock().unwrap_or_else(|e| e.into_inner());
        for by in held.values_mut() {
            by.since -= secs;
        }
    }

    /// Record that `session` has seen these files as they are now.
    pub fn touched(&self, session: u128, paths: &[PathBuf]) {
        let now = rook_store::now_unix();
        let mut seen = self.touched.lock().unwrap_or_else(|e| e.into_inner());
        seen.retain(|_, by| now.saturating_sub(by.since) < HELD_FOR_AT_MOST);
        for path in paths {
            seen.insert(path.clone(), Held { session, since: now });
        }
    }

    /// The path `session` would be overwriting without having looked at it since
    /// somebody else did, if there is one.
    ///
    /// Only a whole-file overwrite asks this. An edit names the text it replaces
    /// and fails on its own when that text has changed; there is nothing for an
    /// overwrite to fail against.
    pub fn overwriting_unseen(&self, session: u128, paths: &[PathBuf]) -> Option<String> {
        let now = rook_store::now_unix();
        let seen = self.touched.lock().unwrap_or_else(|e| e.into_inner());
        paths.iter().find_map(|path| {
            let by = seen.get(path)?;
            let stale = by.session != session && now.saturating_sub(by.since) < HELD_FOR_AT_MOST;
            stale.then(|| {
                format!(
                    "{} was last touched by session {}, not by this one — read it before \
                     overwriting it, or change the part you mean with `edit_file`",
                    path.display(),
                    rook_store::format_session_id(by.session)
                )
            })
        })
    }

    /// What is being written in this project, and by whom.
    ///
    /// A registry nobody can read is one that cannot be debugged when it wedges,
    /// which is the whole complaint about a lock.
    pub fn being_written(&self) -> Vec<(PathBuf, Held)> {
        let now = rook_store::now_unix();
        self.writing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(_, by)| now.saturating_sub(by.since) < HELD_FOR_AT_MOST)
            .map(|(path, by)| (path.clone(), *by))
            .collect()
    }

    /// Which files under the workspace were written since `since`, and whether
    /// the walk saw the whole of it.
    ///
    /// mtime rather than content, because there is nothing to compare against:
    /// a command declares no paths, so no checkpoint holds what its files were
    /// before it ran. That makes the honest question the cheap one — was this
    /// written — and the honest word for the answer "written" rather than
    /// "changed": a command that rewrites a file with the same bytes is in this
    /// list, and a file that changed is never missing from it.
    ///
    /// The second half of the answer is whether the walk finished. A list that
    /// stopped at its ceiling and did not say so is the wrong answer told
    /// confidently, which is the failure this exists to prevent.
    pub fn written_since(&self, since: std::time::SystemTime, limits: &CaptureLimits) -> (Vec<String>, bool) {
        let mut walker = ignore::WalkBuilder::new(&self.workspace);
        walker.hidden(false).git_ignore(limits.respect_ignore_files).git_global(false);
        let mut written = Vec::new();
        let mut seen = 0usize;
        for entry in walker.build().flatten() {
            let path = entry.path();
            let relative = path.strip_prefix(&self.workspace).unwrap_or(path).to_string_lossy();
            let relative = relative.replace('\\', "/");
            if limits.exclude.iter().any(|skip| relative.contains(skip.as_str())) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            seen += 1;
            if seen > limits.max_files {
                return (written, false);
            }
            if meta.modified().is_ok_and(|at| at >= since) {
                written.push(relative);
            }
        }
        (written, true)
    }

    /// Capture `paths` before something modifies them, and record it in the log.
    ///
    /// `limits` because the two callers are different sizes: one tool call
    /// touches a handful of paths, and a rewind's own checkpoint covers every
    /// path the session ever touched. Sized for the first, the second would
    /// refuse to protect a long session — which is when it matters most.
    pub fn checkpoint_paths(
        &self,
        session: u128,
        label: &str,
        paths: &[PathBuf],
        limits: &CaptureLimits,
    ) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let (set, _) =
            fileset::capture_paths(&self.store, "checkpoint", label, &self.workspace, paths, limits)?;
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
        let seq = self.store.append_event(
            session,
            rook_store::NewEvent::new(kind, body_kind, body.as_bytes()).label(label),
        )?;
        if kind == EventKind::Compaction {
            self.set_mark(COMPACTED, session, seq)?;
        }
        Ok(seq)
    }

    /// Every server that will be connected: configured, plus the ones plugins
    /// bring, minus the disabled. A plugin is a way of shipping a server, not a
    /// different kind of one, so nothing downstream distinguishes them.
    pub fn mcp_servers(&self) -> Vec<&rook_mcp::ServerConfig> {
        self.config
            .mcp
            .iter()
            .chain(self.plugins.iter().flat_map(|p| p.mcp.iter()))
            .filter(|c| c.enabled)
            .collect()
    }

    /// Connect every enabled MCP server and collect what they offer.
    ///
    /// Servers are connected concurrently and failures are collected rather than
    /// propagated: one misconfigured server must not stop the agent from
    /// starting with the tools that do work.
    pub async fn connect_mcp(&self) -> McpSession {
        let enabled = self.mcp_servers();
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

    /// What this session is for, as the user stated it.
    ///
    /// Kept in the key-value table rather than on `SessionMeta`: that struct is
    /// stored with postcard, which is not self-describing, so adding a field to
    /// it makes every record already written unreadable.
    pub fn goal(&self, session: u128) -> Result<Option<String>> {
        Ok(self
            .store
            .kv_get(&format!("goal/{:032x}", session))?
            .and_then(|raw| String::from_utf8(raw).ok())
            .filter(|goal| !goal.trim().is_empty()))
    }

    /// The checklist the model keeps, when it is given one to keep.
    ///
    /// Beside the goal and stored the same way, for the same reason: session
    /// state that is not an event, so it survives compaction and costs the
    /// transcript nothing. Only [`crate::Config::agent`]'s `todo_tool` puts a
    /// tool in front of it — the default is a line in the prompt and no
    /// bookkeeping at all ([ADR-0010](../../docs/adr/0010-no-todo-tool.md)).
    pub fn plan(&self, session: u128) -> Result<Option<String>> {
        Ok(self
            .store
            .kv_get(&format!("plan/{:032x}", session))?
            .map(|v| String::from_utf8_lossy(&v).into_owned())
            .filter(|p| !p.trim().is_empty()))
    }

    pub fn set_plan(&self, session: u128, plan: &str) -> Result<()> {
        self.store.kv_set(&format!("plan/{:032x}", session), plan.trim().as_bytes())?;
        self.log(session, EventKind::Note, "plan", plan.trim())?;
        Ok(())
    }

    pub fn set_goal(&self, session: u128, goal: &str) -> Result<()> {
        self.store.kv_set(&format!("goal/{:032x}", session), goal.trim().as_bytes())?;
        self.log(session, EventKind::Note, "goal", goal.trim())?;
        Ok(())
    }

    /// Where the session's last compaction sits, and where a fork left its
    /// parent. Both are recoverable by reading the log; recorded here so that
    /// `session ls` and the start of every turn do not have to.
    ///
    /// In the key-value table for the reason `goal` is — see above.
    fn read_mark(&self, kind: &str, session: u128) -> Result<Option<u64>> {
        Ok(self
            .store
            .kv_get(&format!("{kind}/{session:032x}"))?
            .and_then(|raw| String::from_utf8(raw).ok())
            .and_then(|text| text.parse().ok()))
    }

    fn set_mark(&self, kind: &str, session: u128, seq: u64) -> Result<()> {
        self.store.kv_set(&format!("{kind}/{session:032x}"), seq.to_string().as_bytes())?;
        Ok(())
    }

    /// Whether this session has already been offered a language server it
    /// lacks. Once per session: an offer declined is not one to repeat every
    /// turn, and an install made needs no second.
    pub fn offered_server(&self, session: u128) -> Result<bool> {
        Ok(self.read_mark(OFFERED_SERVER, session)?.is_some())
    }

    pub fn note_offered_server(&self, session: u128) -> Result<()> {
        self.set_mark(OFFERED_SERVER, session, 1)
    }

    pub fn offered_update(&self, session: u128) -> Result<bool> {
        Ok(self.read_mark(OFFERED_UPDATE, session)?.is_some())
    }

    pub fn note_offered_update(&self, session: u128) -> Result<()> {
        self.set_mark(OFFERED_UPDATE, session, 1)
    }

    /// The parent event a forked session diverged at, if it was forked.
    pub fn forked_at(&self, session: u128) -> Result<Option<u64>> {
        self.read_mark(FORK_AT, session)
    }

    /// Sub-tasks this session handed out after `seq`, however deep they nest.
    ///
    /// A scan of the sessions, which is what a rewind can afford: it is asked
    /// for by hand, once, and the alternative is a list on the parent that has
    /// to be kept in step with the sessions themselves.
    fn delegated_after(&self, session: u128, seq: u64) -> Result<Vec<u128>> {
        let all = self.store.list_sessions()?;
        let mut found = Vec::new();
        let mut frontier = vec![session];
        while let Some(parent) = frontier.pop() {
            for child in all.iter().filter(|m| m.parent == Some(parent)) {
                // The parent's own children are the ones that must be past the
                // rewind point; anything below them went with its parent.
                if parent != session || self.forked_at(child.id)?.is_none_or(|at| at >= seq) {
                    found.push(child.id);
                    frontier.push(child.id);
                }
            }
        }
        Ok(found)
    }

    // ---------------------------------------------------------------- memory

    /// The current memory, or an empty book if nothing has been remembered.
    pub fn memory(&self) -> Result<MemoryBook> {
        match self.store.get_ref(MEMORY_HEAD)? {
            Some(id) => MemoryBook::load(&self.store, &id),
            None => Ok(MemoryBook::default()),
        }
    }

    /// Returns whether the fact was new. A repeat that folds in new tags or
    /// pinning is still written — otherwise the merge would be silently lost.
    pub fn remember(&self, fact: Fact, note: Option<String>) -> Result<memory::Learned> {
        let mut book = self.memory()?;
        let learned = book.learn(fact);
        if learned != memory::Learned::Unchanged {
            self.save_memory(&book, note)?;
        }
        Ok(learned)
    }

    pub fn forget(&self, id_or_text: &str, note: Option<String>) -> Result<Option<Fact>> {
        let mut book = self.memory()?;
        let removed = book.forget(id_or_text);
        if removed.is_some() {
            self.save_memory(&book, note)?;
        }
        Ok(removed)
    }

    /// Facts relevant to `query` that fit in `budget` tokens, scoped to this
    /// workspace.
    pub fn recall(&self, query: &str, budget: usize) -> Result<Vec<Fact>> {
        let book = self.memory()?;
        let workspace = self.workspace.display().to_string();
        Ok(memory::select(book.in_scope(&workspace), query, budget))
    }

    /// Facts matching `query`, scored, in this workspace's scope.
    ///
    /// Here rather than in the caller because three front ends ask it and the
    /// answer must not depend on which: `recall` answers a different question
    /// — what fits in a turn's budget — and reading one for the other is how
    /// `memory search` and the agent's own recall would drift apart.
    pub fn memory_search(&self, query: &str) -> Result<Vec<memory::Hit>> {
        let book = self.memory()?;
        let workspace = self.workspace.display().to_string();
        Ok(memory::search(book.in_scope(&workspace), query))
    }

    /// Facts that already say close to this one, in the workspace's scope.
    pub fn similar_facts(&self, text: &str) -> Result<Vec<Fact>> {
        let book = self.memory()?;
        Ok(book.similar_to(text).into_iter().cloned().collect())
    }

    /// The object a person named by its first few characters.
    ///
    /// Resolved here so a command means the same thing whether it opened the
    /// store itself or asked the daemon: a prefix is ambiguous against a
    /// particular store, and only the process holding it can say so.
    pub fn object_named(&self, prefix: &str) -> Result<ObjectId> {
        self.store.resolve_prefix(prefix)?.ok_or_else(|| {
            CoreError::Other(format!("no object matches {prefix:?} (or the prefix is ambiguous)"))
        })
    }

    pub fn memory_history(&self) -> Result<Vec<MemoryVersion>> {
        let mut out = Vec::new();
        for (reference, id) in self.store.list_refs(MEMORY_LOG)? {
            let book = MemoryBook::load(&self.store, &id)?;
            out.push(MemoryVersion {
                reference,
                object: id.to_hex(),
                updated_at: book.updated_at,
                facts: book.facts.len(),
                note: book.note.clone(),
            });
        }
        out.sort_by(|a, b| b.reference.cmp(&a.reference));
        Ok(out)
    }

    pub fn memory_diff(&self, a: &ObjectId, b: &ObjectId) -> Result<Vec<(memory::Change, Fact)>> {
        Ok(MemoryBook::load(&self.store, a)?.diff(&MemoryBook::load(&self.store, b)?))
    }

    /// What the agent learned and forgot since `since` — the answer to "what
    /// changed today", in the order it happened.
    ///
    /// Every recorded state in the window, folded in turn, rather than a diff of
    /// the two ends. A fact learned and forgotten between them cancels out of
    /// such a diff, and that is precisely the story being asked for: an agent
    /// deleting what it had just been told to remember looked, to this, like a
    /// day in which nothing happened.
    pub fn memory_since(&self, since: i64) -> Result<Vec<(memory::Change, Fact)>> {
        let mut history = self.memory_history()?;
        // `memory_history` is newest first, for a listing. This reads forwards
        // in time, and the baseline is the last state before the window opened.
        history.reverse();
        let baseline = history.iter().rposition(|v| v.updated_at <= since);
        let first = baseline.map(|i| i + 1).unwrap_or(0);

        let mut changes = Vec::new();
        let mut previous = baseline.map(|i| &history[i]);
        for version in &history[first..] {
            let to = ObjectId::from_hex(&version.object)
                .ok_or_else(|| CoreError::Other("corrupt memory history".into()))?;
            changes.extend(match previous {
                Some(from) => {
                    let from = ObjectId::from_hex(&from.object)
                        .ok_or_else(|| CoreError::Other("corrupt memory history".into()))?;
                    self.memory_diff(&from, &to)?
                }
                // Nothing was known before the window, so everything in the
                // first state inside it was learned inside it.
                None => MemoryBook::load(&self.store, &to)?
                    .facts
                    .into_iter()
                    .map(|f| (memory::Change::Learned, f))
                    .collect(),
            });
            previous = Some(version);
        }
        Ok(changes)
    }

    fn save_memory(&self, book: &MemoryBook, note: Option<String>) -> Result<ObjectId> {
        let mut book = book.clone();
        book.note = note;
        book.updated_at = rook_store::now_unix();
        let id = book.store(&self.store)?;
        self.store.set_ref(MEMORY_HEAD, &id)?;
        self.store.set_ref(&format!("{MEMORY_LOG}{}", rook_store::history_key()), &id)?;
        Ok(id)
    }

    // ------------------------------------------------------------ maintenance

    pub fn stats(&self) -> Result<StoreStats> {
        Ok(self.store.stats()?)
    }

    /// Stored content, which is what a size cap can actually control. The redb
    /// file is not it: freed pages are reused rather than returned, so
    /// `index.redb` never shrinks and a cap on its size could never be met.
    pub fn content_bytes(&self) -> Result<u64> {
        Ok(self.store.stats()?.bytes_stored)
    }

    /// Prune to the retention policy, collect what that freed, and keep going
    /// until the size cap is met.
    ///
    /// The cap has to be enforced here rather than inside `prune`: bytes are
    /// only released by garbage collection, so nothing can tell whether it has
    /// been met until that has run.
    /// The collection `rook store gc` runs.
    ///
    /// Here rather than in the caller because the expander and the grace are
    /// what make a collection correct, and the command line was assembling its
    /// own: it took the library's ten-minute default and never the configured
    /// `gc_grace_secs`, so a store told to hold new objects for an hour
    /// collected them after ten minutes when a person asked rather than the
    /// timer.
    pub fn collect_garbage(&self, dry_run: bool) -> Result<rook_store::GcReport> {
        Ok(self.store.gc(&GcOptions {
            expand: Some(&fileset::gc_expander),
            dry_run,
            min_age_secs: self.config.storage.gc_grace_secs,
            ..Default::default()
        })?)
    }

    pub fn prune(&self, dry_run: bool) -> Result<rook_store::PruneReport> {
        Ok(self.store.prune(&self.config.storage.retention, dry_run)?)
    }

    pub fn verify(&self) -> Result<Vec<(ObjectId, String)>> {
        Ok(self.store.verify()?)
    }

    pub fn train_dictionaries(&self) -> Result<Vec<(String, usize)>> {
        Ok(self.store.train_dictionaries(
            self.config.storage.train_dictionaries_after,
            self.config.storage.dictionary_bytes,
        )?)
    }

    /// A copy of a session's first `at` events, under a new id.
    pub fn fork_session(&self, session: u128, at: u64) -> Result<rook_store::SessionMeta> {
        let meta =
            self.store.get_session(session)?.ok_or_else(|| CoreError::Other("no such session".into()))?;
        Ok(self.store.fork_session(
            session,
            rook_store::new_session_id(),
            at,
            &format!("{} @{at}", meta.title),
        )?)
    }

    /// Returns how many events went with it.
    pub fn delete_session(&self, session: u128) -> Result<u64> {
        Ok(self.store.delete_session(session)?)
    }

    pub fn maintenance(&self, dry_run: bool) -> Result<MaintenanceReport> {
        let policy = &self.config.storage.retention;
        // Before the collection, so the objects the dropped refs were holding
        // are collectable in the same pass rather than the next one.
        let history_dropped = match policy.max_history_entries {
            Some(keep) => self.trim_histories(keep, dry_run)?,
            None => 0,
        };
        // Kept copies of command output are files rather than objects, so the
        // store's own pruning never sees them — and a directory nothing empties
        // is the unbounded accumulator this codebase does not allow.
        let outputs_dropped = trim_outputs(&self.output_dir, self.config.sandbox.max_output_files, dry_run);
        let mut prune = self.store.prune(policy, dry_run)?;
        let grace = self.config.storage.gc_grace_secs;
        let mut gc = self.store.gc(&GcOptions {
            expand: Some(&fileset::gc_expander),
            dry_run,
            min_age_secs: grace,
            ..Default::default()
        })?;

        let cap = policy.max_total_bytes.unwrap_or(u64::MAX);
        let mut over_budget_by = self.content_bytes()?.saturating_sub(cap);

        if !dry_run {
            // Oldest first, in batches, re-measuring after each collection.
            // Deleting a fixed fraction and hoping was the previous behaviour,
            // and it deleted the newest sessions.
            for _ in 0..MAX_PRUNE_ROUNDS {
                if over_budget_by == 0 {
                    break;
                }
                let remaining = self.store.list_sessions()?.len();
                let batch = (remaining / 8).max(1);
                let oldest = self.store.oldest_unprotected(policy, batch)?;
                if oldest.is_empty() {
                    break;
                }
                for id in oldest {
                    prune.sessions_deleted += 1;
                    prune.events_deleted += self.store.delete_session(id)?;
                }
                let round = self.store.gc(&GcOptions {
                    expand: Some(&fileset::gc_expander),
                    min_age_secs: grace,
                    ..Default::default()
                })?;
                gc.collected += round.collected;
                gc.bytes_freed += round.bytes_freed;
                over_budget_by = self.content_bytes()?.saturating_sub(cap);
                // Deleting the oldest sessions and freeing nothing means what
                // holds the cap is not sessions — a ref pins its objects, and
                // nothing here removes refs. Continuing would spend the rest of
                // the history on a cap it cannot reach, so it stops and says so
                // through `over_budget_by` instead.
                if round.bytes_freed == 0 {
                    tracing::warn!(
                        over_budget_by,
                        "still over the size cap after deleting the oldest sessions freed \
                         nothing; what is left is held by refs, which retention does not cover"
                    );
                    break;
                }
            }
        }

        let trained = if dry_run {
            Vec::new()
        } else {
            self.store.train_dictionaries(
                self.config.storage.train_dictionaries_after,
                self.config.storage.dictionary_bytes,
            )?
        };
        Ok(MaintenanceReport {
            prune,
            gc,
            dictionaries_trained: trained,
            over_budget_by,
            history_dropped,
            outputs_dropped,
        })
    }

    /// Drop all but the newest `keep` entries of every history the agent appends
    /// to on its own.
    ///
    /// Three of them exist and they share a shape — `skill/<name>/h/<stamp>`,
    /// `memory/h/<stamp>`, `checkpoint/<name>/<stamp>` — so one rule covers all
    /// three. What it must not touch is `skill/<name>/v/<version>`, which is one
    /// ref per distinct version rather than an entry per write, and
    /// `memory/head`, which is the current state.
    ///
    /// The stamp is zero-padded, so the order `list_refs` returns is the order
    /// they were written and the newest are the tail.
    fn trim_histories(&self, keep: usize, dry_run: bool) -> Result<u64> {
        let mut by_group: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (name, _) in self.store.list_refs("")? {
            if let Some(group) = appended_history(&name) {
                by_group.entry(group.to_string()).or_default().push(name.clone());
            }
        }

        let mut dropped = 0;
        for (_, mut entries) in by_group {
            if entries.len() <= keep {
                continue;
            }
            entries.sort();
            let stale = entries.len() - keep;
            for name in entries.into_iter().take(stale) {
                dropped += 1;
                if !dry_run {
                    self.store.delete_ref(&name)?;
                }
            }
        }
        Ok(dropped)
    }
}

/// The history a ref belongs to, or `None` when it is not one.
fn appended_history(name: &str) -> Option<&str> {
    let (group, _stamp) = name.rsplit_once('/')?;
    let appended = group.starts_with("checkpoint/")
        || (group.starts_with("skill/") && group.ends_with("/h"))
        || group == "memory/h";
    appended.then_some(group)
}

/// A store that is still over its cap after this many rounds is being kept over
/// it by something a size policy cannot remove — protected sessions, or one
/// session larger than the whole budget.
const MAX_PRUNE_ROUNDS: usize = 16;

/// Result of restoring a captured version over a live directory.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rollback {
    pub restored: usize,
    /// Files present on disk that this capture does not contain. They were left
    /// alone; delete them by hand if the rollback should be exact.
    pub left_behind: Vec<String>,
    pub dir: PathBuf,
    /// The capture taken of what was there first. `None` when the skill was not
    /// on disk to begin with, which is the only case a rollback is not undoable.
    pub undo: Option<ObjectId>,
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

/// The answer to `rook skills why`: what was on offer, and what won.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillWhy {
    pub environment: rook_skills::Environment,
    pub versions: Vec<SkillCandidate>,
    /// The one that applies, as `name@version [source]`.
    pub chosen: Option<String>,
    /// Why none does, when none does.
    pub why_not: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillCandidate {
    pub id: String,
    pub source: String,
    /// Empty when this version applies here. Kept as the mismatch rather than
    /// its rendering, so a UI can lay it out and a person still reads the same
    /// sentence.
    pub mismatches: Vec<rook_skills::Mismatch>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemoryVersion {
    pub reference: String,
    pub object: String,
    pub updated_at: i64,
    pub facts: usize,
    pub note: Option<String>,
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
    /// The event a fresh turn starts replaying from — everything before it is
    /// represented by the last compaction's summary.
    pub replay_from: u64,
    pub by_kind: Vec<(String, KindUsage)>,
}

/// What a user typed where a session was wanted: an id, or `last` for the most
/// recent one in `workspace`.
///
/// `last` because an id is 26 characters of base32 that nobody remembers, and
/// the session you mean is almost always the one you were just in. Scoped to the
/// workspace: continuing a session from another project would carry its whole
/// conversation into this one.
///
/// Takes the listing rather than fetching it, so the rule is the same whether
/// the sessions came from the store or from a running daemon over HTTP.
pub fn session_named(spec: &str, workspace: &Path, sessions: &[SessionSummary]) -> Result<u128> {
    if !spec.eq_ignore_ascii_case("last") {
        return rook_store::parse_session_id(spec)
            .ok_or_else(|| CoreError::Other(format!("{spec:?} is neither a session id nor `last`")));
    }
    let here = workspace.display().to_string();
    sessions
        .iter()
        .find(|s| s.meta.workspace == here)
        .map(|s| s.meta.id)
        .ok_or_else(|| CoreError::Other(format!("no session has been started in {here} yet")))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionSummary {
    #[serde(flatten)]
    pub meta: SessionMeta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    /// The event in the parent this session was forked at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forked_at: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rewind {
    pub session: String,
    pub parent: String,
    pub events_kept: u64,
    pub checkpoints_applied: usize,
    pub files_restored: usize,
    pub files_removed: usize,
    /// Paths captured as they were just before the restore wrote over them.
    pub files_kept: usize,
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
    /// Stored bytes still above the cap once everything prunable is gone.
    pub over_budget_by: u64,
    /// History refs dropped past `max_history_entries`. Reported because each
    /// one was keeping an object alive against collection, and a store that
    /// suddenly has room should say why.
    #[serde(default)]
    pub history_dropped: u64,
    /// Kept copies of command output removed past `[sandbox] max_output_files`.
    #[serde(default)]
    pub outputs_dropped: u64,
}

/// Keep the newest `keep` files under the output directory and delete the rest.
///
/// Newest by name: the name is the nanosecond it was opened, zero-padded, so
/// sorting the names sorts them by age without asking the filesystem for times
/// it may not have.
fn trim_outputs(dir: &Path, keep: usize, dry_run: bool) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    let mut names: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    if names.len() <= keep {
        return 0;
    }
    names.sort();
    let stale = names.len() - keep;
    let mut dropped = 0;
    for path in names.into_iter().take(stale) {
        if dry_run || std::fs::remove_file(&path).is_ok() {
            dropped += 1;
        }
    }
    dropped
}

/// A skill as the agent supplies it, before it is a file.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AuthoredSkill {
    pub name: String,
    pub description: String,
    pub body: String,
    #[serde(default)]
    pub keywords: Vec<String>,
    /// What the skill needs to apply, which is how a skill stays honest about
    /// being platform- or version-specific instead of misfiring elsewhere.
    #[serde(default)]
    pub requires: rook_skills::Requirements,
    /// Files to lay down beside `SKILL.md`, by relative path.
    ///
    /// A procedure often needs a tool that does not exist yet. Writing the
    /// script and the instructions for it together is what makes the skill
    /// repeatable — instructions describing a helper nobody has are not.
    #[serde(default)]
    pub files: BTreeMap<String, String>,
}

impl AuthoredSkill {
    fn to_skill_md(&self) -> Result<String> {
        let manifest = rook_skills::SkillManifest {
            name: self.name.trim().to_string(),
            description: self.description.replace('\n', " "),
            version: "0.1.0".into(),
            keywords: self.keywords.clone(),
            requires: self.requires.clone(),
            license: None,
            allowed_tools: Vec::new(),
            variants: Vec::new(),
            supersedes: Vec::new(),
            extra: Default::default(),
        };
        Ok(manifest.to_skill_md(&self.body)?)
    }
}

/// A shebang is the author saying how the file is meant to be run, so it is
/// made runnable. Nothing else is: a template or a data file has no business
/// being executable.
#[cfg(unix)]
fn make_runnable(path: &Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;
    if contents.starts_with("#!") {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).ok();
    }
}

#[cfg(not(unix))]
fn make_runnable(_path: &Path, _contents: &str) {}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn unprobed(dir: &Path) -> Rook {
        let (skills, _) = SkillIndex::discover(&[]);
        Rook {
            store: Store::open(dir).unwrap().into(),
            config: Config::default(),
            env: OnceLock::new(),
            skills: skills.into(),
            workspace: dir.to_path_buf(),
            skill_errors: Vec::new(),
            plugins: Vec::new(),
            output_dir: dir.join("output"),
            writing: Default::default(),
            touched: Default::default(),
        }
    }

    /// Detection spawns sixteen processes — `java -version` starts a JVM — and
    /// took longer than the whole of `session ls`, which never asks.
    #[test]
    fn nothing_that_ignores_skills_pays_for_probing_the_machine() {
        let dir = tempfile::tempdir().unwrap();
        let rook = unprobed(dir.path());

        let session = rook.start_session("no skills involved").unwrap();
        rook.set_goal(session, "prove the probes stay unspawned").unwrap();
        rook.sessions().unwrap();
        rook.stats().unwrap();
        rook.transcript(session, 0, 10, 100).unwrap();
        assert!(rook.env.get().is_none(), "the machine was probed for a command that ignores it");

        rook.catalog();
        assert!(rook.env.get().is_some(), "and probed once something has to be resolved against it");
    }
}
