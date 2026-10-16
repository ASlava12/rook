//! Frozen review sources and transactional, source-bound comments.
use crate::{CoreError, Result, Rook};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(crate) const TOOL: &str = "review_changes";
pub(crate) const READ_TOOL: &str = "review_read";
const VERSION: u32 = 1;

/// Compact frontend result; original source bodies remain separately paged.
pub fn summary(review: &Review) -> serde_json::Value {
    serde_json::json!({"id":review.id,"source":review.source,"selection":review.snapshot.selection,
        "state":review.state,"reviewer":review.reviewer,"findings":review.findings,"error":review.error,
        "comments":review.comments,"files":review.snapshot.files.iter().map(|file|
            serde_json::json!({"path":file.path,"diff_bytes":file.diff.len(),"before_hash":file.before_hash,"after_hash":file.after_hash,"binary":file.binary()})).collect::<Vec<_>>()})
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub max_reviews: usize,
    pub max_files: usize,
    pub file_bytes: usize,
    pub snapshot_bytes: usize,
    pub state_bytes: usize,
    pub page_bytes: usize,
    pub max_comments: usize,
    pub comment_bytes: usize,
    pub max_findings: usize,
    pub max_steps: u32,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            max_reviews: 8,
            max_files: 32,
            file_bytes: 65536,
            snapshot_bytes: 524288,
            state_bytes: 4194304,
            page_bytes: 16384,
            max_comments: 64,
            comment_bytes: 4096,
            max_findings: 32,
            max_steps: 12,
        }
    }
}
impl Settings {
    pub(crate) fn errors(&self) -> Vec<String> {
        [
            ("max_reviews", self.max_reviews, 1, 32),
            ("max_files", self.max_files, 1, 128),
            ("file_bytes", self.file_bytes, 1024, 262144),
            ("snapshot_bytes", self.snapshot_bytes, 16384, 2097152),
            ("state_bytes", self.state_bytes, 65536, 8388608),
            ("page_bytes", self.page_bytes, 1024, 65536),
            ("max_comments", self.max_comments, 1, 256),
            ("comment_bytes", self.comment_bytes, 1, 8192),
            ("max_findings", self.max_findings, 1, 128),
            ("max_steps", self.max_steps as usize, 1, 32),
        ]
        .into_iter()
        .filter(|(_, value, low, high)| !(*low..=*high).contains(value))
        .map(|(name, _, low, high)| format!("review.{name}: expected {low}..={high}"))
        .collect()
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    #[default]
    Working,
    Commit,
    Base,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Selection {
    pub target: Target,
    pub revision: Option<String>,
    pub paths: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    List,
    Capture {
        #[serde(default)]
        selection: Selection,
    },
    Inspect {
        review: String,
    },
    Source {
        review: String,
        path: String,
        side: Side,
        #[serde(default)]
        offset: usize,
    },
    Diff {
        review: String,
        path: String,
        #[serde(default)]
        offset: usize,
    },
    Comment {
        review: String,
        comment: CommentRequest,
    },
    Feedback {
        review: String,
        source: String,
    },
}
/// Slash commands and the API share the same bounded request shape.
pub fn arguments(text: &str) -> Result<Request> {
    if text.len() > 65536 {
        return Err(bad("review request exceeds 64 KiB"));
    }
    if text.trim().is_empty() {
        return Ok(Request::Capture { selection: Selection::default() });
    }
    serde_json::from_str(text).map_err(Into::into)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Before,
    After,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct File {
    pub path: String,
    pub diff: String,
    pub before: Option<String>,
    pub after: Option<String>,
    pub before_hash: Option<String>,
    pub after_hash: Option<String>,
}
impl File {
    /// Hashes without retained text identify omitted binary content while keeping
    /// the stored snapshot shape readable by previous versions.
    pub fn binary(&self) -> bool {
        self.before.is_none()
            && self.after.is_none()
            && (self.before_hash.is_some() || self.after_hash.is_some())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub selection: Selection,
    pub workspace: String,
    pub base: Option<String>,
    pub head: Option<String>,
    pub files: Vec<File>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub severity: Severity,
    pub path: String,
    pub side: Side,
    pub line: u32,
    pub end_line: u32,
    pub title: String,
    pub body: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub source: String,
    pub path: String,
    pub side: Side,
    pub line: u32,
    pub text: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommentRequest {
    pub source: String,
    pub path: String,
    pub side: Side,
    pub line: u32,
    pub text: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Ready,
    Started,
    Completed,
    Failed,
    Interrupted,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourcePage {
    pub source: String,
    pub path: String,
    pub side: Side,
    pub offset: usize,
    pub first_line: usize,
    pub text: String,
    pub next: Option<usize>,
}

pub(crate) fn page(
    snapshot: &Snapshot,
    source: &str,
    path: &str,
    side: Side,
    offset: usize,
    limit: usize,
) -> Result<SourcePage> {
    let text = file_text(snapshot, path, side)?;
    if offset > text.len() || !text.is_char_boundary(offset) || limit < 4 {
        return Err(bad("source offset must be a UTF-8 boundary; limit must be at least four bytes"));
    }
    let mut end = offset.saturating_add(limit).min(text.len());
    while end > offset && !text.is_char_boundary(end) {
        end -= 1;
    }
    Ok(SourcePage {
        source: source.into(),
        path: path.into(),
        side,
        offset,
        first_line: 1 + text.as_bytes()[..offset].iter().filter(|&&byte| byte == b'\n').count(),
        text: text[offset..end].into(),
        next: (end < text.len()).then_some(end),
    })
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Review {
    pub id: String,
    pub source: String,
    pub snapshot: Snapshot,
    pub state: RunState,
    pub reviewer: Option<String>,
    pub findings: Vec<Finding>,
    pub error: Option<String>,
    pub comments: Vec<Comment>,
}
#[derive(Serialize, Deserialize)]
struct Saved {
    version: u32,
    workspace: String,
    items: Vec<Review>,
}

fn bad(message: impl Into<String>) -> CoreError {
    CoreError::Other(message.into())
}
fn identity(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn key(session: u128) -> String {
    format!("review/{session:032x}")
}
fn workspace(rook: &Rook) -> Result<String> {
    Ok(rook.workspace.canonicalize().map_err(|error| bad(error.to_string()))?.display().to_string())
}
fn decode(bytes: Option<&[u8]>, root: &str, limits: Settings) -> Result<Saved> {
    let saved = match bytes {
        None => Saved { version: VERSION, workspace: root.into(), items: Vec::new() },
        Some(bytes) => {
            if bytes.len() > limits.state_bytes {
                return Err(bad("review state exceeds review.state_bytes"));
            }
            serde_json::from_slice::<Saved>(bytes)?
        }
    };
    if saved.version != VERSION || saved.workspace != root || saved.items.len() > limits.max_reviews {
        return Err(bad("unsupported review state or different workspace"));
    }
    for item in &saved.items {
        let bytes = crate::persistence::encode_with_limit(&item.snapshot, limits.snapshot_bytes)?;
        if identity(&bytes) != item.source
            || item.snapshot.workspace != root
            || item.snapshot.files.len() > limits.max_files
            || item.comments.len() > limits.max_comments
            || item.findings.len() > limits.max_findings
        {
            return Err(bad("review source checksum or configured bounds do not match"));
        }
        let mut paths = std::collections::BTreeSet::new();
        for file in &item.snapshot.files {
            if !valid_path(&file.path)
                || !paths.insert(&file.path)
                || file.before.as_ref().is_some_and(|text| text.len() > limits.file_bytes)
                || file.after.as_ref().is_some_and(|text| text.len() > limits.file_bytes)
            {
                return Err(bad("unsupported retained review source"));
            }
        }
        for comment in &item.comments {
            if comment.source != item.source || comment.text.len() > limits.comment_bytes {
                return Err(bad("unsupported retained review comment"));
            }
            anchor(&item.snapshot, &comment.path, comment.side, comment.line, comment.line)?;
        }
    }
    Ok(saved)
}
fn saved(rook: &Rook, session: u128) -> Result<Saved> {
    if !rook.config.review.errors().is_empty() {
        return Err(bad("invalid review limits; run rook config check"));
    }
    let meta = rook.store.get_session(session)?.ok_or_else(|| bad("review owner session is missing"))?;
    if std::path::Path::new(&meta.workspace).canonicalize().ok().as_ref()
        != Some(&rook.workspace.canonicalize().map_err(|error| bad(error.to_string()))?)
    {
        return Err(bad("review owner belongs to another workspace"));
    }
    let bytes = rook.store.kv_get_limited(&key(session), rook.config.review.state_bytes)?;
    decode(bytes.as_deref(), &workspace(rook)?, rook.config.review)
}
fn update(rook: &Rook, session: u128, change: impl FnOnce(&mut Saved) -> Result<()>) -> Result<Saved> {
    // Check workspace membership before mutation; deletion is checked again in
    // the same transaction as the companion write.
    saved(rook, session)?;
    let root = workspace(rook)?;
    let limits = rook.config.review;
    let encoded = rook.store.kv_update_session(session, &key(session), limits.state_bytes, None, |old| {
        let mut state = decode(old, &root, limits)
            .map_err(|error| rook_store::StoreError::Encoding(error.to_string()))?;
        change(&mut state).map_err(|error| rook_store::StoreError::Encoding(error.to_string()))?;
        let bytes = crate::persistence::encode_with_limit(&state, limits.state_bytes)
            .map_err(|error| rook_store::StoreError::Encoding(error.to_string()))?;
        // Reject invalid mutated state before committing the companion.
        decode(Some(&bytes), &root, limits)
            .map_err(|error| rook_store::StoreError::Encoding(error.to_string()))?;
        Ok(bytes)
    })?;
    decode(Some(&encoded), &root, limits)
}
fn selected<'a>(state: &'a Saved, id: &str) -> Result<&'a Review> {
    state.items.iter().find(|item| item.id == id).ok_or_else(|| bad("review not found in this session"))
}
fn selected_mut<'a>(state: &'a mut Saved, id: &str) -> Result<&'a mut Review> {
    state.items.iter_mut().find(|item| item.id == id).ok_or_else(|| bad("review not found in this session"))
}
fn file_text<'a>(snapshot: &'a Snapshot, path: &str, side: Side) -> Result<&'a str> {
    let file = snapshot
        .files
        .iter()
        .find(|file| file.path == path)
        .ok_or_else(|| bad("file is outside reviewed source"))?;
    if file.binary() {
        return Err(bad("binary content is not retained as text; inspect the frozen hash metadata"));
    }
    match side {
        Side::Before => file.before.as_deref(),
        Side::After => file.after.as_deref(),
    }
    .ok_or_else(|| bad("this side of the reviewed file does not exist"))
}
fn anchor(snapshot: &Snapshot, path: &str, side: Side, line: u32, end: u32) -> Result<()> {
    let text = file_text(snapshot, path, side)?;
    if line == 0 || end < line || end as usize > text.lines().count() {
        return Err(bad("line anchor is outside the reviewed source"));
    }
    Ok(())
}
fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 1024
        && !path.chars().any(char::is_control)
        && std::path::Path::new(path).components().all(|part| matches!(part, std::path::Component::Normal(_)))
}

impl Rook {
    /// Common source/comment surface used by local and remote frontends.
    pub async fn review_request(
        &self,
        session: u128,
        request: Request,
        vault: &crate::Vault,
    ) -> Result<serde_json::Value> {
        match request {
            Request::List => Ok(
                serde_json::json!({"items":saved(self,session)?.items.iter().map(summary).collect::<Vec<_>>() }),
            ),
            Request::Capture { selection } => {
                Ok(summary(&self.review_capture(session, selection, vault).await?))
            }
            Request::Inspect { review } => Ok(summary(&self.review_get(session, &review)?)),
            Request::Source { review, path, side, offset } => Ok(serde_json::to_value(self.review_source(
                session,
                &review,
                &path,
                side,
                offset,
                self.config.review.page_bytes,
            )?)?),
            Request::Diff { review, path, offset } => {
                let saved = self.review_get(session, &review)?;
                let file = saved
                    .snapshot
                    .files
                    .iter()
                    .find(|file| file.path == path)
                    .ok_or_else(|| bad("file is not part of this saved source"))?;
                if offset > file.diff.len() || !file.diff.is_char_boundary(offset) {
                    return Err(bad("invalid diff byte offset"));
                }
                let mut end = offset.saturating_add(self.config.review.page_bytes).min(file.diff.len());
                while !file.diff.is_char_boundary(end) {
                    end -= 1;
                }
                Ok(
                    serde_json::json!({"review":review,"source":saved.source,"path":path,"offset":offset,"next":(end<file.diff.len()).then_some(end),"diff": &file.diff[offset..end],"files":saved.snapshot.files.iter().map(|file|file.path.as_str()).collect::<Vec<_>>(),"before_hash":file.before_hash,"after_hash":file.after_hash,"binary":file.binary()}),
                )
            }
            Request::Comment { review, comment } => {
                Ok(summary(&self.review_comment(session, &review, comment, vault)?))
            }
            Request::Feedback { review, source } => {
                Ok(serde_json::json!({"prompt":self.review_feedback(session,&review,&source)?}))
            }
        }
    }
    /// Inspect immutable sources and their comments without reading current files.
    pub fn review_get(&self, session: u128, id: &str) -> Result<Review> {
        Ok(selected(&saved(self, session)?, id)?.clone())
    }

