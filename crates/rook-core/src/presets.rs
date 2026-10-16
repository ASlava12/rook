//! Bounded reusable inputs with optimistic replacement and no permission fields.
use crate::{CoreError, Result, Rook};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
const KEY: &str = "prompt-presets/v1";
const MAX: usize = 262144;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    pub prompt: String,
    pub model: Option<String>,
    pub effort: Option<rook_llm::Effort>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    List,
    Get { name: String },
    Save { name: String, preset: Preset, expected: Option<String> },
    Delete { name: String, expected: String },
}
pub fn arguments(text: &str) -> Result<Request> {
    if text.len() > 65536 {
        return Err(CoreError::Other("preset request exceeds 64 KiB".into()));
    }
    if text.trim().is_empty() {
        return Ok(Request::List);
    }
    serde_json::from_str(text).map_err(Into::into)
}
fn revision(preset: &Preset) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(preset)?)))
}
impl Rook {
    pub fn preset_request(&self, request: Request) -> Result<serde_json::Value> {
        let old = self.store.kv_get_limited(KEY, MAX)?;
        let mut all: BTreeMap<String, Preset> = match &old {
            Some(bytes) => serde_json::from_slice(bytes)?,
            None => BTreeMap::new(),
        };
        let describe = |name: &str, p: &Preset| -> Result<serde_json::Value> {
            Ok(serde_json::json!({"name":name, "revision":revision(p)?, "preset":p}))
        };
        match request {
            Request::List => {
                return Ok(
                    serde_json::json!({"items":all.iter().map(|(n,p)|describe(n,p)).collect::<Result<Vec<_>>>()?}),
                );
            }
            Request::Get { name } => {
                let p = all.get(&name).ok_or_else(|| CoreError::Other("preset not found".into()))?;
                if let Some(model) = &p.model {
                    crate::models::usable(&self.config, model).map_err(CoreError::Other)?;
                }
                return describe(&name, p);
            }
            Request::Save { name, preset, expected } => {
                if !rook_skills::usable_name(&name)
                    || name.len() > 64
                    || preset.prompt.is_empty()
                    || preset.prompt.len() > 32768
                    || preset.prompt.contains('\0')
                {
                    return Err(CoreError::Other(
                        "preset needs a short name and 1..=32768 UTF-8 bytes of prompt text".into(),
                    ));
                }
                if let Some(model) = &preset.model {
                    crate::models::usable(&self.config, model).map_err(CoreError::Other)?;
                }
                let current = all.get(&name).map(revision).transpose()?;
                if current != expected {
                    return Err(CoreError::Other("preset changed or already exists; inspect and explicitly confirm the current revision".into()));
                }
                all.insert(name, preset);
            }
            Request::Delete { name, expected } => {
                if all.get(&name).map(revision).transpose()?.as_deref() != Some(expected.as_str()) {
                    return Err(CoreError::Other("preset changed; inspect before deleting".into()));
                }
                all.remove(&name);
            }
        }
        if all.len() > 32 {
            return Err(CoreError::Other("at most 32 presets are stored".into()));
        }
        let bytes = serde_json::to_vec(&all)?;
        if bytes.len() > MAX {
            return Err(CoreError::Other("preset catalog exceeds 256 KiB".into()));
        }
        if !self.store.kv_update_checked(KEY, old.as_deref(), &[(KEY, bytes.as_slice())], None)? {
            return Err(CoreError::Other("another client changed presets; reload before saving".into()));
        }
        Ok(serde_json::json!({"saved":true}))
    }
}
