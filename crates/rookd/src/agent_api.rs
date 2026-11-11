//! Bearer-scoped automation and stateless Streamable HTTP MCP.
//! Both projections use the same core admission, receipts and worker supervisor.
use crate::AppState;
use axum::{
    Json, Router,
    extract::{Path, Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::sync::{Arc, LazyLock};

static ADMISSION: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(2)));
const VERSIONS: &[&str] = &["2025-03-26", "2025-06-18", "2025-11-25"];

fn failure(status: StatusCode, text: impl ToString) -> Response {
    let text = text.to_string();
    let mut response =
        (status, Json(rook_proto::ApiError::new("agent_api", rook_llm::truncate(&text, 1024))))
            .into_response();
    if status == StatusCode::UNAUTHORIZED {
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            axum::http::HeaderValue::from_static("Bearer realm=\"rook-agent\""),
        );
    }
    response
}
pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/integrations", get(discovery))
        .route("/api/agent/{route}", post(rest).layer(axum::extract::DefaultBodyLimit::disable()))
        .route(
            "/api/mcp/agent/{route}",
            post(mcp).get(no_stream).delete(no_stream).layer(axum::extract::DefaultBodyLimit::disable()),
        )
}
async fn discovery(State(state): State<Arc<AppState>>) -> Json<Value> {
    let rook = state.rook.read().await;
    let enabled = rook.config.remote_agents.enabled
        && rook.config.inbound.enabled
        && rook.config.validation_errors().is_empty();
    let routes = rook
        .config
        .remote_agents
        .api_routes
        .iter()
        .take(32)
        .map(|route| {
            json!({
                "name":route,"http":format!("/api/agent/{route}"),"mcp":format!("/api/mcp/agent/{route}")
            })
        })
        .collect::<Vec<_>>();
    Json(
        json!({"enabled":enabled,"routes":routes,"authentication":"Bearer route credential from the operator vault",
        "mcp_protocol_versions":VERSIONS,"mcp_transport":"streamable-http-json","tools":rook_core::agent_api::tools(),
        "request_bytes":rook.config.remote_agents.request_bytes,"response_bytes":rook.config.remote_agents.response_bytes}),
    )
}
async fn no_stream() -> Response {
    (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "POST")]).into_response()
}
async fn rest(State(state): State<Arc<AppState>>, Path(route): Path<String>, request: Request) -> Response {
    receive(state, route, request, false).await
}
async fn mcp(State(state): State<Arc<AppState>>, Path(route): Path<String>, request: Request) -> Response {
    receive(state, route, request, true).await
}

