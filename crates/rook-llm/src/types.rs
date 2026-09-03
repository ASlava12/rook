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
    /// A cache breakpoint may be placed at the end of this message. Providers
    /// without prompt caching ignore it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cache: bool,
    /// The model's own reasoning, as the provider gave it back and expects it
    /// returned: whole blocks, never read or rebuilt here.
    ///
    /// Anthropic signs them, and refuses the next request of a turn whose tool
    /// call arrives without the thinking that led to it. Kept verbatim because
    /// a signature covers bytes: anything reconstructed is a different block,
    /// and a field a later version adds would be dropped by anything that
    /// parsed rather than carried.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasoning: Vec<serde_json::Value>,
}

impl Message {
    pub fn system(text: impl Into<String>) -> Self {
        Self::of(Role::System, text)
    }
    pub fn user(text: impl Into<String>) -> Self {
        Self::of(Role::User, text)
    }
    pub fn assistant(text: impl Into<String>) -> Self {
        Self::of(Role::Assistant, text)
    }
    pub fn tool_result(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self { tool_call_id: Some(id.into()), ..Self::of(Role::Tool, text) }
    }

    fn of(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            content: text.into(),
            tool_calls: vec![],
            tool_call_id: None,
            cache: false,
            reasoning: Vec::new(),
        }
    }

    /// Mark this as the end of a stable prefix worth caching.
    pub fn cacheable(mut self) -> Self {
        self.cache = true;
        self
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
    /// The lean form used when tool schemas are loaded lazily: the first
    /// sentence, and each argument's name and type without its prose.
    ///
    /// Full schemas for a few dozen tools cost thousands of tokens on every
    /// single request, and on local models a tool-heavy prompt is an order of
    /// magnitude slower to process than plain text. Most of that weight is
    /// guidance — how to phrase a pattern, what a limit does — which only
    /// matters once the model has decided to use the tool.
    ///
    /// The argument *shape* stays, because a tool advertised without one cannot
    /// be called: the model would have to guess the names. This is why a tool
    /// description must open with a sentence that stands on its own.
    pub fn stub(&self) -> Self {
        let properties = self
            .parameters
            .get("properties")
            .and_then(|p| p.as_object())
            .map(|props| {
                props
                    .iter()
                    .map(|(name, schema)| {
                        let kind = schema.get("type").cloned().unwrap_or(serde_json::json!("string"));
                        (name.clone(), serde_json::json!({ "type": kind }))
                    })
                    .collect::<serde_json::Map<_, _>>()
            })
            .unwrap_or_default();

        let mut parameters = serde_json::json!({ "type": "object", "properties": properties });
        if let Some(required) = self.parameters.get("required") {
            parameters["required"] = required.clone();
        }
        Self {
            name: self.name.clone(),
            description: first_sentence(&self.description).to_string(),
            parameters,
        }
    }
}

/// Up to and including the first full stop that ends a sentence, meaning the
/// text after it does not continue in lower case and the word it closes is not
/// a dotted abbreviation. Both conditions are needed: `e.g. cargo` fails the
/// first, `v1.2. Then` the second, while `.gitignore. Output` and
/// ``a file. `old` `` are real boundaries. Cutting in the wrong place truncates
/// a stub, so the rule keeps too much rather than too little.
fn first_sentence(description: &str) -> &str {
    let ends_sentence = |i: &usize| {
        let word = description[..*i].rsplit(char::is_whitespace).next().unwrap_or("");
        let abbreviation = word.len() <= 4 && word.contains('.');
        !abbreviation && !description[i + 2..].starts_with(char::is_lowercase)
    };
    description
        .match_indices(". ")
        .find(|(i, _)| ends_sentence(i))
        .map(|(i, _)| &description[..=i])
        .unwrap_or(description)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// Read from the prompt cache instead of being processed again.
    #[serde(default)]
    pub cache_read_tokens: u32,
    #[serde(default)]
    pub cache_write_tokens: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Refusal,
    Other,
}

impl StopReason {
    /// One vocabulary for why a turn ended, reported by `session show`, by the
    /// delegation report and by `run --json`. The debug spelling reached all
    /// three as `EndTurn` beside a hand-written `end_turn`.
    pub fn as_str(self) -> &'static str {
        match self {
            StopReason::EndTurn => "end_turn",
            StopReason::ToolUse => "tool_use",
            StopReason::MaxTokens => "max_tokens",
            StopReason::Refusal => "refusal",
            StopReason::Other => "other",
        }
    }
}

/// How much thinking and overall token spend to allow.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    #[default]
    High,
    XHigh,
    Max,
}

/// Tool arguments as they arrive on the wire: a JSON string in two of the three
/// dialects.
///
/// Nothing here is fatal, because a model emitting invalid JSON is common — a
/// call cut off at the output limit produces it every time. But the three cases
/// are not the same, and collapsing them made the second read as the third: no
/// arguments is an empty object, and something that will not parse is `Null`, so
/// whoever dispatches can say which mistake was made instead of complaining
/// about the argument that is missing as a result.
pub fn parse_arguments(raw: &str) -> serde_json::Value {
    if raw.trim().is_empty() {
        return serde_json::json!({});
    }
    serde_json::from_str(raw).unwrap_or(serde_json::Value::Null)
}

