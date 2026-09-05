//! The HTTP surface. Every handler is a thin projection of `rook-core`, so the
//! web UI cannot learn anything the CLI does not also expose.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use rook_core::CoreError;
use rook_proto::{API_VERSION, ApiError, Health, Page};

use crate::AppState;

type Shared = Arc<AppState>;

pub fn router(state: Shared) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/store/stats", get(stats))
        .route("/api/store/objects", get(objects))
        .route("/api/store/objects/{id}", get(object))
        .route("/api/store/refs", get(refs))
        .route("/api/sessions", get(sessions))
        .route("/api/sessions/{id}/transcript", get(transcript))
        .route("/api/sessions/{id}/changes", get(changes))
        .route("/api/sessions/{id}/context", get(context))
        .route("/api/sessions/{id}/goal", post(set_goal))
        .route("/api/sessions/{id}/rewind", post(rewind))
        .route("/api/memory", get(memory).post(forget))
        .route("/api/memory/search", get(memory_search))
        .route("/api/memory/add", post(remember))
        .route("/api/memory/history", get(memory_history))
        .route("/api/memory/diff", get(memory_diff))
        .route("/api/memory/since", get(memory_since))
        .route("/api/skills", get(skills))
        .route("/api/skills/{name}", get(skill))
        .route("/api/skills/{name}/history", get(skill_history))
        .route("/api/skills/{name}/why", get(skill_why))
        .route("/api/skills/offered", get(skills_offered))
        .route("/api/skills/diff", get(skill_diff))
        .route("/api/skills/install", post(install_skill))
        .route("/api/skills/new", post(new_skill))
        .route("/api/skills/{name}/capture", post(capture_skill))
        .route("/api/skills/{name}/rollback", post(rollback_skill))
        .route("/api/checkpoints", get(checkpoints).post(create_checkpoint))
        .route("/api/checkpoints/restore", post(restore_checkpoint))
        .route("/api/writing", get(writing))
        .route("/api/maintenance", post(maintenance))
        .route("/api/shutdown", post(stop))
        .route("/api/store/gc", post(gc))
        .route("/api/store/prune", post(prune))
        .route("/api/store/verify", post(verify))
        .route("/api/store/train", post(train))
        .route("/api/sessions/{id}/fork", post(fork))
        .route("/api/sessions/{id}", axum::routing::delete(delete_session))
        .route(
            "/api/chat",
            get(crate::chat::upgrade).layer(axum::middleware::from_fn(crate::chat::only_from_this_daemon)),
        )
        .route("/api/jobs", get(jobs))
        .route("/api/jobs/{id}", get(job))
        .route("/api/jobs/{id}/stop", post(stop_job))
        .route("/api/search", get(search))
        .with_state(state)
}

/// Errors carry a machine-readable kind and, where one exists, a hint. A UI that
/// only gets a string can do nothing but display it.
struct Fail(StatusCode, ApiError);

impl IntoResponse for Fail {
    fn into_response(self) -> Response {
        (self.0, Json(self.1)).into_response()
    }
}

impl From<CoreError> for Fail {
    fn from(e: CoreError) -> Self {
        let (status, kind, hint) = match &e {
            CoreError::NoSession(_) => (StatusCode::NOT_FOUND, "not_found", None),
            CoreError::Skill(rook_skills::SkillError::NotFound(_)) => {
                (StatusCode::NOT_FOUND, "not_found", None)
            }
            CoreError::Skill(rook_skills::SkillError::NoCompatibleVersion { .. }) => (
                StatusCode::CONFLICT,
                "no_compatible_version",
                Some("check `rook skills why <name>` for the full list of mismatches"),
            ),
            CoreError::CaptureTooBig { .. } => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "capture_too_big",
                Some("narrow the paths, or raise the limits under [storage] in config.toml"),
            ),
            CoreError::Store(rook_store::StoreError::MissingObject(_)) => {
                (StatusCode::NOT_FOUND, "not_found", None)
            }
            _ => (StatusCode::INTERNAL_SERVER_ERROR, "internal", None),
        };
        let mut api = ApiError::new(kind, e.to_string());
        if let Some(h) = hint {
            api = api.with_hint(h);
        }
        Fail(status, api)
    }
}

type ApiResult<T> = std::result::Result<Json<T>, Fail>;

/// Deliberately does not wait for the store.
///
/// A liveness check that blocks behind a running turn reports the one thing it
/// exists to rule out — a daemon that has stopped answering — for a daemon that
/// is working perfectly. What it can say without the lock is enough to tell a
/// client it reached the right process at a version it understands.
async fn health(State(s): State<Shared>) -> ApiResult<Health> {
    Ok(Json(Health {
        ok: true,
        version: rook_core::AGENT_VERSION.to_string(),
        api_version: API_VERSION,
        store_root: s.about.store_root.clone(),
        workspace: s.about.workspace.clone(),
        os: s.about.os.clone(),
        arch: s.about.arch.clone(),
        uptime_secs: s.started.elapsed().as_secs(),
        turns_running: s.turns_running(),
        binary_replaced: s.binary_replaced(),
    }))
}

async fn stats(State(s): State<Shared>) -> ApiResult<rook_store::StoreStats> {
    let rook = s.rook.read().await;
    Ok(Json(rook.stats()?))
}

#[derive(Deserialize)]
struct ListQuery {
    kind: Option<String>,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    100
}

async fn objects(
    State(s): State<Shared>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Page<rook_store::ObjectRow>> {
    let rook = s.rook.read().await;
    let kind = q.kind.as_deref().and_then(parse_kind);
    Ok(Json(Page::new(rook.store.object_rows(kind, q.limit.min(1000)).map_err(CoreError::from)?)))
}

fn parse_kind(s: &str) -> Option<rook_store::Kind> {
    rook_store::Kind::ALL.into_iter().find(|k| k.as_str() == s)
}

#[derive(Deserialize)]
struct ObjectQuery {
    /// Cap on how much of the payload to return. Viewing a 200 MB tool result
    /// must not be the thing that takes the browser down.
    #[serde(default = "default_bytes")]
    max_bytes: usize,
}

fn default_bytes() -> usize {
    64 * 1024
}

async fn object(
    State(s): State<Shared>,
    Path(id): Path<String>,
    Query(q): Query<ObjectQuery>,
) -> ApiResult<serde_json::Value> {
    let rook = s.rook.read().await;
    let object =
        rook.store.resolve_prefix(&id).map_err(CoreError::from)?.ok_or_else(|| {
            Fail(StatusCode::NOT_FOUND, ApiError::new("not_found", format!("no object {id}")))
        })?;
    let data = rook.store.get(&object).map_err(CoreError::from)?;
    let (window, truncated) = rook_core::context::window_bytes(&data, q.max_bytes.min(4 << 20));
    Ok(Json(serde_json::json!({
        "id": object.to_hex(),
        "bytes": data.len(),
        "truncated": truncated,
        "body": String::from_utf8_lossy(&window),
    })))
}

#[derive(Deserialize)]
struct PrefixQuery {
    #[serde(default)]
    prefix: String,
}

async fn refs(State(s): State<Shared>, Query(q): Query<PrefixQuery>) -> ApiResult<Page<rook_store::RefRow>> {
    let rook = s.rook.read().await;
    Ok(Json(Page::new(rook.store.ref_rows(&q.prefix).map_err(CoreError::from)?)))
}

/// Sessions with their goals folded in. The goal lives in the `kv` table rather
/// than on `SessionMeta`, because adding a field to a postcard record breaks
/// every one already written — so it is joined here instead.
async fn sessions(State(s): State<Shared>) -> ApiResult<Page<rook_core::SessionSummary>> {
    let items = s.rook.read().await.session_summaries()?;
    Ok(Json(Page::new(items)))
}

#[derive(Deserialize)]
struct TranscriptQuery {
    #[serde(default)]
    from: u64,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default = "default_body")]
    max_body: usize,
}

