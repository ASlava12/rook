//! The OpenAI chat-completions dialect.
//!
//! Implemented once and pointed at whichever base URL the user configured, which
//! is what makes "works with local models" true rather than aspirational: Ollama,
//! LM Studio, llama.cpp and vLLM all serve this shape.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{LlmError, Message, Provider, Request, Response, Result, Role, StopReason, ToolCall, Usage};

pub struct Config {
    pub base_url: String,
    pub api_key: Option<String>,
    pub context_window: usize,
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
            .map_err(|e| LlmError::Transport(e.to_string()))?;
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

    async fn complete(&self, request: Request) -> Result<Response> {
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
            stream: false,
        };

        let mut req = self
            .http
            .post(format!("{}/chat/completions", self.config.base_url.trim_end_matches('/')))
            .json(&body);
        if let Some(key) = &self.config.api_key {
            req = req.bearer_auth(key);
        }

        let resp = req.send().await.map_err(|e| LlmError::Transport(e.to_string()))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| LlmError::Transport(e.to_string()))?;
        if !status.is_success() {
            return Err(LlmError::Status { status: status.as_u16(), body: truncate(&text, 2000) });
        }

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
                // Arguments arrive as a JSON string; a model that emits invalid
                // JSON here is common enough that it must not be fatal.
                arguments: serde_json::from_str(&c.function.arguments).unwrap_or(serde_json::Value::Null),
            })
            .collect();

        let stop_reason = match choice.finish_reason.as_deref() {
            Some("tool_calls") | Some("function_call") => StopReason::ToolUse,
            Some("length") => StopReason::MaxTokens,
            Some("content_filter") => StopReason::Refusal,
            Some("stop") => StopReason::EndTurn,
            _ if !tool_calls.is_empty() => StopReason::ToolUse,
            _ => StopReason::Other,
        };

        Ok(Response {
            message: Message {
                role: Role::Assistant,
                content: choice.message.content.unwrap_or_default(),
                tool_calls,
                tool_call_id: None,
            },
            stop_reason,
            usage: Usage {
                input_tokens: wire.usage.as_ref().map(|u| u.prompt_tokens).unwrap_or(0),
                output_tokens: wire.usage.as_ref().map(|u| u.completion_tokens).unwrap_or(0),
            },
            model: wire.model.unwrap_or_else(|| self.model.clone()),
        })
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
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
    content: Option<String>,
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
struct WireUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
}
