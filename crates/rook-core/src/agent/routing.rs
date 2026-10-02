//! Phase changes at complete tool-batch boundaries.
use crate::phase_routing::LABEL;
use crate::{CoreError, Result};
use rook_llm::Provider;
use rook_store::EventKind;
use std::sync::Arc;

pub(super) fn settings(provider: &dyn Provider) -> (Option<(String, String)>, bool) {
    let policy = provider.phase_routing();
    let routing = policy
        .filter(|(selected, target)| {
            [*selected, *target]
                .into_iter()
                .all(|name| !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control))
        })
        .map(|(selected, target)| (selected.into(), target.into()));
    let invalid = policy.is_some() && routing.is_none();
    (routing, invalid)
}

impl crate::agent::AgentLoop<'_> {
    pub(super) fn reset_phase_route(&mut self) {
        (self.routing, self.routing_invalid) = settings(self.provider.as_ref());
        self.routed = false;
        self.routing_guard_reported = false;
    }

    pub(super) fn apply_phase_route(
        &mut self,
        messages: &[rook_llm::Message],
        progress: &mut impl FnMut(crate::agent::Progress<'_>),
    ) -> Result<bool> {
        if self.routing_invalid {
            return Err(CoreError::Other(
                "model phase route names must fit 256 bytes without control characters".into(),
            ));
        }
        if self.checking || self.depth > 0 || self.routed {
            return Ok(false);
        }
        let Some((selected, target)) = &self.routing else {
            return Ok(false);
        };
        if !crate::phase_routing::implementing(self.rook, self.session, selected, target)? {
            return Ok(false);
        }
        // A signature/encrypted state or image cannot be silently translated
        // into another model's dialect. Keep the original model for this phase.
        if messages.iter().any(|m| !m.reasoning.is_empty() || !m.images.is_empty()) {
            if !self.routing_guard_reported {
                let said = "Implementation phase recorded; keeping the analysis model to preserve images or provider-owned reasoning.";
                progress(crate::agent::Progress::Working { call: "model routing", said });
                self.routing_guard_reported = true;
            }
            return Ok(false);
        }
        let provider: Arc<dyn Provider> =
            crate::models::provider_for(&self.rook.config, &self.vault, target)?.into();
        if provider.id().len() > 512 {
            return Err(CoreError::Other("implementation model identity exceeds 512 bytes".into()));
        }
        if self.native_tools() && !provider.supports_tools() {
            return Err(CoreError::Other(
                "implementation model cannot preserve native tool schemas; choose a compatible target".into(),
            ));
        }
        let said =
            format!("{selected}: implementation → {}", provider.id().chars().take(256).collect::<String>());
        let body = crate::persistence::encode_with_limit(
            &serde_json::json!({"selected":selected,"target":target,"phase":"implementation","dispatch":provider.id()}),
            4096,
        )?;
        self.rook.log(
            self.session,
            EventKind::Note,
            LABEL,
            std::str::from_utf8(&body).map_err(|e| CoreError::Other(e.to_string()))?,
        )?;
        self.budget = crate::context::ContextBudget::new(
            self.rook.window_to_budget(provider.as_ref()),
            self.rook.config.agent.compact_at,
        );
        self.provider = provider;
        self.routed = true;
        progress(crate::agent::Progress::Working { call: "model routing", said: &said });
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use crate::{Config, ModelSource, Rook, Vault};
    use rook_llm::{Image, Message};

    fn engine(root: &std::path::Path) -> Rook {
        let mut config = Config::default();
        for (name, window) in [("analysis", 65536), ("implementation", 32768), ("recipe", 16384)] {
            config.models.insert(
                name.into(),
                ModelSource {
                    model: name.into(),
                    api: "openai".into(),
                    url: "http://127.0.0.1:1/v1".into(),
                    context_window: Some(window),
                    ..Default::default()
                },
            );
        }
        config.models.get_mut("analysis").unwrap().implementation_model = "implementation".into();
        Rook::from_parts(
            rook_store::Store::open(root.join("store")).unwrap(),
            config,
            rook_skills::Environment::bare("linux", "x86_64", "0.1.0"),
            rook_skills::SkillIndex::default(),
            root.into(),
        )
    }
    #[test]
    fn images_and_opaque_reasoning_hold_the_source_without_mutating_messages_or_repeating_notice() {
        let home = tempfile::tempdir().unwrap();
        let rook = engine(home.path());
        let session = rook.start_session("guarded route").unwrap();
        crate::phase_routing::edited(&rook, session, "analysis", "implementation").unwrap();
        let provider = crate::models::provider_for(&rook.config, &Vault::empty(), "analysis").unwrap();
        let mut agent = crate::agent::AgentLoop::new(&rook, provider.into(), session);
        let initial = agent.provider.id().to_string();
        let mut image = Message::user("inline evidence");
        image.images.push(Image {
            mime_type: "image/png".into(),
            data: "aW1hZ2U=".into(),
            width: 1,
            height: 1,
        });
        let mut opaque = Message::assistant("provider state");
        opaque
            .reasoning
            .push(serde_json::json!({"type":"thinking","thinking":"private","signature":"exact bytes"}));
        let mut notices = 0;
        for message in [image, opaque] {
            let bytes = serde_json::to_vec(&message).unwrap();
            assert!(!agent.apply_phase_route(std::slice::from_ref(&message), &mut |_| notices += 1).unwrap());
            assert_eq!(serde_json::to_vec(&message).unwrap(), bytes);
            assert_eq!(agent.provider.id(), initial);
            assert_eq!(agent.budget.window, 65536);
        }
        assert_eq!(notices, 1);
        assert!(agent.apply_phase_route(&[Message::user("text only")], &mut |_| notices += 1).unwrap());
        assert_eq!(agent.provider.id(), "implementation");
        assert_eq!(agent.budget.window, 32768, "new physical context window replaces the old budget");
        assert_eq!(notices, 2);
        assert!(!agent.apply_phase_route(&[], &mut |_| notices += 1).unwrap());
        assert_eq!(notices, 2);
    }
    #[test]
    fn an_explicit_recipe_model_replaces_the_previous_phase_policy() {
        let home = tempfile::tempdir().unwrap();
        let rook = engine(home.path());
        let session = rook.start_session("recipe override").unwrap();
        crate::phase_routing::edited(&rook, session, "analysis", "implementation").unwrap();
        std::fs::create_dir_all(home.path().join(".rook/recipes")).unwrap();
        std::fs::write(
            home.path().join(".rook/recipes/physical.toml"),
            "version=1\nprompt='answer'\nmodel='recipe'\n",
        )
        .unwrap();
        let provider = crate::models::provider_for(&rook.config, &Vault::empty(), "analysis").unwrap();
        let mut agent = crate::agent::AgentLoop::new(&rook, provider.into(), session);
        agent.options.recipe =
            Some(rook_proto::RecipeInvocation { path: "physical".into(), parameters: Default::default() });
        assert!(agent.prepare_recipe("recipe request").unwrap().is_some());
        assert!(agent.routing.is_none());
        assert!(
            !agent.apply_phase_route(&[], &mut |_| panic!("recipe must keep its explicit model")).unwrap()
        );
        assert_eq!(agent.provider.id(), "recipe");
        assert_eq!(agent.budget.window, 16384);
    }
}