fn default_body() -> usize {
    8192
}

async fn transcript(
    State(s): State<Shared>,
    Path(id): Path<String>,
    Query(q): Query<TranscriptQuery>,
) -> ApiResult<Page<rook_core::TranscriptEntry>> {
    let rook = s.rook.read().await;
    let sid = rook_store::parse_session_id(&id)
        .ok_or_else(|| Fail(StatusCode::BAD_REQUEST, ApiError::new("bad_request", "not a session id")))?;
    let entries = rook.transcript(sid, q.from, q.limit.min(2000), q.max_body.min(1 << 20))?;
    let next = entries.last().map(|e| (e.seq + 1).to_string());
    Ok(Json(Page::new(entries).with_cursor(next)))
}

#[derive(Deserialize)]
struct DiffQuery {
    #[serde(default)]
    diff: bool,
}

/// What a session did to the workspace, from its own checkpoints. The diffs are
/// opt-in: a session that rewrote a large file is a large answer.
async fn context(
    State(s): State<Shared>,
    Path(id): Path<String>,
    Query(q): Query<ContextQuery>,
) -> ApiResult<rook_core::ContextUsage> {
    // Scoped like `/api/memory`: what fraction of the window a session fills
    // depends on the model that project configured, and this daemon serves
    // several projects.
    let engine = s.engine_for(q.workspace.as_deref()).await.map_err(CoreError::Other)?;
    let rook = engine.read().await;
    Ok(Json(rook.context_usage(session_id(&id)?, q.window)?))
}

#[derive(serde::Deserialize)]
struct ContextQuery {
    /// Ask how the session would sit in a different model.
    #[serde(default)]
    window: Option<usize>,
    #[serde(default)]
    workspace: Option<std::path::PathBuf>,
}

async fn changes(
    State(s): State<Shared>,
    Path(id): Path<String>,
    Query(q): Query<DiffQuery>,
) -> ApiResult<rook_core::changes::Changes> {
    let rook = s.rook.read().await;
    Ok(Json(rook.changes(session_id(&id)?, q.diff)?))
}

fn session_id(id: &str) -> std::result::Result<u128, Fail> {
    rook_store::parse_session_id(id)
        .ok_or_else(|| Fail(StatusCode::BAD_REQUEST, ApiError::new("bad_request", "not a session id")))
}

#[derive(Deserialize)]
struct MemoryQuery {
    /// Facts from every workspace, not only this one.
    #[serde(default)]
    all: bool,
    /// Rank against this instead of listing, the way a turn would recall.
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    workspace: Option<std::path::PathBuf>,
}

async fn memory(
    State(s): State<Shared>,
    Query(query): Query<MemoryQuery>,
) -> ApiResult<Page<rook_core::Fact>> {
    // Scoped to the project asked about rather than to the daemon's own. A fact
    // is remembered against a workspace, so answering from the wrong one is not
    // a smaller answer but a different question.
    let engine = s.engine_for(query.workspace.as_deref()).await.map_err(CoreError::Other)?;
    let rook = engine.read().await;
    let book = rook.memory()?;
    let workspace = rook.workspace.display().to_string();
    let facts: Vec<rook_core::Fact> = match &query.q {
        // The budget a turn would spend, doubled: this is a person reading, and
        // seeing one fact more than the agent gets is not a problem.
        Some(text) => rook.recall(text, rook.config.memory.context_budget_tokens * 2)?,
        None if query.all => book.facts.clone(),
        None => book.in_scope(&workspace).cloned().collect(),
    };
    Ok(Json(Page::new(facts)))
}

#[derive(Deserialize)]
struct SearchMemory {
    q: String,
    #[serde(default)]
    workspace: Option<std::path::PathBuf>,
}

/// Scored search, which is not what `GET /api/memory?q=` does: that ranks by
/// what would fit in a turn's budget. Both exist because both are asked.
async fn memory_search(
    State(s): State<Shared>,
    Query(query): Query<SearchMemory>,
) -> ApiResult<Page<rook_core::memory::Hit>> {
    let engine = s.engine_for(query.workspace.as_deref()).await.map_err(CoreError::Other)?;
    let rook = engine.read().await;
    Ok(Json(Page::new(rook.memory_search(&query.q)?)))
}

#[derive(Deserialize)]
struct Remember {
    text: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    global: bool,
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    workspace: Option<std::path::PathBuf>,
}

async fn remember(State(s): State<Shared>, Json(body): Json<Remember>) -> ApiResult<serde_json::Value> {
    let engine = s.engine_for(body.workspace.as_deref()).await.map_err(CoreError::Other)?;
    let rook = engine.read().await;
    let scope = if body.global {
        rook_core::Scope::Global
    } else {
        rook_core::Scope::Project(rook.workspace.display().to_string())
    };
    let mut fact = rook_core::Fact::new(body.text, scope).with_tags(body.tags);
    fact.pinned = body.pinned;
    let id = fact.id.clone();
    let learned = rook.remember(fact, Some("added over the API".into()))?;
    Ok(Json(serde_json::json!({ "id": id, "learned": learned })))
}

async fn memory_history(State(s): State<Shared>) -> ApiResult<Page<rook_core::MemoryVersion>> {
    let rook = s.rook.read().await;
    Ok(Json(Page::new(rook.memory_history()?)))
}

#[derive(Deserialize)]
struct Pair {
    a: String,
    b: String,
}

type Changes = Page<(rook_core::memory::Change, rook_core::Fact)>;

async fn memory_diff(State(s): State<Shared>, Query(pair): Query<Pair>) -> ApiResult<Changes> {
    let rook = s.rook.read().await;
    // The prefixes are resolved here because only the process holding the store
    // can say whether one is ambiguous.
    let (a, b) = (rook.object_named(&pair.a)?, rook.object_named(&pair.b)?);
    Ok(Json(Page::new(rook.memory_diff(&a, &b)?)))
}

#[derive(Deserialize)]
struct Since {
    days: i64,
}

async fn memory_since(State(s): State<Shared>, Query(since): Query<Since>) -> ApiResult<Changes> {
    let rook = s.rook.read().await;
    Ok(Json(Page::new(rook.memory_since(rook_store::now_unix() - since.days * 86_400)?)))
}

