//! The OpenAI chat-completions dialect.
//!
//! Implemented once and pointed at whichever base URL the user configured, which
//! is what makes "works with local models" true rather than aspirational: Ollama,
//! LM Studio, llama.cpp and vLLM all serve this shape.

use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::stream::{Delta, ResponseStream, ToolCallBuffer};
use crate::{
    LlmError, Message, Provider, Request, Response, Result, Role, StopReason, ToolCall, Usage, truncate,
};

/// A frame this large is a broken or hostile endpoint, not a long answer: the
/// dialect sends one small JSON object per frame.
const MAX_FRAME_BYTES: usize = 8 << 20;

pub struct Config {
    pub base_url: String,
    pub api_key: Option<String>,
    pub context_window: usize,
    /// How long the model may go silent mid-stream before the stream is
    /// abandoned. Without this a dropped connection looks like a model that is
    /// merely thinking, and the turn hangs until the overall timeout.
    pub stream_idle_timeout: Duration,
}

impl Config {
    pub fn new(base_url: String, api_key: Option<String>, context_window: usize) -> Self {
        Self { base_url, api_key, context_window, stream_idle_timeout: Duration::from_secs(90) }
    }
}

pub struct OpenAiCompatible {
    id: String,
    model: String,
    config: Config,
    http: reqwest::Client,
}

impl OpenAiCompatible {
    pub fn new(id: &str, model: &str, config: Config) -> Result<Self> {
        crate::init_tls();
        let http = reqwest::Client::builder()
            .user_agent(concat!("rook/", env!("CARGO_PKG_VERSION")))
            // A long-running agent turn can legitimately take minutes on a local
            // model; a short default timeout would look like a provider bug.
            .timeout(std::time::Duration::from_secs(600))
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| LlmError::unreachable(&config.base_url, e))?;
        Ok(Self { id: id.to_string(), model: model.to_string(), config, http })
    }
}

#[async_trait]
impl Provider for OpenAiCompatible {
    fn id(&self) -> &str {
        &self.id
    }

    fn context_window(&self) -> usize {
        self.config.context_window
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    async fn complete(&self, request: Request) -> Result<Response> {
        let resp = self.send(&request, false).await?;
        let status = resp.status();
        if !status.is_success() {
            let asked = crate::retry_after(resp.headers());
            return Err(self.refused(status, asked, &crate::quoted_text(resp).await).await);
        }
        let text = crate::whole_text(resp, &self.config.base_url).await?;

        let wire: WireResponse = serde_json::from_str(&text)
            .map_err(|e| LlmError::Decode(format!("{e}: {}", truncate(&text, 500))))?;
        let choice = wire
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| LlmError::Decode("provider returned no choices".into()))?;

        let tool_calls: Vec<ToolCall> = choice
            .message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(|c| ToolCall {
                id: c.id,
                name: c.function.name,
                arguments: crate::parse_arguments(&c.function.arguments),
            })
            .collect();

        // The calls decide, not the word: Ollama says `stop` for a reply that
        // spoke and called, and read as the word the calls were logged as
        // text and never run.
        let stop_reason = match choice.finish_reason.as_deref() {
            _ if !tool_calls.is_empty() => StopReason::ToolUse,
            Some(reason) => finish_reason(reason),
            None => StopReason::Other,
        };

        Ok(Response {
            message: Message {
                role: Role::Assistant,
                content: choice.message.content.map(Text::into_string).unwrap_or_default(),
                tool_calls,
                tool_call_id: None,
                cache: false,
                reasoning: Vec::new(),
            },
            stop_reason,
            usage: Usage {
                input_tokens: wire.usage.as_ref().map(|u| u.prompt_tokens).unwrap_or(0),
                output_tokens: wire.usage.as_ref().map(|u| u.completion_tokens).unwrap_or(0),
                ..Default::default()
            },
            model: wire.model.unwrap_or_else(|| self.model.clone()),
        })
    }

