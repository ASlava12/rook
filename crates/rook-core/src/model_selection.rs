//! Durable session selection: a queued choice and its retry receipt commit together.
use crate::{CoreError, Result, Rook};
use serde::{Deserialize, Serialize};

const MAX_BYTES: usize = 96 * 1024;
const KEPT: usize = 64;
fn key(session: u128) -> String {
    format!("model-selection/{session:032x}")
}
fn bad(text: &str) -> CoreError {
    CoreError::Other(text.into())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: String,
    pub expected_revision: u64,
    pub model: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub id: String,
    pub model: String,
    pub revision: u64,
    pub expected_revision: u64,
    /// This immutable acknowledgement does not claim a model request already ran.
    pub status: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct View {
    pub revision: u64,
    pub selected: Option<String>,
    pub last_effective: Option<String>,
    pub pending: bool,
}
#[derive(Default, Serialize, Deserialize)]
struct State {
    version: u32,
    revision: u64,
    applied_revision: u64,
    selected: Option<String>,
    route: Option<String>,
    effective: Option<String>,
    receipts: Vec<Receipt>,
}
fn read(rook: &Rook, session: u128) -> Result<(Option<Vec<u8>>, State)> {
    if rook.store.get_session(session)?.is_none() {
        return Err(bad("model selection session disappeared"));
    }
    let bytes = rook.store.kv_get_limited(&key(session), MAX_BYTES)?;
    let state = match &bytes {
        Some(bytes) => {
            let state: State = serde_json::from_slice(bytes)?;
            if state.version != 1
                || state.receipts.len() > KEPT
                || state.applied_revision > state.revision
                || state.selected.as_ref().is_some_and(|name| !valid_name(name))
                || state
                    .route
                    .as_ref()
                    .is_some_and(|hash| hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()))
                || state.selected.is_some() != state.route.is_some()
            {
                return Err(bad("invalid saved model selection; inspect the session before changing it"));
            }
            state
        }
        None => State { version: 1, ..Default::default() },
    };
    Ok((bytes, state))
}
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 256 && name.trim() == name && !name.chars().any(char::is_control)
}
impl State {
    fn view(&self) -> View {
        View {
            revision: self.revision,
            selected: self.selected.clone(),
            last_effective: self.effective.clone(),
            pending: self.revision != self.applied_revision,
        }
    }
}
impl Rook {
    pub fn model_selection(&self, session: u128) -> Result<View> {
        Ok(read(self, session)?.1.view())
    }

    /// Exact retries return their original result before checking today's revision/configuration.
    pub fn select_model(&self, session: u128, request: &Request) -> Result<Receipt> {
        if request.id.is_empty()
            || request.id.len() > 64
            || !request.id.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            || !valid_name(&request.model)
        {
            return Err(bad(
                "model request needs a 1–64 byte alphanumeric ID and a model name of at most 256 bytes",
            ));
        }
        for _ in 0..8 {
            let (old, mut state) = read(self, session)?;
            if let Some(receipt) = state.receipts.iter().find(|r| r.id == request.id) {
                if receipt.model != request.model || receipt.expected_revision != request.expected_revision {
                    return Err(bad("model request ID already belongs to a different selection"));
                }
                return Ok(receipt.clone());
            }
            if state.revision != request.expected_revision {
                return Err(bad("model selection changed; refresh its revision before choosing again"));
            }
            crate::models::usable(&self.config, &request.model).map_err(CoreError::Other)?;
            let vault = crate::Vault::load().unwrap_or_else(|_| crate::Vault::empty());
            let route = crate::models::catalog_scope(&self.config, &vault, &request.model)?;
            state.revision =
                state.revision.checked_add(1).ok_or_else(|| bad("model selection revision exhausted"))?;
            state.selected = Some(request.model.clone());
            state.route = Some(route);
            let receipt = Receipt {
                id: request.id.clone(),
                model: request.model.clone(),
                revision: state.revision,
                expected_revision: request.expected_revision,
                status: "queued".into(),
            };
            if state.receipts.len() == KEPT {
                state.receipts.remove(0);
            }
            state.receipts.push(receipt.clone());
            let bytes = crate::persistence::encode_with_limit(&state, MAX_BYTES)?;
            if self.store.kv_update_checked(
                &key(session),
                old.as_deref(),
                &[(&key(session), &bytes)],
                Some(session),
            )? {
                return Ok(receipt);
            }
        }
        Err(bad("model selection is busy; refresh and retry with the same request ID"))
    }

    /// Called only at a new root turn boundary. Never mutates an in-flight provider.
    pub fn provider_for_turn(
        &self,
        session: u128,
        fallback: Option<&str>,
    ) -> Result<Box<dyn rook_llm::Provider>> {
        for _ in 0..8 {
            let (old, mut state) = read(self, session)?;
            let Some(name) = &state.selected else {
                return Ok(crate::models::chosen(&self.config, fallback)?);
            };
            let vault = crate::Vault::load().unwrap_or_else(|_| crate::Vault::empty());
            if state.route.as_deref()
                != Some(crate::models::catalog_scope(&self.config, &vault, name)?.as_str())
            {
                return Err(bad(
                    "selected model route or credential source changed; explicitly select the model again",
                ));
            }
            let provider = crate::models::provider_for(&self.config, &vault, name)?;
            state.applied_revision = state.revision;
            state.effective = state.selected.clone();
            let bytes = crate::persistence::encode_with_limit(&state, MAX_BYTES)?;
            if self.store.kv_update_session_checked(
                session,
                &key(session),
                old.as_deref(),
                &bytes,
                |meta| {
                    if let Some(name) = &state.selected {
                        meta.model = name.clone();
                    }
                },
            )? {
                return Ok(provider);
            }
        }
        Err(bad("model selection changed during turn admission; retry this turn"))
    }
}