#[derive(Deserialize)]
struct Forget {
    /// An id or the exact text, the same two things `rook memory rm` takes.
    id: String,
}

async fn forget(State(s): State<Shared>, Json(body): Json<Forget>) -> ApiResult<serde_json::Value> {
    let rook = s.rook.read().await;
    match rook.forget(&body.id, Some("forgotten from the web UI".into()))? {
        Some(fact) => Ok(Json(serde_json::json!({ "forgot": fact }))),
        None => {
            Err(Fail(StatusCode::NOT_FOUND, ApiError::new("not_found", format!("no fact {:?}", body.id))))
        }
    }
}

#[derive(Deserialize)]
struct GoalBody {
    goal: String,
}

async fn set_goal(
    State(s): State<Shared>,
    Path(id): Path<String>,
    Json(body): Json<GoalBody>,
) -> ApiResult<serde_json::Value> {
    let rook = s.rook.read().await;
    rook.set_goal(session_id(&id)?, &body.goal)?;
    Ok(Json(serde_json::json!({ "goal": body.goal })))
}

#[derive(Deserialize)]
struct RewindBody {
    /// Where to rewind to. The transcript numbers every event.
    to_seq: u64,
    /// Put the workspace files back as well. Off is a fork of the conversation
    /// alone, which is the reversible half.
    #[serde(default)]
    restore_files: bool,
}

/// Forks rather than truncating, so the rewound-past turns stay readable —
/// which is why this is a POST that answers with a new session id.
async fn rewind(
    State(s): State<Shared>,
    Path(id): Path<String>,
    Json(body): Json<RewindBody>,
) -> ApiResult<rook_core::Rewind> {
    let rook = s.rook.read().await;
    Ok(Json(rook.rewind(session_id(&id)?, body.to_seq, body.restore_files)?))
}

async fn skills(
    State(s): State<Shared>,
    Query(q): Query<WorkspaceQuery>,
) -> ApiResult<Page<rook_skills::SkillCard>> {
    let engine = s.engine_for(q.workspace.as_deref()).await.map_err(CoreError::Other)?;
    let rook = engine.read().await;
    Ok(Json(Page::new(rook.catalog())))
}

/// Which project is being asked about. A skill can come from the project as
/// well as from the user, so the daemon's own is not an answer for another's.
#[derive(Deserialize)]
struct WorkspaceQuery {
    #[serde(default)]
    workspace: Option<std::path::PathBuf>,
}

async fn skill(
    State(s): State<Shared>,
    Path(name): Path<String>,
    Query(q): Query<WorkspaceQuery>,
) -> ApiResult<rook_skills::SkillDetail> {
    let engine = s.engine_for(q.workspace.as_deref()).await.map_err(CoreError::Other)?;
    let rook = engine.read().await;
    Ok(Json(rook.skills().resolve(&name, rook.env()).map_err(CoreError::from)?.detail()))
}

async fn skill_history(
    State(s): State<Shared>,
    Path(name): Path<String>,
) -> ApiResult<Page<rook_core::SkillVersionRecord>> {
    let rook = s.rook.read().await;
    Ok(Json(Page::new(rook.skill_history(&name)?)))
}

#[derive(Deserialize)]
struct Named {
    name: String,
}

/// Fetching a source and unpacking it is not something to hold a runtime
/// worker for, and neither is walking a skill's files into the store.
async fn install_skill(State(s): State<Shared>, Json(body): Json<Named>) -> ApiResult<serde_json::Value> {
    let rook = s.rook.clone().read_owned().await;
    let path = tokio::task::spawn_blocking(move || rook.install_skill(&body.name))
        .await
        .map_err(|e| Fail(StatusCode::INTERNAL_SERVER_ERROR, ApiError::new("panic", e.to_string())))??;
    Ok(Json(serde_json::json!({ "path": path })))
}

#[derive(Deserialize)]
struct NewSkill {
    name: String,
    description: String,
}

async fn new_skill(State(s): State<Shared>, Json(body): Json<NewSkill>) -> ApiResult<serde_json::Value> {
    let rook = s.rook.read().await;
    Ok(Json(serde_json::json!({ "dir": rook.new_skill(&body.name, &body.description)? })))
}

#[derive(Deserialize)]
struct Note {
    #[serde(default)]
    message: Option<String>,
}

async fn capture_skill(
    State(s): State<Shared>,
    Path(name): Path<String>,
    Json(body): Json<Note>,
) -> ApiResult<serde_json::Value> {
    let rook = s.rook.clone().read_owned().await;
    let (set, id) = tokio::task::spawn_blocking(move || rook.capture_skill(&name, body.message))
        .await
        .map_err(|e| Fail(StatusCode::INTERNAL_SERVER_ERROR, ApiError::new("panic", e.to_string())))??;
    Ok(Json(serde_json::json!({ "set": set, "object": id.to_hex() })))
}

#[derive(Deserialize)]
struct Object {
    object: String,
}

async fn rollback_skill(
    State(s): State<Shared>,
    Path(name): Path<String>,
    Json(body): Json<Object>,
) -> ApiResult<rook_core::Rollback> {
    let rook = s.rook.clone().read_owned().await;
    let done = tokio::task::spawn_blocking(move || {
        let id = rook.object_named(&body.object)?;
        rook.rollback_skill(&name, &id)
    })
    .await
    .map_err(|e| Fail(StatusCode::INTERNAL_SERVER_ERROR, ApiError::new("panic", e.to_string())))??;
    Ok(Json(done))
}

type SkillChanges = Page<(String, rook_core::fileset::Change)>;

async fn skill_diff(State(s): State<Shared>, Query(pair): Query<Pair>) -> ApiResult<SkillChanges> {
    let rook = s.rook.read().await;
    let (a, b) = (rook.object_named(&pair.a)?, rook.object_named(&pair.b)?);
    Ok(Json(Page::new(rook.skill_diff(&a, &b)?)))
}

async fn skill_why(
    State(s): State<Shared>,
    Path(name): Path<String>,
    Query(q): Query<WorkspaceQuery>,
) -> ApiResult<rook_core::SkillWhy> {
    let engine = s.engine_for(q.workspace.as_deref()).await.map_err(CoreError::Other)?;
    let rook = engine.read().await;
    Ok(Json(rook.why_skill(&name)?))
}

#[derive(Deserialize)]
struct Offered {
    #[serde(default)]
    q: String,
    /// Fetch the sources again rather than answering from the cache.
    #[serde(default)]
    refresh: bool,
}

/// What the configured sources offer, and what went wrong reaching any of them
/// — the errors travel with the answer because a shorter list and a source
/// that was down look identical without them.
async fn skills_offered(State(s): State<Shared>, Query(q): Query<Offered>) -> ApiResult<serde_json::Value> {
    let rook = s.rook.clone().read_owned().await;
    let (offered, errors) =
        tokio::task::spawn_blocking(move || rook.skills_offered(&q.q, q.refresh))
            .await
            .map_err(|e| Fail(StatusCode::INTERNAL_SERVER_ERROR, ApiError::new("panic", e.to_string())))?;
    Ok(Json(serde_json::json!({ "items": offered, "errors": errors })))
}

