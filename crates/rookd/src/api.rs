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
    let resolved = rook.skills().resolve(&name, &rook.env).map_err(CoreError::from)?;
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

#[derive(Deserialize)]
struct SearchQuery {
    #[serde(default)]
    q: String,
    #[serde(default = "default_limit")]
    limit: usize,
}

async fn search(
    State(s): State<Shared>,
    Query(query): Query<SearchQuery>,
) -> ApiResult<rook_core::search::Found> {
    let rook = s.rook.read().await;
    let options = rook_core::search::Search { limit: query.limit.min(200), ..Default::default() };
    Ok(Json(rook.search(&query.q, &options)?))
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

        let state = Arc::new(AppState {
            rook: Arc::new(tokio::sync::RwLock::new(rook)),
            started: std::time::Instant::now(),
        });
        Fixture { _home: home, _workspace: workspace, router: router(state), session }
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
