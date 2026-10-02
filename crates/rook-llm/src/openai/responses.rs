//! Stateless Responses requests with local replay of the returned output items.
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{Config, OpenAiCompatible};
use crate::{
    Delta, Effort, EffortUse, LlmError, Message, Provider, Request, Response, ResponseStream, Result, Role,
    StopReason, ToolCall, Usage,
};

/// The Responses dialect. Catalogs, proxy policy and credentials share the
/// compatible adapter; generation has its own encoding and context identity.
pub struct Responses {
    inner: OpenAiCompatible,
    context_key: [u8; 32],
}

impl Responses {
    pub fn new(id: &str, model: &str, config: Config) -> Result<Self> {
        let inner = OpenAiCompatible::new(id, model, config)?;
        let mut key = Sha256::new();
        key.update(b"rook-responses-context-v1");
        key.update(inner.context_key);
        Ok(Self { inner, context_key: key.finalize().into() })
    }

    async fn send(&self, request: &Request, stream: bool) -> Result<reqwest::Response> {
        let config = &self.inner.config;
        let body = wire_request(&self.inner.model, &self.context_key, request, stream)?;
        let mut builder =
            self.inner.http.post(format!("{}/responses", config.base_url.trim_end_matches('/'))).json(&body);
        if let Some(key) = &config.api_key {
            builder = builder.bearer_auth(key);
        }
        let bytes = request.prompt_bytes();
        let patience = crate::first_token_patience(config.stream_idle_timeout, bytes);
        let response = crate::once_it_answers(patience, bytes, async {
            builder.send().await.map_err(|error| LlmError::unreachable(&config.base_url, error))
        })
        .await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let retry_after = crate::retry_after(response.headers());
            return Err(LlmError::Status {
                status,
                retry_after,
                body: crate::quoted_text(response, config.stream_idle_timeout).await,
            });
        }
        Ok(response)
    }
}

#[async_trait]
impl Provider for Responses {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn dispatch_identity(&self) -> Option<crate::Dispatch> {
        crate::Dispatch::bounded(self.id(), &self.inner.model, true)
    }

    fn context_window(&self) -> usize {
        self.inner.context_window()
    }
    fn context_key(&self) -> Option<[u8; 32]> {
        Some(self.context_key)
    }

    fn can_replay_reasoning(&self, messages: &[crate::Message]) -> bool {
        messages.iter().all(|message| {
            message.reasoning.is_empty()
                || (message.role == Role::Assistant
                    && message.reasoning.len() == 1
                    && replay(message, &self.context_key).is_some())
        })
    }
    fn context_is_explicit(&self) -> bool {
        self.inner.context_is_explicit()
    }
    async fn discover_context_window(&self, limits: crate::CatalogLimits) -> Result<Option<usize>> {
        self.inner.discover_context_window(limits).await
    }
    fn supports_streaming(&self) -> bool {
        true
    }
    fn takes_effort(&self) -> bool {
        self.inner.takes_effort()
    }
    fn effort_use(&self, effort: Effort) -> EffortUse {
        match super::reasoning_effort(&self.inner.model, effort) {
            Some(value) => EffortUse::parameter("reasoning.effort", value),
            None => EffortUse::Omitted { reason: "no effort mapping for this model" },
        }
    }
    async fn models(&self) -> Result<Vec<crate::ModelInfo>> {
        self.inner.models().await
    }
    async fn models_with(&self, limits: crate::CatalogLimits) -> Result<Vec<crate::ModelInfo>> {
        self.inner.models_with(limits).await
    }

    async fn complete(&self, request: Request) -> Result<Response> {
        self.complete_with_metadata(request).await.map(|completed| completed.response)
    }

    async fn complete_with_metadata(&self, request: Request) -> Result<crate::Completion> {
        let response = self.send(&request, false).await?;
        let text =
            crate::whole_text(response, &self.inner.config.base_url, self.inner.config.stream_idle_timeout)
                .await?;
        let value: Value = serde_json::from_str(&text)
            .map_err(|error| LlmError::Decode(format!("Responses JSON: {error}")))?;
        let response = decode(&value, &self.inner.model, &self.context_key)?;
        Ok(crate::Completion {
            response,
            dispatch: self.dispatch_identity(),
            usage_reported: value.pointer("/usage/input_tokens").is_some_and(Value::is_u64)
                && value.pointer("/usage/output_tokens").is_some_and(Value::is_u64),
            completion_confirmed: matches!(
                value.get("status").and_then(Value::as_str),
                Some("completed" | "incomplete")
            ),
        })
    }