#[derive(Deserialize)]
struct SearchQuery {
    #[serde(default)]
    q: String,
    #[serde(default = "default_limit")]
    limit: usize,
    /// The same two filters the command line has. Without them a client routing
    /// a narrowed search through here would get a wider answer and no sign that
    /// its narrowing was dropped.
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    conversation: bool,
}

/// A job without what it printed. The list is polled, and four jobs are four
/// capped outputs; the one being read asks for itself.
#[derive(serde::Serialize)]
struct JobLine {
    id: String,
    command: String,
    started_at: i64,
    exit_code: Option<i32>,
}

#[derive(Deserialize)]
struct Which {
    workspace: Option<std::path::PathBuf>,
}

/// The commands a project has left running, or nothing if it has run no turn:
/// the registry is built with the rest of its equipment when the first one is
/// served, and asking is not a reason to build it.
async fn running_in(s: &Shared, which: &Which) -> Result<Option<Arc<rook_tools::jobs::Jobs>>, Fail> {
    let engine = s.engine_for(which.workspace.as_deref()).await.map_err(CoreError::Other)?;
    let equipment = s.equipment_for(&engine).await;
    let running = equipment.get().map(|shared| shared.jobs.clone());
    Ok(running)
}

fn no_such_job(id: &str) -> Fail {
    Fail(StatusCode::NOT_FOUND, ApiError::new("not_found", format!("no background command {id}")))
}

async fn jobs(State(s): State<Shared>, Query(which): Query<Which>) -> ApiResult<Page<JobLine>> {
    let Some(jobs) = running_in(&s, &which).await? else { return Ok(Json(Page::new(Vec::new()))) };
    let listed = jobs
        .list()
        .into_iter()
        .map(|j| JobLine { id: j.id, command: j.command, started_at: j.started_at, exit_code: j.exit_code })
        .collect();
    Ok(Json(Page::new(listed)))
}

async fn job(
    State(s): State<Shared>,
    Path(id): Path<String>,
    Query(which): Query<Which>,
) -> ApiResult<rook_tools::jobs::Job> {
    let found = running_in(&s, &which).await?.and_then(|jobs| jobs.get(&id));
    found.map(Json).ok_or_else(|| no_such_job(&id))
}

/// Answers with the job as it stands, not as it will be: the kill happens in
/// the task that owns the child, so the exit code arrives on a later read.
async fn stop_job(
    State(s): State<Shared>,
    Path(id): Path<String>,
    Query(which): Query<Which>,
) -> ApiResult<rook_tools::jobs::Job> {
    let Some(jobs) = running_in(&s, &which).await? else { return Err(no_such_job(&id)) };
    if !jobs.stop(&id) {
        return Err(no_such_job(&id));
    }
    jobs.get(&id).map(Json).ok_or_else(|| no_such_job(&id))
}

async fn search(
    State(s): State<Shared>,
    Query(query): Query<SearchQuery>,
) -> ApiResult<rook_core::search::Found> {
    let rook = s.rook.read().await;
    let session = match query.session.as_deref() {
        Some(spec) => Some(rook_store::parse_session_id(spec).ok_or_else(|| {
            Fail(StatusCode::BAD_REQUEST, ApiError::new("bad_request", "not a session id"))
        })?),
        None => None,
    };
    let options = rook_core::search::Search {
        limit: query.limit.min(200),
        session,
        conversation_only: query.conversation,
        ..Default::default()
    };
    Ok(Json(rook.search(&query.q, &options)?))
}

#[derive(Deserialize)]
struct NewCheckpoint {
    name: String,
    #[serde(default)]
    path: Option<std::path::PathBuf>,
}

async fn create_checkpoint(
    State(s): State<Shared>,
    Json(body): Json<NewCheckpoint>,
) -> ApiResult<serde_json::Value> {
    let rook = s.rook.clone().read_owned().await;
    let (set, id) = blocking(move || rook.checkpoint(&body.name, body.path.as_deref())).await?;
    Ok(Json(serde_json::json!({ "set": set, "object": id.to_hex() })))
}

#[derive(Deserialize)]
struct Restore {
    object: String,
    to: std::path::PathBuf,
}

async fn restore_checkpoint(
    State(s): State<Shared>,
    Json(body): Json<Restore>,
) -> ApiResult<serde_json::Value> {
    let rook = s.rook.clone().read_owned().await;
    let written = blocking(move || {
        let id = rook.object_named(&body.object)?;
        rook.restore_checkpoint(&id, &body.to)
    })
    .await?;
    Ok(Json(serde_json::json!({ "restored": written })))
}

async fn checkpoints(State(s): State<Shared>) -> ApiResult<Page<serde_json::Value>> {
    let rook = s.rook.read().await;
    let items = rook
        .checkpoints()?
        .into_iter()
        .map(|(name, id)| serde_json::json!({ "ref": name, "object": id.to_hex() }))
        .collect();
    Ok(Json(Page::new(items)))
}

#[derive(Deserialize)]
struct MaintenanceBody {
    #[serde(default = "yes")]
    dry_run: bool,
}

fn yes() -> bool {
    true
}

/// Prune and collect. Defaults to a dry run: a destructive default behind a
/// single button click is how a UI eats someone's history.
#[derive(Deserialize)]
struct Stop {
    /// Stop even though a turn is running. Without it a busy daemon says what
    /// it is doing instead: a turn taken out from under someone keeps whatever
    /// it had already written and loses the rest, and that is a decision to be
    /// made with the number in front of you.
    #[serde(default)]
    force: bool,
}

/// Stop the daemon, which otherwise meant finding its process id.
async fn stop(State(s): State<Shared>, Json(body): Json<Stop>) -> ApiResult<serde_json::Value> {
    let running = s.turns_running();
    if running > 0 && !body.force {
        return Err(Fail(
            StatusCode::CONFLICT,
            ApiError::new("busy", format!("{running} turn(s) still running"))
                .with_hint("stop them, or ask again with force to end them where they are"),
        ));
    }
    s.stopping.notify_one();
    Ok(Json(serde_json::json!({ "stopping": true, "turns_interrupted": running })))
}

/// What is being written right now, and by whom.
///
/// A lock nobody can look at is one that cannot be debugged when it wedges,
/// which is the usual complaint about locks. Per project, because that is the
/// scope a claim has.
async fn writing(State(s): State<Shared>, Query(q): Query<WorkspaceQuery>) -> ApiResult<Vec<Writer>> {
    let engine = s.engine_for(q.workspace.as_deref()).await.map_err(rook_core::CoreError::Other)?;
    let held = engine.read().await.being_written();
    Ok(Json(
        held.into_iter()
            .map(|(path, by)| Writer {
                path: path.display().to_string(),
                session: rook_store::format_session_id(by.session),
                held_for_secs: rook_store::now_unix().saturating_sub(by.since).max(0) as u64,
            })
            .collect(),
    ))
}

