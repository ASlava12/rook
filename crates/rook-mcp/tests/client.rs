use std::time::Duration;

use rook_mcp::{McpError, Server, ServerConfig};

fn mock(mode: &str) -> ServerConfig {
    ServerConfig {
        name: format!("mock-{mode}"),
        command: env!("CARGO_BIN_EXE_mcp-mock").to_string(),
        args: vec![mode.to_string()],
        startup_timeout_secs: 5,
        call_timeout_secs: 5,
        ..Default::default()
    }
}

#[tokio::test]
async fn handshake_reports_what_the_server_is() {
    let server = Server::connect(&mock("ok")).await.unwrap();
    assert_eq!(server.info().server.name, "mock");
    assert_eq!(server.info().server.version, "1.2.3");
    assert_eq!(server.info().protocol_version, "2025-06-18");
    server.shutdown().await;
}

#[tokio::test]
async fn tools_are_listed_with_their_schemas_intact() {
    let server = Server::connect(&mock("ok")).await.unwrap();
    let tools = server.list_tools().await.unwrap();
    assert_eq!(tools.len(), 2);

    let echo = tools.iter().find(|t| t.name == "echo").unwrap();
    assert_eq!(echo.description, "Echo a message back.");
    assert_eq!(echo.input_schema["properties"]["text"]["type"], "string");
    server.shutdown().await;
}

#[tokio::test]
async fn a_tool_call_round_trips() {
    let server = Server::connect(&mock("ok")).await.unwrap();
    let result = server.call_tool("echo", &serde_json::json!({ "text": "hello" })).await.unwrap();
    assert!(!result.is_error);
    assert_eq!(result.to_text(), "echo: hello");
    server.shutdown().await;
}

#[tokio::test]
async fn binary_content_is_described_rather_than_inlined() {
    let server = Server::connect(&mock("ok")).await.unwrap();
    let result = server.call_tool("picture", &serde_json::json!({})).await.unwrap();
    assert_eq!(result.to_text(), "[image/png image, 4 bytes base64]");
    server.shutdown().await;
}

#[tokio::test]
async fn concurrent_calls_get_their_own_answers() {
    let server = Server::connect(&mock("ok")).await.unwrap();
    let args: Vec<_> = (0..12).map(|i| serde_json::json!({ "text": format!("m{i}") })).collect();
    let calls = args.iter().map(|a| server.call_tool("echo", a));
    for (i, result) in futures_util::future::join_all(calls).await.into_iter().enumerate() {
        assert_eq!(result.unwrap().to_text(), format!("echo: m{i}"), "responses were mismatched");
    }
    server.shutdown().await;
}

#[tokio::test]
async fn notifications_and_junk_lines_do_not_disturb_a_call() {
    let server = Server::connect(&mock("noise")).await.unwrap();
    let result = server.call_tool("echo", &serde_json::json!({ "text": "x" })).await.unwrap();
    assert_eq!(result.to_text(), "echo: x");
    server.shutdown().await;
}

#[tokio::test]
async fn a_server_error_names_the_method_and_the_code() {
    let server = Server::connect(&mock("error")).await.unwrap();
    let err = server.call_tool("nope", &serde_json::json!({})).await.unwrap_err();
    let message = err.to_string();
    assert!(matches!(err, McpError::Rpc { .. }), "{message}");
    assert!(message.contains("tools/call") && message.contains("-32602"), "{message}");
    server.shutdown().await;
}

#[tokio::test]
async fn a_hung_server_times_out_rather_than_blocking_the_agent() {
    let mut config = mock("slow");
    config.call_timeout_secs = 1;
    let server = Server::connect(&config).await.unwrap();
    let err = server.call_tool("echo", &serde_json::json!({})).await.unwrap_err();
    assert!(matches!(err, McpError::Timeout { .. }), "{err}");
    server.shutdown().await;
}

#[tokio::test]
async fn a_server_that_dies_mid_call_fails_the_call_instead_of_hanging() {
    let server = Server::connect(&mock("crash")).await.unwrap();
    let err = tokio::time::timeout(Duration::from_secs(5), server.call_tool("echo", &serde_json::json!({})))
        .await
        .expect("a dead server must not leave the call pending")
        .unwrap_err();
    assert!(matches!(err, McpError::Closed { .. }), "{err}");
}

#[tokio::test]
async fn a_missing_command_fails_with_the_command_in_the_message() {
    let config = ServerConfig {
        name: "ghost".into(),
        command: "definitely-not-installed-anywhere".into(),
        ..Default::default()
    };
    let Err(err) = Server::connect(&config).await else {
        panic!("a missing command must not appear to connect");
    };
    assert!(matches!(err, McpError::Spawn { .. }));
    assert!(err.to_string().contains("definitely-not-installed-anywhere"), "{err}");
}