    async fn stream(&self, request: Request) -> Result<ResponseStream> {
        let response = self.send(&request, true).await?;
        let idle = self.inner.config.stream_idle_timeout;
        let first = crate::first_token_patience(idle, request.prompt_bytes());
        let tokens = request.prompt_bytes() / crate::BYTES_A_TOKEN_ROUGHLY;
        let endpoint = self.inner.config.base_url.clone();
        let model = self.inner.model.clone();
        let scope = self.context_key;
        Ok(Box::pin(async_stream::try_stream! {
            let mut bytes = response.bytes_stream();
            let mut frames = crate::Frames::new();
            let mut received = 0usize;
            let mut answered = false;
            let mut spoken = String::new();
            let mut thought = String::new();
            let mut finished = false;
            'read: loop {
                let patience = if answered { idle } else { first };
                let chunk = match tokio::time::timeout(patience, bytes.next()).await {
                    Err(_) if answered => Err(LlmError::Stalled { secs: patience.as_secs() })?,
                    Err(_) => Err(LlmError::NeverAnswered { secs: patience.as_secs(), tokens })?,
                    Ok(None) => break,
                    Ok(Some(chunk)) => chunk.map_err(|error| LlmError::unreachable(&endpoint, error))?,
                };
                answered = true;
                received = received.saturating_add(chunk.len());
                if received > crate::MOST_REPLY_BYTES {
                    Err(LlmError::Decode("Responses stream exceeded the response byte budget".into()))?;
                }
                frames.feed(&chunk);
                for frame in frames.ready() {
                    // SSE permits several data lines and CRLF. Joining only
                    // data fields also excludes comment/keepalive frames.
                    let data = frame.lines().filter_map(|line| line.strip_prefix("data:"))
                        .map(|line| line.strip_prefix(' ').unwrap_or(line)).collect::<Vec<_>>().join("\n");
                    if data.is_empty() { continue; }
                    if data.trim() == "[DONE]" { break 'read; }
                    let event: Value = serde_json::from_str(&data)
                        .map_err(|error| LlmError::Decode(format!("Responses event JSON: {error}")))?;
                    let kind = string(&event, "type")?;
                    match kind {
                        "response.output_text.delta" | "response.refusal.delta" => {
                            let text = string(&event, "delta")?;
                            spoken.push_str(text);
                            yield Delta::Text(text.into());
                        }
                        "response.reasoning_summary_text.delta" => {
                            let text = string(&event, "delta")?;
                            thought.push_str(text);
                            yield Delta::Reasoning(text.into());
                        }
                        "error" => Err(response_error(&event))?,
                        "response.failed" | "response.completed" | "response.incomplete" => {
                            let value = event.get("response").ok_or_else(|| LlmError::Decode("Responses terminal event has no response".into()))?;
                            if value.get("status").and_then(Value::as_str) != kind.strip_prefix("response.") {
                                Err(LlmError::Decode("Responses terminal event and status disagree".into()))?;
                            }
                            let decoded = decode(value, &model, &scope)?;
                            // Terminal output is authoritative. A disconnected
                            // stream never releases calls or claims completion.
                            let remainder = decoded.message.content.strip_prefix(&spoken)
                                .ok_or_else(|| LlmError::Decode("Responses final text disagrees with streamed text".into()))?;
                            if !remainder.is_empty() { yield Delta::Text(remainder.into()); }
                            let summary = summaries(value)?;
                            let remainder = summary.strip_prefix(&thought)
                                .ok_or_else(|| LlmError::Decode("Responses final summary disagrees with streamed summary".into()))?;
                            if !remainder.is_empty() { yield Delta::Reasoning(remainder.into()); }
                            for block in decoded.message.reasoning { yield Delta::ReasoningDone(block); }
                            for call in decoded.message.tool_calls { yield Delta::ToolCall(call); }
                            yield Delta::ResponseMetadata {
                                usage_reported: value.pointer("/usage/input_tokens").and_then(Value::as_u64).is_some()
                                    && value.pointer("/usage/output_tokens").and_then(Value::as_u64).is_some(),
                                completion_confirmed: true,
                            };
                            yield Delta::Done { stop_reason: decoded.stop_reason, usage: decoded.usage, model: decoded.model };
                            finished = true;
                            break 'read;
                        }
                        // Item and argument deltas are bounded by received
                        // bytes. Complete calls come from terminal output only.
                        _ => {}
                    }
                }
            }
            if !finished {
                Err(LlmError::Decode("Responses stream ended before a terminal response".into()))?;
            }
        }))
    }
}

