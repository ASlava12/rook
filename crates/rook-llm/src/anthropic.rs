//! The Anthropic Messages API.
//!
//! Not reachable through the OpenAI dialect, and different in four ways that
//! matter: the system prompt is a top-level field rather than a message, tool
//! calls arrive as `tool_use` content blocks rather than a parallel array, tool
//! results go back as `tool_result` blocks inside a *user* message, and the
//! schema field is `input_schema`.
//!
//! Rust has no official Anthropic SDK, so this speaks the documented HTTP shape
//! directly.

use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::stream::{Delta, ResponseStream};
use crate::{
    LlmError, Message, ModelInfo, Provider, Request, Response, Result, Role, StopReason, ToolCall, Usage,
    truncate,
};

const API_VERSION: &str = "2023-06-01";
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

/// Documented context lengths. Anything unrecognised gets the smallest current
/// window rather than an optimistic guess: budgeting against a window the model
/// does not have fails the request, budgeting low only wastes some of it.
fn context_window_for(model: &str) -> usize {
    match model {
        m if m.starts_with("claude-haiku") => 200_000,
        m if m.starts_with("claude-opus") || m.starts_with("claude-sonnet") => 1_000_000,
        m if m.starts_with("claude-fable") || m.starts_with("claude-mythos") => 1_000_000,
        _ => 200_000,
    }
}

pub struct Anthropic {
    id: String,
    model: String,
    config: Config,
    http: reqwest::Client,
    context_key: [u8; 32],
}