#[derive(serde::Serialize)]
struct Writer {
    path: String,
    session: String,
    held_for_secs: u64,
}

/// On a thread of its own, because it is the one request here that is neither
/// quick nor waiting on IO: a prune walks every session, a collection walks
/// every object, and training a dictionary is zstd doing arithmetic for as long
/// as that takes. Run where it was awaited, it holds a runtime worker for all of
/// it, and on a small machine that is the thread the chat socket needed to say
/// anything at all.
async fn maintenance(
    State(s): State<Shared>,
    Json(body): Json<MaintenanceBody>,
) -> ApiResult<rook_core::MaintenanceReport> {
    let rook = s.rook.clone().read_owned().await;
    let report = tokio::task::spawn_blocking(move || rook.maintenance(body.dry_run))
        .await
        .map_err(|e| Fail(StatusCode::INTERNAL_SERVER_ERROR, ApiError::new("panic", e.to_string())))??;
    Ok(Json(report))
}

#[derive(Deserialize)]
struct DryRun {
    #[serde(default)]
    dry_run: bool,
}

/// Each of these walks the whole store, so each goes on a blocking thread: on a
/// small machine the runtime worker it would hold is the one the chat socket
/// needs.
async fn gc(State(s): State<Shared>, Json(body): Json<DryRun>) -> ApiResult<rook_store::GcReport> {
    let rook = s.rook.clone().read_owned().await;
    Ok(Json(blocking(move || rook.collect_garbage(body.dry_run)).await?))
}

async fn prune(State(s): State<Shared>, Json(body): Json<DryRun>) -> ApiResult<rook_store::PruneReport> {
    let rook = s.rook.clone().read_owned().await;
    Ok(Json(blocking(move || rook.prune(body.dry_run)).await?))
}

/// Both of these take a body they do not read. That is the point: an
/// extractor that requires `application/json` is what makes the request
/// preflighted, and a preflight this daemon answers no CORS header to is what
/// stops a page the user has open from POSTing to a daemon on loopback. Every
/// other write here is protected by the body it genuinely needs; these two
/// needed nothing and so were reachable.
async fn verify(State(s): State<Shared>, Json(_): Json<serde_json::Value>) -> ApiResult<serde_json::Value> {
    let rook = s.rook.clone().read_owned().await;
    let bad = blocking(move || rook.verify()).await?;
    let failed: Vec<_> =
        bad.into_iter().map(|(id, why)| serde_json::json!({ "object": id.to_hex(), "why": why })).collect();
    Ok(Json(serde_json::json!({ "failed": failed })))
}

async fn train(State(s): State<Shared>, Json(_): Json<serde_json::Value>) -> ApiResult<serde_json::Value> {
    let rook = s.rook.clone().read_owned().await;
    let trained = blocking(move || rook.train_dictionaries()).await?;
    Ok(Json(serde_json::json!({ "trained": trained })))
}

#[derive(Deserialize)]
struct At {
    at: u64,
}

async fn fork(
    State(s): State<Shared>,
    Path(id): Path<String>,
    Json(body): Json<At>,
) -> ApiResult<serde_json::Value> {
    let rook = s.rook.read().await;
    let forked = rook.fork_session(session_id(&id)?, body.at)?;
    Ok(Json(serde_json::json!({
        "id": rook_store::format_session_id(forked.id),
        "event_count": forked.event_count,
    })))
}

async fn delete_session(State(s): State<Shared>, Path(id): Path<String>) -> ApiResult<serde_json::Value> {
    let rook = s.rook.read().await;
    Ok(Json(serde_json::json!({ "events": rook.delete_session(session_id(&id)?)? })))
}

