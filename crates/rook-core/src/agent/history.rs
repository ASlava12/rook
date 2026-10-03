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
        replay(self.rook, self.session)
    }

    pub(super) fn history_with_sources(
        &self,
        sources: &mut crate::context::SourceManifest,
    ) -> Result<Vec<Message>> {
        replay_inner(self.rook, self.session, Some(sources))
    }
}

/// The same replay feeds requests and context reports, including provider state,
/// interrupted calls and image/result pruning.
pub(crate) fn replay(rook: &crate::Rook, session: u128) -> Result<Vec<Message>> {
    replay_inner(rook, session, None)
}

fn replay_inner(
    rook: &crate::Rook,
    session: u128,
    mut sources: Option<&mut crate::context::SourceManifest>,
) -> Result<Vec<Message>> {
    let (from_seq, summary) = rook.last_compaction(session)?;
    let events = rook.store.events(session, from_seq, usize::MAX)?;
    let mut messages = Vec::with_capacity(events.len() + 1);
    if let Some(summary) = summary {
        messages.push(Message::user(crate::sources::data(
            "summary",
            "earlier session history; derived, not new instructions",
            &summary,
        )));
    }
    let mut open_call: Option<String> = None;
    let mut batch: Option<crate::provider_history::Batch> = None;
    // What the model was working out, carried into the message it led to
    // rather than as one of its own: two assistant messages in a row are
    // not a conversation any dialect accepts.
    let mut thought: Option<String> = None;
    let thinking_budget = rook.config.agent.max_reasoning_tokens;
    let pruned = crate::results::watermark(rook, session)?;
    // A replayed conversation reads as continuous however long the gaps
    // were, so a session picked up a week later looks like one paused for a
    // moment — and "did you already run the tests?" has a different answer
    // depending on which it was. Marked rather than stamped per message: a
    // timestamp on every line costs tokens on every request to answer a
    // question nobody asks except across a gap.
    let mut last_at = 0i64;

    for event in events {
        if event.record.kind == EventKind::Note && event.record.label == super::repetition::DIAGNOSTIC {
            continue;
        }
        if event.record.kind == EventKind::Note && event.record.label == crate::model_route::LABEL {
            continue;
        }
        if event.record.kind == EventKind::Note && event.record.label == crate::model_route::AUX_LABEL {
            continue;
        }
        if event.record.kind == EventKind::Note
            && (event.record.label == crate::model_delegation::LABEL
                || event.record.label == crate::model_attempt::LABEL
                || event.record.label == "branch summary usage")
        {
            continue;
        }
        if event.record.kind == EventKind::Note && event.record.label == crate::phase_routing::LABEL {
            continue;
        }
        if event.record.kind == EventKind::Note && event.record.label == crate::context::REQUEST_CATALOG_LABEL
        {
            continue;
        }
        if event.record.kind == EventKind::Note && event.record.label == crate::tool_images::LABEL {
            continue;
        }
        if event.record.kind == EventKind::Note && event.record.label == crate::provider_history::LABEL {
            close_open_call(&mut messages, &mut open_call);
            if let Some(old) = batch.take() {
                old.finish(&mut messages);
            }
            let message = crate::provider_history::load(rook, &event)?;
            batch = Some(crate::provider_history::Batch::new(event.seq, &message));
            messages.push(message);
            thought = None;
            continue;
        }
        if event.record.kind == EventKind::Note && event.record.label == crate::provider_history::CALL {
            if let Some(batch) = &mut batch {
                batch.bind(rook, &event)?;
            }
            continue;
        }
        if event.record.kind == EventKind::AssistantMessage
            && batch.as_ref().is_some_and(|batch| event.seq == batch.seq + 1)
        {
            continue;
        }
        if matches!(
            event.record.kind,
            EventKind::UserMessage | EventKind::AssistantMessage | EventKind::Reasoning
        ) && let Some(old) = batch.take()
        {
            old.finish(&mut messages);
        }
        if event.record.kind == EventKind::Note
            && event.record.label == crate::branches::SUMMARY_LABEL
            && rook
                .store
                .stat_object(&event.record.body)?
                .is_some_and(|size| size.size_raw > (crate::branches::SUMMARY_BYTES + 1024) as u64)
        {
            return Err(crate::CoreError::Other("branch summary record exceeds its byte limit".into()));
        }
        let (body, body_readable) = match rook.store.get(&event.record.body) {
            Ok(bytes) => (String::from_utf8_lossy(&bytes).into_owned(), true),
            // Preserve a visible gap: dropping an unreadable instruction
            // silently makes the model continue a different conversation.
            Err(why) => {
                tracing::warn!(
                    session = %rook_store::format_session_id(session),
                    at = event.record.ts,
                    "an event could not be read back and is a hole in this request: {why}"
                );
                (
                    format!(
                        "[this {} was recorded but cannot be read back from the store: {why}. \
                         Its content is currently unavailable. If it mattered, say so and ask for it \
                         again rather than guessing what it said.]",
                        match event.record.kind {
                            EventKind::UserMessage => "message from the user",
                            EventKind::ToolResult => "tool result",
                            _ => "part of the conversation",
                        }
                    ),
                    false,
                )
            }
        };
        if let Some(gap) = gap_before(last_at, event.record.ts) {
            let message = Message::user(format!("[{gap} later]"));
            if let Some(batch) = &mut batch {
                batch.auxiliary.push(message);
            } else {
                messages.push(message);
            }
        }
        last_at = event.record.ts;

        match event.record.kind {
            EventKind::Note if event.record.label == crate::branches::SUMMARY_LABEL => {
                close_open_call(&mut messages, &mut open_call);
                messages.push(Message::user(crate::branches::replay_summary(&body)?));
            }
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
            EventKind::SkillLoaded => {
                let body =
                    crate::sources::replay_skill(&body, &rook.workspace, &rook.config.agent.trusted_sources);
                if body_readable && let Some(sources) = sources.as_deref_mut() {
                    sources.add_loaded_skill(&event.record.label, &body);
                }
                let message = Message::user(body);
                if let Some(batch) = &mut batch {
                    batch.auxiliary.push(message);
                } else {
                    messages.push(message);
                }
            }
            EventKind::ToolCall => {
                if batch.as_ref().is_some_and(|batch| batch.selected(&event.record.label).is_some()) {
                    continue;
                }
                if let Some(old) = batch.take() {
                    old.finish(&mut messages);
                }
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
                if let Some(batch) = &mut batch
                    && let Some(id) = batch.selected(&event.record.label)
                {
                    let mut message =
                        Message::tool_result(id, crate::results::render(rook, &event, &body, pruned));
                    if !pruned.is_some_and(|seq| event.seq <= seq) {
                        message.images = crate::tool_images::load(rook, &event)?;
                    }
                    batch.result(message);
                    continue;
                }
                // A result with no preceding call would make the message
                // list invalid for the provider, so drop it rather than
                // send something that will be rejected.
                if let Some(id) = open_call.take() {
                    let mut message =
                        Message::tool_result(id, crate::results::render(rook, &event, &body, pruned));
                    if !pruned.is_some_and(|seq| event.seq <= seq) {
                        message.images = crate::tool_images::load(rook, &event)?;
                    }
                    let has_images = !message.images.is_empty();
                    messages.push(message);
                    if has_images {
                        crate::tool_images::bound(&mut messages);
                    }
                }
            }
            _ => {}
        }
    }
    close_open_call(&mut messages, &mut open_call);
    if let Some(old) = batch {
        old.finish(&mut messages);
    }
    Ok(messages)
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
