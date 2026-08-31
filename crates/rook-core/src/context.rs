//! Context budgeting.
//!
//! Running out of context is the most common way an otherwise-working agent turn
//! becomes unrecoverable: the request is rejected, the transcript is already too
//! large to retry, and the user's only option is to start over and lose the work.
//! Budgeting therefore happens before the request, not after the rejection.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextBudget {
    /// Tokens.
    pub window: usize,
    /// Fraction of the window to fill before compacting.
    pub compact_at: f32,
    /// Held back so there is always room for the reply.
    pub reserve_output: usize,
}

impl ContextBudget {
    pub fn new(window: usize, compact_at: f32) -> Self {
        Self { window, compact_at, reserve_output: (window / 8).clamp(1024, 32_768) }
    }

    pub fn usable(&self) -> usize {
        self.window.saturating_sub(self.reserve_output)
    }

    pub fn threshold(&self) -> usize {
        (self.usable() as f32 * self.compact_at) as usize
    }

    pub fn needs_compaction(&self, used: usize) -> bool {
        used >= self.threshold()
    }
}

/// How a large payload was admitted into context.
///
/// Nothing is refused for being large: the full bytes go to the store and a
/// bounded view goes into context, so the rest stays reachable by offset.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Admission {
    pub inlined: usize,
    pub total: usize,
    /// Holds the full payload.
    pub object: String,
    pub truncated: bool,
}

/// Fit `data` into `max_bytes` of context, keeping the head and the tail — the
/// two parts that carry signal in compiler output, stack traces and diffs.
pub fn window_bytes(data: &[u8], max_bytes: usize) -> (Vec<u8>, bool) {
    if data.len() <= max_bytes {
        return (data.to_vec(), false);
    }
    let head = max_bytes * 2 / 3;
    let tail = max_bytes - head;
    let mut out = Vec::with_capacity(max_bytes + 64);
    out.extend_from_slice(&data[..floor_char_boundary(data, head)]);
    out.extend_from_slice(format!("\n\n... {} bytes elided ...\n\n", data.len() - max_bytes).as_bytes());
    let start = ceil_char_boundary(data, data.len() - tail);
    out.extend_from_slice(&data[start..]);
    (out, true)
}

pub(crate) fn floor_char_boundary(data: &[u8], mut i: usize) -> usize {
    // The end of the slice is a boundary and has no byte to look at. `i` was
    // always short of it here — `window_bytes` returns early when the data fits
    // — until a caller passed a limit larger than its input and this indexed
    // one past the end.
    if i >= data.len() {
        return data.len();
    }
    while i > 0 && (data[i] & 0xC0) == 0x80 {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(data: &[u8], mut i: usize) -> usize {
    while i < data.len() && (data[i] & 0xC0) == 0x80 {
        i += 1;
    }
    i
}

/// Very rough token estimate: fine for budgeting, never for billing.
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

/// Whether an event of this kind becomes a message the model sees.
///
/// One answer for everything that has to agree with the replay in
/// `AgentLoop::history`: what a turn carries, what compaction summarises, and
/// what `rook session context` reports as the cost. They drifted apart twice
/// before this existed.
pub fn reaches_the_model(kind: rook_store::EventKind) -> bool {
    use rook_store::EventKind::*;
    matches!(kind, UserMessage | AssistantMessage | ToolCall | ToolResult | SkillLoaded)
}

/// The same question asked of a kind's printed name, which is what a transcript
/// entry carries.
///
/// Through the enum rather than a second list of names: the doc above says one
/// answer, and two lists that must agree are two answers waiting to differ.
pub fn kind_reaches_the_model(kind: &str) -> bool {
    rook_store::EventKind::named(kind).is_some_and(reaches_the_model)
}

#[cfg(test)]
mod tests {
    /// `window_bytes` returns early when its input fits, so nothing ever asked
    /// this for a boundary at or past the end — until a caller with a limit
    /// larger than its input did, and it indexed one byte off the slice.
    #[test]
    fn the_end_of_the_input_is_a_boundary_and_has_no_byte_to_look_at() {
        let text = "héllo".as_bytes();
        assert_eq!(super::floor_char_boundary(text, text.len()), text.len());
        assert_eq!(super::floor_char_boundary(text, 4096), text.len());
        // And it still walks back off a continuation byte inside the string:
        // `é` occupies bytes 1 and 2.
        assert_eq!(super::floor_char_boundary(text, 2), 1);
    }

    /// The two used to be two lists of the same answer, and the module's own
    /// doc says there should be one.
    #[test]
    fn a_kind_answers_the_same_by_name_as_by_variant() {
        for kind in rook_store::EventKind::ALL {
            assert_eq!(
                super::kind_reaches_the_model(kind.as_str()),
                super::reaches_the_model(kind),
                "{} answers differently by name",
                kind.as_str()
            );
        }
        assert!(!super::kind_reaches_the_model("not-a-kind"));
    }
}