    /// Read a byte page from the exact retained source, never a newer file.
    pub fn review_source(
        &self,
        session: u128,
        id: &str,
        path: &str,
        side: Side,
        offset: usize,
        limit: usize,
    ) -> Result<SourcePage> {
        let review = self.review_get(session, id)?;
        page(
            &review.snapshot,
            &review.source,
            path,
            side,
            offset,
            limit.clamp(4, self.config.review.page_bytes),
        )
    }

    /// Return scoped user corrections with the source they refer to.
    pub fn review_feedback(&self, session: u128, id: &str, source: &str) -> Result<String> {
        let review = self.review_get(session, id)?;
        if source != review.source {
            return Err(bad("feedback must name the inspected review source"));
        }
        let comments =
            crate::persistence::encode_with_limit(&review.comments, self.config.review.state_bytes)?;
        let comments = std::str::from_utf8(&comments).map_err(|error| bad(error.to_string()))?;
        Ok(format!(
            "User corrections for review {} of retained source {}. These anchors refer to that snapshot; inspect current code before changing it.\n{}",
            review.id,
            review.source,
            crate::sources::data("review comments", &review.source, comments)
        ))
    }

    /// Add a user comment transactionally, bound to an exact source identity.
    pub fn review_comment(
        &self,
        session: u128,
        id: &str,
        request: CommentRequest,
        vault: &crate::Vault,
    ) -> Result<Review> {
        let limits = self.config.review;
        if request.text.trim().is_empty() || request.text.len() > limits.comment_bytes {
            return Err(bad("comment must be nonempty and within review.comment_bytes"));
        }
        let text = vault.redact(&request.text);
        let state = update(self, session, |state| {
            let review = selected_mut(state, id)?;
            if request.source != review.source {
                return Err(bad("comment source changed; inspect this review before commenting"));
            }
            anchor(&review.snapshot, &request.path, request.side, request.line, request.line)?;
            if review.comments.len() >= limits.max_comments {
                return Err(bad("review.max_comments reached"));
            }
            review.comments.push(Comment {
                id: rook_store::format_session_id(rook_store::new_session_id()),
                source: review.source.clone(),
                path: request.path,
                side: request.side,
                line: request.line,
                text,
            });
            Ok(())
        })?;
        Ok(selected(&state, id)?.clone())
    }