async fn receive(state: Arc<AppState>, route: String, request: Request, is_mcp: bool) -> Response {
    let Ok(permit) = ADMISSION.clone().try_acquire_owned() else {
        return failure(
            StatusCode::TOO_MANY_REQUESTS,
            "Agent admission is busy. Retry only the original request name and arguments.",
        );
    };
    let (parts, body) = request.into_parts();
    let credential =
        parts.headers.get(header::AUTHORIZATION).and_then(|value| value.to_str().ok()).and_then(|value| {
            let (scheme, token) = value.split_once(' ')?;
            (scheme.eq_ignore_ascii_case("bearer") && token.len() <= 4096).then(|| token.to_string())
        });
    let Some(credential) = credential else {
        return failure(StatusCode::UNAUTHORIZED, "A configured route bearer credential is required");
    };
    let limit = {
        let rook = state.rook.read().await;
        if !rook.config.remote_agents.enabled || !rook.config.remote_agents.api_routes.contains(&route) {
            return failure(StatusCode::NOT_FOUND, "Agent API route is not enabled");
        }
        rook.config.remote_agents.request_bytes.min(rook.config.inbound.body_bytes).min(65536)
    };
    if !parts.headers.get(header::CONTENT_TYPE).and_then(|value| value.to_str().ok()).is_some_and(|value| {
        value.split(';').next().is_some_and(|kind| kind.trim().eq_ignore_ascii_case("application/json"))
    }) {
        return failure(StatusCode::UNSUPPORTED_MEDIA_TYPE, "Agent requests require application/json");
    }
    if is_mcp {
        let accepts = parts.headers.get(header::ACCEPT).and_then(|value| value.to_str().ok()).unwrap_or("");
        let admits = |kind: &str| {
            accepts.split(',').any(|value| value.split(';').next().is_some_and(|value| value.trim() == kind))
        };
        if !admits("application/json") || !admits("text/event-stream") {
            return failure(
                StatusCode::NOT_ACCEPTABLE,
                "MCP Accept must include application/json and text/event-stream",
            );
        }
        if parts
            .headers
            .get("mcp-protocol-version")
            .is_some_and(|value| value.to_str().ok().is_none_or(|version| !VERSIONS.contains(&version)))
        {
            return failure(StatusCode::BAD_REQUEST, "Unsupported MCP-Protocol-Version");
        }
    }
    let bytes =
        match tokio::time::timeout(std::time::Duration::from_secs(20), axum::body::to_bytes(body, limit))
            .await
        {
            Err(_) => return failure(StatusCode::REQUEST_TIMEOUT, "Agent request body timed out"),
            Ok(Err(_)) => {
                return failure(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "Agent request exceeds the configured byte limit",
                );
            }
            Ok(Ok(bytes)) => bytes,
        };
    let rook = state.rook.clone().read_owned().await;
    let operation = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let vault =
            rook_core::Vault::load().map_err(|_| (true, "Agent credential vault unavailable".to_string()))?;
        let result = if is_mcp {
            mcp_reply(&rook, &vault, &route, &credential, &bytes)
        } else {
            rook.receive_agent_api(&route, &credential, &bytes, &vault)
                .and_then(|value| rook.encode_agent_api_response(&value).map(Some))
        };
        result.map_err(|error| {
            let unauthorized = matches!(error, rook_core::CoreError::InboundAuthentication(_));
            (unauthorized, vault.redact(&error.to_string()))
        })
    });
    match tokio::time::timeout(std::time::Duration::from_secs(20), operation).await {
        Ok(Ok(Ok(Some(bytes)))) => {
            ([(header::CONTENT_TYPE, "application/json"), (header::CACHE_CONTROL, "no-store")], bytes)
                .into_response()
        }
        Ok(Ok(Ok(None))) => StatusCode::ACCEPTED.into_response(),
        Ok(Ok(Err((unauthorized, error)))) => {
            failure(if unauthorized { StatusCode::UNAUTHORIZED } else { StatusCode::BAD_REQUEST }, error)
        }
        Ok(Err(_)) => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "Agent admission worker interrupted; a write remains uncertain",
        ),
        Err(_) => failure(
            StatusCode::GATEWAY_TIMEOUT,
            "Agent admission remains uncertain. Retry only the original request name and arguments.",
        ),
    }
}

fn rpc_error(id: Value, code: i32, message: impl Into<String>) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message.into()}})
}

