//! Explicit analysis-to-implementation routing and its bounded branch state.
use std::sync::Mutex;

use async_trait::async_trait;
use rook_llm::{
    CatalogLimits, Effort, EffortUse, LlmError, ModelInfo, Provider, Request, Response, ResponseStream,
};
use rook_store::{EventKind, Kind, NewEvent};
use serde::{Deserialize, Serialize};

use crate::{Config, CoreError, ModelSource, Result, Rook};

pub(crate) const LABEL: &str = "rook:model-phase:v1";
pub(crate) fn handoff_reason(
    messages: &[rook_llm::Message],
    target: &dyn Provider,
    native_tools: bool,
) -> Option<&'static str> {
    if native_tools && !target.supports_tools() {
        return Some("the target cannot preserve native tool schemas");
    }
    if messages.iter().any(|m| !m.images.is_empty()) && target.image_input_support() != Some(true) {
        return Some("target image input support is unsupported or unverified");
    }
    if !target.can_replay_reasoning(messages) {
        return Some("the target cannot preserve provider-owned reasoning");
    }
    None
}
const MAX_BYTES: usize = 16384;
const MAX_POLICIES: usize = 16;
static WRITING: Mutex<()> = Mutex::new(());

pub(crate) fn validate_source(config: &Config, name: &str, source: &ModelSource) -> rook_llm::Result<()> {
    if source.implementation_model.is_empty() {
        return Ok(());
    }
    let target = &source.implementation_model;
    let valid = |value: &str| {
        !value.is_empty()
            && value.len() <= 256
            && value.trim() == value
            && !value.chars().any(char::is_control)
    };
    if !valid(name) || !valid(target) || !valid(&source.model) || target == name {
        return Err(LlmError::Other(
            "model phase policy: use a different named source; names/models must fit 256 bytes without control characters".into()
        ));
    }
    let Some(next) = config.models.get(target) else {
        return Err(LlmError::Other(format!(
            "models.{name}.implementation_model: {target:?} is not a configured model source"
        )));
    };
    if !valid(&next.model) || !next.implementation_model.is_empty() {
        return Err(LlmError::Other(format!(
            "models.{name}.implementation_model: target must be a physical source with a bounded model name and no further phase route"
        )));
    }
    Ok(())
}

/// Keeps the user's selected policy beside the ordinary physical transport.
pub(crate) struct Selected {
    selected: String,
    target: String,
    inner: Box<dyn Provider>,
}
impl Selected {
    pub(crate) fn new(selected: &str, target: &str, inner: Box<dyn Provider>) -> Self {
        Self { selected: selected.into(), target: target.into(), inner }
    }
}
#[async_trait]
impl Provider for Selected {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn dispatch_identity(&self) -> Option<rook_llm::Dispatch> {
        self.inner.dispatch_identity()
    }
    fn phase_routing(&self) -> Option<(&str, &str)> {
        Some((&self.selected, &self.target))
    }
    fn context_window(&self) -> usize {
        self.inner.context_window()
    }
    fn context_key(&self) -> Option<[u8; 32]> {
        self.inner.context_key()
    }
    fn context_is_explicit(&self) -> bool {
        self.inner.context_is_explicit()
    }
    async fn discover_context_window(&self, limits: CatalogLimits) -> rook_llm::Result<Option<usize>> {
        self.inner.discover_context_window(limits).await
    }
    fn supports_tools(&self) -> bool {
        self.inner.supports_tools()
    }

    fn image_input_support(&self) -> Option<bool> {
        self.inner.image_input_support()
    }

    fn can_replay_reasoning(&self, messages: &[rook_llm::Message]) -> bool {
        self.inner.can_replay_reasoning(messages)
    }

    fn can_resume_reasoning(&self, messages: &[rook_llm::Message]) -> bool {
        self.inner.can_resume_reasoning(messages)
    }
    fn takes_effort(&self) -> bool {
        self.inner.takes_effort()
    }
    fn effort_use(&self, effort: Effort) -> EffortUse {
        self.inner.effort_use(effort)
    }
    fn supports_streaming(&self) -> bool {
        self.inner.supports_streaming()
    }
    async fn models(&self) -> rook_llm::Result<Vec<ModelInfo>> {
        self.inner.models().await
    }
    async fn models_with(&self, limits: CatalogLimits) -> rook_llm::Result<Vec<ModelInfo>> {
        self.inner.models_with(limits).await
    }
    async fn reachable(&self) -> rook_llm::Result<()> {
        self.inner.reachable().await
    }
    async fn complete(&self, request: Request) -> rook_llm::Result<Response> {
        self.inner.complete(request).await
    }

