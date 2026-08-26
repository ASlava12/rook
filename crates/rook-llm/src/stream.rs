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
    ToolCall(ToolCall),
    Done { stop_reason: StopReason, usage: Usage, model: String },
}

pub type ResponseStream = Pin<Box<dyn Stream<Item = Result<Delta>> + Send>>;

/// Reassembles a [`ResponseStream`] into the same [`Response`] a non-streaming
/// call would have produced, so a caller that does not care about deltas does
/// not have to handle them.
#[derive(Default)]
pub struct Assembler {
    text: String,
    reasoning: String,
    tool_calls: Vec<ToolCall>,
    finished: Option<(StopReason, Usage, String)>,
}

impl Assembler {
    pub fn push(&mut self, delta: Delta) {
        match delta {
            Delta::Text(t) => self.text.push_str(&t),
            Delta::Reasoning(t) => self.reasoning.push_str(&t),
            Delta::ToolCall(c) => self.tool_calls.push(c),
            Delta::Done { stop_reason, usage, model } => self.finished = Some((stop_reason, usage, model)),
        }
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
    slots: Vec<(String, String, String)>,
}

impl ToolCallBuffer {
    pub fn push(&mut self, index: usize, id: Option<&str>, name: Option<&str>, args: &str) {
        if self.slots.len() <= index {
            self.slots.resize_with(index + 1, Default::default);
        }
        let slot = &mut self.slots[index];
        if let Some(id) = id {
            slot.0 = id.to_string();
        }
        if let Some(name) = name {
            slot.1 = name.to_string();
        }
        slot.2.push_str(args);
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Emit the completed calls. Arguments that failed to parse become `null`
    /// rather than dropping the call: a tool that rejects bad input gives the
    /// model something to correct, while a silently missing call does not.
    pub fn drain(&mut self) -> Vec<ToolCall> {
        self.slots
            .drain(..)
            .filter(|(_, name, _)| !name.is_empty())
            .map(|(id, name, args)| ToolCall {
                id,
                name,
                arguments: serde_json::from_str(&args).unwrap_or(serde_json::Value::Null),
            })
            .collect()
    }
}
