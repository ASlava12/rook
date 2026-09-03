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
}

impl Anthropic {
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

    fn authorized(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request.header("x-api-key", &self.config.api_key).header("anthropic-version", API_VERSION)
    }

    async fn send(&self, request: &Request, stream: bool) -> Result<reqwest::Response> {
        let body = wire_request(&self.model, request, stream);
        self.authorized(self.http.post(self.endpoint("v1/messages")).json(&body))
            .send()
            .await
            .map_err(|e| LlmError::unreachable(&self.config.base_url, e))
    }
}

#[async_trait]
impl Provider for Anthropic {
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
            data: Vec<Entry>,
        }
        #[derive(Deserialize)]
        struct Entry {
            id: String,
            #[serde(default)]
            display_name: Option<String>,
            /// The context window; there is no `context_window` field.
            #[serde(default)]
            max_input_tokens: Option<usize>,
        }

        let response = self
            .authorized(self.http.get(self.endpoint("v1/models")))
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .map_err(|e| LlmError::unreachable(&self.config.base_url, e))?;
        let status = response.status();
        if !status.is_success() {
            return Err(LlmError::Status {
                status: status.as_u16(),
                body: crate::quoted_text(response).await,
            });
        }
        let text = crate::whole_text(response, &self.config.base_url).await?;
        let listing: Listing = serde_json::from_str(&text)
            .map_err(|e| LlmError::Decode(format!("{e}: {}", truncate(&text, 300))))?;
        Ok(listing
            .data
            .into_iter()
            .map(|e| ModelInfo { id: e.id, owned_by: e.display_name, context_window: e.max_input_tokens })
            .collect())
    }

    async fn complete(&self, request: Request) -> Result<Response> {
        let response = self.send(&request, false).await?;
        let status = response.status();
        if !status.is_success() {
            return Err(LlmError::Status {
                status: status.as_u16(),
                body: crate::quoted_text(response).await,
            });
        }
        let text = crate::whole_text(response, &self.config.base_url).await?;

        let wire: WireResponse = serde_json::from_str(&text)
            .map_err(|e| LlmError::Decode(format!("{e}: {}", truncate(&text, 500))))?;

        let mut content = String::new();
        let mut tool_calls = Vec::new();
        for block in wire.content {
            match block {
                Block::Text { text } => content.push_str(&text),
                Block::ToolUse { id, name, input } => {
                    tool_calls.push(ToolCall { id, name, arguments: input })
                }
                Block::Other => {}
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
            message: Message { role: Role::Assistant, content, tool_calls, tool_call_id: None, cache: false },
            stop_reason,
            usage: Usage {
                input_tokens: wire.usage.input_tokens,
                output_tokens: wire.usage.output_tokens,
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
                body: crate::quoted_text(response).await,
            });
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
            let mut stop = None;
            // Tool arguments arrive as JSON text spread over deltas, keyed by
            // the block index they belong to.
            let mut building: std::collections::BTreeMap<usize, (String, String, String)> =
                Default::default();

            'outer: loop {
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
                        let Ok(event) = serde_json::from_str::<Event>(data.trim()) else { continue };
                        match event {
                            Event::MessageStart { message } => {
                                if let Some(m) = message.model {
                                    model = m;
                                }
                                usage.input_tokens = message.usage.input_tokens;
                                usage.cache_read_tokens = message.usage.cache_read_input_tokens;
                                usage.cache_write_tokens = message.usage.cache_creation_input_tokens;
                            }
                            Event::ContentBlockStart { index, content_block } => {
                                if let Block::ToolUse { id, name, .. } = content_block {
                                    building.insert(index, (id, name, String::new()));
                                }
                            }
                            Event::ContentBlockDelta { index, delta } => match delta {
                                BlockDelta::TextDelta { text } if !text.is_empty() => {
                                    yield Delta::Text(text)
                                }
                                BlockDelta::ThinkingDelta { thinking } if !thinking.is_empty() => {
                                    yield Delta::Reasoning(thinking)
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
                                usage.output_tokens = reported.output_tokens;
                            }
                            Event::MessageStop => break 'outer,
                            Event::Error { error } => {
                                Err(LlmError::Other(error.message))?;
                            }
                            Event::Other => {}
                        }
                    }
                }
                scanned = buffer.len().saturating_sub(1);
            }

            let had_tools = !building.is_empty();
            for (_, (id, name, arguments)) in building {
                yield Delta::ToolCall(ToolCall {
                    id,
                    name,
                    arguments: crate::parse_arguments(&arguments),
                });
            }
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

/// Build the request body.
///
/// Three shape differences from the OpenAI dialect are handled here: the system
/// prompt is lifted out of the message list, an assistant turn's tool calls
/// become content blocks, and *consecutive* tool results are merged into one
/// user message — splitting them across several teaches the model to stop
/// making parallel calls.
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
    FAMILIES.iter().any(|f| model.starts_with(f))
        || model.starts_with("claude-fable")
        || model.starts_with("claude-mythos")
}

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
                let block = serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": message.tool_call_id.clone().unwrap_or_default(),
                    "content": message.content,
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
            Role::User => messages.push(serde_json::json!({
                "role": "user",
                "content": [text_block(&message.content, message.cache, request.cache_ttl)],
            })),
            Role::Assistant => {
                let mut blocks = Vec::new();
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
    if takes_adaptive_thinking(model) {
        // `display` defaults to omitted on these models, which streams empty
        // thinking blocks — a long pause with nothing to show for it.
        body["thinking"] = serde_json::json!({ "type": "adaptive", "display": "summarized" });
        if let Some(effort) = request.effort {
            body["output_config"] = serde_json::json!({ "effort": effort.as_str() });
        }
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
    content: Vec<Block>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    usage: WireUsage,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Block {
    Text {
        #[serde(default)]
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: serde_json::Value,
    },
    #[serde(other)]
    Other,
}

#[derive(Default, Deserialize, Serialize)]
struct WireUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
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