    /// Freeze a working diff, one commit, or the branch changes against a base.
    pub async fn review_capture(
        &self,
        session: u128,
        selection: Selection,
        vault: &crate::Vault,
    ) -> Result<Review> {
        let limits = self.config.review;
        if !limits.errors().is_empty() {
            return Err(bad("invalid review limits; run rook config check"));
        }
        let state = saved(self, session)?;
        if state.items.len() >= limits.max_reviews {
            return Err(bad("review.max_reviews reached; start a new session"));
        }
        if selection.paths.len() > limits.max_files || selection.paths.iter().any(|path| !valid_path(path)) {
            return Err(bad("review paths must be bounded relative file names without traversal"));
        }
        if selection.revision.as_ref().is_some_and(|revision| {
            revision.is_empty()
                || revision.len() > 256
                || revision.starts_with('-')
                || revision.chars().any(char::is_control)
        }) {
            return Err(bad("review revision must be a bounded Git revision, not an option"));
        }
        let snapshot =
            tokio::time::timeout(std::time::Duration::from_secs(60), capture(self, selection, vault))
                .await
                .map_err(|_| bad("review capture exceeded 60 seconds; narrow the selected paths"))??;
        let encoded = crate::persistence::encode_with_limit(&snapshot, limits.snapshot_bytes)?;
        let review = Review {
            id: rook_store::format_session_id(rook_store::new_session_id()),
            source: identity(&encoded),
            snapshot,
            state: RunState::Ready,
            reviewer: None,
            findings: Vec::new(),
            error: None,
            comments: Vec::new(),
        };
        let retained = review.clone();
        update(self, session, |state| {
            if state.items.len() >= limits.max_reviews {
                return Err(bad("review.max_reviews reached"));
            }
            state.items.push(retained);
            Ok(())
        })?;
        Ok(review)
    }
}

pub(crate) fn begin(rook: &Rook, session: u128, id: &str, reviewer: u128) -> Result<()> {
    update(rook, session, |state| {
        let review = selected_mut(state, id)?;
        if matches!(review.state, RunState::Started) {
            return Err(bad("review has a started run; inspect its reviewer session before retrying"));
        }
        review.state = RunState::Started;
        review.reviewer = Some(rook_store::format_session_id(reviewer));
        review.error = None;
        review.findings.clear();
        Ok(())
    })?;
    Ok(())
}
pub(crate) fn finish(
    rook: &Rook,
    session: u128,
    id: &str,
    findings: std::result::Result<Vec<Finding>, String>,
) -> Result<Review> {
    let state = update(rook, session, |state| {
        let review = selected_mut(state, id)?;
        match findings {
            Ok(findings) => {
                if findings.len() > rook.config.review.max_findings {
                    return Err(bad("review.max_findings reached"));
                }
                for finding in &findings {
                    anchor(&review.snapshot, &finding.path, finding.side, finding.line, finding.end_line)?;
                    if finding.end_line - finding.line > 9
                        || finding.title.is_empty()
                        || finding.title.len() > 256
                        || finding.body.is_empty()
                        || finding.body.len() > rook.config.review.comment_bytes
                    {
                        return Err(bad("review finding text or line range exceeds its bound"));
                    }
                }
                review.findings = findings;
                review.state = RunState::Completed;
            }
            Err(error) => {
                if error.len() > rook.config.review.comment_bytes {
                    return Err(bad("review failure text exceeds its bound"));
                }
                review.error = Some(error);
                review.state = RunState::Failed;
            }
        }
        Ok(())
    })?;
    Ok(selected(&state, id)?.clone())
}

pub(crate) struct RunGuard<'a> {
    rook: &'a Rook,
    owner: u128,
    review: String,
    open: bool,
}
impl<'a> RunGuard<'a> {
    pub(crate) fn new(rook: &'a Rook, owner: u128, review: &str) -> Self {
        Self { rook, owner, review: review.into(), open: true }
    }
    pub(crate) fn complete(&mut self) {
        self.open = false;
    }
}
impl Drop for RunGuard<'_> {
    fn drop(&mut self) {
        if self.open {
            let _ = update(self.rook, self.owner, |state| {
                let review = selected_mut(state, &self.review)?;
                if matches!(review.state, RunState::Started) {
                    review.state = RunState::Interrupted;
                }
                Ok(())
            });
        }
    }
}

