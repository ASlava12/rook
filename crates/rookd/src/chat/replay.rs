//! A bounded recent view. The durable session remains the complete record.
use rook_proto::ChatEvent;
use std::collections::VecDeque;

struct Entry {
    sequence: u64,
    event: ChatEvent,
    bytes: usize,
    shortened: bool,
}

pub(super) struct Replay {
    entries: VecDeque<Entry>,
    state: VecDeque<Entry>,
    bytes: usize,
    max_events: usize,
    max_bytes: usize,
    max_frame: usize,
    sequence: u64,
    truncated: bool,
    finished: bool,
    /// Latest goal identity survives transcript eviction for an attached view.
    goal_generation: Option<Option<String>>,
    turn_id: Option<String>,
}

impl Default for Replay {
    fn default() -> Self {
        let config = rook_core::Config::default().server;
        Self::new(config.chat_replay_events, config.chat_replay_bytes, config.chat_queue_bytes)
    }
}

impl Replay {
    pub(super) fn new(events: usize, bytes: usize, frame_bytes: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            state: VecDeque::new(),
            bytes: 0,
            max_events: events.clamp(1, 4096),
            max_bytes: bytes.clamp(4096, 32 * 1024 * 1024),
            max_frame: frame_bytes.clamp(4096, 32 * 1024 * 1024).min(bytes.clamp(4096, 32 * 1024 * 1024)),
            sequence: 0,
            truncated: false,
            finished: false,
            goal_generation: None,
            turn_id: None,
        }
    }

    pub(super) fn push(&mut self, mut event: ChatEvent) -> u64 {
        self.sequence = self.sequence.wrapping_add(1);
        let sequence = self.sequence;
        if self.finished {
            return sequence;
        }
        if let ChatEvent::Goal { generation } = &event {
            self.goal_generation = Some(generation.as_ref().filter(|g| g.len() <= 64).cloned());
        }
        if let ChatEvent::Turn { id } = &event {
            self.turn_id = (id.len() <= 64).then(|| id.clone());
        }
        let kind = state_kind(&event);
        if let Some(kind) = kind
            && let Some(index) = self.state.iter().position(|entry| state_kind(&entry.event) == Some(kind))
            && let Some(old) = self.state.remove(index)
        {
            self.bytes -= old.bytes;
        }
        let terminal =
            matches!(event, ChatEvent::Done { .. } | ChatEvent::Failed { .. } | ChatEvent::Cancelled);
        let mut bytes = super::delivery::encoded_size(&event, self.max_frame);
        let shortened = bytes.is_none();
        if bytes.is_none() {
            self.truncated = true;
            // Keep the terminal outcome even when its detailed answer cannot
            // fit in a live view. The complete answer stays in durable history.
            match &mut event {
                ChatEvent::Done {
                    reply,
                    delegated,
                    decisions,
                    open_questions,
                    files_changed,
                    stopped,
                    ..
                } => {
                    *reply = None;
                    *delegated = Vec::new();
                    *decisions = Vec::new();
                    *open_questions = Vec::new();
                    *files_changed = Vec::new();
                    *stopped = stopped.chars().take(256).collect();
                }
                ChatEvent::Failed { message } => {
                    *message = message.chars().take(256).collect::<String>()
                        + " … [error shortened for live replay]";
                }
                _ => return sequence,
            }
            bytes = super::delivery::encoded_size(&event, self.max_frame);
        }
        let Some(bytes) = bytes else { return sequence };
        while self.entries.len() + self.state.len() >= self.max_events || self.bytes > self.max_bytes - bytes
        {
            let old = self.entries.pop_front().or_else(|| {
                // Preserve current metrics over another transcript chunk. A
                // terminal outcome or newer state can displace older state.
                if terminal || kind.is_some() { self.state.pop_front() } else { None }
            });
            self.truncated = true;
            let Some(old) = old else { return sequence };
            self.bytes -= old.bytes;
        }
        let entry = Entry { sequence, event, bytes, shortened };
        if kind.is_some() {
            self.state.push_back(entry);
        } else {
            self.entries.push_back(entry);
        }
        self.bytes += bytes;
        self.finished = terminal;
        sequence
    }

    pub(super) fn get(&self, sequence: u64) -> Option<ChatEvent> {
        self.entries
            .iter()
            .rev()
            .chain(self.state.iter())
            .find(|entry| entry.sequence == sequence)
            // A live subscriber also needs the partial-view flag when a
            // terminal payload was shortened. Rejoin emits it before Done.
            .filter(|entry| !entry.shortened)
            .map(|entry| entry.event.clone())
    }

    pub(super) fn snapshot(&self) -> (Vec<ChatEvent>, bool) {
        let mut events: Vec<_> = self.entries.iter().map(|entry| entry.event.clone()).collect();
        let terminal = if self.finished { events.pop() } else { None };
        // Started resets frontend counters. Restore current counters after
        // replaying history, but before the terminal event lets a CLI exit.
        events.extend(self.state.iter().map(|entry| entry.event.clone()));
        if let Some(generation) = &self.goal_generation {
            events.push(ChatEvent::Goal { generation: generation.clone() });
        }
        if let Some(id) = &self.turn_id {
            events.push(ChatEvent::Turn { id: id.clone() });
        }
        if let Some(terminal) = terminal {
            events.push(terminal);
        }
        (events, self.truncated)
    }
}

