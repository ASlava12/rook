//! Authenticated peer ingress publishes bounded work; the normal supervisor owns execution.
use crate::AppState;
use axum::response::IntoResponse;
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
fn fail(status: StatusCode, message: impl ToString) -> Failure {
    (status, Json(rook_proto::ApiError::new("a2a", message.to_string())))
}
pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/api/a2a/{route}", post(receive).layer(axum::extract::DefaultBodyLimit::disable()))
}
async fn receive(
    State(state): State<Arc<AppState>>,
    Path(route): Path<String>,
    request: Request,
) -> Result<axum::response::Response, Failure> {
    let permit = ADMISSION.clone().try_acquire_owned().map_err(|_| {
        fail(StatusCode::TOO_MANY_REQUESTS, "remote agent admission busy; retry the same request")
    })?;
    let (parts, body) = request.into_parts();
    let header = |name: &str| {
        parts
            .headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .filter(|value| value.len() <= 128)
            .map(str::to_owned)
            .ok_or_else(|| fail(StatusCode::UNAUTHORIZED, "missing or invalid A2A authentication headers"))
    };
    let delivery = header("x-rook-delivery")?;
    let timestamp = header("x-rook-timestamp")?;
    let signature = header("x-rook-signature")?;
    let limit = {
        let rook = state.rook.read().await;
        if !rook.config.remote_agents.enabled
            || !rook.config.remote_agents.routes.contains(&route)
            || !rook.config.inbound.enabled
            || !rook.config.inbound.routes.contains_key(&route)
        {
            return Err(fail(StatusCode::NOT_FOUND, "remote agent route is not configured"));
        }
        rook.config.remote_agents.request_bytes.min(rook.config.inbound.body_bytes).min(65536)
    };
    let bytes = tokio::time::timeout(std::time::Duration::from_secs(20), axum::body::to_bytes(body, limit))
        .await
        .map_err(|_| fail(StatusCode::REQUEST_TIMEOUT, "remote request body timed out"))?
        .map_err(|_| fail(StatusCode::PAYLOAD_TOO_LARGE, "remote request exceeds body limit"))?;
    let rook = state.rook.clone().read_owned().await;
    let receiving = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let vault = rook_core::Vault::load().map_err(|_| {
            rook_core::CoreError::InboundAuthentication("credential vault unavailable".into())
        })?;
        rook.agent_http_reply(
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
    let (bytes, signature) = tokio::time::timeout(std::time::Duration::from_secs(20), receiving)
        .await
        .map_err(|_| {
            fail(StatusCode::GATEWAY_TIMEOUT, "A2A admission remains uncertain; retry only the same request")
        })?
        .map_err(|_| fail(StatusCode::SERVICE_UNAVAILABLE, "A2A admission worker interrupted"))?
        .map_err(|error| {
            fail(
                if matches!(error, rook_core::CoreError::InboundAuthentication(_)) {
                    StatusCode::UNAUTHORIZED
                } else {
                    StatusCode::BAD_REQUEST
                },
                error,
            )
        })?;
    Ok(([("content-type", "application/json"), ("x-rook-response-signature", signature.as_str())], bytes)
        .into_response())
}