const REPLAY: &str = "rook_responses_output";

fn wire_request(model: &str, scope: &[u8; 32], request: &Request, stream: bool) -> Result<Value> {
    let mut input = Vec::new();
    // Keep every result in a batch before any synthetic image message. This
    // also works with gateways that only accept string function outputs.
    for message in crate::images::for_wire(&request.messages) {
        match message.role {
            Role::Tool => {
                let id = message
                    .tool_call_id
                    .as_deref()
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| LlmError::Decode("tool result has no call_id for Responses".into()))?;
                input.push(json!({"type":"function_call_output", "call_id":id, "output":message.content}));
            }
            Role::Assistant => {
                if let Some(items) = replay(&message, scope) {
                    input.extend(items.iter().cloned());
                    continue;
                }
                if !message.content.is_empty() {
                    input.push(json!({"role":"assistant", "content":message.content}));
                }
                for call in &message.tool_calls {
                    input.push(json!({"type":"function_call", "call_id":call.id,
                        "name":call.name, "arguments":call.arguments.to_string()}));
                }
            }
            Role::User | Role::System => {
                let mut content = vec![json!({"type":"input_text", "text":message.content})];
                content.extend(message.images.iter().map(|image| {
                    json!({"type":"input_image",
                    "image_url":format!("data:{};base64,{}", image.mime_type, image.data)})
                }));
                input.push(json!({"role":if message.role == Role::System { "system" } else { "user" }, "content":content}));
            }
        }
    }
    let mut body = json!({"model":model, "input":input, "stream":stream, "store":false,
        "max_output_tokens":request.max_output_tokens});
    if let Some(effort) = request.effort.and_then(|effort| super::reasoning_effort(model, effort)) {
        body["reasoning"] = json!({"effort":effort, "summary":"auto"});
    }
    if super::reasons(model) {
        // Accepted by older Responses implementations too. Current OpenAI
        // returns encrypted content automatically for store=false.
        body["include"] = json!(["reasoning.encrypted_content"]);
    } else {
        body["temperature"] = json!(request.temperature);
    }
    if !request.tools.is_empty() {
        body["tools"] = request
            .tools
            .iter()
            .map(|tool| {
                json!({"type":"function", "name":tool.name,
            "description":tool.description, "parameters":tool.parameters, "strict":false})
            })
            .collect();
    }
    Ok(body)
}

/// Replay only an unmodified assistant response. Prompt-tool adoption or any
/// later edit must not be undone by a stale provider snapshot.
fn replay<'a>(message: &'a Message, scope: &[u8; 32]) -> Option<&'a Vec<Value>> {
    for block in &message.reasoning {
        if !crate::catalog::matches_scope(block.get("rook_responses_scope"), scope) {
            continue;
        }
        if crate::stream::json_bytes(block).is_err() {
            continue;
        }
        let Some(items) = block.get(REPLAY).and_then(Value::as_array) else { continue };
        let Ok((text, calls, _)) = project(items) else { continue };
        if text == message.content
            && calls.len() == message.tool_calls.len()
            && calls
                .iter()
                .zip(&message.tool_calls)
                .all(|(a, b)| a.id == b.id && a.name == b.name && a.arguments == b.arguments)
        {
            return Some(items);
        }
    }
    None
}

fn string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| LlmError::Decode(format!("Responses object has no string {field}")))
}

fn output(value: &Value) -> Result<&Vec<Value>> {
    let items = value
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| LlmError::Decode("Responses reply has no output array".into()))?;
    if items.len() > 1024 {
        return Err(LlmError::Decode("Responses output exceeds 1024 items".into()));
    }
    Ok(items)
}

