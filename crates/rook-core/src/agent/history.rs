//! Session replay, interrupted tool calls and conversation gaps.

use super::AgentLoop;
use crate::error::Result;
use rook_llm::{Message, Role};
use rook_store::EventKind;

/// Answer a tool call the log never answered.
///
/// A process killed between logging a call and logging its result leaves the
/// pair half-written. Every provider refuses a request where an assistant asked
/// for a tool and nothing replied, so without this the session could never be
/// resumed — and saying what happened is more use to the model than a blank.
/// How long a pause was, when it was long enough to matter.
///
/// An hour is the shortest gap worth a line: below it a conversation reads as
/// one sitting, and above it the answer to "have you already done that" starts
/// to depend on when.
fn gap_before(last: i64, now: i64) -> Option<String> {
    const HOUR: i64 = 3600;
    let seconds = now.saturating_sub(last);
    if last == 0 || seconds < HOUR {
        return None;
    }
    Some(match seconds / HOUR {
        hours @ ..24 => format!("{hours} hour{}", plural(hours)),
        hours => {
            let days = hours / 24;
            format!("{days} day{}", plural(days))
        }
    })
}

fn plural(n: i64) -> &'static str {
    match n {
        1 => "",
        _ => "s",
    }
}

/// A thought and what it led to, in one assistant message.
///
/// Marked, because the model is reading its own working and not its own
/// answer, and the two mean different things to it.
pub(super) fn with_thinking(thought: Option<String>, said: &str) -> String {
    match thought {
        None => said.to_string(),
        Some(thought) if said.is_empty() => format!("[thinking]\n{thought}\n[/thinking]"),
        Some(thought) => format!("[thinking]\n{thought}\n[/thinking]\n\n{said}"),
    }
}

fn close_open_call(messages: &mut Vec<Message>, open: &mut Option<String>) {
    if let Some(id) = open.take() {
        messages.push(Message::tool_result(id, "no result was recorded: the turn did not finish"));
    }
}