fn mcp_reply(
    rook: &rook_core::Rook,
    vault: &rook_core::Vault,
    route: &str,
    credential: &str,
    bytes: &[u8],
) -> rook_core::Result<Option<Vec<u8>>> {
    // Every message, including discovery and notifications, authenticates first.
    rook.authorize_agent_api(route, credential, vault)?;
    let call: Value = match serde_json::from_slice(bytes) {
        Ok(call) => call,
        Err(_) => {
            return rook.encode_agent_api_response(&rpc_error(Value::Null, -32700, "Invalid JSON")).map(Some);
        }
    };
    let id = call.get("id").cloned().unwrap_or(Value::Null);
    if !call.is_object()
        || call["jsonrpc"] != "2.0"
        || !id.is_null() && !(id.is_i64() || id.is_u64() || id.as_str().is_some_and(|id| id.len() <= 128))
    {
        return rook
            .encode_agent_api_response(&rpc_error(Value::Null, -32600, "Invalid JSON-RPC request"))
            .map(Some);
    }
    let method = call["method"].as_str().unwrap_or("");
    if call.get("id").is_none() {
        return Ok(None);
    }
    if method.is_empty() {
        return Ok(None);
    }
    let params = call.get("params").cloned().unwrap_or_else(|| json!({}));
    let reply = if !params.is_object() {
        rpc_error(id, -32602, "params must be an object")
    } else {
        match method {
            "initialize" => {
                let proposed = params["protocolVersion"].as_str().unwrap_or("");
                if proposed.is_empty() {
                    rpc_error(id, -32602, "initialize requires protocolVersion")
                } else {
                    json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":if VERSIONS.contains(&proposed){proposed}else{"2025-11-25"},
                "capabilities":{"tools":{}},"serverInfo":{"name":"rook-agent","version":rook_core::AGENT_VERSION},
                "instructions":"Submit durable work with unique stable request names. Inspect status and result pages. Worker policy and finite route budgets apply. Cancel explicitly; disconnecting a client does not stop its tasks."}})
                }
            }
            "ping" => json!({"jsonrpc":"2.0","id":id,"result":{}}),
            "tools/list" => {
                if params.get("cursor").is_some_and(|cursor| !cursor.is_null()) {
                    rpc_error(id, -32602, "The complete bounded tool list has no cursor")
                } else {
                    json!({"jsonrpc":"2.0","id":id,"result":{"tools":rook_core::agent_api::tools()}})
                }
            }
            "tools/call" => {
                let name = params["name"].as_str().unwrap_or("");
                if !rook_core::agent_api::tools().iter().any(|tool| tool["name"] == name) {
                    rpc_error(id, -32602, "Unknown Rook agent tool")
                } else if let Some(mut arguments) =
                    params.get("arguments").cloned().unwrap_or_else(|| json!({})).as_object().cloned()
                {
                    if arguments.contains_key("action") {
                        rpc_error(id, -32602, "action is assigned by the named tool")
                    } else {
                        let request = arguments.remove("request");
                        arguments.insert("action".into(), json!(name.trim_start_matches("rook_")));
                        let mut envelope = json!({"operation":arguments});
                        if let Some(request) = request {
                            envelope["request"] = request;
                        }
                        let input = serde_json::to_vec(&envelope)?;
                        let result = match rook.receive_agent_api(route, credential, &input, vault) {
                            Ok(value) => {
                                json!({"content":[{"type":"text","text":serde_json::to_string(&value)?}],"isError":false})
                            }
                            Err(error) => {
                                json!({"content":[{"type":"text","text":vault.redact(&error.to_string())}],"isError":true})
                            }
                        };
                        json!({"jsonrpc":"2.0","id":id,"result":result})
                    }
                } else {
                    rpc_error(id, -32602, "Tool arguments must be an object")
                }
            }
            _ => rpc_error(id, -32601, "Method not implemented"),
        }
    };
    match rook.encode_agent_api_response(&reply){
        Ok(bytes)=>Ok(Some(bytes)),
        Err(_)=>rook.encode_agent_api_response(&rpc_error(call["id"].clone(),-32603,"Reply exceeds the configured byte limit. A write may already be accepted; retry only the original request name and arguments.")).map(Some),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const TOKEN: &str = "mcp-agent-fixture-private-token";
    fn fixture() -> (tempfile::TempDir, rook_core::Rook, rook_core::Vault) {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        let mut config = rook_core::Config::default();
        config.remote_agents.enabled = true;
        config.remote_agents.routes = ["assistant".into()].into();
        config.remote_agents.api_routes = config.remote_agents.routes.clone();
        config.inbound.enabled = true;
        config.inbound.routes.insert(
            "assistant".into(),
            rook_core::inbound::Route {
                workspace: workspace.canonicalize().unwrap().display().to_string(),
                secret: "api".into(),
                task: "Complete operation.text within this project.".into(),
                autonomous: false,
                max_iterations: 2,
                max_tokens: 10000,
                max_seconds: 120,
            },
        );
        let rook = rook_core::Rook::from_parts(
            rook_store::Store::open(root.path().join("store")).unwrap(),
            config,
            rook_skills::Environment::bare("test", "test", "test"),
            rook_skills::SkillIndex::default(),
            workspace,
        );
        let mut vault = rook_core::Vault::load_from(root.path().join("secrets.toml")).unwrap();
        vault.keep("api", TOKEN).unwrap();
        (root, rook, vault)
    }
    fn rpc(rook: &rook_core::Rook, vault: &rook_core::Vault, id: u32, method: &str, params: Value) -> Value {
        let bytes =
            serde_json::to_vec(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})).unwrap();
        serde_json::from_slice(&mcp_reply(rook, vault, "assistant", TOKEN, &bytes).unwrap().unwrap()).unwrap()
    }
    #[test]
    fn mcp_agent_discovery_and_calls_share_the_rest_receipt_without_running_notification_writes() {
        let (_root, rook, vault) = fixture();
        let initialized = rpc(
            &rook,
            &vault,
            1,
            "initialize",
            json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}),
        );
        assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
        let list = rpc(&rook, &vault, 2, "tools/list", json!({}));
        let tools = list["result"]["tools"].as_array().unwrap();
        assert!(tools.iter().any(|tool| tool["name"] == "rook_submit"));
        assert_eq!(rpc(&rook, &vault, 2, "tools/list", json!({"cursor":null})), list);
        let params = json!({"name":"rook_submit","arguments":{"request":"one","text":"Inspect example.js"}});
        let notify =
            serde_json::to_vec(&json!({"jsonrpc":"2.0","method":"tools/call","params":params})).unwrap();
        assert!(mcp_reply(&rook, &vault, "assistant", TOKEN, &notify).unwrap().is_none());
        assert!(rook.pending_events().unwrap().is_empty());
        let submitted = rpc(&rook, &vault, 3, "tools/call", params);
        assert_eq!(submitted["result"]["isError"], false);
        let first: Value =
            serde_json::from_str(submitted["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        let retried = rook
            .receive_agent_api(
                "assistant",
                TOKEN,
                br#"{"request":"one","operation":{"action":"submit","text":"Inspect example.js"}}"#,
                &vault,
            )
            .unwrap();
        assert_eq!(retried["result"]["id"], first["result"]["id"]);
        assert_eq!(retried["result"]["duplicate"], true);
        assert_eq!(rook.pending_events().unwrap().len(), 1);
        let forged = rpc(
            &rook,
            &vault,
            4,
            "tools/call",
            json!({"name":"rook_info","arguments":{"action":"submit","text":"forged"}}),
        );
        assert_eq!(forged["error"]["code"], -32602);
        let invalid = rpc(
            &rook,
            &vault,
            5,
            "tools/call",
            json!({"name":"rook_submit","arguments":{"text":"missing stable request"}}),
        );
        assert_eq!(invalid["result"]["isError"], true);
        assert_eq!(rook.pending_events().unwrap().len(), 1);
    }
    #[test]
    fn mcp_authentication_precedes_parsing_and_discovery_does_not_depend_on_a_large_worker_description() {
        let (_root, mut rook, vault) = fixture();
        assert!(matches!(
            mcp_reply(&rook, &vault, "assistant", "incorrect-but-long-token", b"not json"),
            Err(rook_core::CoreError::InboundAuthentication(_))
        ));
        let invalid = mcp_reply(&rook, &vault, "assistant", TOKEN, b"not json").unwrap().unwrap();
        let error: Value = serde_json::from_slice(&invalid).unwrap();
        assert_eq!(error["error"]["code"], -32700);
        rook.config.remote_agents.response_bytes = 32768;
        rook.config.inbound.routes.get_mut("assistant").unwrap().task = format!("x{}", "\n".repeat(16382));
        let oversized = json!({"task":rook.config.inbound.routes["assistant"].task});
        assert!(serde_json::to_vec(&oversized).unwrap().len() > rook.config.remote_agents.response_bytes);
        let listed = rpc(&rook, &vault, 1, "tools/list", json!({}));
        assert!(listed["result"]["tools"].is_array());
        assert!(serde_json::to_vec(&listed).unwrap().len() < rook.config.remote_agents.response_bytes);
    }
}