async fn git(rook: &Rook, args: &[&str]) -> Result<String> {
    git_limit(rook, args, 256 * 1024).await
}
async fn git_limit(rook: &Rook, args: &[&str], maximum: usize) -> Result<String> {
    String::from_utf8(git_data(rook, args, maximum).await?)
        .map_err(|_| bad("review requires UTF-8 file names"))
}
async fn git_data(rook: &Rook, args: &[&str], maximum: usize) -> Result<Vec<u8>> {
    let mut safe =
        vec!["--literal-pathspecs", "-c", "core.fsmonitor=false", "-c", "core.untrackedCache=false"];
    safe.extend_from_slice(args);
    crate::worktrees::git_bytes(&rook.workspace, &safe, maximum).await
}
async fn revision(rook: &Rook, value: &str) -> Result<String> {
    let spec = format!("{value}^{{commit}}");
    Ok(git(rook, &["rev-parse", "--verify", &spec]).await?.trim().into())
}
async fn blob(rook: &Rook, revision: Option<&str>, path: &str) -> Result<Option<Vec<u8>>> {
    let Some(revision) = revision else { return Ok(None) };
    let name = format!("{revision}:{path}");
    let exists = git(rook, &["ls-tree", "-z", revision, "--", path]).await?;
    if exists.is_empty() {
        return Ok(None);
    }
    if !exists.starts_with("100644 ") && !exists.starts_with("100755 ") {
        return Err(bad(format!("review supports regular source files; {path} is a link or submodule")));
    }
    Ok(Some(
        git_data(rook, &["show", "--no-ext-diff", "--no-textconv", &name], rook.config.review.file_bytes)
            .await?,
    ))
}
async fn capture(rook: &Rook, selection: Selection, vault: &crate::Vault) -> Result<Snapshot> {
    let root = git(rook, &["rev-parse", "--show-toplevel"]).await?;
    if std::path::Path::new(root.trim()).canonicalize().map_err(|error| bad(error.to_string()))?
        != rook.workspace.canonicalize().map_err(|error| bad(error.to_string()))?
    {
        return Err(bad("review workspace must be the repository root"));
    }
    let (base, head) = match selection.target {
        Target::Working => {
            if selection.revision.is_some() {
                return Err(bad("working review does not accept revision"));
            }
            (Some(revision(rook, "HEAD").await?), None)
        }
        Target::Commit => {
            let head = revision(
                rook,
                selection.revision.as_deref().ok_or_else(|| bad("commit review requires revision"))?,
            )
            .await?;
            let parents = git(rook, &["rev-list", "--parents", "-n", "1", &head]).await?;
            (parents.split_whitespace().nth(1).map(str::to_owned), Some(head))
        }
        Target::Base => {
            let head = revision(rook, "HEAD").await?;
            let base = revision(
                rook,
                selection.revision.as_deref().ok_or_else(|| bad("base review requires revision"))?,
            )
            .await?;
            let common = git(rook, &["merge-base", &base, &head]).await?;
            (Some(common.trim().into()), Some(head))
        }
    };
    let mut args = if let Some(base) = &base {
        let mut args = vec![
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--raw",
            "--no-abbrev",
            "-z",
            base,
        ];
        if let Some(head) = &head {
            args.push(head);
        }
        args
    } else {
        vec![
            "diff-tree",
            "--root",
            "--no-commit-id",
            "--no-renames",
            "--raw",
            "--no-abbrev",
            "-r",
            "-z",
            head.as_deref().ok_or_else(|| bad("review source commit missing"))?,
        ]
    };
    args.push("--");
    args.extend(selection.paths.iter().map(String::as_str));
    let names = git(rook, &args).await?;
    let mut paths: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut collect = |names: &str| -> Result<()> {
        for path in names.split('\0').filter(|name| !name.is_empty()) {
            if !valid_path(path) {
                return Err(bad("unsupported review file name"));
            }
            if paths.len() >= rook.config.review.max_files && !paths.contains(path) {
                return Err(bad("review.max_files reached; select specific paths"));
            }
            paths.insert(path.into());
        }
        Ok(())
    };
    let mut metadata = std::collections::BTreeMap::new();
    let mut raw = names.split('\0').filter(|part| !part.is_empty());
    while let Some(header) = raw.next() {
        if !header.starts_with(':') || header.len() > 256 {
            return Err(bad("unsupported Git review metadata"));
        }
        let path = raw.next().ok_or_else(|| bad("missing path in Git review metadata"))?;
        collect(path)?;
        metadata.insert(path.to_owned(), header.to_owned());
    }
    if matches!(selection.target, Target::Working) {
        let mut args = vec!["ls-files", "--others", "--exclude-standard", "-z", "--"];
        args.extend(selection.paths.iter().map(String::as_str));
        collect(&git(rook, &args).await?)?;
    }
    let mut files = Vec::new();
    let mut bytes = 0usize;
    for path in paths {
        let before = blob(rook, base.as_deref(), &path).await?;
        let after = if head.is_some() {
            blob(rook, head.as_deref(), &path).await?
        } else {
            match rook_contain::files::RecoveryFiles::open(&rook.workspace, std::path::Path::new("."), false)
                .and_then(|files| files.read(std::path::Path::new(&path), rook.config.review.file_bytes))
            {
                Ok(bytes) => Some(bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => {
                    return Err(bad(format!("cannot freeze {path} within review.file_bytes: {error}")));
                }
            }
        };
        bytes = bytes
            .saturating_add(before.as_ref().map_or(0, Vec::len))
            .saturating_add(after.as_ref().map_or(0, Vec::len));
        if bytes > rook.config.review.snapshot_bytes / 2 {
            return Err(bad("review snapshot source budget reached; select fewer files"));
        }
        let binary = before
            .iter()
            .chain(after.iter())
            .any(|bytes| bytes.contains(&0) || std::str::from_utf8(bytes).is_err());
        let before_hash = before.as_ref().map(|bytes| identity(bytes));
        let after_hash = after.as_ref().map(|bytes| identity(bytes));
        let readable = |bytes: Option<Vec<u8>>| {
            if binary {
                None
            } else {
                bytes.and_then(|bytes| String::from_utf8(bytes).ok()).map(|text| vault.redact_source(&text))
            }
        };
        let before = readable(before);
        let after = readable(after);
        let text_diff = similar::TextDiff::from_lines(
            before.as_deref().unwrap_or_default(),
            after.as_deref().unwrap_or_default(),
        )
        .unified_diff()
        .context_radius(3)
        .header("before", "after")
        .to_string();
        let text_diff = if binary {
            format!(
                "Binary content omitted; hashes retained only. Before: {} · After: {}. Content has not been independently reviewed.\n",
                before_hash.as_deref().unwrap_or("absent"),
                after_hash.as_deref().unwrap_or("absent")
            )
        } else {
            text_diff
        };
        let diff = format!(
            "Git metadata: {}\n{text_diff}",
            metadata.get(&path).map(String::as_str).unwrap_or("untracked regular source file")
        );
        files.push(File { path, diff, before, after, before_hash, after_hash });
    }
    Ok(Snapshot { selection, workspace: workspace(rook)?, base, head, files })
}

pub(crate) struct Reader {
    pub snapshot: std::sync::Arc<Snapshot>,
    pub source: String,
    pub maximum: usize,
    pub seen: std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, usize>>>,
}
#[async_trait::async_trait]
impl rook_tools::Tool for Reader {
    fn name(&self) -> &str {
        READ_TOOL
    }
    fn spec(&self) -> rook_llm::ToolSpec {
        rook_llm::ToolSpec { name: READ_TOOL.into(),
            description: "Read frozen review sources. Omit path to list files. With path, read diff byte pages in order; side=before/after reads original file text. offset defaults to zero; next is the next byte offset. Never reads current workspace files.".into(),
            parameters: serde_json::json!({"type":"object","properties":{
                "path":{"type":"string"},"side":{"type":"string","enum":["before","after"]},
                "offset":{"type":"integer","minimum":0}},"additionalProperties":false}) }
    }
    async fn call(
        &self,
        _ctx: &rook_tools::ToolContext,
        args: &serde_json::Value,
    ) -> rook_tools::Result<rook_tools::ToolOutcome> {
        let result = (|| -> Result<serde_json::Value> {
            let Some(path) = args.get("path").and_then(|value| value.as_str()) else {
                return Ok(
                    serde_json::json!({"source":self.source,"files":self.snapshot.files.iter().map(|file|
                    serde_json::json!({"path":file.path,"diff_bytes":file.diff.len(),
                        "before_lines":file.before.as_deref().map(|text|text.lines().count()),
                        "after_lines":file.after.as_deref().map(|text|text.lines().count()),"binary":file.binary()})).collect::<Vec<_>>() }),
                );
            };
            let offset = args.get("offset").and_then(|value| value.as_u64()).unwrap_or(0);
            let offset = usize::try_from(offset).map_err(|_| bad("source offset exceeds platform range"))?;
            if let Some(side) = args.get("side") {
                let side = serde_json::from_value::<Side>(side.clone())?;
                return serde_json::to_value(page(
                    &self.snapshot,
                    &self.source,
                    path,
                    side,
                    offset,
                    self.maximum,
                )?)
                .map_err(Into::into);
            }
            let file = self
                .snapshot
                .files
                .iter()
                .find(|file| file.path == path)
                .ok_or_else(|| bad("file outside reviewed source"))?;
            if offset > file.diff.len() || !file.diff.is_char_boundary(offset) {
                return Err(bad("invalid diff offset"));
            }
            let mut end = offset.saturating_add(self.maximum).min(file.diff.len());
            while end > offset && !file.diff.is_char_boundary(end) {
                end -= 1;
            }
            let mut seen = self.seen.lock().unwrap_or_else(|error| error.into_inner());
            let through = seen.entry(path.into()).or_default();
            if offset <= *through {
                *through = (*through).max(end);
            }
            Ok(
                serde_json::json!({"source":self.source,"path":path,"offset":offset,"diff":&file.diff[offset..end],
                "next":(end<file.diff.len()).then_some(end)}),
            )
        })();
        let (content, is_error) = match result {
            Ok(value) => (value.to_string(), false),
            Err(error) => (error.to_string(), true),
        };
        Ok(if is_error {
            rook_tools::ToolOutcome::error(content)
        } else {
            rook_tools::ToolOutcome::ok(content)
        })
    }
}
