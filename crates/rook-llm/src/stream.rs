use std::pin::Pin;

use futures_util::Stream;

use crate::{Message, Response, Result, StopReason, ToolCall, Usage};

/// One piece of a response as it arrives.
///
/// Text arrives in fragments; tool calls do not. A half-parsed argument object
/// is useless to the caller and dangerous to act on, so a `ToolCall` is emitted
/// only once its arguments are complete.
#[derive(Clone, Debug)]
pub enum Delta {
    Text(String),
    Reasoning(String),
    /// A whole block of reasoning, as the provider will want it back. Text for
    /// a person is [`Delta::Reasoning`]; this is for the wire.
    ReasoningDone(serde_json::Value),
    ToolCall(ToolCall),
    Done {
        stop_reason: StopReason,
        usage: Usage,
        model: String,
    },
}

pub type ResponseStream = Pin<Box<dyn Stream<Item = Result<Delta>> + Send>>;

/// Reassembles a [`ResponseStream`] into the same [`Response`] a non-streaming
/// call would have produced, so a caller that does not care about deltas does
/// not have to handle them.
#[derive(Default)]
pub struct Assembler {
    bytes: usize,
    text: String,
    reasoning: String,
    reasoning_blocks: Vec<serde_json::Value>,
    tool_calls: Vec<ToolCall>,
    finished: Option<(StopReason, Usage, String)>,
}

impl Assembler {
    pub fn push(&mut self, delta: Delta) -> Result<()> {
        let bytes = match &delta {
            Delta::Text(t) | Delta::Reasoning(t) => t.len(),
            Delta::ReasoningDone(block) => json_bytes(block)?,
            Delta::ToolCall(call) => {
                call.id.len().saturating_add(call.name.len()).saturating_add(json_bytes(&call.arguments)?)
            }
            Delta::Done { model, .. } => model.len(),
        };
        self.bytes = self.bytes.saturating_add(bytes);
        if self.bytes > crate::MOST_REPLY_BYTES
            || (matches!(&delta, Delta::ToolCall(_)) && self.tool_calls.len() >= 256)
            || (matches!(&delta, Delta::ReasoningDone(_)) && self.reasoning_blocks.len() >= 1024)
        {
            return Err(crate::LlmError::Decode(
                "the reply passed its byte or block limit — the provider is not ending the stream".into(),
            ));
        }
        match delta {
            Delta::Text(t) => self.text.push_str(&t),
            Delta::Reasoning(t) => self.reasoning.push_str(&t),
            Delta::ReasoningDone(block) => self.reasoning_blocks.push(block),
            Delta::ToolCall(c) => self.tool_calls.push(c),
            Delta::Done { stop_reason, usage, model } => self.finished = Some((stop_reason, usage, model)),
        }
        Ok(())
    }

    pub fn reasoning(&self) -> &str {
        &self.reasoning
    }

    pub fn finish(self) -> Response {
        let (stop_reason, usage, model) = self.finished.unwrap_or((
            if self.tool_calls.is_empty() { StopReason::EndTurn } else { StopReason::ToolUse },
            Usage::default(),
            String::new(),
        ));
        Response {
            message: Message {
                role: crate::Role::Assistant,
                content: self.text,
                tool_calls: self.tool_calls,
                tool_call_id: None,
                cache: false,
                reasoning: self.reasoning_blocks,
            },
            stop_reason,
            usage,
            model,
        }
    }
}

/// Accumulates OpenAI-style `tool_calls` deltas, which arrive as fragments
/// indexed by position, with the name in the first fragment and the arguments
/// spread across the rest.
#[derive(Default)]
pub struct ToolCallBuffer {
    bytes: usize,
    slots: Vec<Option<(String, String, String)>>,
}

impl ToolCallBuffer {
    pub fn push(&mut self, index: usize, id: Option<&str>, name: Option<&str>, args: &str) -> Result<()> {
        let added =
            args.len().saturating_add(id.map_or(0, str::len)).saturating_add(name.map_or(0, str::len));
        self.bytes = self.bytes.saturating_add(added);
        if index >= 256 || self.bytes > crate::MOST_REPLY_BYTES {
            return Err(crate::LlmError::Decode(
                "tool call index or arguments exceed the response limit".into(),
            ));
        }
        if self.slots.len() <= index {
            self.slots.resize_with(index + 1, Default::default);
        }
        let slot = self.slots[index].get_or_insert_with(Default::default);
        // Empty is not an update. Some gateways repeat the `id` and `name` keys
        // on every continuation chunk with nothing in them, and taking those at
        // face value wipes the name — after which `drain` discards the call as
        // nameless and the model's tool call has silently not happened.
        if let Some(id) = id.filter(|id| !id.is_empty()) {
            slot.0 = id.to_string();
        }
        // The other direction is real too: a name that arrives in a later chunk
        // than the index it belongs to has to be taken when it does.
        if let Some(name) = name.filter(|name| !name.is_empty()) {
            slot.1 = name.to_string();
        }
        slot.2.push_str(args);
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Emit the completed calls. Arguments that failed to parse become `null`
    /// rather than dropping the call: a tool that rejects bad input gives the
    /// model something to correct, while a silently missing call does not.
    pub fn drain(&mut self) -> Result<Vec<ToolCall>> {
        self.bytes = 0;
        self.slots
            .drain(..)
            .flatten()
            .map(|(id, name, args)| {
                if name.is_empty() {
                    return Err(crate::LlmError::Decode("provider returned a tool call with no name".into()));
                }
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                let id = if id.is_empty() {
                    format!("streamed-{}", NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
                } else {
                    id
                };
                Ok(ToolCall {
                    id,
                    name,
                    arguments: serde_json::from_str(&args).unwrap_or(serde_json::Value::Null),
                })
            })
            .collect()
    }
}

fn json_bytes(value: &serde_json::Value) -> Result<usize> {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            if self.0 > crate::MOST_REPLY_BYTES {
                return Err(std::io::Error::other("reply is too large"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    serde_json::to_writer(&mut count, value).map_err(|e| crate::LlmError::Decode(e.to_string()))?;
    Ok(count.0)
}
