//! Context budgeting.
//!
//! Running out of context is the most common way an otherwise-working agent turn
//! becomes unrecoverable: the request is rejected, the transcript is already too
//! large to retry, and the user's only option is to start over and lose the work.
//! Budgeting therefore happens before the request, not after the rejection.

use serde::{Deserialize, Serialize};

/// A bounded record of the tools offered in one attempted model request.
/// It contains names and sizes, never schemas or prompt bodies.
pub const REQUEST_CATALOG_LABEL: &str = "request-tool-catalog";
pub const REQUEST_CATALOG_MAX_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequestTool {
    pub name: String,
    /// Approximate tokens for the advertised name, description and schema.
    pub estimated_tokens: usize,
}

/// One harness-selected source in the request prefix. Its content stays in
/// the actual request and is never copied into the inspection record.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequestSource {
    pub kind: String,
    pub name: String,
    pub origin: String,
    /// `included`, `card`, or `inline`; a card is not a loaded skill body.
    pub inclusion: String,
    pub estimated_tokens: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub complete: Option<bool>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SourceManifest {
    pub discovered_skills: usize,
    pub applicable_skills: usize,
    pub advertised_skills: usize,
    pub sources: Vec<RequestSource>,
    pub omitted_sources: usize,
}

impl SourceManifest {
    pub(crate) fn add(
        &mut self,
        kind: &str,
        name: &str,
        origin: &str,
        inclusion: &str,
        estimated_tokens: usize,
        complete: Option<bool>,
    ) {
        if self.sources.len() >= 32 {
            self.omitted_sources += 1;
            return;
        }
        self.sources.push(RequestSource {
            kind: kind.into(),
            name: prefix(name, 64),
            origin: prefix(origin, 192),
            inclusion: inclusion.into(),
            estimated_tokens,
            complete,
        });
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequestCatalog {
    /// Configured provider ID; a routing provider may choose another endpoint.
    pub provider_id: String,
    /// Native tool definitions or schemas embedded in the prompt.
    pub delivery: String,
    /// Stub schemas retain argument shapes; full schemas include descriptions.
    pub detail: String,
    pub used_tokens: usize,
    pub tool_count: usize,
    pub tools: Vec<RequestTool>,
    pub omitted_tools: usize,
    #[serde(default)]
    pub sources: SourceManifest,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedRequestCatalog {
    pub event_seq: u64,
    pub catalog: RequestCatalog,
}

fn prefix(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let end = floor_char_boundary(text.as_bytes(), max_bytes.saturating_sub('…'.len_utf8()));
    format!("{}…", &text[..end])
}

/// Count serialized bytes without making another copy of a potentially large
/// external tool schema. The result is an estimate, not a provider token bill.
fn json_bytes(value: &impl Serialize) -> usize {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    let _ = serde_json::to_writer(&mut count, value);
    count.0
}

impl RequestCatalog {
    pub(crate) fn capture(
        provider_id: &str,
        native: bool,
        lazy: bool,
        used_tokens: usize,
        specs: &[rook_llm::ToolSpec],
        sources: SourceManifest,
    ) -> Self {
        const MAX_TOOLS: usize = 32;
        let tools = specs
            .iter()
            .take(MAX_TOOLS)
            .map(|spec| RequestTool {
                name: prefix(&spec.name, 64),
                estimated_tokens: json_bytes(spec).div_ceil(4),
            })
            .collect::<Vec<_>>();
        Self {
            provider_id: prefix(provider_id, 96),
            delivery: if native { "native" } else { "prompt" }.into(),
            detail: if lazy { "stub" } else { "full" }.into(),
            used_tokens,
            tool_count: specs.len(),
            omitted_tools: specs.len().saturating_sub(tools.len()),
            tools,
            sources,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextBudget {
    /// Tokens.
    pub window: usize,
    /// Fraction of the window to fill before compacting.
    pub compact_at: f32,
    /// Held back so there is always room for the reply.
    pub reserve_output: usize,
}

// Shared with offline configuration checks; hand-written legacy configs still
// receive the runtime guard when they bypass those checks.
const MIN_COMPACT_AT: f32 = 0.1;
const MAX_COMPACT_AT: f32 = 0.9;

pub(crate) fn check_compact_at(value: f32) -> Result<(), String> {
    if (MIN_COMPACT_AT..=MAX_COMPACT_AT).contains(&value) {
        Ok(())
    } else {
        Err(format!("agent.compact_at must be between {MIN_COMPACT_AT} and {MAX_COMPACT_AT} inclusive"))
    }
}

impl ContextBudget {
    /// `compact_at` comes from config, so it is clamped here rather than
    /// trusted: the check runs before a request is built, and a threshold near
    /// the top of the window is one a turn reaches with nowhere left to put the
    /// tool results it is about to receive. Near the bottom it summarises a
    /// transcript that has barely started, every turn.
    pub fn new(window: usize, compact_at: f32) -> Self {
        let compact_at =
            if compact_at.is_finite() { compact_at.clamp(MIN_COMPACT_AT, MAX_COMPACT_AT) } else { 0.75 };
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

pub(crate) fn ceil_char_boundary(data: &[u8], mut i: usize) -> usize {
    while i < data.len() && (data[i] & 0xC0) == 0x80 {
        i += 1;
    }
    i
}

/// Very rough token estimate: fine for budgeting, never for billing.
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

/// Whether an ordinary event of this kind becomes a message the model sees.
///
/// One answer for everything that has to agree with the replay in
/// `AgentLoop::history`: what a turn carries, what compaction summarises, and
/// what `rook session context` reports as the cost. They drifted apart twice
/// before this existed.
pub fn reaches_the_model(kind: rook_store::EventKind) -> bool {
    use rook_store::EventKind::*;
    matches!(kind, UserMessage | AssistantMessage | ToolCall | ToolResult | SkillLoaded | Reasoning)
}

/// Companion notes with this label are also replayed as source data.
pub(crate) fn record_reaches_the_model(kind: rook_store::EventKind, label: &str) -> bool {
    reaches_the_model(kind)
        || (kind == rook_store::EventKind::Note && label == crate::branches::SUMMARY_LABEL)
}

/// How much of a thought is carried into the next request.
///
/// Head and tail, because a thought's subject is its first lines and its
/// conclusion is its last, and what goes is the working in between — the part
/// the conclusion already stands for. The marker says how much went, so a
/// model reading its own shortened thinking is not left thinking it wrote
/// that little.
///
/// The result is never longer than the budget, which is what lets
/// [`thinking_tokens`] price a thought from its size alone.
pub fn shorten_thinking(text: &str, budget_tokens: usize) -> String {
    let text = text.trim();
    if budget_tokens == 0 || text.is_empty() {
        return String::new();
    }
    if estimate_tokens(text) <= budget_tokens {
        return text.to_string();
    }
    let dropped = estimate_tokens(text) - budget_tokens;
    let marker = format!("\n\n[… {dropped} tokens of working elided …]\n\n");
    // A budget too small to hold even the marker carries nothing rather than
    // carrying a note about what it could not carry.
    let Some(room) = budget_tokens.checked_sub(estimate_tokens(&marker)).map(|left| left * 4) else {
        return String::new();
    };
    let head = at_boundary(text, room * 2 / 3);
    let tail = from_end(text, room - room * 2 / 3);
    format!("{}{marker}{}", &text[..head], &text[tail..])
}

/// The largest offset no further than `bytes` into `text` that is a character
/// boundary — thinking is prose in whatever language the model thinks in.
pub(crate) fn at_boundary(text: &str, bytes: usize) -> usize {
    let bytes = bytes.min(text.len());
    (0..=bytes).rev().find(|at| text.is_char_boundary(*at)).unwrap_or(0)
}

/// The same, measured from the end: the first boundary at or after the last
/// `bytes` bytes begin.
fn from_end(text: &str, bytes: usize) -> usize {
    let from = text.len().saturating_sub(bytes);
    (from..=text.len()).find(|at| text.is_char_boundary(*at)).unwrap_or(text.len())
}

/// How much of a tool's answer is carried into the next request.
///
/// Head and tail, for the reason a command's own capture keeps both: a
/// compiler's first error is at the head and the reason it stopped is at the
/// tail, and the middle is the part the two already stand for. The marker says
/// what went and where the whole of it still is, because a model reading a
/// shortened result must not read it as the whole answer.
///
/// Measured on a real turn before it was written: thirty-eight results, a median
/// of 826 bytes and three of 29, 13 and 13 KiB — so a ceiling touches the few
/// that are large and leaves the many that are not, which is the shape that
/// makes it worth having.
///
/// **Deterministic on purpose.** A prompt cache is a prefix match, so the same
/// result has to render the same way at every step of a turn: shortening by
/// recency — the newest few whole, older ones cut — rewrites a message that was
/// already sent, which breaks the prefix there and makes everything after it
/// uncached. The separate result-pruning pass batches that tradeoff behind a
/// minimum savings threshold and a durable watermark.
pub fn shorten_result(text: &str, budget_tokens: usize) -> String {
    if budget_tokens == 0 || estimate_tokens(text) <= budget_tokens {
        return text.to_string();
    }
    let dropped = estimate_tokens(text) - budget_tokens;
    let marker = format!(
        "\n\n[… {dropped} tokens elided; use `read_result` with this result_id for the stored text …]\n\n"
    );
    let Some(room) = budget_tokens.checked_sub(estimate_tokens(&marker)).map(|left| left * 4) else {
        return String::new();
    };
    // More head than tail: a result is usually read from the top — a listing, a
    // file, the first error — where a thought is read for its conclusion.
    let head = at_boundary(text, room * 3 / 4);
    let tail = from_end(text, room - room * 3 / 4);
    format!("{}{marker}{}", &text[..head], &text[tail..])
}

#[cfg(test)]
mod tests {
    use super::ContextBudget;

    #[test]
    fn request_catalog_bounds_names_and_never_copies_full_schema_text() {
        let specs = (0..100)
            .map(|i| rook_llm::ToolSpec {
                name: format!("tool_{i}_{}", "\u{1f}".repeat(100)),
                description: if i == 0 { "hidden schema text ".repeat(50_000) } else { "short".into() },
                parameters: serde_json::json!({"type":"object"}),
            })
            .collect::<Vec<_>>();
        let catalog = super::RequestCatalog::capture(
            "configured/test",
            true,
            true,
            123,
            &specs,
            super::SourceManifest::default(),
        );
        let encoded = serde_json::to_string(&catalog).unwrap();
        assert_eq!(catalog.tool_count, 100);
        assert_eq!(catalog.tools.len(), 32);
        assert_eq!(catalog.omitted_tools, 68);
        assert!(catalog.tools.iter().all(|tool| tool.name.len() <= 64));
        assert!(catalog.tools[0].name.ends_with('…'), "a shortened name must be visibly partial");
        assert!(encoded.len() <= super::REQUEST_CATALOG_MAX_BYTES);
        assert!(!encoded.contains("hidden schema text"));
    }

    /// Tool results were 79% of a forty-step turn's context and were re-sent on
    /// every step of it. The ceiling touches the few that are large — a median
    /// result was 826 bytes and the largest was 29 KiB — and the price
    /// `session context` reports has to agree with what is actually sent, or
    /// the number that exists to explain the bill explains a different one.
    #[test]
    fn a_long_result_is_carried_by_its_ends_and_priced_as_it_is_carried() {
        let long = format!("first line\n{}\nlast line", "middle ".repeat(4_000));
        let kept = super::shorten_result(&long, 200);

        assert!(super::estimate_tokens(&kept) <= 200, "it fits the budget: {}", kept.len());
        assert!(kept.starts_with("first line"), "the head is where a result is read from");
        assert!(kept.ends_with("last line"), "and the tail is why it stopped");
        assert!(kept.contains("elided"), "and it says the middle went: {kept}");
        assert!(kept.contains("read_result"), "and where the whole of it still is");

        // A result that fits is untouched, which is most of them.
        let short = "port = 8080\n";
        assert_eq!(super::shorten_result(short, 200), short);
        // And 0 is the budget that carries everything, as it did before.
        assert_eq!(super::shorten_result(&long, 0), long);
    }

    /// A prompt cache is a prefix match, so the same result has to render the
    /// same way at every step of a turn. Shortening by recency would rewrite a
    /// message that was already sent, break the prefix there, and make
    /// everything after it uncached — costing more than it saved.
    #[test]
    fn the_same_result_is_carried_the_same_way_every_time() {
        let long = "x".repeat(40_000);
        let once = super::shorten_result(&long, 300);
        let again = super::shorten_result(&long, 300);
        assert_eq!(once, again, "nothing about the step it is sent on changes it");
    }

    /// The bound holds even when there is too little room for the omission
    /// marker itself. Context reporting prices the actual replayed text.
    #[test]
    fn a_thought_carried_into_the_next_request_never_exceeds_its_budget() {
        // In a language whose characters are not bytes, which is what a model
        // thinking out loud in Russian writes.
        let long = "думает вслух, подробно. ".repeat(1_000);
        for budget in [0, 1, 20, 800, 5_000] {
            let kept = super::shorten_thinking(&long, budget);
            assert!(
                super::estimate_tokens(&kept) <= budget,
                "budget {budget} carried {} tokens",
                super::estimate_tokens(&kept)
            );
        }

        assert_eq!(super::shorten_thinking("worked it out", 800), "worked it out", "short enough is kept");
        assert_eq!(super::shorten_thinking(&long, 0), "", "and none means none");
    }

    /// A fraction is config, and config is written by hand: `compact_at = 1.0`
    /// leaves the turn that trips the threshold no room to receive anything,
    /// and `0` compacts a transcript of one message.
    #[test]
    fn a_threshold_out_of_config_cannot_be_one_no_turn_can_work_under() {
        let usable = ContextBudget::new(200_000, 0.75).usable();
        for absurd in [1.0, 5.0, 0.0, -1.0, f32::NAN, f32::INFINITY] {
            let threshold = ContextBudget::new(200_000, absurd).threshold();
            assert!(
                threshold >= usable / 10 && threshold <= usable * 9 / 10,
                "compact_at {absurd} gave a threshold of {threshold} in {usable} usable tokens"
            );
        }
        assert_eq!(ContextBudget::new(200_000, 0.75).threshold(), usable * 3 / 4, "a sane one is untouched");
    }

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

    #[test]
    fn only_attributed_branch_notes_join_model_context() {
        assert!(super::record_reaches_the_model(rook_store::EventKind::Note, crate::branches::SUMMARY_LABEL));
        assert!(!super::record_reaches_the_model(rook_store::EventKind::Note, "ordinary note"));
    }
}