impl Anthropic {
    pub fn new(id: &str, model: &str, config: Config) -> Result<Self> {
        let context_key = crate::catalog::context_key(
            &[
                "anthropic",
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

    fn authorized(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request.header("x-api-key", &self.config.api_key).header("anthropic-version", API_VERSION)
    }

    async fn send(&self, request: &Request, stream: bool) -> Result<reqwest::Response> {
        let body = wire_request(&self.model, request, stream);
        let req = self.authorized(self.http.post(self.endpoint("v1/messages")).json(&body));
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
impl Provider for Anthropic {
    fn id(&self) -> &str {
        &self.id
    }

    fn dispatch_identity(&self) -> Option<crate::Dispatch> {
        crate::Dispatch::bounded(self.id(), &self.model, false)
    }

    fn context_window(&self) -> usize {
        self.config.context_window
    }

    fn context_key(&self) -> Option<[u8; 32]> {
        Some(self.context_key)
    }

    fn can_replay_reasoning(&self, messages: &[Message]) -> bool {
        messages.iter().all(|message| {
            (message.reasoning.is_empty() || message.role == Role::Assistant)
                && message.reasoning.iter().all(|block| {
                    crate::catalog::matches_scope(block.get("rook_anthropic_scope"), &self.context_key)
                        && match block.get("type").and_then(serde_json::Value::as_str) {
                            Some("thinking") => {
                                block.get("thinking").and_then(serde_json::Value::as_str).is_some()
                                    && block
                                        .get("signature")
                                        .and_then(serde_json::Value::as_str)
                                        .is_some_and(|s| !s.is_empty())
                            }
                            Some("redacted_thinking") => block
                                .get("data")
                                .and_then(serde_json::Value::as_str)
                                .is_some_and(|s| !s.is_empty()),
                            _ => false,
                        }
                })
        })
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
        effort_value(&self.model, crate::Effort::High).is_some()
    }

    fn effort_use(&self, effort: crate::Effort) -> crate::EffortUse {
        match effort_value(&self.model, effort) {
            Some(value) => crate::EffortUse::parameter("output_config.effort", value),
            None => crate::EffortUse::Omitted { reason: "no effort mapping for this model" },
        }
    }

    async fn models(&self) -> Result<Vec<ModelInfo>> {
        self.models_with(crate::CatalogLimits::default()).await
    }

    async fn models_with(&self, limits: crate::CatalogLimits) -> Result<Vec<ModelInfo>> {
        #[derive(Deserialize)]
        struct Entry {
            id: String,
            #[serde(default)]
            display_name: Option<String>,
            /// The context window; there is no `context_window` field.
            #[serde(default)]
            max_input_tokens: Option<usize>,
            #[serde(default)]
            capabilities: serde_json::Value,
        }

        let page_size = limits.bounded().max_models.min(1000);
        let endpoint = reqwest::Url::parse(&self.endpoint("v1/models"))
            .map_err(|e| LlmError::Other(format!("invalid model catalog endpoint: {e}")))?;
        let mut budget = crate::catalog::Budget::new(limits);
        let entries: Vec<Entry> = budget
            .list(
                |cursor| {
                    let mut url = endpoint.clone();
                    url.query_pairs_mut().append_pair("limit", &page_size.to_string());
                    if let Some(cursor) = cursor {
                        url.query_pairs_mut().append_pair("after_id", cursor);
                    }
                    self.authorized(self.http.get(url))
                },
                &self.config.base_url,
                crate::catalog::Paging::Anthropic,
                |entry: &Entry| &entry.id,
            )
            .await?;
        Ok(entries
            .into_iter()
            .map(|e| ModelInfo {
                id: e.id,
                owned_by: e.display_name,
                context_window: e.max_input_tokens,
                // A hosted model is neither loaded nor quantised from here.
                max_context_window: None,
                loaded: None,
                quantization: None,
                capabilities: crate::ModelCapabilities::anthropic(&e.capabilities),
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

        let mut content = String::new();
        let mut tool_calls = Vec::new();
        let mut reasoning = Vec::new();
        for block in wire.content {
            match block.get("type").and_then(serde_json::Value::as_str) {
                Some("text") => content
                    .push_str(block.get("text").and_then(serde_json::Value::as_str).unwrap_or_default()),
                Some("tool_use") => tool_calls.push(ToolCall {
                    id: block.get("id").and_then(serde_json::Value::as_str).unwrap_or_default().into(),
                    name: block.get("name").and_then(serde_json::Value::as_str).unwrap_or_default().into(),
                    arguments: block.get("input").cloned().unwrap_or(serde_json::Value::Null),
                }),
                // Carried, not read: the signature covers these bytes, and the
                // next request of this turn is refused without them.
                Some("thinking" | "redacted_thinking") => {
                    let mut block = block;
                    block["rook_anthropic_scope"] = serde_json::json!(self.context_key);
                    reasoning.push(block);
                }
                _ => {}
            }
        }

        // The calls decide, not the word: the API itself says `tool_use`
        // beside them, and a gateway imitating it need not.
        let stop_reason = if tool_calls.is_empty() {
            stop_reason(wire.stop_reason.as_deref())
        } else {
            StopReason::ToolUse
        };
        Ok(Response {
            message: Message {
                role: Role::Assistant,
                content,
                tool_calls,
                tool_call_id: None,
                cache: false,
                images: Vec::new(),
                reasoning,
            },
            stop_reason,
            usage: Usage {
                input_tokens: wire.usage.input_tokens.unwrap_or(0),
                output_tokens: wire.usage.output_tokens.unwrap_or(0),
                cache_read_tokens: wire.usage.cache_read_input_tokens,
                cache_write_tokens: wire.usage.cache_creation_input_tokens,
            },
            model: wire.model.unwrap_or_else(|| self.model.clone()),
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
        let scope = self.context_key;

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
            let mut input_reported = false;
            let mut output_reported = false;
            let mut completion_confirmed = false;
            let mut stop = None;
            // Tool arguments arrive as JSON text spread over deltas, keyed by
            // the block index they belong to.
            let mut building: std::collections::BTreeMap<usize, (String, String, String)> =
                Default::default();
            // Thinking, by block index: the text and the signature that makes it
            // acceptable back. Kept apart from `building` because a turn can have
            // both, and their order on the wire is not the order they finish.
            let mut thinking: std::collections::BTreeMap<usize, (String, String)> = Default::default();

            'outer: loop {
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
                        let Ok(event) = serde_json::from_str::<Event>(data.trim()) else { continue };
                        match event {
                            Event::MessageStart { message } => {
                                if let Some(m) = message.model {
                                    model = m;
                                }
                                input_reported = message.usage.input_tokens.is_some();
                                usage.input_tokens = message.usage.input_tokens.unwrap_or(0);
                                usage.cache_read_tokens = message.usage.cache_read_input_tokens;
                                usage.cache_write_tokens = message.usage.cache_creation_input_tokens;
                            }
                            Event::ContentBlockStart { index, content_block } => match content_block {
                                Block::ToolUse { id, name } => {
                                    building.insert(index, (id, name, String::new()));
                                }
                                Block::Thinking => {
                                    thinking.insert(index, (String::new(), String::new()));
                                }
                                // Opaque by design: it arrives whole and goes
                                // back whole.
                                Block::RedactedThinking { data } => {
                                    yield Delta::ReasoningDone(
                                        serde_json::json!({ "type": "redacted_thinking", "data": data, "rook_anthropic_scope": scope }),
                                    )
                                }
                                Block::Other => {}
                            },
                            Event::ContentBlockDelta { index, delta } => match delta {
                                BlockDelta::TextDelta { text } if !text.is_empty() => {
                                    yield Delta::Text(text)
                                }
                                BlockDelta::ThinkingDelta { thinking: said } => {
                                    if let Some(slot) = thinking.get_mut(&index) {
                                        slot.0.push_str(&said);
                                    }
                                    if !said.is_empty() {
                                        yield Delta::Reasoning(said);
                                    }
                                }
                                // Last, and what makes the block acceptable
                                // back: a thinking block without it is refused.
                                BlockDelta::SignatureDelta { signature } => {
                                    if let Some(slot) = thinking.get_mut(&index) {
                                        slot.1 = signature;
                                    }
                                }
                                BlockDelta::InputJsonDelta { partial_json } => {
                                    if let Some(slot) = building.get_mut(&index) {
                                        slot.2.push_str(&partial_json);
                                    }
                                }
                                _ => {}
                            },
                            Event::MessageDelta { delta, usage: reported } => {
                                if let Some(reason) = delta.stop_reason {
                                    stop = Some(stop_reason(Some(&reason)));
                                }
                                output_reported = reported.output_tokens.is_some();
                                usage.output_tokens = reported.output_tokens.unwrap_or(0);
                            }
                            Event::MessageStop => {
                                completion_confirmed = true;
                                break 'outer;
                            }
                            Event::Error { error } => {
                                Err(LlmError::Other(error.message))?;
                            }
                            Event::Other => {}
                        }
                    }
                }
            }

            // Before the calls, as the API wants them ordered, and only the
            // signed ones: an unsigned block is one this stream did not finish.
            for (_, (said, signature)) in thinking {
                if !signature.is_empty() {
                    yield Delta::ReasoningDone(
                        serde_json::json!({ "type": "thinking", "thinking": said, "signature": signature, "rook_anthropic_scope": scope }),
                    );
                }
            }

            let had_tools = !building.is_empty();
            for (_, (id, name, arguments)) in building {
                yield Delta::ToolCall(ToolCall {
                    id,
                    name,
                    arguments: crate::parse_arguments(&arguments),
                });
            }
            yield Delta::ResponseMetadata { usage_reported: input_reported && output_reported, completion_confirmed };
            yield Delta::Done {
                stop_reason: if had_tools { StopReason::ToolUse } else { stop.unwrap_or(StopReason::EndTurn) },
                usage,
                model,
            };
        }))
    }
}

fn stop_reason(raw: Option<&str>) -> StopReason {
    match raw {
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens") => StopReason::MaxTokens,
        Some("refusal") => StopReason::Refusal,
        Some("end_turn") | Some("stop_sequence") => StopReason::EndTurn,
        _ => StopReason::Other,
    }
}

/// Whether the model takes adaptive thinking and `output_config.effort`.
///
/// Sent only to families documented to accept them: on an older model
/// `thinking: {type: "adaptive"}` is rejected outright, and guessing wrong
/// fails every request rather than degrading.
fn takes_adaptive_thinking(model: &str) -> bool {
    const FAMILIES: [&str; 6] = [
        "claude-opus-5",
        "claude-opus-4-8",
        "claude-opus-4-7",
        "claude-opus-4-6",
        "claude-sonnet-5",
        "claude-sonnet-4-6",
    ];
    FAMILIES.iter().any(|f| crate::effort::model_family(model, f))
        || crate::effort::model_family(model, "claude-fable-5")
        || crate::effort::model_family(model, "claude-mythos-5")
        || crate::effort::model_family(model, "claude-mythos-preview")
}

/// Effort and adaptive thinking are separate capabilities: Opus 4.5 accepts
/// effort without adaptive thinking; 4.6 accepts max but not xhigh.
fn effort_value(model: &str, effort: crate::Effort) -> Option<&'static str> {
    use crate::Effort::*;
    use crate::effort::model_family;
    if model_family(model, "claude-opus-4-5") {
        return Some(match effort {
            Low => "low",
            Medium => "medium",
            _ => "high",
        });
    }
    if !takes_adaptive_thinking(model) {
        return None;
    }
    let without_xhigh = ["claude-opus-4-6", "claude-sonnet-4-6", "claude-mythos-preview"]
        .iter()
        .any(|family| model_family(model, family));
    Some(if effort == XHigh && without_xhigh { "high" } else { effort.as_str() })
}

/// Build the request body.
///
/// Three shape differences from the OpenAI dialect are handled here: the system
/// prompt is lifted out of the message list, an assistant turn's tool calls
/// become content blocks, and *consecutive* tool results are merged into one
/// user message — splitting them across several teaches the model to stop
/// making parallel calls.
fn wire_request(model: &str, request: &Request, stream: bool) -> serde_json::Value {
    let mut system = String::new();
    let mut cache_system = false;
    let mut messages: Vec<serde_json::Value> = Vec::new();

    for message in &request.messages {
        match message.role {
            Role::System => {
                if !system.is_empty() {
                    system.push_str("\n\n");
                }
                system.push_str(&message.content);
                cache_system |= message.cache;
            }
            Role::Tool => {
                let content = if message.images.is_empty() {
                    serde_json::json!(message.content)
                } else {
                    let mut parts = vec![serde_json::json!({"type":"text", "text":message.content})];
                    parts.extend(message.images.iter().map(|image| serde_json::json!({
                        "type":"image", "source":{"type":"base64", "media_type":image.mime_type,"data":image.data}
                    })));
                    serde_json::json!(parts)
                };
                let block = serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": message.tool_call_id.clone().unwrap_or_default(),
                    "content": content,
                });
                match messages.last_mut() {
                    Some(last) if last["role"] == "user" && last["content"].is_array() => {
                        if let Some(blocks) = last["content"].as_array_mut() {
                            blocks.push(block);
                        }
                    }
                    _ => messages.push(serde_json::json!({
                        "role": "user",
                        "content": [block],
                    })),
                }
            }
            Role::User => {
                let mut blocks: Vec<_> = message.images.iter().map(|image| serde_json::json!({
                    "type":"image", "source":{"type":"base64", "media_type":image.mime_type,"data":image.data}
                })).collect();
                blocks.push(text_block(&message.content, message.cache, request.cache_ttl));
                messages.push(serde_json::json!({"role":"user", "content":blocks}));
            }
            Role::Assistant => {
                // First, as the API orders them, and only when this turn is
                // still going: thinking is required back beside the tool call
                // it led to, and is neither wanted nor kept for a turn that
                // ended — a replayed conversation carries none.
                let mut blocks: Vec<serde_json::Value> = message
                    .reasoning
                    .iter()
                    .filter(|block| {
                        matches!(
                            block.get("type").and_then(serde_json::Value::as_str),
                            Some("thinking" | "redacted_thinking")
                        )
                    })
                    .map(|block| {
                        let mut block = block.clone();
                        if let Some(object) = block.as_object_mut() {
                            object.remove("rook_anthropic_scope");
                        }
                        block
                    })
                    .collect();
                if !message.content.trim().is_empty() {
                    blocks.push(text_block(&message.content, false, request.cache_ttl));
                }
                for call in &message.tool_calls {
                    blocks.push(serde_json::json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.name,
                        "input": call.arguments,
                    }));
                }
                // A breakpoint sits on the last block of the message it marks.
                if message.cache
                    && let Some(last) = blocks.last_mut()
                {
                    last["cache_control"] = ephemeral(request.cache_ttl);
                }
                // An assistant turn with nothing in it is rejected.
                if !blocks.is_empty() {
                    messages.push(serde_json::json!({ "role": "assistant", "content": blocks }));
                }
            }
        }
    }

    let mut body = serde_json::json!({
        "model": model,
        "max_tokens": request.max_output_tokens,
        "messages": messages,
        "stream": stream,
    });
    if !system.is_empty() {
        // As an array so the breakpoint can sit on it; tools render before
        // system, so one marker here caches both.
        body["system"] = serde_json::json!([text_block(&system, cache_system, request.cache_ttl)]);
    }
    if request.model_capabilities.reasoning != Some(false)
        && request.model_capabilities.adaptive_thinking.unwrap_or_else(|| takes_adaptive_thinking(model))
    {
        // `display` defaults to omitted on these models, which streams empty
        // thinking blocks — a long pause with nothing to show for it.
        body["thinking"] = serde_json::json!({ "type": "adaptive", "display": "summarized" });
    }
    if let Some(effort) = request.effort.and_then(|effort| effort_value(model, effort)) {
        body["output_config"] = serde_json::json!({ "effort": effort });
    }
    if !request.tools.is_empty() {
        body["tools"] = request
            .tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                })
            })
            .collect::<Vec<_>>()
            .into();
    }
    body
}

