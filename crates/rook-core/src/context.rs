//! Context budgeting.
//!
//! Running out of context is the most common way an otherwise-working agent turn
//! becomes unrecoverable: the request is rejected, the transcript is already too
//! large to retry, and the user's only option is to start over and lose the work.
//! Budgeting therefore happens before the request, not after the rejection.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextBudget {
    /// The model's total context window, in tokens.
    pub window: usize,
    /// Fraction of the window to fill before compacting.
    pub compact_at: f32,
    /// Always leave room for the model's reply.
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

/// How a large payload is admitted into context.
///
/// Nothing is ever refused outright for being large: the full bytes go to the
/// store and a bounded view goes into context, so the agent can still page
/// through the rest by offset.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Admission {
    /// Bytes placed into context.
    pub inlined: usize,
    /// Total size of the payload as stored.
    pub total: usize,
    /// Object id holding the full payload.
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

fn floor_char_boundary(data: &[u8], mut i: usize) -> usize {
    i = i.min(data.len());
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
