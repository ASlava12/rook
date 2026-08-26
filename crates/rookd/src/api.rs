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
        .route("/api/skills", get(skills))
        .route("/api/skills/{name}", get(skill))
        .route("/api/skills/{name}/history", get(skill_history))
        .route("/api/checkpoints", get(checkpoints))
        .route("/api/maintenance", post(maintenance))
        .route("/api/chat", get(crate::chat::upgrade))
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

async fn health(State(s): State<Shared>) -> ApiResult<Health> {
    let rook = s.rook.read().await;
    Ok(Json(Health {
        ok: true,
        version: rook_core::AGENT_VERSION.to_string(),
        api_version: API_VERSION,
        store_root: rook.store.root().display().to_string(),
        workspace: rook.workspace.display().to_string(),
        os: rook.env.os.clone(),
        arch: rook.env.arch.clone(),
        uptime_secs: s.started.elapsed().as_secs(),
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

async fn objects(State(s): State<Shared>, Query(q): Query<ListQuery>) -> ApiResult<Page<serde_json::Value>> {
    let rook = s.rook.read().await;
    let kind = q.kind.as_deref().and_then(parse_kind);
    let items = rook
        .store
        .list_objects(kind, q.limit.min(1000))
        .map_err(CoreError::from)?
        .into_iter()
        .map(|(id, m)| {
            serde_json::json!({
                "id": id.to_hex(),
                "short": id.short(),
                "kind": rook_store::Kind::from_u8(m.kind).as_str(),
                "size_raw": m.size_raw,
                "size_stored": m.size_stored,
                "external": m.external,
                "created_at": m.created_at,
            })
        })
        .collect();
    Ok(Json(Page::new(items)))
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

async fn refs(State(s): State<Shared>, Query(q): Query<PrefixQuery>) -> ApiResult<Page<serde_json::Value>> {
    let rook = s.rook.read().await;
    let items = rook
        .store
        .list_refs(&q.prefix)
        .map_err(CoreError::from)?
        .into_iter()
        .map(|(name, id)| serde_json::json!({ "ref": name, "object": id.to_hex(), "short": id.short() }))
        .collect();
    Ok(Json(Page::new(items)))
}

async fn sessions(State(s): State<Shared>) -> ApiResult<Page<rook_store::SessionMeta>> {
    let rook = s.rook.read().await;
    Ok(Json(Page::new(rook.sessions()?)))
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

async fn skills(State(s): State<Shared>) -> ApiResult<Page<rook_skills::SkillCard>> {
    let rook = s.rook.read().await;
    Ok(Json(Page::new(rook.catalog())))
}

async fn skill(State(s): State<Shared>, Path(name): Path<String>) -> ApiResult<serde_json::Value> {
    let rook = s.rook.read().await;
    let resolved = rook.skills.resolve(&name, &rook.env).map_err(CoreError::from)?;
    Ok(Json(serde_json::json!({
        "name": resolved.skill.manifest.name,
        "version": resolved.skill.version().to_string(),
        "source": resolved.skill.source.label(),
        "dir": resolved.skill.dir,
        "variant": resolved.variant.as_ref().map(|v| v.body.display().to_string()),
        "rejected": resolved.rejected,
        "body": resolved.body,
    })))
}

async fn skill_history(
    State(s): State<Shared>,
    Path(name): Path<String>,
) -> ApiResult<Page<rook_core::SkillVersionRecord>> {
    let rook = s.rook.read().await;
    Ok(Json(Page::new(rook.skill_history(&name)?)))
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
async fn maintenance(
    State(s): State<Shared>,
    Json(body): Json<MaintenanceBody>,
) -> ApiResult<rook_core::MaintenanceReport> {
    let rook = s.rook.read().await;
    Ok(Json(rook.maintenance(body.dry_run)?))
}