// Fixed state keys, not an ever-growing map of tools or model ids.
fn state_kind(event: &ChatEvent) -> Option<u8> {
    match event {
        ChatEvent::Context { .. } => Some(0),
        ChatEvent::Spent { .. } => Some(1),
        ChatEvent::Step { .. } => Some(2),
        ChatEvent::ModelRequest { .. } => Some(3),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> ChatEvent {
        ChatEvent::Text { text: value.into() }
    }

    #[test]
    fn latest_goal_identity_survives_replay_eviction_and_completion() {
        let mut replay = Replay::new(2, 4096, 4096);
        replay.push(ChatEvent::Goal { generation: Some("first".into()) });
        replay.push(ChatEvent::Goal { generation: Some("second".into()) });
        for _ in 0..8 {
            replay.push(text("output"));
        }
        let (events, truncated) = replay.snapshot();
        assert!(truncated);
        assert!(matches!(events.last(), Some(ChatEvent::Goal { generation: Some(g) }) if g == "second"));
        replay.push(ChatEvent::Goal { generation: None });
        let (events, _) = replay.snapshot();
        assert!(matches!(events.last(), Some(ChatEvent::Goal { generation: None })));
    }

    #[test]
    fn latest_ordinary_turn_identity_survives_replay_eviction() {
        let mut replay = Replay::new(2, 4096, 4096);
        replay.push(ChatEvent::Turn { id: "first".into() });
        replay.push(ChatEvent::Turn { id: "second".into() });
        for _ in 0..8 {
            replay.push(text("output"));
        }
        let (events, truncated) = replay.snapshot();
        assert!(truncated);
        assert!(matches!(events.last(), Some(ChatEvent::Turn { id }) if id == "second"));
        assert!(replay.turn_id.as_ref().is_some_and(|id| id == "second"));
    }

    #[test]
    fn replay_evicts_before_accepting_bytes_and_marks_missing_history() {
        let mut replay = Replay::new(8, 4096, 4096);
        let value = "\n".repeat(1100);
        let size = serde_json::to_vec(&text(&value)).unwrap().len();
        assert!(size < 4096 && size * 2 > 4096);
        let first = replay.push(text(&value));
        let second = replay.push(text(&value));
        assert!(replay.get(first).is_none());
        assert!(replay.get(second).is_some());
        assert_eq!(replay.bytes, size);
        let (events, truncated) = replay.snapshot();
        assert!(truncated);
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn event_count_and_oversized_payloads_cannot_grow_the_replay() {
        let mut replay = Replay::new(2, 4096, 4096);
        let first = replay.push(text("one"));
        replay.push(text("two"));
        replay.push(text("three"));
        assert_eq!(replay.entries.len(), 2);
        assert!(replay.get(first).is_none());
        let oversized = replay.push(text(&"x".repeat(4097)));
        assert!(replay.get(oversized).is_none());
        assert_eq!(replay.entries.len(), 2);
        assert!(replay.snapshot().1);
    }

    #[test]
    fn an_oversized_terminal_error_still_ends_the_recovered_view() {
        let mut replay = Replay::new(2, 4096, 4096);
        let terminal = replay.push(ChatEvent::Failed { message: "я".repeat(4096) });
        assert!(replay.bytes <= 4096);
        assert!(replay.get(terminal).is_none(), "live readers must receive the partial snapshot marker");
        let (events, truncated) = replay.snapshot();
        let Some(ChatEvent::Failed { message }) = events.last() else { panic!("terminal outcome lost") };
        assert!(message.contains("error shortened for live replay"));
        assert!(truncated);
    }

    #[test]
    fn current_metrics_survive_transcript_eviction_and_terminal_state_survives_late_progress() {
        let mut replay = Replay::new(3, 4096, 4096);
        replay.push(ChatEvent::Context { used: 120, size: 1000 });
        replay.push(ChatEvent::Spent { input_tokens: 500, output_tokens: 90, cached_tokens: 400 });
        for n in 0..20 {
            replay.push(text(&n.to_string()));
        }
        let (events, truncated) = replay.snapshot();
        assert!(truncated);
        assert_eq!(events.len(), 3);
        assert!(events.iter().any(|e| matches!(e, ChatEvent::Context { used: 120, .. })));
        assert!(events.iter().any(|e| matches!(e, ChatEvent::Spent { cached_tokens: 400, .. })));
        let terminal = replay.push(ChatEvent::Cancelled);
        replay.push(ChatEvent::Step { at: 100, of: 200 });
        assert!(replay.get(terminal).is_some());
        assert!(matches!(replay.snapshot().0.last(), Some(ChatEvent::Cancelled)));
        assert!(replay.bytes <= 4096);
    }

    #[test]
    fn restored_counters_follow_the_start_marker_and_precede_the_ending() {
        let mut replay = Replay::new(8, 4096, 4096);
        replay.push(ChatEvent::Started { session: "session".into() });
        replay.push(ChatEvent::Context { used: 15, size: 100 });
        replay.push(text("answer"));
        replay.push(ChatEvent::Cancelled);
        let (events, _) = replay.snapshot();
        assert!(matches!(events[0], ChatEvent::Started { .. }));
        assert!(matches!(events[2], ChatEvent::Context { used: 15, .. }));
        assert!(matches!(events[3], ChatEvent::Cancelled));
    }

    #[test]
    fn terminal_details_fit_a_socket_even_when_replay_can_hold_more() {
        let mut replay = Replay::new(8, 16384, 4096);
        let terminal = replay.push(ChatEvent::Failed { message: "x".repeat(5000) });
        assert!(replay.get(terminal).is_none());
        let (events, truncated) = replay.snapshot();
        let event = events.last().unwrap();
        assert!(super::super::delivery::encoded_size(event, 4096).is_some());
        assert!(truncated);
    }
}