/// A store pass on a blocking thread, with the panic reported as one.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> rook_core::Result<T> + Send + 'static,
) -> std::result::Result<T, Fail> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| Fail(StatusCode::INTERNAL_SERVER_ERROR, ApiError::new("panic", e.to_string())))?
        .map_err(Fail::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    struct Fixture {
        _home: tempfile::TempDir,
        _workspace: tempfile::TempDir,
        router: Router,
        state: Shared,
        session: u128,
    }

    fn fixture() -> Fixture {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ROOK_HOME", home.path()) };

        let store = rook_store::Store::open(home.path().join("store")).unwrap();
        let env = rook_skills::Environment::bare("linux", "x86_64", "0.1.0");
        let (skills, _) = rook_skills::SkillIndex::discover(&[]);
        let rook = rook_core::Rook::from_parts(
            store,
            rook_core::Config::default(),
            env,
            skills,
            workspace.path().to_path_buf(),
        );
        let session = rook.start_session("api test").unwrap();
        rook.log(session, rook_store::EventKind::UserMessage, "prompt", "find the leak").unwrap();

        let about = crate::About {
            store_root: rook.store.root().display().to_string(),
            workspace: rook.workspace.display().to_string(),
            os: rook.env().os.clone(),
            arch: rook.env().arch.clone(),
        };
        let state = Arc::new(AppState {
            rook: Arc::new(tokio::sync::RwLock::new(rook)),
            elsewhere: tokio::sync::RwLock::new(std::collections::HashMap::new()),
            equipment: tokio::sync::RwLock::new(std::collections::HashMap::new()),
            max_projects: 3,
            started: std::time::Instant::now(),
            about,
            config_read: std::sync::Mutex::new(None),
            config_path: home.path().join("config.toml"),
            started_at: std::time::SystemTime::now(),
            turns: std::sync::atomic::AtomicU32::new(0),
            stopping: tokio::sync::Notify::new(),
        });
        Fixture { _home: home, _workspace: workspace, router: router(state.clone()), state, session }
    }

    /// The daemon read its configuration once at start, so changing
    /// `[agent] model` — the setting people change most — took a restart, and
    /// the restart was something a person had to be told to do rather than
    /// something that happened.
    #[tokio::test]
    async fn a_changed_configuration_is_read_before_the_next_turn() {
        let f = fixture();
        let was = f.state.rook.read().await.config.agent.model.clone();
        assert_ne!(was, "lmstudio/somebody-else", "the precondition: it is not that yet");

        // Written as a person writes it, which is also what the daemon reads:
        // a partial file, with the defaults behind it. Named from the state
        // rather than from `paths`, because every fixture in this file points
        // `ROOK_HOME` at its own directory and the next one to start would
        // otherwise decide which file this test wrote.
        std::fs::write(&f.state.config_path, "[agent]\nmodel = \"lmstudio/somebody-else\"\n").unwrap();

        let said = f.state.config_if_changed().await;

        assert!(said.is_some_and(|s| s.contains("somebody-else")), "it says what changed");
        assert_eq!(
            f.state.rook.read().await.config.agent.model,
            "lmstudio/somebody-else",
            "and the next turn is asked of the model that is configured now"
        );
        assert!(f.state.config_if_changed().await.is_none(), "and an unchanged file costs nothing");
    }

    /// Stopping the daemon meant finding a process id, and stopping one that
    /// was in the middle of a turn meant finding out afterwards.
    #[tokio::test]
    async fn a_stop_names_the_turns_it_would_interrupt_rather_than_dropping_them() {
        let f = fixture();
        let turn = f.state.turn_started();

        let (status, body) = post(&f, "/api/shutdown", serde_json::json!({})).await;
        assert_eq!(status, StatusCode::CONFLICT, "a running turn is not dropped for the asking: {body}");
        assert_eq!(body["kind"], "busy");
        assert!(body["error"].as_str().is_some_and(|e| e.contains('1')), "it says how many: {body}");

        let (status, body) = post(&f, "/api/shutdown", serde_json::json!({ "force": true })).await;
        assert_eq!(status, StatusCode::OK, "asked again, it goes: {body}");
        assert_eq!(body["turns_interrupted"], 1, "and says what it ended: {body}");

        drop(turn);
        let (status, body) = post(&f, "/api/shutdown", serde_json::json!({})).await;
        assert_eq!(status, StatusCode::OK, "with nothing running there is nothing to weigh: {body}");
        assert_eq!(body["turns_interrupted"], 0);
    }

    /// An upgrade leaves the running daemon on the old code: the store, the
    /// API and the web UI all keep working, at the previous version, and the
    /// only symptom is a fix that did not take.
    #[test]
    fn a_daemon_whose_binary_has_been_replaced_says_so() {
        assert!(
            crate::replaced_since(std::time::SystemTime::UNIX_EPOCH),
            "this binary was written after 1970, so a process started then is running an old one"
        );
        assert!(
            !crate::replaced_since(std::time::SystemTime::now()),
            "and nothing has been written since this instant"
        );
    }

    /// The CLI and the TUI have had `/jobs` since they had jobs. A browser
    /// could start a dev server through the agent and then have no way to see
    /// it, let alone stop it.
    #[tokio::test]
    async fn a_browser_can_see_and_stop_what_the_agent_left_running() {
        let f = fixture();
        let (status, body) = get(&f, "/api/jobs").await;
        assert_eq!(
            status,
            StatusCode::OK,
            "a project that has run no turn has no jobs, which is not an error"
        );
        assert_eq!(body["items"].as_array().unwrap().len(), 0);

        let equipment = f.state.equipment_for(&f.state.rook).await;
        let workspace = {
            let rook = f.state.rook.read().await;
            equipment.get_or_init(|| crate::chat::Shared::for_project(&rook)).await;
            rook.workspace.clone()
        };
        let jobs = equipment.get().unwrap().jobs.clone();
        let id = jobs.start("echo dev-server-up; sleep 30", &workspace, None).unwrap();

        let (status, body) = get(&f, "/api/jobs").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["items"][0]["id"], id);
        assert_eq!(body["items"][0]["exit_code"], serde_json::Value::Null, "it is still running");
        assert!(body["items"][0].get("output").is_none(), "the list does not carry what four jobs printed");

        let (status, body) = post(&f, &format!("/api/jobs/{id}/stop"), serde_json::json!({})).await;
        assert_eq!(status, StatusCode::OK, "{body}");

        let (status, body) = get(&f, "/api/jobs/nosuch").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"].as_str().unwrap().contains("nosuch"), "{body}");
    }

    /// The language-server pool, the MCP session and the commands left running
    /// were built per websocket, so reloading the page re-indexed every server,
    /// respawned every MCP server, and killed every background command the
    /// agent had started.
    #[tokio::test]
    async fn reconnecting_to_a_project_does_not_rebuild_what_it_is_running() {
        let f = fixture();
        let first = f.state.equipment_for(&f.state.rook).await;
        let again = f.state.equipment_for(&f.state.rook).await;
        assert!(Arc::ptr_eq(&first, &again), "a second connection to one project must reuse them");

        let elsewhere = tempfile::tempdir().unwrap();
        let other = f.state.engine_for(Some(elsewhere.path())).await.unwrap();
        let theirs = f.state.equipment_for(&other).await;
        assert!(!Arc::ptr_eq(&first, &theirs), "another project has its own servers and its own jobs");
    }

    async fn post(f: &Fixture, path: &str, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
        let response = f
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 4 << 20).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or_default())
    }

    async fn get(f: &Fixture, path: &str) -> (StatusCode, serde_json::Value) {
        let response = f
            .router
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 4 << 20).await.unwrap();
        let json = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| serde_json::json!(String::from_utf8_lossy(&bytes)));
        (status, json)
    }

    #[tokio::test]
    async fn health_names_the_api_version_a_client_must_match() {
        let f = fixture();
        let (status, body) = get(&f, "/api/health").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ok"], true);
        assert_eq!(body["api_version"], rook_proto::API_VERSION);
        assert!(body["store_root"].is_string(), "a client needs to know which store it reached");
    }

    /// A turn holds its read guard for as long as it runs and maintenance wants
    /// the write lock, so if liveness went through either one it would report a
    /// working daemon as a dead one for minutes at a time.
    #[tokio::test]
    async fn health_answers_while_the_store_is_held() {
        let f = fixture();
        let held = f.state.rook.write().await;

        let (status, body) = get(&f, "/api/health").await;

        assert_eq!(status, StatusCode::OK, "liveness must not wait for the store");
        assert!(!body["store_root"].as_str().unwrap().is_empty(), "and still say which store: {body}");
        drop(held);
    }

    #[tokio::test]
    async fn the_paged_endpoints_all_answer_with_items() {
        let f = fixture();
        for path in
            ["/api/sessions", "/api/skills", "/api/store/objects", "/api/store/refs", "/api/checkpoints"]
        {
            let (status, body) = get(&f, path).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert!(body["items"].is_array(), "{path} answered {body}");
        }
    }

    #[tokio::test]
    async fn a_session_transcript_comes_back_in_order() {
        let f = fixture();
        let id = rook_store::format_session_id(f.session);
        let (status, body) = get(&f, &format!("/api/sessions/{id}/transcript")).await;

        assert_eq!(status, StatusCode::OK);
        let items = body["items"].as_array().unwrap();
        assert_eq!(items.len(), 1, "{body}");
        assert!(items[0]["body"].as_str().unwrap().contains("find the leak"), "{body}");
    }

    #[tokio::test]
    async fn a_missing_skill_is_a_typed_error_rather_than_a_five_hundred() {
        let f = fixture();
        let (status, body) = get(&f, "/api/skills/not-a-skill").await;

        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["kind"], "not_found", "the discriminant is what a client branches on");
        assert!(body["error"].as_str().unwrap().contains("not-a-skill"), "{body}");
    }

    #[tokio::test]
    async fn an_unparseable_session_id_does_not_reach_the_store() {
        let f = fixture();
        let (status, body) = get(&f, "/api/sessions/not-a-ulid/transcript").await;

        assert!(status.is_client_error(), "answered {status}: {body}");
    }

    #[tokio::test]
    async fn search_answers_with_what_it_found() {
        let f = fixture();
        let (status, body) = get(&f, "/api/search?q=leak").await;

        assert_eq!(status, StatusCode::OK, "{body}");
        // `hits`, not `items`: a search result carries what it scanned and
        // whether it stopped early, which a page of rows does not.
        assert_eq!(body["hits"].as_array().unwrap().len(), 1, "{body}");
        assert_eq!(body["truncated"], false);
    }

    #[tokio::test]
    async fn an_unknown_route_is_a_404_not_a_panic() {
        let f = fixture();
        let (status, _) = get(&f, "/api/nope").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_session_reports_its_goal_and_what_it_changed() {
        let f = fixture();
        // Set here rather than in the fixture: recording a goal writes an event,
        // and every other test counts them.
        f.state.rook.read().await.set_goal(f.session, "find the leak").unwrap();
        let id = rook_store::format_session_id(f.session);

        let (status, body) = get(&f, &format!("/api/sessions/{id}/changes")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["files"].is_array(), "a session that changed nothing is an empty list: {body}");

        let (_, sessions) = get(&f, "/api/sessions").await;
        let session = &sessions["items"][0];
        assert_eq!(session["goal"], "find the leak", "the goal is joined in: {session}");
    }

    #[tokio::test]
    async fn memory_is_listed_scoped_and_can_be_corrected() {
        let f = fixture();
        {
            let rook = f.state.rook.read().await;
            let here = rook_core::Scope::Project(rook.workspace.display().to_string());
            rook.remember(rook_core::Fact::new("prefer tabs", here), None).unwrap();
            rook.remember(
                rook_core::Fact::new("somebody else's project", rook_core::Scope::Project("/nowhere".into())),
                None,
            )
            .unwrap();
        }

        let (_, here) = get(&f, "/api/memory").await;
        assert_eq!(here["items"].as_array().unwrap().len(), 1, "scoped to this workspace: {here}");
        let (_, everywhere) = get(&f, "/api/memory?all=true").await;
        assert_eq!(everywhere["items"].as_array().unwrap().len(), 2, "{everywhere}");

        let id = here["items"][0]["id"].as_str().unwrap().to_string();
        let forgotten = post(&f, "/api/memory", serde_json::json!({ "id": id })).await;
        assert_eq!(forgotten.0, StatusCode::OK, "{:?}", forgotten.1);
        assert_eq!(forgotten.1["forgot"]["text"], "prefer tabs");

        let (status, _) = post(&f, "/api/memory", serde_json::json!({ "id": id })).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "forgetting twice is not a success");
    }

    #[tokio::test]
    async fn a_goal_can_be_set_and_reads_back_with_the_session() {
        let f = fixture();
        let id = rook_store::format_session_id(f.session);

        let (status, said) =
            post(&f, &format!("/api/sessions/{id}/goal"), serde_json::json!({ "goal": "ship it" })).await;
        assert_eq!(status, StatusCode::OK, "{said}");

        let (_, sessions) = get(&f, "/api/sessions").await;
        assert_eq!(sessions["items"][0]["goal"], "ship it", "{sessions}");
    }

    #[tokio::test]
    async fn a_rewind_forks_rather_than_truncating() {
        let f = fixture();
        let id = rook_store::format_session_id(f.session);

        let (status, done) = post(
            &f,
            &format!("/api/sessions/{id}/rewind"),
            serde_json::json!({ "to_seq": 0, "restore_files": false }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{done}");
        assert_eq!(done["parent"], id, "the original is what it forked from: {done}");
        assert_ne!(done["session"], id, "and the answer is a new one: {done}");

        let (_, sessions) = get(&f, "/api/sessions").await;
        assert_eq!(sessions["items"].as_array().unwrap().len(), 2, "nothing was thrown away");
    }

    #[tokio::test]
    async fn a_goal_or_rewind_for_something_that_is_not_a_session_is_a_client_error() {
        let f = fixture();
        for (path, body) in [
            ("/api/sessions/not-a-ulid/goal", serde_json::json!({ "goal": "x" })),
            ("/api/sessions/not-a-ulid/rewind", serde_json::json!({ "to_seq": 0 })),
        ] {
            let (status, _) = post(&f, path, body).await;
            assert!(status.is_client_error(), "{path} answered {status}");
        }
    }

    /// The denominator is the project's, not a constant: a session at 55% of a
    /// 6k model reads as 1% of 128k, and this daemon serves several projects.
    #[tokio::test]
    async fn what_a_session_costs_is_measured_against_the_window_asked_for() {
        let f = fixture();
        let id = rook_store::format_session_id(f.session);

        let (status, own) = get(&f, &format!("/api/sessions/{id}/context")).await;
        assert_eq!(status, StatusCode::OK, "{own}");
        let configured = f.state.rook.read().await.context_window();
        assert_eq!(own["window"], configured, "the project's own window when none is named: {own}");

        let (_, narrow) = get(&f, &format!("/api/sessions/{id}/context?window=6000")).await;
        assert_eq!(narrow["window"], 6000, "{narrow}");
        assert!(
            narrow["usable"].as_u64().unwrap() < own["usable"].as_u64().unwrap(),
            "a smaller window leaves less room: {narrow}"
        );
    }

    #[tokio::test]
    async fn a_changes_request_for_something_that_is_not_a_session_is_a_client_error() {
        let f = fixture();
        let (status, _) = get(&f, "/api/sessions/not-a-ulid/changes").await;
        assert!(status.is_client_error(), "answered {status}");
    }

    /// How many projects a daemon is asked for is decided by whoever connects,
    /// which makes it a limit rather than a preference — and an engine holds a
    /// skill index and a plugin list.
    #[tokio::test]
    async fn the_engines_kept_for_projects_are_bounded() {
        let f = fixture();
        let dirs: Vec<tempfile::TempDir> = (0..5).map(|_| tempfile::tempdir().unwrap()).collect();

        for dir in &dirs {
            f.state.engine_for(Some(dir.path())).await.unwrap();
        }
        assert_eq!(f.state.elsewhere.read().await.len(), 3, "the cap is `[server] max_projects`");

        // The one asked for most recently is the one that survives.
        let newest = dirs.last().unwrap().path().canonicalize().unwrap();
        assert!(f.state.elsewhere.read().await.contains_key(&newest), "and the least wanted goes first");
        let oldest = dirs[0].path().canonicalize().unwrap();
        assert!(!f.state.elsewhere.read().await.contains_key(&oldest));

        // Dropped is not broken: naming it again builds it again.
        let again = f.state.engine_for(Some(dirs[0].path())).await.unwrap();
        assert_eq!(again.read().await.workspace, oldest);
    }

    /// A lock nobody can look at is one that cannot be debugged when it wedges.
    #[tokio::test]
    async fn what_is_being_written_can_be_read() {
        let f = fixture();
        let (empty_status, empty) = get(&f, "/api/writing").await;
        assert_eq!(empty_status, StatusCode::OK);
        assert_eq!(empty.as_array().unwrap().len(), 0, "nothing is being written yet: {empty}");

        // The guard borrows the engine, so the read is done while it is alive
        // rather than after — which is also the situation the endpoint exists
        // for: a claim is only interesting while somebody holds it.
        let held = {
            let rook = f.state.rook.read().await;
            let path = vec![rook.workspace.join("main.rs")];
            let _held = rook.writing(f.session, &path).unwrap();
            get(&f, "/api/writing").await.1
        };

        assert_eq!(held[0]["session"], rook_store::format_session_id(f.session), "{held}");
        assert!(held[0]["path"].as_str().unwrap().ends_with("main.rs"), "{held}");
        assert!(held[0]["held_for_secs"].is_number(), "how long it has been held is the useful part");
    }

    /// A project this daemon was not started in is served all the same: the
    /// store it holds is one per home, and a workspace is one per project.
    #[tokio::test]
    async fn a_connection_can_name_a_project_the_daemon_was_not_started_in() {
        let f = fixture();
        let elsewhere = tempfile::tempdir().unwrap();

        let engine = f.state.engine_for(Some(elsewhere.path())).await.unwrap();
        assert_eq!(
            engine.read().await.workspace,
            elsewhere.path().canonicalize().unwrap(),
            "the engine has to be looking at the project that was named"
        );
        assert!(
            std::ptr::eq(
                std::sync::Arc::as_ptr(&engine.read().await.store),
                std::sync::Arc::as_ptr(&f.state.rook.read().await.store)
            ),
            "and share the one store, or a second project is a second history"
        );

        let again = f.state.engine_for(Some(elsewhere.path())).await.unwrap();
        assert!(std::sync::Arc::ptr_eq(&engine, &again), "built once and kept, not rebuilt per prompt");

        // Naming the daemon's own project is not naming another one: a second
        // engine for the same directory is a second registry of who is writing
        // what, and two agents blind to each other's claims are what the claims
        // exist to prevent.
        let own = f.state.rook.read().await.workspace.clone();
        let named = f.state.engine_for(Some(&own)).await.unwrap();
        assert!(
            std::sync::Arc::ptr_eq(&named, &f.state.rook),
            "naming the daemon's own workspace must reach the daemon's own engine"
        );

        let missing = f.state.engine_for(Some(std::path::Path::new("/no/such/project"))).await;
        assert!(missing.is_err(), "a path that is not a directory is refused rather than created");
    }

    /// A websocket is not covered by the same-origin policy and is not
    /// preflighted, so without this any page the user has open could open one
    /// to a daemon on loopback — and this socket runs turns.
    fn upgrade_request(origin: Option<&str>) -> Request<Body> {
        let mut request = Request::builder()
            .uri("/api/chat")
            .header("host", "127.0.0.1:7717")
            .header("connection", "Upgrade")
            .header("upgrade", "websocket")
            .header("sec-websocket-version", "13")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==");
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        request.body(Body::empty()).unwrap()
    }

    /// A simple cross-origin POST — `text/plain`, no preflight — is what a
    /// page the user has open can send to a daemon on loopback without asking
    /// anybody. Every write here has to refuse it, and what makes them refuse
    /// is requiring a JSON body: two of them took no body at all and so ran.
    #[tokio::test]
    async fn every_write_refuses_a_request_that_was_never_preflighted() {
        let f = fixture();
        for path in [
            "/api/store/gc",
            "/api/store/prune",
            "/api/store/verify",
            "/api/store/train",
            "/api/maintenance",
            "/api/shutdown",
            "/api/memory/add",
            "/api/skills/install",
            "/api/skills/new",
            "/api/checkpoints",
        ] {
            let simple = Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "text/plain;charset=UTF-8")
                .header("origin", "http://evil.example")
                .body(Body::from("{}"))
                .unwrap();
            let answered = f.router.clone().oneshot(simple).await.unwrap();
            assert_eq!(
                answered.status(),
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "{path} accepted a request no browser had to ask permission for"
            );
        }
    }

    #[tokio::test]
    async fn a_chat_socket_refuses_an_upgrade_from_another_page() {
        let f = fixture();
        let refused = f.router.clone().oneshot(upgrade_request(Some("http://evil.example"))).await.unwrap();
        assert_eq!(refused.status(), StatusCode::FORBIDDEN, "a cross-origin upgrade must not connect");

        // Past the gate is as far as this harness goes: `oneshot` hands the
        // router a request with no connection under it, so the upgrade itself
        // answers "426 Upgrade Required". What matters here is which requests
        // the gate turns away, and 426 is not that.
        let own = f.router.clone().oneshot(upgrade_request(Some("http://127.0.0.1:7717"))).await.unwrap();
        assert_eq!(own.status(), StatusCode::UPGRADE_REQUIRED, "the daemon's own page gets through");

        // curl, an editor, these tests: not a browser, and a browser is what the
        // origin check exists for.
        let headless = f.router.clone().oneshot(upgrade_request(None)).await.unwrap();
        assert_eq!(headless.status(), StatusCode::UPGRADE_REQUIRED, "a client with no origin gets through");
    }

    /// Maintenance is the one request here that is neither quick nor waiting on
    /// IO — a prune, a collection and zstd training, all arithmetic and disk.
    /// Run where it was awaited it holds a runtime worker for the whole of it,
    /// and this test runs on a single-threaded runtime, which is what a small
    /// machine amounts to: the daemon has to keep answering.
    #[tokio::test]
    async fn a_long_maintenance_does_not_stop_the_daemon_answering() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let f = fixture();
        // Enough that the work is tens of milliseconds rather than none: with
        // an empty store there would be nothing to be blocked by.
        {
            let rook = f.state.rook.read().await;
            for i in 0..400 {
                rook.log(f.session, rook_store::EventKind::UserMessage, "p", &format!("event {i}")).unwrap();
            }
        }

        let finished = Arc::new(AtomicBool::new(false));
        let flag = finished.clone();
        let router = f.router.clone();
        let maintaining = tokio::spawn(async move {
            let response = router
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/maintenance")
                        .header("content-type", "application/json")
                        .body(Body::from(r#"{"dry_run":false}"#))
                        .unwrap(),
                )
                .await
                .unwrap();
            flag.store(true, Ordering::SeqCst);
            response.status()
        });
        // Let it start and reach its blocking call. Run on this thread, it
        // would instead run to completion here and the flag would already be
        // set — which is the failure this is about.
        tokio::task::yield_now().await;

        let (status, _) = get(&f, "/api/health").await;

        assert_eq!(status, StatusCode::OK);
        assert!(!finished.load(Ordering::SeqCst), "the daemon answered only after maintenance was done");
        assert_eq!(maintaining.await.unwrap(), StatusCode::OK, "and the maintenance itself finished");
    }

    #[tokio::test]
    async fn maintenance_reports_what_it_did_and_a_dry_run_does_nothing() {
        let f = fixture();
        let response = f
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/maintenance")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"dry_run":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["prune"]["dry_run"], true, "{body}");
        assert!(body["gc"].is_object(), "{body}");
        assert!(body["over_budget_by"].is_number(), "{body}");
    }
}
