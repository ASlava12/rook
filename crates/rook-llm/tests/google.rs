//! Google's own shape, which the OpenAI dialect does not cover: two roles, the
//! system prompt beside the conversation rather than in it, and tool calls and
//! results as parts of a message.

use std::time::Duration;

use rook_llm::google::{Config, Google};
use rook_llm::{Message, Provider, Request, Role, StopReason, ToolCall, ToolSpec};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Answers once and hands back what it was sent, so the request can be asserted
/// on rather than assumed.
async fn serve(body: &'static str) -> (String, tokio::sync::oneshot::Receiver<String>) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut scratch = vec![0u8; 65536];
        let read = socket.read(&mut scratch).await.unwrap_or(0);
        let _ = tx.send(String::from_utf8_lossy(&scratch[..read]).into_owned());
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let _ = socket.write_all(response.as_bytes()).await;
    });
    (format!("http://{addr}/v1beta"), rx)
}

fn provider(url: String) -> Google {
    let mut config = Config::new(url, "test-key".into(), "gemini-2.5-pro");
    config.stream_idle_timeout = Duration::from_secs(5);
    Google::new("google/gemini-2.5-pro", "gemini-2.5-pro", config).unwrap()
}

fn sent(raw: &str) -> serde_json::Value {
    let body = raw.split_once("\r\n\r\n").expect("a request with a body").1;
    serde_json::from_str(body).expect("the body is json")
}

const ANSWERED: &str = r#"{
  "candidates": [{"content": {"role":"model","parts": [{"text": "the sky is blue"}]}, "finishReason": "STOP"}],
  "usageMetadata": {"promptTokenCount": 11, "candidatesTokenCount": 4, "cachedContentTokenCount": 7},
  "modelVersion": "gemini-2.5-pro-002"
}"#;

#[tokio::test]
async fn a_plain_answer_carries_its_usage_and_the_model_that_replied() {
    let (url, _sent) = serve(ANSWERED).await;
    let response = provider(url).complete(Request::new(vec![Message::user("what colour?")])).await.unwrap();

    assert_eq!(response.message.content, "the sky is blue");
    assert_eq!(response.stop_reason, StopReason::EndTurn);
    assert_eq!(response.usage.input_tokens, 11);
    assert_eq!(response.usage.cache_read_tokens, 7, "cached content is prompt tokens not paid for");
    assert_eq!(response.model, "gemini-2.5-pro-002", "the version that answered, not the one asked for");
}

#[tokio::test]
async fn the_system_prompt_travels_beside_the_conversation_not_inside_it() {
    let (url, sent_body) = serve(ANSWERED).await;
    let request = Request::new(vec![
        Message::system("be terse"),
        Message::system("and precise"),
        Message::user("hello"),
    ]);
    provider(url).complete(request).await.unwrap();

    let body = sent(&sent_body.await.unwrap());
    assert_eq!(
        body["systemInstruction"]["parts"][0]["text"], "be terse\n\nand precise",
        "there is one system field and no system role, so several become one: {body}"
    );
    assert_eq!(body["contents"].as_array().unwrap().len(), 1, "and none of them is a turn: {body}");
    assert_eq!(body["contents"][0]["role"], "user");
}

#[tokio::test]
async fn a_tool_result_names_the_call_it_answers() {
    let (url, sent_body) = serve(ANSWERED).await;
    let asked = Message {
        role: Role::Assistant,
        content: String::new(),
        tool_calls: vec![ToolCall {
            id: "read_file-0".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({ "path": "a.txt" }),
        }],
        tool_call_id: None,
        cache: false,
        reasoning: Vec::new(),
    };
    let request =
        Request::new(vec![Message::user("read a.txt"), asked, Message::tool_result("read_file-0", "hello")]);
    provider(url).complete(request).await.unwrap();

    let body = sent(&sent_body.await.unwrap());
    assert_eq!(body["contents"][1]["role"], "model");
    assert_eq!(body["contents"][1]["parts"][0]["functionCall"]["name"], "read_file");
    assert_eq!(
        body["contents"][2]["role"], "user",
        "a result goes back as a user turn, which is the part that surprises: {body}"
    );
    assert_eq!(
        body["contents"][2]["parts"][0]["functionResponse"]["name"], "read_file",
        "the protocol pairs by name, so the call the id referred to has to be remembered: {body}"
    );
}

#[tokio::test]
async fn tools_are_declared_under_one_entry() {
    let (url, sent_body) = serve(ANSWERED).await;
    let mut request = Request::new(vec![Message::user("go")]);
    request.tools = vec![
        ToolSpec {
            name: "read_file".into(),
            description: "Read a file.".into(),
            parameters: serde_json::json!({"type": "object"}),
        },
        ToolSpec {
            name: "run".into(),
            description: "Run a command.".into(),
            parameters: serde_json::json!({"type": "object"}),
        },
    ];
    provider(url).complete(request).await.unwrap();

    let body = sent(&sent_body.await.unwrap());
    let declarations = body["tools"][0]["functionDeclarations"].as_array().unwrap();
    assert_eq!(declarations.len(), 2, "one tools entry holding every function: {body}");
    assert_eq!(declarations[0]["name"], "read_file");
}

