use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// Present on assistant messages that requested tools.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Present on tool messages, matching the call being answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn system(text: impl Into<String>) -> Self {
        Self { role: Role::System, content: text.into(), tool_calls: vec![], tool_call_id: None }
    }
    pub fn user(text: impl Into<String>) -> Self {
        Self { role: Role::User, content: text.into(), tool_calls: vec![], tool_call_id: None }
    }
    pub fn assistant(text: impl Into<String>) -> Self {
        Self { role: Role::Assistant, content: text.into(), tool_calls: vec![], tool_call_id: None }
    }
    pub fn tool_result(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self { role: Role::Tool, content: text.into(), tool_calls: vec![], tool_call_id: Some(id.into()) }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// JSON arguments as the model produced them.
    pub arguments: serde_json::Value,
}

/// A tool as advertised to the model.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments.
    pub parameters: serde_json::Value,
}

impl ToolSpec {
    /// The one-line form used when tool schemas are loaded lazily: name and
    /// description only, deferring the schema until the model asks for it.
    ///
    /// Full schemas for a few dozen tools cost thousands of tokens on every
    /// single request, and on local models a tool-heavy prompt is an order of
    /// magnitude slower to process than plain text.
    pub fn stub(&self) -> Self {
        Self {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: serde_json::json!({ "type": "object", "properties": {} }),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Refusal,
    Other,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub messages: Vec<Message>,
    #[serde(default)]
    pub tools: Vec<ToolSpec>,
    pub max_output_tokens: u32,
    pub temperature: f32,
}

impl Request {
    pub fn new(messages: Vec<Message>) -> Self {
        Self { messages, tools: Vec::new(), max_output_tokens: 4096, temperature: 0.0 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Response {
    pub message: Message,
    pub stop_reason: StopReason,
    pub usage: Usage,
    pub model: String,
}

/// A model the provider says it can serve.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default)]
    pub owned_by: Option<String>,
    /// Reported context length, where the endpoint gives one. Most do not.
    #[serde(default)]
    pub context_window: Option<usize>,
}
