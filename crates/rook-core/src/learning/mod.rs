//! Reviewable knowledge changes anchored to a completed, retained task.
mod apply;
mod produce;
mod source;
use crate::shell_undo::{File, Snapshot};
use crate::{CoreError, Result, Rook, Vault};
use rook_store::ObjectId;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
static CHANGES: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub automatic: bool,
    pub auto_apply_memory: bool,
    pub max_proposals: usize,
    pub max_attempts: usize,
    pub state_bytes: usize,
    pub source_bytes: usize,
    pub source_events: usize,
    pub output_bytes: usize,
    pub timeout_secs: u64,
    pub memory_bytes: usize,
    pub skill_files: usize,
    pub skill_entries: usize,
    pub skill_file_bytes: usize,
    pub skill_total_bytes: usize,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            automatic: false,
            auto_apply_memory: false,
            max_proposals: 32,
            max_attempts: 32,
            state_bytes: 2 * 1024 * 1024,
            source_bytes: 65536,
            source_events: 128,
            output_bytes: 16384,
            timeout_secs: 60,
            memory_bytes: 2 * 1024 * 1024,
            skill_files: 64,
            skill_entries: 512,
            skill_file_bytes: 262144,
            skill_total_bytes: 1024 * 1024,
        }
    }
}
impl Settings {
    pub(crate) fn errors(&self) -> Vec<String> {
        let mut errors = Vec::new();
        for (name, value, min, max) in [
            ("max_proposals", self.max_proposals, 1, 64),
            ("max_attempts", self.max_attempts, 1, 64),
            ("state_bytes", self.state_bytes, 65536, 4 * 1024 * 1024),
            ("source_bytes", self.source_bytes, 4096, 262144),
            ("source_events", self.source_events, 1, 512),
            ("output_bytes", self.output_bytes, 1024, 65536),
            ("memory_bytes", self.memory_bytes, 65536, 8 * 1024 * 1024),
        ] {
            if !(min..=max).contains(&value) {
                errors.push(format!("learning.{name}: expected {min}..={max}"));
            }
        }
        if !(1..=300).contains(&self.timeout_secs) {
            errors.push("learning.timeout_secs: expected 1..=300".into());
        }
        errors.extend(
            self.capture().errors().into_iter().map(|e| e.replace("shell_undo.", "learning skill capture ")),
        );
        errors
    }
    fn capture(&self) -> crate::shell_undo::Settings {
        crate::shell_undo::Settings {
            max_files: self.skill_files,
            max_entries: self.skill_entries,
            file_bytes: self.skill_file_bytes,
            total_bytes: self.skill_total_bytes,
            state_bytes: self.state_bytes,
            ..Default::default()
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Target {
    Memory {
        text: String,
        #[serde(default)]
        supersedes: Option<String>,
    },
    Skill {
        skill: Box<crate::AuthoredSkill>,
        #[serde(default)]
        remove: Vec<String>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Draft {
    pub target: Target,
    pub reason: String,
    pub evidence: Vec<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    List,
    Source { result: u64 },
    Suggest { result: u64 },
    Propose { result: u64, draft: Draft },
    Revise { proposal: String, draft: Draft },
    Inspect { proposal: String },
    Prepare { proposal: String },
    Apply { proposal: String, token: String },
    PrepareRollback { proposal: String },
    Rollback { proposal: String, token: String },
    Reject { proposal: String, note: String },
    Recover { proposal: String, note: String },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Evidence {
    seq: u64,
    kind: String,
    object: String,
    text: String,
    truncated: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Source {
    result: u64,
    turn: String,
    result_object: String,
    signature: String,
    events: Vec<Evidence>,
    omitted_events: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Proposal {
    id: String,
    predecessor: Option<String>,
    draft: Draft,
    source: Source,
    state: String,
    error: Option<String>,
    note: Option<String>,
    effect: Option<Effect>,
    original_effect: Option<Effect>,
    operations: usize,
    session: Option<String>,
    pending: Option<String>,
    completed: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Effect {
    Memory {
        before: Option<crate::memory::Fact>,
        after: crate::memory::Fact,
    },
    Skill {
        name: String,
        directory: String,
        before: Snapshot,
        after: Snapshot,
        changed: Vec<String>,
        versions: Vec<String>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Attempt {
    result: u64,
    state: String,
    session: String,
    error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Book {
    version: u32,
    workspace: String,
    proposals: Vec<Proposal>,
    attempts: Vec<Attempt>,
}
fn bad(message: impl Into<String>) -> CoreError {
    CoreError::Other(message.into())
}
fn key(owner: u128) -> String {
    format!("learning/{owner:032x}")
}
fn hash<T: Serialize>(value: &T, maximum: usize) -> Result<String> {
    crate::shell_undo::hash(value, maximum)
}
fn validate_draft(d: &Draft, s: &Settings) -> Result<()> {
    if d.reason.trim().is_empty() || d.reason.len() > 4096 || d.evidence.is_empty() || d.evidence.len() > 16 {
        return Err(bad("learning needs a reason of 1..4096 bytes and 1..16 source event anchors"));
    }
    match &d.target {
        Target::Memory { text, supersedes } => {
            if text.trim().is_empty()
                || text.len() > 8192
                || supersedes
                    .as_ref()
                    .is_some_and(|id| id.len() != 8 || !id.bytes().all(|b| b.is_ascii_hexdigit()))
            {
                return Err(bad("memory proposal needs text of 1..8192 bytes and an optional exact fact ID"));
            }
        }
        Target::Skill { skill, remove } => {
            if !rook_skills::usable_name(&skill.name)
                || skill.description.trim().is_empty()
                || skill.description.len() > 512
                || skill.body.len() > 1200
                || skill.body.trim().is_empty()
                || skill.files.len() > s.skill_files
                || remove.len() > s.skill_files
                || skill.keywords.len() > 32
                || skill.keywords.iter().any(|k| k.len() > 128)
                || skill.files.keys().chain(remove).any(|p| !relative(p) || p == "SKILL.md")
            {
                return Err(bad(
                    "skill proposal needs a valid name, description, a 1..1200 byte body and bounded literal relative file paths",
                ));
            }
            let card = skill.to_skill_md()?;
            rook_skills::parse_manifest(&card, std::path::Path::new("SKILL.md"))?;
            let total = skill.files.values().try_fold(card.len(), |sum, v| {
                if v.len() > s.skill_file_bytes {
                    return Err(bad("learning.skill_file_bytes exceeded"));
                }
                Ok(sum.saturating_add(v.len()))
            })?;
            if total > s.skill_total_bytes {
                return Err(bad("learning.skill_total_bytes exceeded"));
            }
            if remove.iter().any(|p| skill.files.contains_key(p)) {
                return Err(bad("a skill path cannot be both replaced and removed"));
            }
        }
    }
    Ok(())
}
fn relative(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 1024
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path.split('/').all(|p| !p.is_empty() && p != "." && p != "..")
        && std::path::Path::new(path).components().all(|p| matches!(p, std::path::Component::Normal(_)))
}
fn load(rook: &Rook, owner: u128) -> Result<Book> {
    let s = &rook.config.learning;
    if !s.errors().is_empty() {
        return Err(bad("invalid learning configuration; run rook config check"));
    }
    let meta =
        rook.store.get_session_limited(owner, 256 * 1024)?.ok_or_else(|| bad("learning owner missing"))?;
    let root = rook.workspace.canonicalize().map_err(|e| bad(e.to_string()))?;
    if std::path::Path::new(&meta.workspace).canonicalize().ok().as_ref() != Some(&root) {
        return Err(bad("learning owner belongs to another workspace"));
    }
    let Some(bytes) = rook.store.kv_get_limited(&key(owner), s.state_bytes)? else {
        return Ok(Book {
            version: 1,
            workspace: root.display().to_string(),
            proposals: Vec::new(),
            attempts: Vec::new(),
        });
    };
    let b: Book = serde_json::from_slice(&bytes)?;
    if b.version != 1
        || b.workspace != root.display().to_string()
        || b.proposals.len() > s.max_proposals
        || b.attempts.len() > s.max_attempts
    {
        return Err(bad("learning state version, workspace or count mismatch"));
    }
    for p in &b.proposals {
        validate_draft(&p.draft, s)?;
        source::validate(&p.source, s)?;
        if rook_store::parse_session_id(&p.id).is_none()
            || !matches!(
                p.state.as_str(),
                "proposed"
                    | "applying"
                    | "applied"
                    | "partial"
                    | "rolling_back"
                    | "rolled_back"
                    | "rejected"
                    | "rollback_partial"
            )
            || p.error.as_ref().is_some_and(|v| v.len() > 4096)
            || p.note.as_ref().is_some_and(|v| v.len() > 4096)
            || p.completed.len() > 2 * s.skill_files
            || p.completed.iter().any(|v| !relative(v))
            || p.operations > 8
            || p.pending.as_ref().is_some_and(|v| !relative(v))
        {
            return Err(bad("invalid learning proposal receipt"));
        }
        apply::validate_effect(p, s)?;
    }
    if b.attempts.iter().any(|a| {
        !matches!(a.state.as_str(), "started" | "finished" | "failed")
            || a.error.as_ref().is_some_and(|e| e.len() > 4096)
            || rook_store::parse_session_id(&a.session).is_none()
    }) {
        return Err(bad("invalid learning producer receipt"));
    }
    Ok(b)
}
fn encode(rook: &Rook, b: &Book) -> Result<Vec<u8>> {
    crate::persistence::encode_with_limit(b, rook.config.learning.state_bytes)
}
fn save(rook: &Rook, owner: u128, b: &Book) -> Result<()> {
    let bytes = encode(rook, b)?;
    rook.store
        .kv_update_session(owner, &key(owner), rook.config.learning.state_bytes, None, |_| Ok(bytes))?;
    Ok(())
}
fn position(b: &Book, id: &str) -> Result<usize> {
    b.proposals.iter().position(|p| p.id == id).ok_or_else(|| bad("proposal does not belong to this session"))
}
fn show(p: &Proposal) -> Value {
    json!(p)
}
pub fn arguments(text: &str) -> Result<Request> {
    if text.len() > 65536 {
        return Err(bad("learning request exceeds 64 KiB"));
    }
    if text.trim().is_empty() { Ok(Request::List) } else { Ok(serde_json::from_str(text)?) }
}
impl Rook {
    pub async fn learning_request(&self, owner: u128, request: Request, vault: Arc<Vault>) -> Result<Value> {
        if let Request::Suggest { result } = request {
            return self.suggest_learning(owner, result, vault).await;
        }
        let _changing = CHANGES.lock().unwrap_or_else(|e| e.into_inner());
        let mut book = load(self, owner)?;
        match request {
            Request::List => Ok(
                json!({"items":book.proposals.iter().map(show).collect::<Vec<_>>(),"attempts":book.attempts}),
            ),
            Request::Source { result } => Ok(json!(source::capture(self, owner, result, &vault)?)),
            Request::Propose { result, mut draft } => {
                match &mut draft.target {
                    Target::Memory { text, .. } => *text = vault.redact(text),
                    Target::Skill { skill, .. } => {
                        skill.body = vault.redact(&skill.body);
                        skill.description = vault.redact(&skill.description);
                        for text in skill.files.values_mut() {
                            *text = vault.redact(text);
                        }
                    }
                }
                draft.reason = vault.redact(&draft.reason);
                validate_draft(&draft, &self.config.learning)?;
                let source = source::capture(self, owner, result, &vault)?;
                for seq in &draft.evidence {
                    if !source.events.iter().any(|e| e.seq == *seq && !e.truncated) {
                        return Err(bad(
                            "proposal evidence must name an available complete user/tool source in this finished turn",
                        ));
                    }
                }
                let identity = hash(&draft.target, self.config.learning.state_bytes)?;
                if let Some(p) = book.proposals.iter().find(|p| {
                    hash(&p.draft.target, self.config.learning.state_bytes).ok().as_ref() == Some(&identity)
                        && p.state != "rejected"
                        && p.state != "rolled_back"
                }) {
                    return Ok(json!({"duplicate":p.id,"proposal":show(p)}));
                }
                if book.proposals.len() >= self.config.learning.max_proposals {
                    return Err(bad(
                        "learning.max_proposals reached; retain these records and use a new owning session",
                    ));
                }
                let id = rook_store::format_session_id(rook_store::new_session_id());
                book.proposals.push(Proposal {
                    id: id.clone(),
                    predecessor: None,
                    draft,
                    source,
                    state: "proposed".into(),
                    error: None,
                    note: None,
                    effect: None,
                    original_effect: None,
                    operations: 0,
                    session: None,
                    pending: None,
                    completed: Vec::new(),
                });
                encode(self, &book)?;
                save(self, owner, &book)?;
                Ok(show(&book.proposals[position(&book, &id)?]))
            }
            Request::Revise { proposal, mut draft } => {
                let index = position(&book, &proposal)?;
                if book.proposals[index].state != "proposed" {
                    return Err(bad("only an unapplied proposal may be revised"));
                }
                if book.proposals.len() >= self.config.learning.max_proposals {
                    return Err(bad(
                        "learning.max_proposals reached; retain revision history and use a new session",
                    ));
                }
                draft.reason = vault.redact(&draft.reason);
                match &mut draft.target {
                    Target::Memory { text, .. } => *text = vault.redact(text),
                    Target::Skill { skill, .. } => {
                        skill.body = vault.redact(&skill.body);
                        skill.description = vault.redact(&skill.description);
                        for text in skill.files.values_mut() {
                            *text = vault.redact(text);
                        }
                    }
                }
                validate_draft(&draft, &self.config.learning)?;
                let source = book.proposals[index].source.clone();
                if draft
                    .evidence
                    .iter()
                    .any(|seq| !source.events.iter().any(|e| e.seq == *seq && !e.truncated))
                {
                    return Err(bad("revision evidence must remain bound to the retained complete sources"));
                }
                let id = rook_store::format_session_id(rook_store::new_session_id());
                let mut next = book.proposals[index].clone();
                next.id = id.clone();
                next.predecessor = Some(proposal.clone());
                next.draft = draft;
                book.proposals[index].state = "rejected".into();
                book.proposals[index].note = Some(format!("Superseded by reviewed revision {id}"));
                book.proposals.push(next);
                encode(self, &book)?;
                save(self, owner, &book)?;
                Ok(show(&book.proposals[position(&book, &id)?]))
            }
            Request::Inspect { proposal } => Ok(show(&book.proposals[position(&book, &proposal)?])),
            Request::Prepare { proposal } => apply::prepare(self, owner, &book, &proposal, false),
            Request::PrepareRollback { proposal } => apply::prepare(self, owner, &book, &proposal, true),
            Request::Apply { proposal, token } => {
                apply::apply(self, owner, &mut book, &proposal, &token, false)
            }
            Request::Rollback { proposal, token } => {
                apply::apply(self, owner, &mut book, &proposal, &token, true)
            }
            Request::Reject { proposal, note } => {
                if note.len() > 4096 {
                    return Err(bad("rejection note exceeds 4096 bytes"));
                }
                let index = position(&book, &proposal)?;
                let p = &mut book.proposals[index];
                if p.state != "proposed" {
                    return Err(bad("only an unapplied proposal may be rejected"));
                }
                p.state = "rejected".into();
                p.note = Some(vault.redact(&note));
                save(self, owner, &book)?;
                Ok(show(&book.proposals[position(&book, &proposal)?]))
            }
            Request::Recover { proposal, note } => {
                apply::recover(self, owner, &mut book, &proposal, &vault.redact(&note))
            }
            Request::Suggest { .. } => unreachable!("suggestion handled before taking the mutation lock"),
        }
    }
}
