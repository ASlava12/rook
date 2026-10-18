//! Operator-declared controls, restricted to implemented dialect wire fields.
use crate::{Api, Effort, EffortUse, LlmError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Control {
    OpenaiEffort,
    ResponsesEffort,
    AnthropicEffort,
    GoogleLevel,
    GoogleBudget,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Override {
    pub supported: Option<bool>,
    pub control: Option<Control>,
    pub effort_levels: Option<Vec<Effort>>,
    pub budget_min: Option<u32>,
    pub budget_max: Option<u32>,
}

impl Override {
    pub fn validate(&self, api: Api) -> crate::Result<()> {
        let compatible = match self.control {
            Some(Control::OpenaiEffort) => api == Api::OpenAi,
            Some(Control::ResponsesEffort) => api == Api::Responses,
            Some(Control::AnthropicEffort) => api == Api::Anthropic,
            Some(Control::GoogleLevel | Control::GoogleBudget) => api == Api::Google,
            None => true,
        };
        let levels_valid = self.effort_levels.as_ref().is_none_or(|levels| {
            levels.len() <= 5 && levels.iter().enumerate().all(|(i, level)| !levels[..i].contains(level))
        });
        let min = self.budget_min.unwrap_or(512);
        let max = self.budget_max.unwrap_or(32768);
        if !compatible
            || !levels_valid
            || (self.supported == Some(false)
                && (self.control.is_some()
                    || self.effort_levels.is_some()
                    || self.budget_min.is_some()
                    || self.budget_max.is_some()))
            || (self.control != Some(Control::GoogleBudget)
                && (self.budget_min.is_some() || self.budget_max.is_some()))
            || min == 0
            || min > max
            || max > 131072
        {
            return Err(LlmError::Other("reasoning declaration: use a compatible implemented control, unique effort levels and a valid Google budget (1..131072)".into()));
        }
        Ok(())
    }

    pub fn apply(&self, facts: &mut crate::ModelCapabilities) {
        facts.reasoning = self.supported.or_else(|| self.control.map(|_| true)).or(facts.reasoning);
        if let Some(levels) = &self.effort_levels {
            facts.effort_levels = Some(levels.clone());
        }
        if self.supported == Some(false) {
            facts.adaptive_thinking = Some(false);
        }
        if self.control == Some(Control::AnthropicEffort) {
            facts.adaptive_thinking = Some(true);
        }
        facts.reasoning_override = Some(self.clone());
    }

    pub fn parameter(&self, effort: Effort) -> Option<(&'static str, Value)> {
        if self.supported == Some(false) {
            return None;
        }
        Some(match self.control? {
            Control::OpenaiEffort => ("reasoning_effort", json!(effort.as_str())),
            Control::ResponsesEffort => ("reasoning.effort", json!(effort.as_str())),
            Control::AnthropicEffort => (
                "output_config.effort",
                json!(match effort {
                    Effort::XHigh => "high",
                    _ => effort.as_str(),
                }),
            ),
            Control::GoogleLevel => (
                "generationConfig.thinkingConfig.thinkingLevel",
                json!(match effort {
                    Effort::Low => "low",
                    Effort::Medium => "medium",
                    _ => "high",
                }),
            ),
            Control::GoogleBudget => {
                let low = self.budget_min.unwrap_or(512).clamp(1, 131072);
                let high = self.budget_max.unwrap_or(32768).clamp(low, 131072);
                let rank = Effort::ALL.iter().position(|level| *level == effort).unwrap_or(0) as u32;
                ("generationConfig.thinkingConfig.thinkingBudget", json!(low + (high - low) * rank / 4))
            }
        })
    }

    pub fn parameter_for_output(&self, effort: Effort, output: u32) -> Option<(&'static str, Value)> {
        let (name, mut value) = self.parameter(effort)?;
        if self.control == Some(Control::GoogleBudget) {
            value = json!(value.as_u64()?.min(output.saturating_sub(128) as u64));
        }
        Some((name, value))
    }

    pub fn effort_use_for_output(&self, effort: Effort, output: u32) -> EffortUse {
        match self.parameter_for_output(effort, output) {
            Some((name, value)) => EffortUse::parameter(
                name,
                value.as_str().map(str::to_owned).unwrap_or_else(|| value.to_string()),
            ),
            None => self.effort_use(effort),
        }
    }

    pub fn effort_use(&self, effort: Effort) -> EffortUse {
        match self.parameter(effort) {
            Some((name, value)) => EffortUse::parameter(
                name,
                value.as_str().map(str::to_owned).unwrap_or_else(|| value.to_string()),
            ),
            None => {
                EffortUse::Omitted { reason: "configured reasoning controls are disabled or unspecified" }
            }
        }
    }
}

pub(crate) fn effort(request: &crate::Request) -> Option<Effort> {
    if request.model_capabilities.reasoning == Some(false)
        || request
            .model_capabilities
            .reasoning_override
            .as_ref()
            .is_some_and(|settings| settings.supported == Some(false))
    {
        None
    } else {
        request.effort
    }
}