    async fn models(&self) -> Result<Vec<crate::ModelInfo>> {
        #[derive(Deserialize)]
        struct Listing {
            #[serde(default)]
            data: Vec<Entry>,
        }
        #[derive(Deserialize)]
        struct Entry {
            id: String,
            #[serde(default)]
            owned_by: Option<String>,
            /// Not in the OpenAI shape, but several compatible servers add it.
            #[serde(default, alias = "max_model_len", alias = "context_length")]
            context_window: Option<usize>,
        }

        let mut request = self.http.get(format!("{}/models", self.config.base_url.trim_end_matches('/')));
        if let Some(key) = &self.config.api_key {
            request = request.bearer_auth(key);
        }
        let response = request
            .timeout(std::time::Duration::from_secs(20))
            .send()
            .await
            .map_err(|e| LlmError::unreachable(&self.config.base_url, e))?;

        let status = response.status();
        if !status.is_success() {
            return Err(LlmError::Status {
                status: status.as_u16(),
                retry_after: crate::retry_after(response.headers()),
                body: crate::quoted_text(response).await,
            });
        }
        let text = crate::whole_text(response, &self.config.base_url).await?;
        let listing: Listing = serde_json::from_str(&text)
            .map_err(|e| LlmError::Decode(format!("{e}: {}", truncate(&text, 300))))?;
        let mut models: Vec<crate::ModelInfo> = listing
            .data
            .into_iter()
            .map(|e| crate::ModelInfo { id: e.id, owned_by: e.owned_by, context_window: e.context_window })
            .collect();
        // The OpenAI shape has no context length, and only some servers add
        // one. LM Studio is not among them — it answers that on an endpoint of
        // its own, and the number matters: a model that serves 262144 was being
        // budgeted at the 32768 this crate assumes for anything self-hosted,
        // which is a quarter of the reading it could have held. Asked only when
        // the compatible listing said nothing, so a server that does answer
        // properly pays no second round trip.
        if models.iter().all(|m| m.context_window.is_none()) {
            for (id, window) in self.windows_lm_studio_reports().await {
                if let Some(model) = models.iter_mut().find(|m| m.id == id) {
                    model.context_window = Some(window);
                }
            }
        }
        Ok(models)
    }

    async fn stream(&self, request: Request) -> Result<ResponseStream> {
        let resp = self.send(&request, true).await?;
        let status = resp.status();
        if !status.is_success() {
            let asked = crate::retry_after(resp.headers());
            return Err(self.refused(status, asked, &crate::quoted_text(resp).await).await);
        }

        let idle = self.config.stream_idle_timeout;
        let endpoint = self.config.base_url.clone();
        let fallback_model = self.model.clone();

        Ok(Box::pin(async_stream::try_stream! {
            let mut bytes = resp.bytes_stream();
            let mut buffer = String::new();
            // Where the search for a frame boundary resumes. Without it, every
            // chunk rescans the whole buffer, which is quadratic against an
            // endpoint that streams without ever sending a separator.
            let mut scanned = 0usize;
            let mut tools = ToolCallBuffer::default();
            let mut usage = Usage::default();
            let mut model = fallback_model;
            let mut stop = None;

            'outer: loop {
                let chunk = match tokio::time::timeout(idle, bytes.next()).await {
                    Err(_) => Err(LlmError::Stalled { secs: idle.as_secs() })?,
                    Ok(None) => break,
                    Ok(Some(chunk)) => chunk.map_err(|e| LlmError::unreachable(&endpoint, e))?,
                };
                buffer.push_str(&String::from_utf8_lossy(&chunk));
                if buffer.len() > MAX_FRAME_BYTES {
                    Err(LlmError::Decode(format!(
                        "a single SSE frame passed {MAX_FRAME_BYTES} bytes with no separator"
                    )))?;
                }

                // SSE frames are separated by a blank line; a frame can span
                // several transport chunks, and a chunk can hold several frames.
                while let Some(offset) = buffer[scanned..].find("\n\n") {
                    let end = scanned + offset;
                    scanned = 0;
                    let frame: String = buffer.drain(..end + 2).collect();
                    for line in frame.lines() {
                        let Some(data) = line.strip_prefix("data:") else { continue };
                        let data = data.trim();
                        if data == "[DONE]" {
                            break 'outer;
                        }
                        let Ok(parsed) = serde_json::from_str::<WireChunk>(data) else { continue };
                        if let Some(m) = parsed.model {
                            model = m;
                        }
                        if let Some(u) = parsed.usage {
                            usage = Usage {
                                input_tokens: u.prompt_tokens,
                                output_tokens: u.completion_tokens,
                                ..Default::default()
                            };
                        }
                        for choice in parsed.choices {
                            if let Some(text) = choice.delta.content.map(Text::into_string).filter(|t| !t.is_empty()) {
                                yield Delta::Text(text);
                            }
                            if let Some(text) = choice.delta.reasoning_content.map(Text::into_string).filter(|t| !t.is_empty()) {
                                yield Delta::Reasoning(text);
                            }
                            for call in choice.delta.tool_calls.unwrap_or_default() {
                                let function = call.function.unwrap_or_default();
                                tools.push(
                                    call.index,
                                    call.id.as_deref(),
                                    function.name.as_deref(),
                                    function.arguments.as_deref().unwrap_or_default(),
                                );
                            }
                            if let Some(reason) = choice.finish_reason {
                                stop = Some(finish_reason(&reason));
                            }
                        }
                    }
                }
                // A separator can straddle two chunks, so resume one byte back.
                scanned = buffer.len().saturating_sub(1);
            }

            let had_tools = !tools.is_empty();
            for call in tools.drain() {
                yield Delta::ToolCall(call);
            }
            yield Delta::Done {
                // The calls decide, not the word, as above.
                stop_reason: if had_tools { StopReason::ToolUse } else { stop.unwrap_or(StopReason::EndTurn) },
                usage,
                model,
            };
        }))
    }
}