impl Effort {
    pub const ALL: [Effort; 5] = [Effort::Low, Effort::Medium, Effort::High, Effort::XHigh, Effort::Max];

    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::XHigh => "xhigh",
            Effort::Max => "max",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text.trim().to_lowercase().as_str() {
            "low" => Effort::Low,
            "medium" => Effort::Medium,
            "high" => Effort::High,
            "xhigh" => Effort::XHigh,
            "max" => Effort::Max,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub messages: Vec<Message>,
    #[serde(default)]
    pub tools: Vec<ToolSpec>,
    pub max_output_tokens: u32,
    pub temperature: f32,
    /// Providers that have no notion of effort ignore it.
    #[serde(default)]
    pub effort: Option<Effort>,
    /// How long the cached prefix should be kept, where the provider offers a
    /// choice. Ignored by the ones that do not.
    #[serde(default)]
    pub cache_ttl: CacheTtl,
}

/// How long a cached prompt prefix is kept.
///
/// The trade is which side of a pause you pay on. A write costs more for the
/// longer one and a hit costs a tenth either way, so an hour is the better deal
/// exactly when a conversation outlives five minutes — which a person thinking
/// does and a scripted `rook run` does not, since its one turn never reads the
/// cache it wrote.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheTtl {
    #[default]
    #[serde(rename = "5m")]
    FiveMinutes,
    #[serde(rename = "1h")]
    OneHour,
}

impl CacheTtl {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text.trim().to_lowercase().as_str() {
            "5m" => CacheTtl::FiveMinutes,
            "1h" => CacheTtl::OneHour,
            _ => return None,
        })
    }
}

impl Request {
    pub fn new(messages: Vec<Message>) -> Self {
        Self {
            messages: joined_user_turns(messages),
            tools: Vec::new(),
            max_output_tokens: 4096,
            temperature: 0.0,
            effort: None,
            cache_ttl: CacheTtl::default(),
        }
    }
}

/// Fold consecutive user turns into one.
///
/// The agent produces them honestly: a compaction summary sits in front of the
/// first replayed message, and a loaded skill is announced beside the prompt
/// that caused it. Hosted APIs accept the pair, but a chat template on a
/// self-hosted server often does not — alternation is baked into the template
/// rather than checked at the edge, and this is aimed at local models. Two turns
/// with a blank line between them say the same thing to every one of them.
///
/// Tool results are left alone: the dialects have their own rules for those, and
/// merging a result into a prompt would lose the id it answers.
fn joined_user_turns(messages: Vec<Message>) -> Vec<Message> {
    let mut folded: Vec<Message> = Vec::with_capacity(messages.len());
    for message in messages {
        match folded.last_mut() {
            Some(last)
                if last.role == Role::User
                    && message.role == Role::User
                    && last.tool_call_id.is_none()
                    && message.tool_call_id.is_none() =>
            {
                last.content.push_str("\n\n");
                last.content.push_str(&message.content);
                // The marker belongs to the end of what is cached, and the end
                // has just moved.
                last.cache = message.cache;
            }
            _ => folded.push(message),
        }
    }
    folded
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

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(description: &str) -> ToolSpec {
        ToolSpec {
            name: "t".into(),
            description: description.into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "absolute, and it must exist" },
                    "lines": { "type": "integer", "minimum": 1, "description": "how many" }
                },
                "required": ["path"]
            }),
        }
    }

    #[test]
    fn a_stub_keeps_the_first_sentence_and_drops_the_argument_guidance() {
        let s = spec("Read a text file. Pass an absolute path. Binary files are refused.").stub();
        assert_eq!(s.description, "Read a text file.");
        assert_eq!(
            s.parameters["properties"],
            serde_json::json!({ "path": { "type": "string" }, "lines": { "type": "integer" } }),
            "the prose goes, the shape stays"
        );
        assert_eq!(s.parameters["required"], serde_json::json!(["path"]));
    }

    #[test]
    fn a_stub_without_the_argument_names_could_not_be_called() {
        let s = spec("Read a text file.").stub();
        let properties = s.parameters["properties"].as_object().unwrap();
        assert!(properties.contains_key("path"), "the model would have to guess the name");
        assert!(
            properties["path"].as_object().unwrap().len() == 1,
            "and nothing but the type survives: {:?}",
            properties["path"]
        );
    }

    #[test]
    fn a_one_sentence_description_survives_whole() {
        assert_eq!(spec("List a directory.").stub().description, "List a directory.");
        assert_eq!(spec("List a directory").stub().description, "List a directory");
    }

    #[test]
    fn an_abbreviation_does_not_cut_the_sentence_in_half() {
        let s = spec("Run a command, e.g. cargo test. Escapes are your own problem.").stub();
        assert_eq!(s.description, "Run a command, e.g. cargo test.");
        let s = spec("Pin v1.2. Then build.").stub();
        assert_eq!(s.description, "Pin v1.2. Then build.", "a version is not a sentence end either");
    }

    #[test]
    fn a_dotted_filename_and_a_code_span_are_still_sentence_boundaries() {
        let s = spec("List a directory, honouring .gitignore. Output is capped.").stub();
        assert_eq!(s.description, "List a directory, honouring .gitignore.");
        let s = spec("Replace an exact string. `old` must appear once.").stub();
        assert_eq!(s.description, "Replace an exact string.");
    }
}
