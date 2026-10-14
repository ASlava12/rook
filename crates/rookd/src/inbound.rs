//! Incoming requests only publish durable data; the ordinary supervisor runs work.
use crate::AppState;
use axum::{
    Json, Router,
    extract::{Path, Request, State},
    http::StatusCode,
    routing::post,
};
use std::sync::{Arc, LazyLock};
static ADMISSION: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(2)));
type Failure = (StatusCode, Json<rook_proto::ApiError>);
fn failure(status: StatusCode, message: impl ToString) -> Failure {
    (status, Json(rook_proto::ApiError::new("inbound", message.to_string())))
}
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/inbound/{route}", post(receive).layer(axum::extract::DefaultBodyLimit::disable()))
        .route("/api/events", post(inspect).layer(axum::extract::DefaultBodyLimit::max(8192)))
}
async fn receive(
    State(state): State<Arc<AppState>>,
    Path(route): Path<String>,
    request: Request,
) -> Result<(StatusCode, Json<serde_json::Value>), Failure> {
    let permit = ADMISSION.clone().try_acquire_owned().map_err(|_| {
        failure(StatusCode::TOO_MANY_REQUESTS, "event admission is busy; retry the same delivery")
    })?;
    let (parts, body) = request.into_parts();
    let header = |name: &str| -> Result<String, Failure> {
        let value =
            parts.headers.get(name).and_then(|v| v.to_str().ok()).filter(|v| v.len() <= 128).ok_or_else(
                || failure(StatusCode::UNAUTHORIZED, "missing or invalid event authentication headers"),
            )?;
        Ok(value.to_owned())
    };
    let delivery = header("x-rook-delivery")?;
    let timestamp = header("x-rook-timestamp")?;
    let signature = header("x-rook-signature")?;
    let limit = {
        let rook = state.rook.read().await;
        if !rook.config.inbound.enabled || !rook.config.inbound.routes.contains_key(&route) {
            return Err(failure(StatusCode::NOT_FOUND, "event handler is not configured"));
        }
        rook.config.inbound.body_bytes
    };
    let bytes = tokio::time::timeout(std::time::Duration::from_secs(20), axum::body::to_bytes(body, limit))
        .await
        .map_err(|_| failure(StatusCode::REQUEST_TIMEOUT, "event body did not arrive within its deadline"))?
        .map_err(|_| {
            failure(StatusCode::PAYLOAD_TOO_LARGE, "event body exceeded its byte limit or could not be read")
        })?;
    let rook = state.rook.clone().read_owned().await;
    let receiving = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let vault = rook_core::Vault::load().map_err(|_| {
            rook_core::CoreError::InboundAuthentication(
                "credential vault is unavailable; repair its configuration".into(),
            )
        })?;
        rook.receive_event(
            rook_core::inbound::Delivery {
                route: &route,
                id: &delivery,
                timestamp: &timestamp,
                signature: &signature,
                body: &bytes,
            },
            &vault,
        )
    });
    let result = tokio::time::timeout(std::time::Duration::from_secs(20), receiving)
        .await
        .map_err(|_| {
            failure(
                StatusCode::GATEWAY_TIMEOUT,
                "event admission remains uncertain; retry the same delivery after inspecting its receipt",
            )
        })?
        .map_err(|e| failure(StatusCode::SERVICE_UNAVAILABLE, e))?
        .map_err(|e| {
            failure(
                if matches!(e, rook_core::CoreError::InboundAuthentication(_)) {
                    StatusCode::UNAUTHORIZED
                } else {
                    StatusCode::BAD_REQUEST
                },
                e,
            )
        })?;
    Ok((StatusCode::ACCEPTED, Json(result)))
}
async fn inspect(
    State(state): State<Arc<AppState>>,
    Json(request): Json<rook_core::inbound::Request>,
) -> Result<Json<serde_json::Value>, Failure> {
    let rook = state.rook.read().await;
    Ok(Json(rook.event_request(request).map_err(|e| failure(StatusCode::BAD_REQUEST, e))?))
}
pub(crate) async fn tick(state: &Arc<AppState>) {
    let (pending, enabled) = {
        let rook = state.rook.read().await;
        (rook.pending_events(), rook.config.inbound.enabled)
    };
    if !enabled {
        return;
    }
    let pending = match pending {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("cannot read inbound deliveries: {e}");
            return;
        }
    };
    static CURSOR: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let start = CURSOR.fetch_add(16, std::sync::atomic::Ordering::Relaxed) % pending.len().max(1);
    let mut seen = std::collections::BTreeSet::new();
    for id in pending.iter().skip(start).chain(pending.iter().take(start)).take(16) {
        let workspace = {
            let rook = state.rook.read().await;
            rook.event_workspace(id)
        };
        let Ok(workspace) = workspace else { continue };
        if !seen.insert(workspace.clone()) {
            continue;
        }
        let engine = match state.engine_for(Some(std::path::Path::new(&workspace))).await {
            Ok(engine) => engine,
            Err(e) => {
                tracing::warn!("cannot prepare inbound workspace: {e}");
                continue;
            }
        };
        let rook = engine.read().await;
        // Busy workspaces leave their bounded delivery in the durable inbox.
        let _ = rook.launch_event(id);
    }
}
