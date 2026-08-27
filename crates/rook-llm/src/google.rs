//! Google's `generateContent` API.
//!
//! Gemini is reachable through the OpenAI dialect, but not fully: its own shape
//! differs in four ways that matter. There are two roles, `user` and `model`,
//! and no system one — the system prompt is a separate top-level field. Tool
//! calls and their results are `parts` of a message rather than a parallel
//! array. A result goes back inside a *user* message. And a call carries no id,
//! only the function's name, so answering one means knowing which call it
//! belonged to.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::stream::{Delta, ResponseStream};
use crate::{
    LlmError, Message, ModelInfo, Provider, Request, Response, Result, Role, StopReason, ToolCall, Usage,
    truncate,
};

const MAX_FRAME_BYTES: usize = 8 << 20;

pub struct Config {
    pub base_url: String,
    pub api_key: String,
    pub context_window: usize,
    pub stream_idle_timeout: Duration,
}

impl Config {
    pub fn new(base_url: String, api_key: String, model: &str) -> Self {
        Self {
            base_url,
            api_key,
            context_window: context_window_for(model),
            stream_idle_timeout: Duration::from_secs(90),
        }
    }
}

/// Anything unrecognised gets a small window rather than an optimistic guess:
/// budgeting against a window the model does not have fails the request,
/// budgeting low only wastes some of it.
fn context_window_for(model: &str) -> usize {
    match model {
        m if m.starts_with("gemini-1.5-pro") => 2_097_152,
        m if m.starts_with("gemini-") => 1_048_576,
        _ => 32_768,
    }
}

pub struct Google {
    id: String,
    model: String,
    config: Config,
    http: reqwest::Client,
}

impl Google {
    pub fn new(id: &str, model: &str, config: Config) -> Result<Self> {
        crate::init_tls();
        let http = reqwest::Client::builder()
            .user_agent(concat!("rook/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(600))
            .connect_timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| LlmError::unreachable(&config.base_url, e))?;
        Ok(Self { id: id.to_string(), model: model.to_string(), config, http })
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/{path}", self.config.base_url.trim_end_matches('/'))
    }

    /// In a header rather than the `?key=` query parameter the docs lead with:
    /// a url carrying a credential ends up in logs and in error messages.
    fn authorized(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request.header("x-goog-api-key", &self.config.api_key)
    }

    async fn send(&self, request: &Request, stream: bool) -> Result<reqwest::Response> {
        let path = match stream {
            true => format!("models/{}:streamGenerateContent?alt=sse", self.model),
            false => format!("models/{}:generateContent", self.model),
        };
        self.authorized(self.http.post(self.endpoint(&path)).json(&wire_request(request)))
            .send()
            .await
            .map_err(|e| LlmError::unreachable(&self.config.base_url, e))
    }
}

#[async_trait]
impl Provider for Google {
    fn id(&self) -> &str {
        &self.id
    }

