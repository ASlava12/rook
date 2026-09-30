//! Bounded edit deltas: a long draft does not get cloned on each keystroke.
use std::collections::VecDeque;

pub(super) struct Edit {
    pub at: usize,
    pub removed: String,
    pub inserted: String,
    pub before: usize,
    pub after: usize,
}

impl Edit {
    fn bytes(&self) -> usize {
        self.removed.len() + self.inserted.len()
    }
}

pub(super) struct History {
    undo: VecDeque<Edit>,
    redo: Vec<Edit>,
    bytes: usize,
    max_bytes: usize,
    max_events: usize,
    grouping: bool,
}

impl Default for History {
    fn default() -> Self {
        Self::new(&rook_core::keybindings::Settings::default())
    }
}

impl History {
    pub(super) fn new(settings: &rook_core::keybindings::Settings) -> Self {
        Self {
            undo: VecDeque::new(),
            redo: Vec::new(),
            bytes: 0,
            max_bytes: settings.undo_bytes.clamp(4096, 16 * 1024 * 1024),
            max_events: settings.undo_events.clamp(1, 4096),
            grouping: false,
        }
    }

    pub(super) fn boundary(&mut self) {
        self.grouping = false;
    }

    pub(super) fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.bytes = 0;
        self.grouping = false;
    }

    pub(super) fn record(
        &mut self,
        text: &str,
        start: usize,
        end: usize,
        inserted: &str,
        cursor: usize,
        typing: bool,
    ) {
        let bytes = end - start + inserted.len();
        if bytes > self.max_bytes {
            // An unrecorded edit breaks the coordinate chain of older edits.
            self.clear();
            return;
        }
        for edit in self.redo.drain(..) {
            self.bytes -= edit.bytes();
        }
        // Admit retained text before copying it; a replacement can be as big
        // as the whole budget, even when older edits have already filled it.
        while self.bytes > self.max_bytes - bytes {
            if let Some(old) = self.undo.pop_front() {
                self.bytes -= old.bytes();
            } else {
                break;
            }
        }
        // Coalesce adjacent word input, never paste, cursor movement or deletion.
        let word = typing && !inserted.chars().any(char::is_whitespace);
        if word
            && self.grouping
            && start == end
            && let Some(last) = self.undo.back_mut()
            && last.removed.is_empty()
            && last.after == cursor
            && last.at + last.inserted.len() == start
            && last.bytes() + bytes <= self.max_bytes
        {
            last.inserted.push_str(inserted);
            last.after = start + inserted.len();
        } else {
            if self.undo.len() == self.max_events
                && let Some(old) = self.undo.pop_front()
            {
                self.bytes -= old.bytes();
            }
            self.undo.push_back(Edit {
                at: start,
                removed: text[start..end].to_string(),
                inserted: inserted.into(),
                before: cursor,
                after: start + inserted.len(),
            });
        }
        self.bytes += bytes;
        self.grouping = word;
    }

    pub(super) fn undo(&mut self, text: &mut String, cursor: &mut usize) {
        self.boundary();
        if let Some(edit) = self.undo.pop_back() {
            text.replace_range(edit.at..edit.at + edit.inserted.len(), &edit.removed);
            *cursor = edit.before;
            self.redo.push(edit);
        }
    }

    pub(super) fn redo(&mut self, text: &mut String, cursor: &mut usize) {
        self.boundary();
        if let Some(edit) = self.redo.pop() {
            text.replace_range(edit.at..edit.at + edit.removed.len(), &edit.inserted);
            *cursor = edit.after;
            self.undo.push_back(edit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_deltas_respect_the_shared_undo_redo_budget() {
        let settings =
            rook_core::keybindings::Settings { undo_bytes: 4096, undo_events: 3, ..Default::default() };
        let mut history = History::new(&settings);
        let mut text = String::new();
        let mut cursor = 0;
        for _ in 0..10 {
            let inserted = "я".repeat(900);
            history.record(&text, cursor, cursor, &inserted, cursor, false);
            text.push_str(&inserted);
            cursor = text.len();
        }
        assert_eq!(history.bytes, 3600, "the byte bound actually evicted older edits");
        assert_eq!(history.undo.len(), 2);
        history.undo(&mut text, &mut cursor);
        assert_eq!(history.bytes, 3600, "moving to redo does not free retained text");
        assert_eq!(history.redo.len(), 1);
        history.record(&text, cursor, cursor, "!", cursor, false);
        assert!(history.redo.is_empty());
        assert_eq!(history.bytes, 1801);
    }

    #[test]
    fn event_limit_evicts_whole_edits_and_oversized_edits_break_the_chain() {
        let settings =
            rook_core::keybindings::Settings { undo_bytes: 4096, undo_events: 2, ..Default::default() };
        let mut history = History::new(&settings);
        let mut text = String::new();
        let mut cursor = 0;
        for inserted in ["one", "two", "three"] {
            history.record(&text, cursor, cursor, inserted, cursor, false);
            text.push_str(inserted);
            cursor = text.len();
        }
        assert_eq!(history.undo.len(), 2, "three separate edits exceed the event cap");
        for _ in 0..3 {
            history.undo(&mut text, &mut cursor);
        }
        assert_eq!(text, "one", "only the two retained edits can be undone");
        let oversized = "x".repeat(4097);
        assert!(oversized.len() > settings.undo_bytes);
        history.record(&text, cursor, cursor, &oversized, cursor, false);
        assert!(history.undo.is_empty());
        assert!(history.redo.is_empty());
        assert_eq!(history.bytes, 0);
    }
}
