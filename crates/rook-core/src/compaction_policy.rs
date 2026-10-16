//! Source-scoped compaction knobs; defaults deliberately retain the old policy.
use crate::{Config, ModelSource, context::ContextBudget};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Overrides {
    pub compact_at: Option<f32>,
    pub reserve_output: Option<usize>,
    pub keep_recent_tokens: Option<usize>,
}

pub(crate) fn budget(config: &Config, source: Option<&str>, window: usize) -> ContextBudget {
    let overrides = source.and_then(|name| config.models.get(name)).map(|s| &s.compaction);
    let mut budget =
        ContextBudget::new(window, overrides.and_then(|o| o.compact_at).unwrap_or(config.agent.compact_at));
    if let Some(reserve) = overrides.and_then(|o| o.reserve_output) {
        // A discovered/shrunken window may be smaller than a valid configured policy.
        budget.reserve_output = reserve.max(1).min((window / 2).max(1));
    }
    budget.keep_recent_tokens = overrides
        .and_then(|o| o.keep_recent_tokens)
        .map(|keep| keep.max(1).min(budget.threshold().saturating_sub(1)));
    budget
}

pub(crate) fn errors(name: &str, source: &ModelSource) -> Vec<String> {
    let mut errors = Vec::new();
    let o = &source.compaction;
    if let Some(at) = o.compact_at
        && crate::context::check_compact_at(at).is_err()
    {
        errors.push(format!("models.{name}.compaction.compact_at: expected 0.1..=0.9"));
    }
    for (field, value) in [("reserve_output", o.reserve_output), ("keep_recent_tokens", o.keep_recent_tokens)]
    {
        if let Some(value) = value
            && (!(1..=8_388_608).contains(&value) || source.context_window.is_some_and(|w| value >= w))
        {
            errors.push(format!(
                "models.{name}.compaction.{field}: expected 1..=8388608 and less than context_window"
            ));
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_follows_named_source_and_stays_inside_a_smaller_recovery_window() {
        let mut config = Config::default();
        let source = ModelSource {
            compaction: Overrides {
                compact_at: Some(0.6),
                reserve_output: Some(2048),
                keep_recent_tokens: Some(3000),
            },
            ..Default::default()
        };
        config.models.insert("small".into(), source);
        let original = ContextBudget::new(32768, config.agent.compact_at);
        let default = budget(&config, Some("unconfigured"), 32768);
        assert_eq!(default.threshold(), original.threshold());
        assert_eq!(default.keep_recent_tokens, None);
        let small = budget(&config, Some("small"), 8192);
        assert_eq!(small.reserve_output, 2048);
        assert_eq!(small.keep_recent_tokens, Some(3000));
        assert!(small.needs_compaction(4000));
        assert!(!default.needs_compaction(4000));
        let shrunk = budget(&config, Some("small"), 1024);
        assert_eq!(shrunk.reserve_output, 512);
        assert!(shrunk.keep_recent_tokens.unwrap() < shrunk.threshold());
        config.models.get_mut("small").unwrap().compaction.compact_at = Some(f32::NAN);
        assert!(!errors("small", &config.models["small"]).is_empty());
    }
}
