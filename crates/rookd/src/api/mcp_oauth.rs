//! Bounded, daemon-owned consent attempts. No token or verifier enters an API DTO.
use super::*;
use axum::http::{HeaderMap, Uri};
use rook_core::mcp_auth::{self, Login};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Mutex;
use tokio::time::{Duration, Instant};

pub(super) fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/mcp/oauth", get(list))
        .route("/api/mcp/oauth/complete", post(complete))
        .route("/api/mcp/oauth/{id}", get(status).delete(cancel))
        .route("/api/mcp/{name}/login", post(begin))
        .route("/api/mcp/{name}/logout", post(logout))
        .route_layer(axum::middleware::from_fn(private_response))
}
async fn private_response(request: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert("cache-control", axum::http::HeaderValue::from_static("no-store"));
    response
}

#[derive(Default)]
pub(crate) struct Logins {
    entries: Mutex<BTreeMap<u128, Attempt>>,
}
struct Attempt {
    workspace: std::path::PathBuf,
    server: String,
    deadline: Instant,
    login: Option<Login>,
    status: &'static str,
    error: Option<String>,
}
#[derive(Serialize)]
pub(super) struct Report {
    id: String,
    server: String,
    status: &'static str,
    authorization_url: Option<String>,
    expires_in: u64,
    error: Option<String>,
}
impl Attempt {
    fn report(&self, id: u128) -> Report {
        Report {
            id: id.to_string(),
            server: self.server.clone(),
            status: self.status,
            authorization_url: self.login.as_ref().map(|l| l.url().to_owned()),
            expires_in: self.deadline.saturating_duration_since(Instant::now()).as_secs(),
            error: self.error.clone(),
        }
    }
}
impl Logins {
    fn entries(&self) -> std::sync::MutexGuard<'_, BTreeMap<u128, Attempt>> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.retain(|_, value| value.status == "completing" || value.deadline > Instant::now());
        entries
    }
}
// A disconnected/cancelled request must not leave an admission slot occupied.
struct Active {
    state: Shared,
    id: u128,
    armed: bool,
}
impl Drop for Active {
    fn drop(&mut self) {
        if self.armed {
            self.state.oauth.entries().remove(&self.id);
        }
    }
}
fn failure(message: impl Into<String>) -> Fail {
    Fail(StatusCode::CONFLICT, ApiError::new("mcp_oauth_failed", message.into()))
}
fn absent() -> Fail {
    Fail(
        StatusCode::NOT_FOUND,
        ApiError::new("mcp_oauth_missing", "sign-in is missing, cancelled or expired; start again"),
    )
}

#[derive(Deserialize)]
pub(super) struct Start {
    redirect_uri: String,
}
#[derive(Deserialize)]
pub(super) struct Callback {
    callback: String,
}

fn check_callback(headers: &HeaderMap, callback: &str, query: bool) -> Result<(), Fail> {
    if callback.len() > 32 * 1024 {
        return Err(failure("OAuth callback exceeds 32 KiB"));
    }
    let uri: Uri = callback.parse().map_err(|_| failure("invalid OAuth callback URL"))?;
    let host =
        headers.get("host").and_then(|h| h.to_str().ok()).ok_or_else(|| failure("missing callback Host"))?;
    let scheme = uri.scheme_str().ok_or_else(|| failure("callback requires an absolute URL"))?;
    if !matches!(scheme, "http" | "https")
        || uri.authority().map(|a| a.as_str()) != Some(host)
        || uri.path() != "/mcp-oauth-callback.html"
        || (!query && uri.query().is_some())
        || callback.contains('#')
    {
        return Err(failure("OAuth callback must use this Rook origin and /mcp-oauth-callback.html"));
    }
    let origin = format!("{scheme}://{host}");
    if headers.get("origin").is_some_and(|h| h.to_str().ok() != Some(origin.as_str())) {
        return Err(failure("OAuth callback origin does not match the browser"));
    }
    Ok(())
}

