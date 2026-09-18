//! Turn admission and closing: hooks, recipes, execution receipts and persistence.

use super::{AgentLoop, Progress, TurnOutcome};
use crate::context::ContextBudget;
use crate::error::{CoreError, Result};
use crate::hooks;
use rook_store::EventKind;

impl AgentLoop<'_> {
    pub(super) fn prepare_recipe(&mut self, prompt: &str) -> Result<Option<String>> {
        self.effective_options = None;
        let prepared = crate::recipes::prepare(self.rook, prompt, &self.options)?;
        let mut prepared_prompt = None;
        self.recipe_skill = None;
        self.recipe_output = false;
        if let Some(recipe) = prepared {
            self.effective_options = Some(recipe.options);
            self.recipe_skill = recipe.skill;
            self.recipe_output = recipe.output_needs_approval;
            if let Some(model) = recipe.model {
                self.provider = crate::models::provider_for(&self.rook.config, &self.vault, &model)?.into();
                self.budget = ContextBudget::new(
                    self.rook.window_to_budget(self.provider.context_window()),
                    self.rook.config.agent.compact_at,
                );
                self.rook.store.update_session(self.session, |meta| meta.model = model.clone())?;
            }
            if let Some(steps) = recipe.limits.steps {
                self.max_steps = self.max_steps.min(steps);
            }
            if let Some(tokens) = recipe.limits.tokens {
                self.max_turn_tokens =
                    if self.max_turn_tokens == 0 { tokens } else { self.max_turn_tokens.min(tokens) };
            }
            if let Some(seconds) = recipe.limits.seconds {
                self.max_turn_secs =
                    if self.max_turn_secs == 0 { seconds } else { self.max_turn_secs.min(seconds) };
            }
            prepared_prompt = Some(recipe.prompt);
        }
        Ok(prepared_prompt)
    }

    pub(super) async fn begin_turn<F: FnMut(Progress<'_>)>(
        &mut self,
        prompt: &str,
        on_progress: &mut F,
    ) -> Result<Option<String>> {
        self.rook.name_session_from(self.session, prompt).ok();
        let setup = if self.hooks.is_empty() {
            None
        } else {
            Some(self.record_background("harness:prompt hooks", "configured session_start and prompt hooks")?)
        };
        self.run_session_hooks().await;
        if self.rook.recovery_block(self.session)?.is_none() {
            self.offer_language_server().await;
            self.offer_server_update().await;
        }
        let gate = self
            .hooks
            .run(hooks::Event::Prompt, prompt, &self.payload(serde_json::json!({ "prompt": prompt })))
            .await;
        if let Some(setup) = setup {
            setup.finish("prompt hooks returned")?;
        }
        if let Some(rook_tools::policy::Decision::Deny(why)) = gate.decision {
            return Err(CoreError::Other(format!("the turn was refused before it began: {why}")));
        }

        // Before the prompt is logged, so this turn's span starts at the prompt
        // itself and carries no part of an earlier one.
        self.began_at_seq =
            self.rook.store.get_session(self.session).ok().flatten().map(|m| m.next_seq).unwrap_or(0);
        if self.turn_options().attachments.is_empty() {
            self.rook.log(self.session, EventKind::UserMessage, "", prompt)?;
        } else {
            let message = crate::attachments::prepare(prompt, &self.turn_options().attachments)?;
            self.rook.log(
                self.session,
                EventKind::UserMessage,
                crate::attachments::LABEL,
                &serde_json::to_string(&message)?,
            )?;
        }
        if let Some(journal) = self.execution.as_ref().and_then(std::sync::Weak::upgrade) {
            journal.admit(&self.vault.redact(prompt))?;
        }
        // Set here and not in `new`: a front end builds the loop and may hold
        // it before there is a prompt, and what is being bounded is the turn.
        // Only at the top, because a sub-agent is given the parent's and a
        // fresh one each would be the multiplication this exists to stop.
        if self.depth == 0 && self.by.is_none() && self.max_turn_secs > 0 {
            self.by = Some(std::time::Instant::now() + std::time::Duration::from_secs(self.max_turn_secs));
        }
        if let Some(context) = gate.context() {
            self.rook.log(self.session, EventKind::Note, "hook", &context)?;
        }

        if let Some(recipe) = &self.options.recipe {
            let settings = serde_json::json!({
                "recipe":recipe.path,"model":self.provider.id(),"skill":self.recipe_skill,
                "max_steps":self.max_steps,"max_turn_tokens":self.max_turn_tokens,"max_turn_secs":self.max_turn_secs,
            });
            self.rook.log(
                self.session,
                EventKind::Note,
                "recipe settings",
                &self.vault.redact(&settings.to_string()),
            )?;
            let said =
                format!("{}; model {}; at most {} steps", recipe.path, self.provider.id(), self.max_steps);
            on_progress(Progress::Working { call: "recipe", said: &said });
        }
        let loaded_recipe_skill = if let Some(name) = &self.recipe_skill {
            let resolved = self
                .rook
                .skills()
                .resolve(name, self.rook.env())
                .map_err(|e| CoreError::Other(format!("recipe skill {name:?}: {e}")))?;
            let id = resolved.skill.id();
            let source = self.skill_source(&resolved);
            self.rook.log(self.session, EventKind::SkillLoaded, &id, &source)?;
            Some(id)
        } else {
            None
        };
        Ok(loaded_recipe_skill)
    }

    /// Record intent before harness-owned operations can have side effects.
    pub(super) fn record_background(
        &self,
        tool: &str,
        arguments: &str,
    ) -> Result<crate::execution::Background> {
        if let Some(reason) = self.rook.recovery_block(self.session)? {
            return Err(CoreError::Other(reason));
        }
        let journal = self
            .execution
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .ok_or_else(|| CoreError::Other("execution receipt is missing".into()))?;
        journal.background(tool, &self.vault.redact(arguments))
    }

    async fn run_session_hooks(&self) {
        if self.hooks.is_empty() || self.session_context.lock().is_ok_and(|c| c.is_some()) {
            return;
        }
        let outcome =
            self.hooks.run(hooks::Event::SessionStart, "", &self.payload(serde_json::json!({}))).await;
        if let Ok(mut slot) = self.session_context.lock() {
            *slot = Some(outcome.context().unwrap_or_default());
        }
    }

    /// Everything that happens once the turn is over, in the order it has to
    /// happen in: what was fetched while it ran is collected first, because
    /// settling the reports is what copies them into the outcome and a line
    /// written after that is a line nobody reads.
    pub(super) async fn end_of_turn(&self, outcome: &mut TurnOutcome) -> Result<()> {
        self.collect_install().await;
        self.settle_reports(outcome);
        // A turn is the unit somebody would miss. Its events were written
        // without waiting for the disk — eight milliseconds each, which a
        // two-hundred-step turn paid four seconds for — and this is where they
        // are made to survive a power cut, once rather than four hundred times.
        // Operation boundaries already flush their intent and receipt; this
        // final barrier also keeps replies and housekeeping that ran no tools.
        //
        // Not fatal, and not silent: the turn is over and its work is on disk
        // in the workspace either way, but a store that cannot write is a
        // session about to be lost and the next thing to go wrong will be
        // stranger than this.
        if let Err(why) = self.rook.store.flush() {
            tracing::warn!("the session log is not on disk yet: {why}");
        }
        self.finish(outcome).await
    }

    pub(super) async fn finish(&self, outcome: &TurnOutcome) -> Result<()> {
        if self.hooks.is_empty() {
            return Ok(());
        }
        let receipt = self.record_background("harness:turn_end hooks", "configured turn_end hooks")?;
        let payload = self.payload(serde_json::json!({
            "steps": outcome.steps,
            "stopped": outcome.stopped,
            "reply": outcome.reply,
            "input_tokens": outcome.input_tokens,
            "output_tokens": outcome.output_tokens,
        }));
        self.hooks.run(hooks::Event::TurnEnd, &outcome.stopped, &payload).await;
        receipt.finish("turn_end hooks returned")
    }

    pub(super) fn payload(&self, mut extra: serde_json::Value) -> serde_json::Value {
        extra["session"] = rook_store::format_session_id(self.session).into();
        extra["cwd"] = self.rook.workspace.display().to_string().into();
        extra["model"] = self.rook.config.agent.model.clone().into();
        extra
    }
}
