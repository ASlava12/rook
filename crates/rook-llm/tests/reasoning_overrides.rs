//! Declared controls must reach real dialect bodies, including unknown model aliases.
use rook_llm::{
    Api, Effort, Message, Request,
    reasoning::{Control, Override},
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn server(api: Api, status: u16) -> (String, tokio::sync::oneshot::Receiver<Value>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut raw = Vec::new();
        let mut scratch = [0; 4096];
        loop {
            let n = socket.read(&mut scratch).await.unwrap();
            assert!(n > 0 && raw.len() + n < 65536);
            raw.extend_from_slice(&scratch[..n]);
            let Some(split) = raw.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
            let head = String::from_utf8_lossy(&raw[..split]);
            let length: usize = head
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase().strip_prefix("content-length:").and_then(|v| v.trim().parse().ok())
                })
                .unwrap();
            if raw.len() < split + 4 + length {
                continue;
            }
            tx.send(serde_json::from_slice(&raw[split + 4..split + 4 + length]).unwrap()).unwrap();
            break;
        }
        let body=if status!=200 {json!({"error":{"message":"reasoning control rejected"}})} else {match api {
            Api::OpenAi=>json!({"id":"x","model":"opaque-alias","choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}),
            Api::Responses=>json!({"id":"x","object":"response","status":"completed","model":"opaque-alias","output":[{"type":"message","id":"msg","role":"assistant","content":[{"type":"output_text","text":"ok"}]}]}),
            Api::Anthropic=>json!({"id":"x","type":"message","role":"assistant","model":"opaque-alias","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}),
            Api::Google=>json!({"candidates":[{"content":{"role":"model","parts":[{"text":"ok"}]},"finishReason":"STOP"}]}),
        }}.to_string();
        let response = format!(
            "HTTP/1.1 {status} Reply\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });
    (url, rx)
}

#[tokio::test]
async fn every_implemented_control_has_an_actual_wire_field_and_disable_removes_it() {
    for (api, control, path, expected) in [
        (Api::OpenAi, Control::OpenaiEffort, "/reasoning_effort", json!("high")),
        (Api::Responses, Control::ResponsesEffort, "/reasoning/effort", json!("high")),
        (Api::Anthropic, Control::AnthropicEffort, "/output_config/effort", json!("high")),
        (Api::Google, Control::GoogleLevel, "/generationConfig/thinkingConfig/thinkingLevel", json!("high")),
        (Api::Google, Control::GoogleBudget, "/generationConfig/thinkingConfig/thinkingBudget", json!(896)),
    ] {
        for disabled in [false, true] {
            let (url, seen) = server(api, 200).await;
            let mut endpoint = rook_llm::endpoint_from_spec("openai/opaque-alias", Some(8192)).unwrap();
            endpoint.api = api;
            endpoint.url = url;
            endpoint.key = Some("fixture-key".into());
            let provider = rook_llm::from_endpoints_with(
                vec![endpoint],
                std::time::Duration::from_secs(3),
                rook_llm::Prefer::AsConfigured,
            )
            .unwrap();
            let settings = if disabled {
                Override { supported: Some(false), ..Default::default() }
            } else {
                Override { control: Some(control), ..Default::default() }
            };
            settings.validate(api).unwrap();
            let mut request = Request::new(vec![Message::user("hi")]);
            request.effort = Some(Effort::High);
            request.max_output_tokens = 1024;
            settings.apply(&mut request.model_capabilities);
            let result = tokio::time::timeout(std::time::Duration::from_secs(20), provider.complete(request))
                .await
                .unwrap();
            assert!(result.is_ok(), "{api:?}: {result:?}");
            let body = seen.await.unwrap();
            if disabled {
                assert!(body.pointer(path).is_none(), "{body}");
            } else {
                assert_eq!(body.pointer(path), Some(&expected), "{body}");
            }
        }
    }
}

#[test]
fn incompatible_or_ambiguous_declarations_are_rejected_before_any_request() {
    assert!(
        Override { control: Some(Control::GoogleBudget), ..Default::default() }
            .validate(Api::OpenAi)
            .is_err()
    );
    assert!(
        Override { supported: Some(false), control: Some(Control::OpenaiEffort), ..Default::default() }
            .validate(Api::OpenAi)
            .is_err()
    );
    assert!(
        Override { effort_levels: Some(vec![Effort::High, Effort::High]), ..Default::default() }
            .validate(Api::OpenAi)
            .is_err()
    );
    assert!(
        Override {
            control: Some(Control::GoogleBudget),
            budget_min: Some(200),
            budget_max: Some(100),
            ..Default::default()
        }
        .validate(Api::Google)
        .is_err()
    );
}