#[tokio::test]
async fn a_turn_that_called_a_tool_is_not_reported_as_finished() {
    let (url, _sent) = serve(
        r#"{"candidates":[{"content":{"role":"model","parts":[
             {"text":"looking"},
             {"functionCall":{"name":"read_file","args":{"path":"a.txt"}}}
           ]},"finishReason":"STOP"}]}"#,
    )
    .await;
    let response = provider(url).complete(Request::new(vec![Message::user("go")])).await.unwrap();

    assert_eq!(
        response.stop_reason,
        StopReason::ToolUse,
        "a turn ending in a call still reports STOP, so the calls are what says it is not over"
    );
    assert_eq!(response.message.tool_calls.len(), 1);
    assert_eq!(response.message.tool_calls[0].arguments["path"], "a.txt");
    assert!(!response.message.tool_calls[0].id.is_empty(), "the loop pairs results by id");
}

#[tokio::test]
async fn thinking_is_asked_for_only_when_an_effort_is() {
    let (url, sent_body) = serve(ANSWERED).await;
    provider(url).complete(Request::new(vec![Message::user("go")])).await.unwrap();
    let body = sent(&sent_body.await.unwrap());
    assert!(
        body["generationConfig"]["thinkingConfig"].is_null(),
        "a model without a thinking budget rejects the field outright: {body}"
    );

    let (url, sent_body) = serve(ANSWERED).await;
    let mut request = Request::new(vec![Message::user("go")]);
    request.effort = Some(rook_llm::Effort::Low);
    provider(url).complete(request).await.unwrap();
    let body = sent(&sent_body.await.unwrap());
    assert_eq!(body["generationConfig"]["thinkingConfig"]["thinkingBudget"], 1024);
}

#[tokio::test]
async fn the_listing_strips_the_prefix_every_name_carries() {
    let (url, _sent) = serve(
        r#"{"models":[{"name":"models/gemini-2.5-pro","displayName":"Gemini 2.5 Pro","inputTokenLimit":1048576}]}"#,
    )
    .await;
    let models = provider(url).models().await.unwrap();

    assert_eq!(models[0].id, "gemini-2.5-pro", "`models/` is addressing, not part of the name");
    assert_eq!(models[0].context_window, Some(1_048_576));
}

/// Frames as the SSE transport delivers them: each is a whole response object
/// carrying the next slice, not a diff of one.
async fn serve_sse(frames: &'static [&'static str]) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut scratch = vec![0u8; 65536];
        let _ = socket.read(&mut scratch).await;
        let body: String = frames.iter().map(|f| format!("data: {f}\n\n")).collect();
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        let _ = socket.write_all(head.as_bytes()).await;
        let _ = socket.write_all(body.as_bytes()).await;
    });
    format!("http://{addr}/v1beta")
}

#[tokio::test]
async fn a_stream_yields_text_as_it_arrives_and_calls_whole() {
    use futures_util::StreamExt;
    use rook_llm::stream::Delta;

    let url = serve_sse(&[
        r#"{"candidates":[{"content":{"parts":[{"text":"the "}]}}],"modelVersion":"gemini-2.5-flash"}"#,
        r#"{"candidates":[{"content":{"parts":[{"text":"sky "}]}}]}"#,
        r#"{"candidates":[{"content":{"parts":[{"thought":true,"text":"considering"}]}}]}"#,
        r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"look","args":{"at":"up"}}}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":9,"candidatesTokenCount":3}}"#,
    ])
    .await;

    let mut stream = provider(url).stream(Request::new(vec![Message::user("go")])).await.unwrap();
    let (mut text, mut reasoning, mut calls, mut done) = (String::new(), String::new(), 0, None);
    while let Some(delta) = stream.next().await {
        match delta.unwrap() {
            Delta::Text(t) => text.push_str(&t),
            Delta::Reasoning(t) => reasoning.push_str(&t),
            // Google signs nothing and asks for nothing back.
            Delta::ReasoningDone(block) => panic!("no blocks here: {block}"),
            Delta::ToolCall(c) => {
                assert_eq!(c.arguments["at"], "up", "a call is delivered complete or not at all");
                calls += 1;
            }
            Delta::Done { stop_reason, usage, model } => done = Some((stop_reason, usage, model)),
        }
    }

    assert_eq!(text, "the sky ");
    assert_eq!(reasoning, "considering", "thinking parts are marked rather than separated");
    assert_eq!(calls, 1);
    let (stop, usage, model) = done.expect("the stream says how it ended");
    assert_eq!(stop, StopReason::ToolUse);
    assert_eq!(usage.input_tokens, 9);
    assert_eq!(model, "gemini-2.5-flash", "reported once, on the first frame, and kept");
}
