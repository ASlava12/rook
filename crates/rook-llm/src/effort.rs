//! The effort parameter a dialect actually writes, independently of user intent.
use crate::Effort;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EffortUse {
    Parameter {
        name: &'static str,
        value: String,
    },
    Omitted {
        reason: &'static str,
    },
    /// A provider without a wire mapping must not advertise support by guess.
    Unknown,
}

impl EffortUse {
    pub(crate) fn parameter(name: &'static str, value: impl ToString) -> Self {
        Self::Parameter { name, value: value.to_string() }
    }

    pub fn describe(&self) -> String {
        match self {
            Self::Parameter { name, value } => format!("sent {name}={value}"),
            Self::Omitted { reason } => format!("not sent: {reason}"),
            Self::Unknown => "wire mapping unknown".into(),
        }
    }
}

/// An observation of one accepted HTTP request, not proof of model behavior.
/// Created inside the retry/failover wrappers so it names the answering route.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffortReport {
    pub provider: String,
    pub requested: Effort,
    pub applied: EffortUse,
}

impl EffortReport {
    pub fn describe(&self) -> String {
        format!(
            "{}: effort requested {}; {}",
            self.provider,
            self.requested.as_str(),
            self.applied.describe()
        )
    }
}

/// Match a model family and its dated/variant suffixes without matching a
/// different version (`4-6` must not include `4-60`, nor `5.2` include `5.20`).
pub(crate) fn model_family(model: &str, family: &str) -> bool {
    model.strip_prefix(family).is_some_and(|suffix| suffix.is_empty() || suffix.starts_with('-'))
}