pub(super) async fn begin(
    State(s): State<Shared>,
    Path(name): Path<String>,
    Query(q): Query<McpQuery>,
    headers: HeaderMap,
    Json(body): Json<Start>,
) -> ApiResult<Report> {
    check_callback(&headers, &body.redirect_uri, false)?;
    let (_, workspace) = mcp_session(&s, &q).await?;
    let (config, proxy, limits) = mcp_auth::configured(&workspace, &name).map_err(failure)?;
    let id = rook_store::new_session_id();
    {
        let mut entries = s.oauth.entries();
        if entries.len() >= limits.oauth_max_pending {
            return Err(failure("too many MCP sign-ins; cancel one or wait for expiry"));
        }
        if entries.values().any(|entry| {
            entry.workspace == workspace
                && entry.server == name
                && matches!(entry.status, "starting" | "waiting" | "completing")
        }) {
            return Err(failure("this server already has a pending sign-in; complete or cancel it first"));
        }
        if entries.contains_key(&id) {
            return Err(failure("OAuth attempt identifiers exhausted; restart the daemon"));
        }
        entries.insert(
            id,
            Attempt {
                workspace,
                server: name,
                deadline: Instant::now()
                    + Duration::from_secs(config.oauth.timeout_secs + config.oauth.login_timeout_secs),
                login: None,
                status: "starting",
                error: None,
            },
        );
    }
    let mut active = Active { state: s.clone(), id, armed: true };
    let login = Login::begin(&config, &proxy, limits, body.redirect_uri).await.map_err(failure)?;
    let mut entries = s.oauth.entries();
    let attempt = entries.get_mut(&id).ok_or_else(absent)?;
    attempt.deadline = Instant::now() + Duration::from_secs(config.oauth.login_timeout_secs);
    attempt.login = Some(login);
    attempt.status = "waiting";
    let report = attempt.report(id);
    active.armed = false;
    Ok(Json(report))
}

pub(super) async fn list(State(s): State<Shared>, Query(q): Query<McpQuery>) -> ApiResult<Vec<Report>> {
    let (_, workspace) = mcp_session(&s, &q).await?;
    Ok(Json(
        s.oauth
            .entries()
            .iter()
            .filter(|(_, a)| a.workspace == workspace)
            .map(|(id, a)| a.report(*id))
            .collect(),
    ))
}

pub(super) async fn status(State(s): State<Shared>, Path(id): Path<u128>) -> ApiResult<Report> {
    let entries = s.oauth.entries();
    let attempt = entries.get(&id).ok_or_else(absent)?;
    Ok(Json(attempt.report(id)))
}

pub(super) async fn cancel(State(s): State<Shared>, Path(id): Path<u128>) -> ApiResult<serde_json::Value> {
    let mut entries = s.oauth.entries();
    if entries.get(&id).is_some_and(|a| a.status == "completing") {
        return Err(failure("OAuth code exchange is already running; wait for its result"));
    }
    entries.remove(&id);
    Ok(Json(serde_json::json!({"cancelled":true})))
}