fn text_block(text: &str, cache: bool, ttl: crate::CacheTtl) -> serde_json::Value {
    let mut block = serde_json::json!({ "type": "text", "text": text });
    if cache {
        block["cache_control"] = ephemeral(ttl);
    }
    block
}

/// The five-minute default is unnamed on the wire, so only the hour is sent —
/// an unknown field is a rejected request on a model that does not offer it.
fn ephemeral(ttl: crate::CacheTtl) -> serde_json::Value {
    match ttl {
        crate::CacheTtl::FiveMinutes => serde_json::json!({ "type": "ephemeral" }),
        crate::CacheTtl::OneHour => serde_json::json!({ "type": "ephemeral", "ttl": "1h" }),
    }
}

#[derive(Deserialize)]
struct WireResponse {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    content: Vec<serde_json::Value>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    usage: WireUsage,
}

/// A block as a stream announces it. The whole block is read from the raw
/// JSON where it matters — a thinking block is carried verbatim — so this
/// only has to say which kind started, and with what.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Block {
    ToolUse {
        id: String,
        name: String,
    },
    Thinking,
    RedactedThinking {
        #[serde(default)]
        data: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Default, Deserialize, Serialize)]
struct WireUsage {
    #[serde(default)]
    input_tokens: Option<u32>,
    #[serde(default)]
    output_tokens: Option<u32>,
    #[serde(default)]
    cache_read_input_tokens: u32,
    #[serde(default)]
    cache_creation_input_tokens: u32,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event {
    MessageStart {
        message: StartedMessage,
    },
    ContentBlockStart {
        index: usize,
        content_block: Block,
    },
    ContentBlockDelta {
        index: usize,
        delta: BlockDelta,
    },
    MessageDelta {
        delta: StopDelta,
        #[serde(default)]
        usage: WireUsage,
    },
    MessageStop,
    Error {
        error: WireError,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct StartedMessage {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: WireUsage,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum BlockDelta {
    TextDelta {
        text: String,
    },
    ThinkingDelta {
        thinking: String,
    },
    SignatureDelta {
        signature: String,
    },
    InputJsonDelta {
        #[serde(default)]
        partial_json: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct StopDelta {
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct WireError {
    #[serde(default)]
    message: String,
}
