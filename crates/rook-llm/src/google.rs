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
    pub context_window_explicit: bool,
    pub stream_idle_timeout: Duration,
    /// How a request to this base leaves the machine — see [`crate::Proxy`].
    /// The default is whatever the environment says, which is what every
    /// provider did before an endpoint could say otherwise.
    pub proxy: crate::Proxy,
}

impl Config {
    pub fn new(base_url: String, api_key: String, model: &str) -> Self {
        Self {
            base_url,
            api_key,
            context_window: context_window_for(model),
            context_window_explicit: false,
            stream_idle_timeout: Duration::from_secs(90),
            proxy: Default::default(),
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
    context_key: [u8; 32],
}

impl Google {
    pub fn new(id: &str, model: &str, config: Config) -> Result<Self> {
        let context_key = crate::catalog::context_key(
            &[
                "google",
                &config.base_url,
                &config.api_key,
                model,
                &config.context_window.to_string(),
                if config.context_window_explicit { "explicit" } else { "assumed" },
            ],
            &config.proxy,
        );
        let http = crate::client_for(&config.base_url, &config.proxy)?;
        Ok(Self { id: id.to_string(), model: model.to_string(), config, http, context_key })
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
        let req =
            self.authorized(self.http.post(self.endpoint(&path)).json(&wire_request(&self.model, request)));
        let bytes = request.prompt_bytes();
        let patience = crate::first_token_patience(self.config.stream_idle_timeout, bytes);
        let base = self.config.base_url.clone();
        crate::once_it_answers(patience, bytes, async move {
            req.send().await.map_err(|e| LlmError::unreachable(&base, e))
        })
        .await
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

    fn context_key(&self) -> Option<[u8; 32]> {
        Some(self.context_key)
    }

    fn context_is_explicit(&self) -> bool {
        self.config.context_window_explicit
    }

    async fn discover_context_window(&self, limits: crate::CatalogLimits) -> Result<Option<usize>> {
        if self.context_is_explicit() {
            return Ok(Some(self.context_window()));
        }
        Ok(self
            .models_with(limits)
            .await?
            .into_iter()
            .find(|entry| entry.id == self.model)
            .and_then(|entry| entry.context_window)
            .filter(|window| *window > 0))
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn takes_effort(&self) -> bool {
        thinking_parameter(&self.model, crate::Effort::High).is_some()
    }

    fn effort_use(&self, effort: crate::Effort) -> crate::EffortUse {
        match thinking_parameter(&self.model, effort) {
            Some(("thinkingLevel", value)) => crate::EffortUse::parameter(
                "generationConfig.thinkingConfig.thinkingLevel",
                value.as_str().unwrap_or_default(),
            ),
            Some((_, value)) => {
                crate::EffortUse::parameter("generationConfig.thinkingConfig.thinkingBudget", value)
            }
            None => crate::EffortUse::Omitted { reason: "no effort mapping for this model" },
        }
    }

    async fn models(&self) -> Result<Vec<ModelInfo>> {
        self.models_with(crate::CatalogLimits::default()).await
    }

    async fn models_with(&self, limits: crate::CatalogLimits) -> Result<Vec<ModelInfo>> {
        #[derive(Deserialize)]
        struct Entry {
            /// Fully qualified, as `models/gemini-2.5-pro`.
            name: String,
            #[serde(default, rename = "displayName")]
            display_name: Option<String>,
            #[serde(default, rename = "inputTokenLimit")]
            input_token_limit: Option<usize>,
            #[serde(default)]
            thinking: Option<bool>,
        }

        let page_size = limits.bounded().max_models.min(1000);
        let endpoint = reqwest::Url::parse(&self.endpoint("models"))
            .map_err(|e| LlmError::Other(format!("invalid model catalog endpoint: {e}")))?;
        let mut budget = crate::catalog::Budget::new(limits);
        let entries: Vec<Entry> = budget
            .list(
                |cursor| {
                    let mut url = endpoint.clone();
                    url.query_pairs_mut().append_pair("pageSize", &page_size.to_string());
                    if let Some(cursor) = cursor {
                        url.query_pairs_mut().append_pair("pageToken", cursor);
                    }
                    self.authorized(self.http.get(url))
                },
                &self.config.base_url,
                crate::catalog::Paging::Google,
                |entry: &Entry| &entry.name,
            )
            .await?;
        Ok(entries
            .into_iter()
            .map(|e| ModelInfo {
                id: e.name.trim_start_matches("models/").to_string(),
                owned_by: e.display_name,
                context_window: e.input_token_limit,
                max_context_window: None,
                loaded: None,
                quantization: None,
                capabilities: crate::ModelCapabilities { reasoning: e.thinking, ..Default::default() },
            })
            .collect())
    }

    async fn complete(&self, request: Request) -> Result<Response> {
        let response = self.send(&request, false).await?;
        let status = response.status();
        if !status.is_success() {
            return Err(LlmError::Status {
                status: status.as_u16(),
                retry_after: crate::retry_after(response.headers()),
                body: crate::quoted_text(response, self.config.stream_idle_timeout).await,
            });
        }
        let text =
            crate::whole_text(response, &self.config.base_url, self.config.stream_idle_timeout).await?;

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
            message: Message {
                role: Role::Assistant,
                content,
                tool_calls,
                tool_call_id: None,
                cache: false,
                images: Vec::new(),
                reasoning: Vec::new(),
            },
            usage: wire.usage.into(),
            model: wire.model_version.unwrap_or_else(|| self.model.clone()),
        })
    }

    async fn stream(&self, request: Request) -> Result<ResponseStream> {
        let response = self.send(&request, true).await?;
        let status = response.status();
        if !status.is_success() {
            return Err(LlmError::Status {
                status: status.as_u16(),
                retry_after: crate::retry_after(response.headers()),
                body: crate::quoted_text(response, self.config.stream_idle_timeout).await,
            });
        }

        let idle = self.config.stream_idle_timeout;
        // The first token waits longer, in proportion to what the model has to
        // read before it can say anything. See `first_token_patience`.
        let first = crate::first_token_patience(idle, request.prompt_bytes());
        // Roughly, and said as such: it is here to rule the context out as the
        // explanation, not to be a token count anybody bills from.
        let asked_to_read = request.prompt_bytes() / crate::BYTES_A_TOKEN_ROUGHLY;
        let endpoint = self.config.base_url.clone();
        let fallback_model = self.model.clone();

        Ok(Box::pin(async_stream::try_stream! {
            let mut bytes = response.bytes_stream();
            let mut frames = crate::Frames::new();
            let mut received = 0usize;
            // Whether anything has come back yet: until it has, the model is
            // still reading, and reading is the part that scales with the
            // prompt.
            let mut said_anything = false;
            let mut model = fallback_model;
            let mut usage = Usage::default();
            let mut finish = None;
            let mut calls = 0usize;

            loop {
                let patience = match said_anything {
                    false => first,
                    true => idle,
                };
                let chunk = match tokio::time::timeout(patience, bytes.next()).await {
                    Err(_) if said_anything => Err(LlmError::Stalled { secs: patience.as_secs() })?,
                    Err(_) => Err(LlmError::NeverAnswered {
                        secs: patience.as_secs(),
                        tokens: asked_to_read,
                    })?,
                    Ok(None) => break,
                    Ok(Some(chunk)) => chunk.map_err(|e| LlmError::unreachable(&endpoint, e))?,
                };
                said_anything = true;
                received = received.saturating_add(chunk.len());
                if received > crate::MOST_REPLY_BYTES { Err(LlmError::Decode("stream exceeded the response byte budget".into()))?; }
                frames.feed(&chunk);
                if frames.held() > MAX_FRAME_BYTES {
                    Err(LlmError::Decode("an event exceeded the frame cap".into()))?;
                }

                for frame in frames.ready() {
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

fn wire_request(model: &str, request: &Request) -> Value {
    let mut system = String::new();
    let mut contents: Vec<Value> = Vec::new();
    // A result carries only the function's name, so the call it answers has to
    // be remembered from the assistant message that asked for it.
    let mut called: HashMap<&str, &str> = HashMap::new();

    let messages = crate::images::for_wire(&request.messages);
    for message in &messages {
        match message.role {
            Role::System => {
                if !system.is_empty() {
                    system.push_str("\n\n");
                }
                system.push_str(&message.content);
            }
            Role::User => {
                let mut parts = vec![json!({"text":message.content})];
                parts.extend(message.images.iter().map(|image| {
                    json!({
                        "inlineData":{"mimeType":image.mime_type,"data":image.data}
                    })
                }));
                append_user_parts(&mut contents, parts);
            }
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
                append_user_parts(
                    &mut contents,
                    vec![json!({
                        "functionResponse": {
                            "name": name,
                            "response": { "result": message.content },
                        }
                    })],
                );
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
    if let Some((name, value)) = request.effort.and_then(|effort| thinking_parameter(model, effort)) {
        body["generationConfig"]["thinkingConfig"] = json!({ name: value });
    }
    body
}

/// GenerateContent uses budgets for 2.5 and levels for 3.x. This is not
/// the Interactions API, whose 2.5 examples also use levels.
fn thinking_parameter(model: &str, effort: crate::Effort) -> Option<(&'static str, Value)> {
    use crate::Effort::*;
    use crate::effort::model_family;
    let model = model.strip_prefix("models/").unwrap_or(model);
    if model_family(model, "gemini-3.1-flash-lite-image") {
        return Some(("thinkingLevel", json!(if effort == Low { "minimal" } else { "high" })));
    }
    if [
        "gemini-3",
        "gemini-3.1",
        "gemini-3.5",
        "gemini-3.6",
        "gemini-3.7",
        "gemini-3.8",
        "gemini-robotics-er-2",
    ]
    .iter()
    .any(|family| model_family(model, family))
    {
        let value = match effort {
            Low => "low",
            Medium if !model_family(model, "gemini-3-pro") => "medium",
            _ => "high",
        };
        return Some(("thinkingLevel", json!(value)));
    }
    // Image-only and TTS variants do not inherit the text model's controls.
    if (model_family(model, "gemini-2.5-pro")
        || model_family(model, "gemini-2.5-flash")
        || model_family(model, "gemini-robotics-er-1.6"))
        && !model.contains("image")
        && !model.contains("tts")
    {
        return Some(("thinkingBudget", json!(thinking_budget(model, effort))));
    }
    None
}

/// Explicit upper levels use the model limit rather than switching back to
/// dynamic thinking (`-1`), which could spend less than the selected high level.
fn thinking_budget(model: &str, effort: crate::Effort) -> i32 {
    use crate::Effort::*;
    match effort {
        Low => 1_024,
        Medium => 8_192,
        High => 24_576,
        XHigh | Max if crate::effort::model_family(model, "gemini-2.5-pro") => 32_768,
        XHigh | Max => 24_576,
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

// All results of a parallel function call belong to the same user turn. The
// following image parts must come after those results, not between them.
fn append_user_parts(contents: &mut Vec<Value>, parts: Vec<Value>) {
    if let Some(last) = contents.last_mut()
        && last["role"] == "user"
        && let Some(existing) = last["parts"].as_array_mut()
    {
        existing.extend(parts);
    } else {
        contents.push(json!({"role":"user", "parts":parts}));
    }
}