    async fn complete_with_metadata(&self, request: Request) -> rook_llm::Result<rook_llm::Completion> {
        self.inner.complete_with_metadata(request).await
    }
    async fn stream(&self, request: Request) -> rook_llm::Result<ResponseStream> {
        self.inner.stream(request).await
    }
}

#[derive(Serialize, Deserialize)]
struct Transition {
    selected: String,
    target: String,
    since: u64,
}
fn key(session: u128) -> String {
    format!("model-phase/{session:032x}")
}
fn load(rook: &Rook, session: u128) -> Result<Vec<Transition>> {
    let Some(bytes) = rook.store.kv_get_limited(&key(session), MAX_BYTES)? else {
        return Ok(Vec::new());
    };
    let states: Vec<Transition> = serde_json::from_slice(&bytes)?;
    if states.len() > MAX_POLICIES
        || states.iter().any(|s| {
            s.selected.is_empty()
                || s.target.is_empty()
                || s.selected.len() > 256
                || s.target.len() > 256
                || s.selected.chars().chain(s.target.chars()).any(char::is_control)
        })
    {
        return Err(CoreError::Other(
            "unsupported or oversized model phase state; preserve the store and inspect it".into(),
        ));
    }
    Ok(states)
}
pub(crate) fn implementing(rook: &Rook, session: u128, selected: &str, target: &str) -> Result<bool> {
    Ok(load(rook, session)?.iter().any(|s| s.selected == selected && s.target == target))
}
pub(crate) fn edited(rook: &Rook, session: u128, selected: &str, target: &str) -> Result<()> {
    let _writing = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let mut states = load(rook, session)?;
    if states.iter().any(|s| s.selected == selected && s.target == target) {
        return Ok(());
    }
    if states.len() == MAX_POLICIES {
        return Err(CoreError::Other(
            "model phase state holds 16 policies; inspect the branch before adding another".into(),
        ));
    }
    if selected.len() > 256 || target.len() > 256 {
        return Err(CoreError::Other("model route names exceed 256 bytes".into()));
    }
    let body = crate::persistence::encode_with_limit(
        &serde_json::json!({"selected":selected,"target":target,"phase":"implementation","trigger":"successful file change"}),
        4096,
    )?;
    states.push(Transition { selected: selected.into(), target: target.into(), since: 0 });
    rook.store.append_event_with_receipt(
        session,
        NewEvent::new(EventKind::Note, Kind::Message, &body).label(LABEL),
        &key(session),
        |seq| {
            if let Some(state) = states.last_mut() {
                state.since = seq;
            }
            crate::persistence::encode_with_limit(&states, MAX_BYTES)
                .map_err(|e| rook_store::StoreError::Encoding(e.to_string()))
        },
    )?;
    Ok(())
}
pub(crate) fn inherit(rook: &Rook, source: u128, target: u128, before: u64) -> Result<()> {
    let mut states = load(rook, source)?;
    states.retain(|s| s.since < before);
    if !states.is_empty() {
        let bytes = crate::persistence::encode_with_limit(&states, MAX_BYTES)?;
        rook.store.kv_update_session_values(target, &[(&key(target), &bytes)])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn engine(root: &std::path::Path) -> Rook {
        Rook::from_parts(
            rook_store::Store::open(root.join("store")).unwrap(),
            Config::default(),
            rook_skills::Environment::bare("windows", "x86_64", "0.1.0"),
            rook_skills::SkillIndex::default(),
            root.to_path_buf(),
        )
    }
    #[test]
    fn phase_state_survives_reopen_and_compaction_and_follows_only_the_forked_prefix() {
        let home = tempfile::tempdir().unwrap();
        let rook = engine(home.path());
        let session = rook.start_session("phases").unwrap();
        rook.log(session, EventKind::UserMessage, "", "change evidence").unwrap();
        let before = rook.store.get_session(session).unwrap().unwrap().next_seq;
        assert!(!implementing(&rook, session, "analysis", "implementation").unwrap());
        edited(&rook, session, "analysis", "implementation").unwrap();
        let after = rook.store.get_session(session).unwrap().unwrap().next_seq;
        edited(&rook, session, "analysis", "implementation").unwrap();
        assert_eq!(rook.store.get_session(session).unwrap().unwrap().next_seq, after, "one transition");
        rook.log(session, EventKind::Compaction, "", "the earlier work was summarized").unwrap();
        let early = rook.fork_session(session, before).unwrap().id;
        let late = rook.fork_session(session, after).unwrap().id;
        assert!(!implementing(&rook, early, "analysis", "implementation").unwrap());
        assert!(implementing(&rook, late, "analysis", "implementation").unwrap());
        assert!(!implementing(&rook, late, "analysis", "replacement").unwrap());
        drop(rook);
        let rook = engine(home.path());
        assert!(implementing(&rook, session, "analysis", "implementation").unwrap());
        assert!(implementing(&rook, late, "analysis", "implementation").unwrap());
        rook.delete_session(late).unwrap();
        assert!(rook.store.kv_get(&key(late)).unwrap().is_none());
    }
    #[test]
    fn phase_state_refuses_oversize_before_copying_or_appending_a_transition() {
        let home = tempfile::tempdir().unwrap();
        let rook = engine(home.path());
        let session = rook.start_session("bounded").unwrap();
        let mut bytes = vec![b' '; MAX_BYTES + 1];
        bytes[..2].copy_from_slice(b"[]");
        assert!(bytes.len() > MAX_BYTES);
        assert!(serde_json::from_slice::<serde_json::Value>(&bytes).is_ok());
        rook.store.kv_set(&key(session), &bytes).unwrap();
        let before = rook.store.get_session(session).unwrap().unwrap().next_seq;
        assert!(
            implementing(&rook, session, "analysis", "implementation")
                .unwrap_err()
                .to_string()
                .contains("exceeds")
        );
        assert!(edited(&rook, session, "analysis", "implementation").is_err());
        assert_eq!(rook.store.kv_get(&key(session)).unwrap().unwrap(), bytes);
        assert_eq!(rook.store.get_session(session).unwrap().unwrap().next_seq, before);
    }
    #[test]
    fn policy_history_reaches_its_cap_without_evicting_saved_phases() {
        let home = tempfile::tempdir().unwrap();
        let rook = engine(home.path());
        let session = rook.start_session("phase capacity").unwrap();
        for i in 0..MAX_POLICIES {
            edited(&rook, session, "analysis", &format!("implementation-{i}")).unwrap();
        }
        assert_eq!(load(&rook, session).unwrap().len(), MAX_POLICIES);
        let bytes = rook.store.kv_get(&key(session)).unwrap().unwrap();
        let next = rook.store.get_session(session).unwrap().unwrap().next_seq;
        assert!(
            edited(&rook, session, "analysis", "seventeenth")
                .unwrap_err()
                .to_string()
                .contains("16 policies")
        );
        assert_eq!(rook.store.kv_get(&key(session)).unwrap().unwrap(), bytes);
        assert_eq!(rook.store.get_session(session).unwrap().unwrap().next_seq, next);
        assert!(implementing(&rook, session, "analysis", "implementation-0").unwrap());
        assert!(implementing(&rook, session, "analysis", "implementation-15").unwrap());
    }

    #[test]
    fn phase_policy_is_opt_in_and_refuses_unknown_chained_or_self_targets_offline() {
        let mut config = Config::default();
        let mut source = ModelSource { model: "analysis".into(), ..Default::default() };
        assert!(validate_source(&config, "analysis", &source).is_ok());
        source.implementation_model = "missing".into();
        assert!(validate_source(&config, "analysis", &source).is_err());
        source.implementation_model = "analysis".into();
        assert!(validate_source(&config, "analysis", &source).is_err());
        source.implementation_model = "implementation".into();
        config
            .models
            .insert("implementation".into(), ModelSource { model: "fast".into(), ..Default::default() });
        assert!(validate_source(&config, "analysis", &source).is_ok());
        config.models.get_mut("implementation").unwrap().implementation_model = "another".into();
        assert!(validate_source(&config, "analysis", &source).is_err());
        let oversized = "🙂".repeat(65);
        assert!(oversized.len() > 256);
        source.implementation_model = oversized;
        assert!(validate_source(&config, "analysis", &source).is_err());
    }
}
