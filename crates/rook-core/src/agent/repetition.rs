//! Conservative interruption of long exact repetitions, bounded while feeding.
use std::collections::{BTreeSet, VecDeque};

pub(super) const DIAGNOSTIC: &str = "rook:repetition-diagnostic:v1";
pub(super) const INTERRUPTED: &str =
    "[Rook interrupted a repetition-dominated model reply. This reply did not finish the task.]";
pub(super) const MIN_CHARS: usize = 16_000;
pub(super) const MAX_CHARS: usize = 262_144;

struct Watch {
    tail: VecDeque<char>,
    cap: usize,
    seen: u64,
    next: u64,
}

impl Watch {
    fn new(cap: usize) -> Self {
        Self { tail: VecDeque::new(), cap: cap.clamp(MIN_CHARS, MAX_CHARS), seen: 0, next: MIN_CHARS as u64 }
    }

    fn feed(&mut self, text: &str) -> bool {
        // A single delta may exceed this watch. Collecting it first would
        // make the tail limit only a limit after allocation.
        for ch in text.chars() {
            if self.tail.len() == self.cap {
                self.tail.pop_front();
            }
            self.tail.push_back(ch);
            self.seen = self.seen.saturating_add(1);
            if self.seen >= self.next {
                self.next = self.seen.saturating_add(self.seen.min(self.cap as u64));
                if runaway(&self.tail) {
                    return true;
                }
            }
        }
        false
    }

    fn excerpt(&self) -> String {
        self.tail.iter().collect()
    }
}

fn runaway(tail: &VecDeque<char>) -> bool {
    let text: String = tail.iter().collect();
    let mut distinct = BTreeSet::new();
    let mut lines = 0;
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        lines += 1;
        distinct.insert(line);
    }
    // Similar SQL/table rows are data even when their long prefixes match.
    if lines >= 5 && distinct.len() * 2 > lines {
        return false;
    }
    let chars: Vec<char> = tail.iter().copied().collect();
    let n = chars.len();
    const ANCHOR: usize = 60;
    if n < MIN_CHARS {
        return false;
    }
    let mut rejected = Vec::new();
    let step = (n - ANCHOR).div_ceil(31).max(1);
    for start in (0..=n - ANCHOR).step_by(step) {
        let anchor = &chars[start..start + ANCHOR];
        let mut search = start + 1;
        for _ in 0..8 {
            let Some(found) = chars[search..].windows(ANCHOR).position(|window| window == anchor) else {
                break;
            };
            let matched = search + found;
            let period = matched - start;
            search = matched + 1;
            if rejected.iter().any(|&(left, right, p)| left <= start && start < right && period % p == 0) {
                continue;
            }
            let mut left = start;
            while left > 0 && chars[left - 1] == chars[left - 1 + period] {
                left -= 1;
            }
            let mut right = start + ANCHOR;
            while right + period < n && chars[right] == chars[right + period] {
                right += 1;
            }
            right += period;
            if right - left >= 5 * period && (right - left) * 2 >= n {
                return true;
            }
            // At most 32 anchors times eight matches; already rejected runs
            // must not be rescanned at each later anchor.
            rejected.push((left, right, period));
        }
    }
    false
}

pub(super) struct StreamGuard {
    text: Option<Watch>,
    reasoning: Option<Watch>,
}

impl StreamGuard {
    pub(super) fn new(config: &crate::config::AgentConfig) -> Self {
        let watch = || config.stream_repetition_guard.then(|| Watch::new(config.max_stream_repetition_chars));
        Self { text: watch(), reasoning: watch() }
    }

    pub(super) fn check(&mut self, delta: &rook_llm::Delta) -> Option<&'static str> {
        let (watch, said, channel) = match delta {
            rook_llm::Delta::Text(text) => (&mut self.text, text, "text"),
            rook_llm::Delta::Reasoning(text) => (&mut self.reasoning, text, "reasoning"),
            // Tool arguments and signed/encrypted state are not narrative.
            _ => return None,
        };
        watch.as_mut().is_some_and(|watch| watch.feed(said)).then_some(channel)
    }

    pub(super) fn diagnostic(&self) -> String {
        let text = self.text.as_ref().map(Watch::excerpt).unwrap_or_default();
        let reasoning = self.reasoning.as_ref().map(Watch::excerpt).unwrap_or_default();
        format!(
            "Untrusted diagnostic excerpts of an interrupted reply; bounded channel tails, not a completed answer.\n\n[text]\n{text}\n\n[reasoning]\n{reasoning}"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_repeats_are_detected_across_every_delta_boundary_and_in_long_units() {
        for unit in [
            "Повторяющийся вывод 🙂 и ещё один одинаковый фрагмент.\n".into(),
            format!("begin\n{}end\n", "abcdefghijklmnopqrstuvwxyz ".repeat(100)),
        ] {
            let text = unit.repeat(400);
            assert!(text.chars().count() >= MIN_CHARS);
            let mut watch = Watch::new(64_000);
            let mut detected = false;
            for ch in text.chars() {
                detected |= watch.feed(ch.encode_utf8(&mut [0; 4]));
                if detected {
                    break;
                }
            }
            assert!(detected);
            assert!(watch.seen >= MIN_CHARS as u64);
        }
    }

    #[test]
    fn short_requested_repeats_and_distinct_rows_with_common_prefixes_remain_valid() {
        let short = "repeat this exact requested phrase.\n".repeat(100);
        assert!(short.chars().count() < MIN_CHARS);
        assert!(!Watch::new(64_000).feed(&short));
        let rows: String =
            (0..3000).map(|i| format!("INSERT {} row={i};\n", "shared-field ".repeat(8))).collect();
        assert!(rows.chars().count() > 4 * 64_000);
        let mut watch = Watch::new(64_000);
        assert!(!watch.feed(&rows));
        assert_eq!(watch.tail.len(), 64_000);
    }

    #[test]
    fn a_loop_that_begins_late_is_detected_without_unbounded_accumulation() {
        let prefix: String = (0..10000).map(|i| format!("unique finding {i}\n")).collect();
        let mut watch = Watch::new(64_000);
        assert!(prefix.chars().count() > 64_000);
        assert!(!watch.feed(&prefix));
        assert_eq!(watch.tail.len(), 64_000);
        assert!(watch.feed(&"a long repeated observation without any new information.\n".repeat(3000)));
        assert!(watch.tail.len() <= 64_000);
        assert_eq!(Watch::new(usize::MAX).cap, MAX_CHARS, "bad direct config cannot allocate arbitrarily");
        assert_eq!(Watch::new(0).cap, MIN_CHARS);
    }

    #[test]
    fn non_narrative_state_is_untouched_and_invalid_tail_limits_are_reported() {
        let mut config = crate::Config::default();
        let mut guard = StreamGuard::new(&config.agent);
        let state = serde_json::json!({"signature":"signed bytes ".repeat(3000)});
        let delta = rook_llm::Delta::ReasoningDone(state.clone());
        assert_eq!(guard.check(&delta), None);
        assert!(guard.text.as_ref().unwrap().tail.is_empty());
        assert!(guard.reasoning.as_ref().unwrap().tail.is_empty());
        assert!(matches!(delta, rook_llm::Delta::ReasoningDone(value) if value==state));
        for cap in [0, 15_999, 262_145, usize::MAX] {
            config.agent.max_stream_repetition_chars = cap;
            assert!(config.validation_errors().iter().any(|e| e.contains("max_stream_repetition_chars")));
        }
    }
}