    fn context_window(&self) -> usize {
        self.config.context_window
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    async fn models(&self) -> Result<Vec<ModelInfo>> {
        #[derive(Deserialize)]
        struct Listing {
            #[serde(default)]
            models: Vec<Entry>,
        }
        #[derive(Deserialize)]
        struct Entry {
            /// Fully qualified, as `models/gemini-2.5-pro`.
            name: String,
            #[serde(default, rename = "displayName")]
            display_name: Option<String>,
            #[serde(default, rename = "inputTokenLimit")]
            input_token_limit: Option<usize>,
        }

        let response = self
            .authorized(self.http.get(self.endpoint("models")))
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .map_err(|e| LlmError::unreachable(&self.config.base_url, e))?;
        let status = response.status();
        let text = response.text().await.map_err(|e| LlmError::unreachable(&self.config.base_url, e))?;
        if !status.is_success() {
            return Err(LlmError::Status { status: status.as_u16(), body: truncate(&text, 500) });
        }
        let listing: Listing = serde_json::from_str(&text)
            .map_err(|e| LlmError::Decode(format!("{e}: {}", truncate(&text, 300))))?;
        Ok(listing
            .models
            .into_iter()
            .map(|e| ModelInfo {
                id: e.name.trim_start_matches("models/").to_string(),
                owned_by: e.display_name,
                context_window: e.input_token_limit,
            })
            .collect())
    }

    async fn complete(&self, request: Request) -> Result<Response> {
        let response = self.send(&request, false).await?;
        let status = response.status();
        let text = response.text().await.map_err(|e| LlmError::unreachable(&self.config.base_url, e))?;
        if !status.is_success() {
            return Err(LlmError::Status { status: status.as_u16(), body: truncate(&text, 2000) });
        }

        let wire: WireResponse = serde_json::from_str(&text)
            .map_err(|e| LlmError::Decode(format!("{e}: {}", truncate(&text, 500))))?;
        let candidate = wire.candidates.into_iter().next();
        let finish = candidate.as_ref().and_then(|c| c.finish_reason.clone());

        let mut content = String::new();
        let mut tool_calls = Vec::new();
        for part in candidate.into_iter().flat_map(|c| c.content.parts) {
            match part.into_piece(tool_calls.len()) {
                Piece::Text(text) => content.push_str(&text),
                Piece::Thought(_) => {}
                Piece::Call(call) => tool_calls.push(call),
            }
        }

        Ok(Response {
            stop_reason: stop_reason(finish.as_deref(), !tool_calls.is_empty()),
            message: Message { role: Role::Assistant, content, tool_calls, tool_call_id: None, cache: false },
            usage: wire.usage.into(),
            model: wire.model_version.unwrap_or_else(|| self.model.clone()),
        })
    }

    async fn stream(&self, request: Request) -> Result<ResponseStream> {
        let response = self.send(&request, true).await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(LlmError::Status { status: status.as_u16(), body: truncate(&body, 2000) });
        }

        let idle = self.config.stream_idle_timeout;
        let endpoint = self.config.base_url.clone();
        let fallback_model = self.model.clone();

        Ok(Box::pin(async_stream::try_stream! {
            let mut bytes = response.bytes_stream();
            let mut buffer = String::new();
            let mut scanned = 0usize;
            let mut model = fallback_model;
            let mut usage = Usage::default();
            let mut finish = None;
            let mut calls = 0usize;

            loop {
                let chunk = match tokio::time::timeout(idle, bytes.next()).await {
                    Err(_) => Err(LlmError::Stalled { secs: idle.as_secs() })?,
                    Ok(None) => break,
                    Ok(Some(chunk)) => chunk.map_err(|e| LlmError::unreachable(&endpoint, e))?,
                };
                buffer.push_str(&String::from_utf8_lossy(&chunk));
                if buffer.len() > MAX_FRAME_BYTES {
                    Err(LlmError::Decode("an event exceeded the frame cap".into()))?;
                }

                while let Some(offset) = buffer[scanned..].find("\n\n") {
                    let end = scanned + offset;
                    scanned = 0;
                    let frame: String = buffer.drain(..end + 2).collect();
                    for line in frame.lines() {
                        let Some(data) = line.strip_prefix("data:") else { continue };
                        let Ok(wire) = serde_json::from_str::<WireResponse>(data.trim()) else { continue };
                        if let Some(m) = wire.model_version {
                            model = m;
                        }
                        if wire.usage.prompt_token_count > 0 {
                            usage = wire.usage.into();
                        }
                        for candidate in wire.candidates {
                            if candidate.finish_reason.is_some() {
                                finish = candidate.finish_reason;
                            }
                            // A function call arrives whole in one part rather
                            // than as text spread over deltas, so there is
                            // nothing to accumulate.
                            for part in candidate.content.parts {
                                match part.into_piece(calls) {
                                    Piece::Text(text) if !text.is_empty() => yield Delta::Text(text),
                                    Piece::Thought(text) if !text.is_empty() => {
                                        yield Delta::Reasoning(text)
                                    }
                                    Piece::Call(call) => {
                                        calls += 1;
                                        yield Delta::ToolCall(call)
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            }

            yield Delta::Done {
                stop_reason: stop_reason(finish.as_deref(), calls > 0),
                usage,
                model,
            };
        }))
    }
}

/// `STOP` is what a turn ending in tool calls also reports, so the calls
/// themselves are what says the turn is not over.
fn stop_reason(raw: Option<&str>, called_tools: bool) -> StopReason {
    match raw {
        _ if called_tools => StopReason::ToolUse,
        Some("MAX_TOKENS") => StopReason::MaxTokens,
        Some("SAFETY") | Some("BLOCKLIST") | Some("PROHIBITED_CONTENT") | Some("SPII") => StopReason::Refusal,
        Some("STOP") => StopReason::EndTurn,
        _ => StopReason::Other,
    }
}

fn wire_request(request: &Request) -> Value {
    let mut system = String::new();
    let mut contents: Vec<Value> = Vec::new();
    // A result carries only the function's name, so the call it answers has to
    // be remembered from the assistant message that asked for it.
    let mut called: HashMap<&str, &str> = HashMap::new();

    for message in &request.messages {
        match message.role {
            Role::System => {
                if !system.is_empty() {
                    system.push_str("\n\n");
                }
                system.push_str(&message.content);
            }
            Role::User => contents.push(json!({ "role": "user", "parts": [{ "text": message.content }] })),
            Role::Assistant => {
                let mut parts: Vec<Value> = Vec::new();
                if !message.content.is_empty() {
                    parts.push(json!({ "text": message.content }));
                }
                for call in &message.tool_calls {
                    called.insert(&call.id, &call.name);
                    parts.push(json!({
                        "functionCall": { "name": call.name, "args": call.arguments }
                    }));
                }
                if !parts.is_empty() {
                    contents.push(json!({ "role": "model", "parts": parts }));
                }
            }
            Role::Tool => {
                let name = message
                    .tool_call_id
                    .as_deref()
                    .and_then(|id| called.get(id).copied())
                    .unwrap_or_default();
                contents.push(json!({
                    "role": "user",
                    "parts": [{
                        "functionResponse": {
                            "name": name,
                            "response": { "result": message.content },
                        }
                    }],
                }));
            }
        }
    }

    let mut body = json!({
        "contents": contents,
        "generationConfig": {
            "temperature": request.temperature,
            "maxOutputTokens": request.max_output_tokens,
        },
    });
    if !system.is_empty() {
        body["systemInstruction"] = json!({ "parts": [{ "text": system }] });
    }
    if !request.tools.is_empty() {
        let declarations: Vec<Value> = request
            .tools
            .iter()
            .map(|t| json!({ "name": t.name, "description": t.description, "parameters": t.parameters }))
            .collect();
        body["tools"] = json!([{ "functionDeclarations": declarations }]);
    }
    // Only when the caller asked: a model without a thinking budget rejects the
    // field outright, and most requests do not set an effort.
    if let Some(effort) = request.effort {
        body["generationConfig"]["thinkingConfig"] = json!({ "thinkingBudget": thinking_budget(effort) });
    }
    body
}

/// `-1` asks the model to decide, which is what "as much as it takes" means here.
fn thinking_budget(effort: crate::Effort) -> i32 {
    use crate::Effort::*;
    match effort {
        Low => 1_024,
        Medium => 8_192,
        High => 24_576,
        XHigh | Max => -1,
    }
}

#[derive(Deserialize)]
struct WireResponse {
    #[serde(default)]
    candidates: Vec<Candidate>,
    #[serde(default, rename = "usageMetadata")]
    usage: UsageMetadata,
    #[serde(default, rename = "modelVersion")]
    model_version: Option<String>,
}

#[derive(Deserialize)]
struct Candidate {
    #[serde(default)]
    content: Content,
    #[serde(default, rename = "finishReason")]
    finish_reason: Option<String>,
}

#[derive(Default, Deserialize)]
struct Content {
    #[serde(default)]
    parts: Vec<Part>,
}

#[derive(Deserialize)]
struct Part {
    #[serde(default)]
    text: Option<String>,
    /// Set on the parts that are the model's own reasoning.
    #[serde(default)]
    thought: bool,
    #[serde(default, rename = "functionCall")]
    function_call: Option<FunctionCall>,
}

#[derive(Deserialize)]
struct FunctionCall {
    name: String,
    #[serde(default)]
    args: Value,
}

enum Piece {
    Text(String),
    Thought(String),
    Call(ToolCall),
}

impl Part {
    /// The protocol gives a call no id, and the loop needs one to pair a result
    /// with the call it answers, so the position in the turn stands in.
    fn into_piece(self, index: usize) -> Piece {
        match (self.function_call, self.thought) {
            (Some(call), _) => Piece::Call(ToolCall {
                id: format!("{}-{index}", call.name),
                name: call.name,
                arguments: call.args,
            }),
            (None, true) => Piece::Thought(self.text.unwrap_or_default()),
            (None, false) => Piece::Text(self.text.unwrap_or_default()),
        }
    }
}

#[derive(Default, Deserialize)]
struct UsageMetadata {
    #[serde(default, rename = "promptTokenCount")]
    prompt_token_count: u32,
    #[serde(default, rename = "candidatesTokenCount")]
    candidates_token_count: u32,
    #[serde(default, rename = "cachedContentTokenCount")]
    cached_content_token_count: u32,
}

impl From<UsageMetadata> for Usage {
    fn from(m: UsageMetadata) -> Self {
        Usage {
            input_tokens: m.prompt_token_count,
            output_tokens: m.candidates_token_count,
            cache_read_tokens: m.cached_content_token_count,
            cache_write_tokens: 0,
        }
    }
}
