//! Rebuild turn-local state while retaining the frontend's shared equipment.
use super::{AgentLoop, Progress, TurnOutcome};
use crate::Result;

impl<'a> AgentLoop<'a> {
    pub(super) fn continuation(&self, provider: std::sync::Arc<dyn rook_llm::Provider>) -> Self {
        let mut next = Self::new(self.rook, provider, self.session);
        next.tools = self.tools.clone();
        next.tool_ctx = self.tool_ctx.clone();
        next.tool_ctx.secrets = Some(next.vault.clone());
        next.policy = self.policy.clone();
        next.approver = self.approver.clone();
        next.asker = self.asker.clone();
        next.servers = self.servers.clone();
        next.hooks = self.hooks.clone();
        next.observer = self.observer.clone();
        next.summariser = self.summariser.clone();
        next.interjections = self.interjections.clone();
        next.effort = self.effort;
        next.followup_model = self.followup_model.clone();
        next.max_steps = self.max_steps;
        next.max_turn_tokens = self.max_turn_tokens;
        next.max_turn_secs = self.max_turn_secs;
        // Options describe the preceding request (attachments, recipe and output
        // contract). A queued text message authorizes a fresh ordinary turn.
        next
    }

    /// Drain eligible follow-ups without closing the frontend's live stream.
    /// Returns the last turn; each outcome commits before the next reservation.
    pub async fn run_followups(
        &mut self,
        mut progress: impl FnMut(Progress<'_>),
    ) -> Result<Option<TurnOutcome>> {
        if self.depth != 0 || self.checking {
            return Ok(None);
        }
        let mut last = None;
        while crate::message_queue::followups::ready(self.rook, self.session)? {
            let (provider, effort) = match &self.followup_model {
                Some(resolve) => resolve()?,
                None => (self.provider.clone(), self.effort),
            };
            let Some((journal, message)) =
                crate::execution::Journal::reserve_follow_up(self.rook, self.session)?
            else {
                break;
            };
            let mut next = self.continuation(provider);
            next.effort = effort;
            next.reserved_execution = Some(journal);
            progress(Progress::FollowUp { id: &message.id });
            let outcome = next.run_once(&message.text, &mut progress).await?;
            let complete = super::finished(&outcome.stopped);
            last = Some(outcome);
            if !complete || next.managed_work.is_some() {
                break;
            }
        }
        Ok(last)
    }
}