impl OpenAiCompatible {
    /// What LM Studio says each model will hold, by its own API rather than the
    /// compatible one.
    ///
    /// `loaded_context_length` first: a model that supports 262144 may have
    /// been loaded with 8192, and the number worth budgeting against is the one
    /// it will actually serve. Failures are silence — this is a guess being
    /// improved, and a server that is not LM Studio simply answers 404.
    async fn windows_lm_studio_reports(&self) -> Vec<(String, usize)> {
        #[derive(serde::Deserialize)]
        struct Listing {
            #[serde(default)]
            data: Vec<Entry>,
        }
        #[derive(serde::Deserialize)]
        struct Entry {
            id: String,
            #[serde(default)]
            loaded_context_length: Option<usize>,
            #[serde(default)]
            max_context_length: Option<usize>,
        }

        let root = self.config.base_url.trim_end_matches('/').trim_end_matches("/v1");
        let Ok(response) = self
            .http
            .get(format!("{root}/api/v0/models"))
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
        else {
            return Vec::new();
        };
        if !response.status().is_success() {
            return Vec::new();
        }
        let Ok(listing) = response.json::<Listing>().await else { return Vec::new() };
        listing
            .data
            .into_iter()
            .filter_map(|e| Some((e.id, e.loaded_context_length.or(e.max_context_length)?)))
            .collect()
    }

    /// A 404 from a server that is otherwise answering means the model is not
    /// there — the common first-run failure, because the default spec names a
    /// model nobody has pulled yet. The server knows which it does have.
    async fn refused(
        &self,
        status: reqwest::StatusCode,
        retry_after: Option<std::time::Duration>,
        body: &str,
    ) -> LlmError {
        if status != reqwest::StatusCode::NOT_FOUND {
            return LlmError::Status { status: status.as_u16(), body: truncate(body, 2000), retry_after };
        }
        // If the listing does not answer either, the base URL is the likelier
        // fault and "the model is missing" would be a guess: say what happened.
        let Ok(models) = self.models().await else {
            return LlmError::Status { status: status.as_u16(), body: truncate(body, 2000), retry_after };
        };
        LlmError::NoSuchModel {
            model: self.model.clone(),
            endpoint: crate::origin(&self.config.base_url),
            available: models.into_iter().map(|m| m.id).collect(),
        }
    }

    async fn send(&self, request: &Request, stream: bool) -> Result<reqwest::Response> {
        let body = WireRequest {
            model: &self.model,
            messages: request.messages.iter().map(WireMessage::from).collect(),
            tools: request
                .tools
                .iter()
                .map(|t| WireTool {
                    r#type: "function",
                    function: WireFunction {
                        name: &t.name,
                        description: &t.description,
                        parameters: t.parameters.clone(),
                    },
                })
                .collect(),
            max_tokens: request.max_output_tokens,
            temperature: request.temperature,
            stream,
            stream_options: stream.then_some(StreamOptions { include_usage: true }),
            reasoning_effort: request.effort.and_then(|e| reasoning_effort(&self.model, e)),
        };

        let mut req = self
            .http
            .post(format!("{}/chat/completions", self.config.base_url.trim_end_matches('/')))
            .json(&body);
        if let Some(key) = &self.config.api_key {
            req = req.bearer_auth(key);
        }
        req.send().await.map_err(|e| LlmError::unreachable(&self.config.base_url, e))
    }
}

fn finish_reason(raw: &str) -> StopReason {
    match raw {
        "tool_calls" | "function_call" => StopReason::ToolUse,
        "length" => StopReason::MaxTokens,
        "content_filter" => StopReason::Refusal,
        "stop" => StopReason::EndTurn,
        _ => StopReason::Other,
    }
}