fn project(items: &[Value]) -> Result<(String, Vec<ToolCall>, bool)> {
    let mut text = String::new();
    let mut calls = Vec::new();
    let mut refusal = false;
    for item in items {
        match string(item, "type")? {
            "message" => {
                if item.get("role").and_then(Value::as_str) != Some("assistant") {
                    return Err(LlmError::Decode(
                        "Responses output message is not from the assistant".into(),
                    ));
                }
                let content = item
                    .get("content")
                    .and_then(Value::as_array)
                    .ok_or_else(|| LlmError::Decode("Responses message has no content array".into()))?;
                for part in content {
                    match string(part, "type")? {
                        "output_text" => text.push_str(string(part, "text")?),
                        "refusal" => {
                            refusal = true;
                            text.push_str(string(part, "refusal")?);
                        }
                        other => {
                            return Err(LlmError::Decode(format!(
                                "unsupported Responses output content: {other}"
                            )));
                        }
                    }
                }
            }
            "function_call" => {
                if calls.len() >= 256 {
                    return Err(LlmError::Decode("Responses reply exceeds 256 tool calls".into()));
                }
                if item.get("status").and_then(Value::as_str).is_some_and(|status| status != "completed") {
                    return Err(LlmError::Decode("Responses ended before a tool call completed".into()));
                }
                let id = string(item, "call_id")?;
                let name = string(item, "name")?;
                if id.is_empty() || name.is_empty() || calls.iter().any(|call: &ToolCall| call.id == id) {
                    return Err(LlmError::Decode(
                        "Responses tool call has an empty or duplicate identity".into(),
                    ));
                }
                calls.push(ToolCall {
                    id: id.into(),
                    name: name.into(),
                    arguments: crate::parse_arguments(string(item, "arguments")?),
                });
            }
            "reasoning" => {}
            other => return Err(LlmError::Decode(format!("unsupported Responses output item: {other}"))),
        }
    }
    Ok((text, calls, refusal))
}

fn summaries(value: &Value) -> Result<String> {
    let mut summary = String::new();
    for item in output(value)? {
        if item.get("type").and_then(Value::as_str) == Some("reasoning")
            && let Some(parts) = item.get("summary").and_then(Value::as_array)
        {
            for part in parts {
                if part.get("type").and_then(Value::as_str) == Some("summary_text") {
                    summary.push_str(string(part, "text")?);
                }
            }
        }
    }
    Ok(summary)
}

fn response_error(value: &Value) -> LlmError {
    let error = value.get("error").filter(|v| v.is_object()).unwrap_or(value);
    let code = error.get("code").and_then(Value::as_str).unwrap_or_default();
    let status = match code {
        "rate_limit_exceeded" => 429,
        "server_error" => 503,
        _ => 400,
    };
    LlmError::Status { status, retry_after: None, body: crate::truncate(&error.to_string(), 2000) }
}

fn decode(value: &Value, model: &str, scope: &[u8; 32]) -> Result<Response> {
    let status = string(value, "status")?;
    if status == "failed" || value.get("error").is_some_and(|error| !error.is_null()) {
        return Err(response_error(value));
    }
    let items = output(value)?;
    let (content, tool_calls, refusal) = project(items)?;
    let stop_reason = match status {
        "completed" if refusal => StopReason::Refusal,
        "completed" if !tool_calls.is_empty() => StopReason::ToolUse,
        "completed" => StopReason::EndTurn,
        "incomplete" if !tool_calls.is_empty() => {
            return Err(LlmError::Decode("incomplete Responses reply contained tool calls".into()));
        }
        "incomplete" => match value.pointer("/incomplete_details/reason").and_then(Value::as_str) {
            Some("max_output_tokens") => StopReason::MaxTokens,
            Some("content_filter") => StopReason::Refusal,
            _ => StopReason::Other,
        },
        _ => return Err(LlmError::Decode(format!("Responses reply is not terminal: {status}"))),
    };
    let usage = value.get("usage").unwrap_or(&Value::Null);
    let mut message = Message::assistant(content);
    message.tool_calls = tool_calls;
    // One provider-owned envelope preserves item IDs, order, annotations,
    // encrypted state and future fields without inventing signed content.
    message.reasoning.push(json!({REPLAY:items, "rook_responses_scope":scope}));
    Ok(Response {
        message,
        stop_reason,
        model: value.get("model").and_then(Value::as_str).unwrap_or(model).into(),
        usage: Usage {
            input_tokens: token_count(usage, "/input_tokens")?,
            output_tokens: token_count(usage, "/output_tokens")?,
            cache_read_tokens: token_count(usage, "/input_tokens_details/cached_tokens")?,
            cache_write_tokens: token_count(usage, "/input_tokens_details/cache_write_tokens")?,
        },
    })
}

fn token_count(usage: &Value, path: &str) -> Result<u32> {
    match usage.pointer(path) {
        None | Some(Value::Null) => Ok(0),
        Some(value) => value
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| LlmError::Decode(format!("Responses usage {path} is not a u32 token count"))),
    }
}