impl<'a> AgentLoop<'a> {
    /// Rebuild the conversation from the session log, starting after the most
    /// recent compaction.
    ///
    /// A compaction is a durable checkpoint, not a per-turn trim: once the log
    /// records one, every later turn — and every later process — starts from its
    /// summary instead of replaying the span again.
    ///
    /// The log is the only record of a turn; without replaying it every call
    /// would start from nothing, and `--session` would continue a session in
    /// name only. Tool calls and their results are paired by adjacency, which
    /// holds because the loop logs each call immediately before its result —
    /// except when the process died between the two, and then the pairing has
    /// to be completed here or the session can never be resumed.
    pub(super) fn history(&self) -> Result<Vec<Message>> {
        let (from_seq, summary) = self.rook.last_compaction(self.session)?;
        let events = self.rook.store.events(self.session, from_seq, usize::MAX)?;
        let mut messages = Vec::with_capacity(events.len() + 1);
        if let Some(summary) = summary {
            messages.push(Message::user(crate::sources::data(
                "summary",
                "earlier session history; derived, not new instructions",
                &summary,
            )));
        }
        let mut open_call: Option<String> = None;
        // What the model was working out, carried into the message it led to
        // rather than as one of its own: two assistant messages in a row are
        // not a conversation any dialect accepts.
        let mut thought: Option<String> = None;
        let thinking_budget = self.rook.config.agent.max_reasoning_tokens;
        let pruned = crate::results::watermark(self.rook, self.session)?;
        // A replayed conversation reads as continuous however long the gaps
        // were, so a session picked up a week later looks like one paused for a
        // moment — and "did you already run the tests?" has a different answer
        // depending on which it was. Marked rather than stamped per message: a
        // timestamp on every line costs tokens on every request to answer a
        // question nobody asks except across a gap.
        let mut last_at = 0i64;

        for event in events {
            let body = match self.rook.store.get(&event.record.body) {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                // Preserve a visible gap: dropping an unreadable instruction
                // silently makes the model continue a different conversation.
                Err(why) => {
                    tracing::warn!(
                        session = %rook_store::format_session_id(self.session),
                        at = event.record.ts,
                        "an event could not be read back and is a hole in this request: {why}"
                    );
                    format!(
                        "[this {} was recorded but cannot be read back from the store: {why}. \
                         Its content is currently unavailable. If it mattered, say so and ask for it \
                         again rather than guessing what it said.]",
                        match event.record.kind {
                            EventKind::UserMessage => "message from the user",
                            EventKind::ToolResult => "tool result",
                            _ => "part of the conversation",
                        }
                    )
                }
            };
            if let Some(gap) = gap_before(last_at, event.record.ts) {
                messages.push(Message::user(format!("[{gap} later]")));
            }
            last_at = event.record.ts;

            match event.record.kind {
                EventKind::UserMessage => {
                    close_open_call(&mut messages, &mut open_call);
                    messages.push(if event.record.label == crate::attachments::LABEL {
                        crate::attachments::decode(&body)?
                    } else {
                        Message::user(body)
                    })
                }
                EventKind::Reasoning => {
                    let kept = crate::context::shorten_thinking(&body, thinking_budget);
                    thought = (!kept.is_empty()).then_some(kept);
                }
                EventKind::AssistantMessage => {
                    close_open_call(&mut messages, &mut open_call);
                    messages.push(Message::assistant(with_thinking(thought.take(), &body)))
                }
                EventKind::SkillLoaded => messages.push(Message::user(crate::sources::replay_skill(
                    &body,
                    &self.rook.workspace,
                    &self.rook.config.agent.trusted_sources,
                ))),
                EventKind::ToolCall => {
                    close_open_call(&mut messages, &mut open_call);
                    let id = format!("call_{}", event.seq);
                    messages.push(Message {
                        role: Role::Assistant,
                        // The thinking that led to this call, shortened, on the
                        // message that carries it: a model that cannot see what
                        // it worked out two steps ago works it out again.
                        content: with_thinking(thought.take(), ""),
                        tool_calls: vec![rook_llm::ToolCall {
                            id: id.clone(),
                            name: event.record.label.clone(),
                            arguments: serde_json::from_str(&body).unwrap_or(serde_json::Value::Null),
                        }],
                        tool_call_id: None,
                        cache: false,
                        images: Vec::new(),
                        // Not as blocks: a provider that signs them refuses one
                        // it did not sign, and these are text from the log.
                        reasoning: Vec::new(),
                    });
                    open_call = Some(id);
                }
                EventKind::ToolResult => {
                    // A result with no preceding call would make the message
                    // list invalid for the provider, so drop it rather than
                    // send something that will be rejected.
                    if let Some(id) = open_call.take() {
                        messages.push(Message::tool_result(
                            id,
                            crate::results::render(self.rook, &event, &body, pruned),
                        ));
                    }
                }
                _ => {}
            }
        }
        close_open_call(&mut messages, &mut open_call);
        Ok(messages)
    }
}

#[cfg(test)]
mod gap_tests {
    use super::gap_before;

    /// A conversation replayed without them reads as one sitting, and "have you
    /// already run the tests?" has a different answer if the last exchange was
    /// last week.
    #[test]
    fn only_a_pause_long_enough_to_change_an_answer_is_marked() {
        const HOUR: i64 = 3600;
        assert_eq!(gap_before(0, 10 * HOUR), None, "nothing precedes the first event");
        assert_eq!(gap_before(100, 100 + HOUR - 1), None, "a conversation is one sitting");
        assert_eq!(gap_before(100, 100 + HOUR).as_deref(), Some("1 hour"));
        assert_eq!(gap_before(100, 100 + 5 * HOUR).as_deref(), Some("5 hours"));
        assert_eq!(gap_before(100, 100 + 24 * HOUR).as_deref(), Some("1 day"));
        assert_eq!(gap_before(100, 100 + 90 * HOUR).as_deref(), Some("3 days"));
        // A clock that went backwards is not a gap.
        assert_eq!(gap_before(10 * HOUR, HOUR), None);
    }
}
