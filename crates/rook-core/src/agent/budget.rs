//! Token and time allowances, measured context costs and cache boundaries.

use super::{AgentLoop, TurnOutcome};
use crate::context::{ContextBudget, estimate_tokens};
use rook_llm::Message;

/// A breakpoint below the minimum cacheable prefix only pays the write premium,
/// so a small system prompt is left unmarked.
pub(super) fn cacheable(message: Message) -> Message {
    const MINIMUM_TOKENS: usize = 1024;
    if estimate_tokens(&message.content) >= MINIMUM_TOKENS { message.cacheable() } else { message }
}

/// What the request costs, anchored on what the provider last said it cost.
///
/// Estimation is `len / 4` and it is wrong in the direction that hurts: it counts
/// the messages and not the tool schemas, which are ~750 tokens of every request
/// by default and more when they are sent in full. Both the compaction threshold
/// and the overflow check turn on this number, so under-counting means a request
/// that comes back as a limit error the user never saw coming.
///
/// The provider counted what it received exactly. Everything up to that point
/// therefore needs no estimating, and the error shrinks to whatever has been
/// appended since.
pub(super) fn measured(messages: &[Message], anchor: Option<(usize, usize)>) -> usize {
    match anchor {
        Some((counted, reported)) if counted <= messages.len() => reported + measure(&messages[counted..]),
        _ => measure(messages),
    }
}

pub(super) fn images_in(messages: &[Message]) -> usize {
    messages.iter().map(|message| message.images.len()).sum()
}

pub(super) fn measure(messages: &[Message]) -> usize {
    messages.iter().map(|m| crate::attachments::tokens(m) + 4).sum()
}

impl<'a> AgentLoop<'a> {
    /// The system prompt: identity, environment, and the skill catalog.
    ///
    /// The environment block matters more than it looks. A model told it is on
    /// FreeBSD with BSD userland stops reaching for `sed -i` with a GNU argument
    /// order, which is the single most common cross-platform failure in agent
    /// transcripts.
    /// Deliberately independent of the current prompt.
    ///
    /// Everything here renders at the front of the request, so anything that
    /// varies per turn invalidates the cached prefix behind it. Recalled memory
    /// used to live here and now travels next to the prompt instead.
    /// Whether this turn sends its tools with the request or describes them in
    /// the prompt. Both the provider and the user get a say: the provider knows
    /// its dialect, and only the user knows what the endpoint behind a
    /// `base_url` will accept.
    pub(super) fn native_tools(&self) -> bool {
        self.rook.config.agent.native_tools && self.provider.supports_tools()
    }

    /// Whether this turn has spent its allowance, its sub-agents included.
    ///
    /// Input and output together, because both are billed and a turn that
    /// writes a great deal is spending as surely as one that reads.
    pub(super) fn overspent(&self, outcome: &TurnOutcome) -> bool {
        let spent = outcome.input_tokens as u64 + outcome.output_tokens as u64;
        self.max_turn_tokens > 0 && spent >= self.max_turn_tokens
    }

    /// What is left of the allowance for a sub-agent to spend.
    ///
    /// The remainder rather than a fresh allowance: `max_steps` is inherited
    /// whole, so nine errands are nine times that bound, and a ceiling that
    /// multiplies is not one. A turn already at its limit hands out nothing,
    /// which the `1` says — 0 would lift the ceiling for the child.
    pub(super) fn left_to_spend(&self, outcome: &TurnOutcome) -> u64 {
        if self.max_turn_tokens == 0 {
            return 0;
        }
        let spent = outcome.input_tokens as u64 + outcome.output_tokens as u64;
        self.max_turn_tokens.saturating_sub(spent).max(1)
    }

    /// Whether this turn has run out of time.
    pub(super) fn out_of_time(&self) -> bool {
        self.by.is_some_and(|by| std::time::Instant::now() >= by)
    }

    pub(super) fn time_note(&self) -> String {
        format!(
            "this turn has used its {} seconds, sub-agents included — answering with what it \
             has rather than going on",
            self.max_turn_secs
        )
    }

    pub(super) fn spend_note(&self) -> String {
        format!(
            "this turn has spent its allowance of {} tokens, sub-agents included — \
             answering with what it has rather than going on",
            self.max_turn_tokens
        )
    }

    /// Shrink the context window this loop budgets against, so a test can reach
    /// compaction without a hundred thousand tokens of fixture.
    #[doc(hidden)]
    pub fn set_window_for_test(&mut self, window: usize) {
        self.budget = ContextBudget::new(window, self.rook.config.agent.compact_at);
    }

    /// Mark the end of the conversation as it stood before this turn, so each
    /// request reuses the whole prior prefix instead of only the system block.
    pub(super) fn mark_stable_prefix(&self, messages: &mut [Message]) {
        if messages.len() >= 3 {
            let last_stable = messages.len() - 2;
            messages[last_stable].cache = true;
        }
    }

    /// What the endpoint says it will hold, asked once for the life of the
    /// process, only when nobody has set a number, and only when a turn is
    /// about to summarise itself to fit the guess.
    ///
    /// The assumption for anything self-hosted is 32768, and the machine this
    /// was written for serves 262144 — so a turn compacted five times to fit a
    /// quarter of what was there. Failures are silence: a provider that lists
    /// no models, or lists no window, leaves the assumption where it was, and
    /// an assumption that turns out too large is answered by the refusal.
    pub(super) async fn ask_the_window(&mut self) {
        if self.rook.config.agent.context_window.is_some()
            || self.rook.window_to_budget(0) > 0
            || self.depth > 0
        {
            return;
        }
        let (_, wanted) = rook_llm::split_spec(&self.rook.config.agent.model);
        let Ok(models) = self.provider.models().await else { return };
        let Some(window) = models.iter().find(|m| m.id == wanted).and_then(|m| m.context_window) else {
            return;
        };
        if window <= self.budget.window {
            return;
        }
        tracing::info!("the endpoint holds {window} tokens, not the {} assumed", self.budget.window);
        self.rook.learn_window(window);
        self.budget = ContextBudget::new(window, self.rook.config.agent.compact_at);
    }

    /// How much the reply may be, when nobody has set a number.
    ///
    /// What is left of the window after this prompt, which is the most that
    /// could arrive anyway: no API says what a model's own ceiling is, and an
    /// endpoint with a lower one refuses and is asked for less. Never below
    /// what the budget reserves — that is the room the rest of the loop plans
    /// around, and a reply with less than it has nowhere to write a call.
    pub(super) fn room_for_output(&self, used: usize) -> u32 {
        let asked = self.rook.config.agent.max_output_tokens;
        if asked > 0 {
            return asked;
        }
        let left = self.budget.usable().saturating_sub(used).max(self.budget.reserve_output);
        left.min(u32::MAX as usize) as u32
    }
}