pub(super) async fn complete(
    State(s): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<Callback>,
) -> ApiResult<Report> {
    check_callback(&headers, &body.callback, true)?;
    let (id, workspace, name, login) = {
        let mut entries = s.oauth.entries();
        let (id, attempt) = entries
            .iter_mut()
            .find(|(_, a)| a.login.as_ref().is_some_and(|l| l.accepts(&body.callback)))
            .ok_or_else(absent)?;
        let login = attempt.login.take().ok_or_else(absent)?;
        attempt.status = "completing";
        (*id, attempt.workspace.clone(), attempt.server.clone(), login)
    };
    let mut active = Active { state: s.clone(), id, armed: true };
    let outcome = async {
        let (current, _, _) = mcp_auth::configured(&workspace, &name).map_err(str::to_owned)?;
        if !login.matches_config(&current) {
            return Err("MCP config changed during sign-in; start again".into());
        }
        login.complete(&body.callback).await.map_err(str::to_owned)?;
        let q = McpQuery { workspace: Some(workspace), session: None };
        let (session, workspace) = mcp_session(&s, &q).await.map_err(|_| {
            "signed in, but workspace equipment is unavailable; reconnect manually".to_string()
        })?;
        session.reconnect_in(&workspace, &name).await.map_err(|_| {
            "signed in, but reconnect failed; use /mcp reconnect or the reconnect button".to_string()
        })?;
        Ok::<(), String>(())
    }
    .await;
    let mut entries = s.oauth.entries();
    let attempt = entries.get_mut(&id).ok_or_else(absent)?;
    attempt.status = if outcome.is_ok() { "connected" } else { "failed" };
    attempt.error = outcome.err();
    attempt.deadline = Instant::now() + Duration::from_secs(60);
    let report = attempt.report(id);
    active.armed = false;
    Ok(Json(report))
}

pub(super) async fn logout(
    State(s): State<Shared>,
    Path(name): Path<String>,
    Query(q): Query<McpQuery>,
    Json(_): Json<serde_json::Value>,
) -> ApiResult<serde_json::Value> {
    let (_, workspace) = mcp_session(&s, &q).await?;
    let (config, _, limits) = mcp_auth::configured(&workspace, &name).map_err(failure)?;
    {
        let mut entries = s.oauth.entries();
        if entries.values().any(|a| a.workspace == workspace && a.server == name && a.status == "completing")
        {
            return Err(failure("sign-in completion is running; wait before signing out"));
        }
        entries.retain(|_, a| a.workspace != workspace || a.server != name);
    }
    mcp_auth::logout(&config, limits).await.map_err(failure)?;
    Ok(Json(serde_json::json!({"server":name,"authenticated":false})))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callbacks_stay_on_the_trusted_browser_origin_and_fixed_path() {
        let mut headers = HeaderMap::new();
        headers.insert("host", "rook.example".parse().unwrap());
        headers.insert("origin", "https://rook.example".parse().unwrap());
        assert!(check_callback(&headers, "https://rook.example/mcp-oauth-callback.html", false).is_ok());
        assert!(
            check_callback(&headers, "https://rook.example/mcp-oauth-callback.html?code=one&state=two", true)
                .is_ok()
        );
        for url in [
            "http://rook.example/mcp-oauth-callback.html",
            "https://other.example/mcp-oauth-callback.html",
            "https://rook.example/other",
            "https://rook.example/mcp-oauth-callback.html#fragment",
            "https://user@rook.example/mcp-oauth-callback.html",
        ] {
            assert!(check_callback(&headers, url, true).is_err(), "accepted {url}");
        }
        assert!(
            check_callback(&headers, "https://rook.example/mcp-oauth-callback.html?state=preset", false)
                .is_err()
        );
        let oversized = format!("https://rook.example/mcp-oauth-callback.html?code={}", "x".repeat(32768));
        assert!(oversized.len() > 32768);
        assert!(check_callback(&headers, &oversized, true).is_err());
    }
    #[tokio::test(start_paused = true)]
    async fn expired_attempts_leave_the_bounded_registry_but_active_completion_keeps_its_slot() {
        let logins = Logins::default();
        for (id, status) in [(0, "starting"), (1, "waiting"), (2, "completing"), (3, "connected")] {
            logins.entries().insert(
                id,
                Attempt {
                    workspace: "/test".into(),
                    server: "fixture".into(),
                    deadline: Instant::now() + Duration::from_secs(30),
                    login: None,
                    status,
                    error: None,
                },
            );
        }
        assert_eq!(logins.entries().len(), 4);
        tokio::time::advance(Duration::from_secs(31)).await;
        let entries = logins.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries.get(&2).unwrap().status, "completing");
    }
}