#[derive(Serialize)]
struct WireRequest<'a> {
    model: &'a str,
    messages: Vec<WireMessage<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<WireTool<'a>>,
    max_tokens: u32,
    temperature: f32,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'a str>,
}

/// The rung this dialect understands, or `None` for a model that has no
/// reasoning to spend.
///
/// Sent only to families documented to accept it. A strict OpenAI-compatible
/// server rejects an unknown field outright rather than ignoring it, and most
/// of what speaks this dialect is not OpenAI — so the default is to say
/// nothing, which is also what the field did before it was mapped at all.
fn reasoning_effort(model: &str, effort: crate::Effort) -> Option<&'static str> {
    let reasons = model.starts_with("gpt-5")
        || model.starts_with("o1")
        || model.starts_with("o3")
        || model.starts_with("o4");
    if !reasons {
        return None;
    }
    Some(match effort {
        // Four rungs against five: the two above `high` are the same request
        // here, and pretending otherwise would be a value the API rejects.
        crate::Effort::Low => "low",
        crate::Effort::Medium => "medium",
        crate::Effort::High | crate::Effort::XHigh | crate::Effort::Max => "high",
    })
}

/// Without this, a streamed response carries no token counts at all, and the
/// context budget has nothing to work from.
#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Serialize)]
struct WireMessage<'a> {
    role: &'static str,
    content: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<WireToolCall<'a>>,
}

impl<'a> From<&'a Message> for WireMessage<'a> {
    fn from(m: &'a Message) -> Self {
        Self {
            role: match m.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            },
            content: &m.content,
            tool_call_id: m.tool_call_id.as_deref(),
            tool_calls: m
                .tool_calls
                .iter()
                .map(|c| WireToolCall {
                    id: &c.id,
                    r#type: "function",
                    function: WireCallFunction { name: &c.name, arguments: c.arguments.to_string() },
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct WireToolCall<'a> {
    id: &'a str,
    r#type: &'static str,
    function: WireCallFunction<'a>,
}

#[derive(Serialize)]
struct WireCallFunction<'a> {
    name: &'a str,
    arguments: String,
}

#[derive(Serialize)]
struct WireTool<'a> {
    r#type: &'static str,
    function: WireFunction<'a>,
}

#[derive(Serialize)]
struct WireFunction<'a> {
    name: &'a str,
    description: &'a str,
    parameters: serde_json::Value,
}

#[derive(Deserialize)]
struct WireResponse {
    #[serde(default)]
    model: Option<String>,
    choices: Vec<WireChoice>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct WireChoice {
    message: WireRespMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct WireRespMessage {
    #[serde(default)]
    content: Option<Text>,
    #[serde(default)]
    tool_calls: Option<Vec<WireRespToolCall>>,
}

#[derive(Deserialize)]
struct WireRespToolCall {
    id: String,
    function: WireRespFunction,
}

#[derive(Deserialize)]
struct WireRespFunction {
    name: String,
    #[serde(default)]
    arguments: String,
}

#[derive(Deserialize)]
struct WireChunk {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    choices: Vec<WireChunkChoice>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct WireChunkChoice {
    #[serde(default)]
    delta: WireDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Default, Deserialize)]
struct WireDelta {
    #[serde(default)]
    content: Option<Text>,
    /// Non-standard but widely emitted by reasoning models.
    #[serde(default, alias = "reasoning")]
    reasoning_content: Option<Text>,
    #[serde(default)]
    tool_calls: Option<Vec<WireDeltaToolCall>>,
}

/// The text of a message, however the server spells it.
///
/// The dialect as written says a string. Several servers that implement it send
/// a list of parts instead — the Responses shape leaking into chat-completions —
/// and one field typed as a string makes the whole frame fail to parse. A frame
/// that fails to parse is skipped, so the reply arrives empty with nothing
/// anywhere saying why.
#[derive(Deserialize)]
#[serde(untagged)]
enum Text {
    One(String),
    Parts(Vec<Part>),
}

#[derive(Deserialize)]
struct Part {
    #[serde(default)]
    text: String,
}

impl Text {
    fn into_string(self) -> String {
        match self {
            Self::One(text) => text,
            Self::Parts(parts) => parts.into_iter().map(|p| p.text).collect(),
        }
    }
}

#[derive(Deserialize)]
struct WireDeltaToolCall {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<WireDeltaFunction>,
}

#[derive(Default, Deserialize)]
struct WireDeltaFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct WireUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
}
