//! A bounded historical receipt for one main-conversation response. It is not
//! total session spend or a claim about today's configuration/workspace.
use crate::{Result, Vault};
use rook_llm::{Dispatch, Response, Usage};
use serde::{Deserialize, Serialize};

pub(crate) const LABEL: &str = "rook:model-route:v1";
pub(crate) const MAX_BYTES: usize = 4096;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cost {
    pub estimated_usd: f64,
    pub input_usd_per_million: f64,
    pub output_usd_per_million: f64,
    pub cache_read_usd_per_million: Option<f64>,
    pub cache_write_usd_per_million: Option<f64>,
}

fn valid_rate(rate: f64) -> bool {
    rate.is_finite() && (0.0..=1_000_000.0).contains(&rate)
}

pub(crate) fn validate_prices(source: &crate::ModelSource) -> rook_llm::Result<()> {
    if [
        source.input_usd_per_million,
        source.output_usd_per_million,
        source.cache_read_usd_per_million,
        source.cache_write_usd_per_million,
    ]
    .into_iter()
    .flatten()
    .any(|rate| !valid_rate(rate))
    {
        return Err(rook_llm::LlmError::Other(
            "model USD rates per million tokens must be finite and between 0 and 1000000".into(),
        ));
    }
    Ok(())
}

impl Cost {
    fn calculate(&self, usage: &Usage, dispatch: &Dispatch) -> Option<f64> {
        if !valid_rate(self.input_usd_per_million)
            || !valid_rate(self.output_usd_per_million)
            || [self.cache_read_usd_per_million, self.cache_write_usd_per_million]
                .into_iter()
                .flatten()
                .any(|rate| !valid_rate(rate))
        {
            return None;
        }
        let cache = u64::from(usage.cache_read_tokens) + u64::from(usage.cache_write_tokens);
        let fresh = if dispatch.input_includes_cache {
            u64::from(usage.input_tokens).checked_sub(cache)?
        } else {
            u64::from(usage.input_tokens)
        };
        let read = if usage.cache_read_tokens == 0 {
            0.0
        } else {
            f64::from(usage.cache_read_tokens) * self.cache_read_usd_per_million?
        };
        let write = if usage.cache_write_tokens == 0 {
            0.0
        } else {
            f64::from(usage.cache_write_tokens) * self.cache_write_usd_per_million?
        };
        Some(
            (fresh as f64 * self.input_usd_per_million
                + f64::from(usage.output_tokens) * self.output_usd_per_million
                + read
                + write)
                / 1_000_000.0,
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Receipt {
    pub selected: Option<String>,
    pub phase: String,
    pub dispatch: Option<Dispatch>,
    pub reported_model: Option<String>,
    pub usage: Usage,
    #[serde(default)]
    pub complete: bool,
    #[serde(default)]
    pub usage_reported: bool,
    #[serde(default)]
    pub elapsed_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<Cost>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedReceipt {
    pub event_seq: u64,
    pub receipt: Receipt,
}

impl Receipt {
    pub(crate) fn new(
        selected: &str,
        phase: &str,
        dispatch: Option<Dispatch>,
        response: &Response,
        vault: &Vault,
    ) -> Self {
        let identity = |s: &str| {
            (!s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control) && vault.redact(s) == s)
                .then(|| s.to_owned())
        };
        let dispatch = dispatch.filter(|d| identity(&d.provider).is_some() && identity(&d.model).is_some());
        Self {
            selected: identity(selected),
            phase: phase.into(),
            dispatch,
            reported_model: identity(&response.model),
            usage: response.usage.clone(),
            complete: false,
            usage_reported: false,
            elapsed_ms: 0,
            cost: None,
        }
    }

    pub(crate) fn price(&mut self, config: &crate::Config) {
        self.cost = None;
        if !self.complete
            || !self.usage_reported
            || (self.usage.input_tokens == 0
                && self.usage.output_tokens == 0
                && self.usage.cache_read_tokens == 0
                && self.usage.cache_write_tokens == 0)
        {
            return;
        }
        let Some(dispatch) = &self.dispatch else { return };
        let Some(source) = config.models.get(&dispatch.provider).filter(|s| s.model.trim() == dispatch.model)
        else {
            return;
        };
        let (Some(input), Some(output)) = (source.input_usd_per_million, source.output_usd_per_million)
        else {
            return;
        };
        let mut cost = Cost {
            estimated_usd: 0.0,
            input_usd_per_million: input,
            output_usd_per_million: output,
            cache_read_usd_per_million: source.cache_read_usd_per_million,
            cache_write_usd_per_million: source.cache_write_usd_per_million,
        };
        let Some(usd) = cost.calculate(&self.usage, dispatch) else { return };
        cost.estimated_usd = usd;
        self.cost = Some(cost);
    }

    pub(crate) fn read(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_BYTES {
            return Err(crate::CoreError::Other("saved model route receipt exceeds 4096 bytes".into()));
        }
        let receipt: Self = serde_json::from_slice(bytes)?;
        let invalid_cost = receipt.cost.as_ref().is_some_and(|cost| {
            !receipt.complete
                || !receipt.usage_reported
                || !cost.estimated_usd.is_finite()
                || receipt.dispatch.as_ref().and_then(|d| cost.calculate(&receipt.usage, d))
                    != Some(cost.estimated_usd)
        });
        if invalid_cost
            || !["ordinary", "analysis", "implementation", "implementation_held"]
                .contains(&receipt.phase.as_str())
            || receipt
                .selected
                .iter()
                .chain(receipt.reported_model.iter())
                .any(|s| s.len() > 256 || s.chars().any(char::is_control))
            || receipt
                .dispatch
                .as_ref()
                .is_some_and(|d| Dispatch::bounded(&d.provider, &d.model, d.input_includes_cache).is_none())
        {
            return Err(crate::CoreError::Other(
                "unsupported model route receipt; preserve the store and inspect it".into(),
            ));
        }
        Ok(receipt)
    }
}

/// Text fallback shared by CLI and TUI. Counters belong to this one response;
/// missing pricing is unknown, never a zero-dollar estimate.
pub fn describe(saved: &SavedReceipt) -> String {
    let r = &saved.receipt;
    let physical = r
        .dispatch
        .as_ref()
        .map(|d| format!("{} / {}", d.provider, d.model))
        .unwrap_or_else(|| "unknown (legacy/custom provider)".into());
    let money = r
        .cost
        .as_ref()
        .map(|c| {
            let amount = if c.estimated_usd > 0.0 && c.estimated_usd < 1e-8 {
                format!("{:.3e}", c.estimated_usd)
            } else {
                format!("{:.8}", c.estimated_usd)
            };
            format!("USD {amount} estimate from recorded configured rates; not an invoice")
        })
        .unwrap_or_else(|| "unknown (missing pricing, identity or complete usage)".into());
    format!(
        "Last response · event #{} · historical receipt\nSelected: {} · phase {}\nDispatched: {}\nReported model: {} (adapter value; configured fallback if omitted by server)\nProvider counters: {} input · {} output · {} cache read · {} cache write\nMonetary cost: {}\nElapsed: {} ms · completion confirmed: {} · input/output counters reported: {}\nThis is one response, not total session spend. Zero counters may mean omitted server usage.\n",
        saved.event_seq,
        r.selected.as_deref().unwrap_or("unknown"),
        r.phase,
        physical,
        r.reported_model.as_deref().unwrap_or("unknown"),
        r.usage.input_tokens,
        r.usage.output_tokens,
        r.usage.cache_read_tokens,
        r.usage.cache_write_tokens,
        money,
        r.elapsed_ms,
        r.complete,
        r.usage_reported
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Config, Rook};
    use rook_llm::{Message, StopReason};
    use rook_store::{EventKind, Store};
    fn engine(root: &std::path::Path) -> Rook {
        Rook::from_parts(
            Store::open(root.join("store")).unwrap(),
            Config::default(),
            rook_skills::Environment::bare("linux", "x86_64", "0.1.0"),
            rook_skills::SkillIndex::default(),
            root.into(),
        )
    }
    fn response() -> Response {
        Response {
            message: Message::assistant("reply"),
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 120,
                output_tokens: 7,
                cache_read_tokens: 80,
                cache_write_tokens: 10,
            },
            model: "server-echo".into(),
        }
    }
    #[test]
    fn known_secrets_are_not_copied_into_model_identity_receipts() {
        let vault = Vault::empty();
        vault.also_hide("private-access-token");
        let mut answer = response();
        answer.model = "private-access-token".into();
        let receipt = Receipt::new(
            "private-access-token",
            "ordinary",
            Dispatch::bounded("source", "private-access-token", true),
            &answer,
            &vault,
        );
        let bytes = crate::persistence::encode_with_limit(&receipt, MAX_BYTES).unwrap();
        assert!(!String::from_utf8(bytes).unwrap().contains("private-access-token"));
        assert!(receipt.selected.is_none());
        assert!(receipt.dispatch.is_none());
        assert!(receipt.reported_model.is_none());
    }

    #[test]
    fn configured_rate_estimates_use_actual_dispatch_and_do_not_double_count_cached_input() {
        let mut config = Config::default();
        config.models.insert(
            "fallback".into(),
            crate::ModelSource {
                model: "physical-fast".into(),
                input_usd_per_million: Some(2.0),
                output_usd_per_million: Some(6.0),
                cache_read_usd_per_million: Some(0.2),
                cache_write_usd_per_million: Some(3.0),
                ..Default::default()
            },
        );
        let mut inclusive = Receipt::new(
            "preferred",
            "implementation",
            Dispatch::bounded("fallback", "physical-fast", true),
            &response(),
            &Vault::empty(),
        );
        inclusive.complete = true;
        inclusive.price(&config);
        assert!(inclusive.cost.is_none(), "a completion marker does not prove counters were reported");
        inclusive.usage_reported = true;
        inclusive.price(&config);
        assert!(
            (inclusive.cost.as_ref().unwrap().estimated_usd
                - (30.0 * 2.0 + 7.0 * 6.0 + 80.0 * 0.2 + 10.0 * 3.0) / 1_000_000.0)
                .abs()
                < 1e-12
        );
        let bytes = crate::persistence::encode_with_limit(&inclusive, MAX_BYTES).unwrap();
        assert!(Receipt::read(&bytes).unwrap().cost.is_some());
        let mut separate = Receipt::new(
            "preferred",
            "implementation",
            Dispatch::bounded("fallback", "physical-fast", false),
            &response(),
            &Vault::empty(),
        );
        separate.complete = true;
        separate.usage_reported = true;
        separate.price(&config);
        assert!(
            (separate.cost.as_ref().unwrap().estimated_usd
                - (120.0 * 2.0 + 7.0 * 6.0 + 80.0 * 0.2 + 10.0 * 3.0) / 1_000_000.0)
                .abs()
                < 1e-12
        );
        separate.usage.input_tokens = 0;
        separate.price(&config);
        assert!(separate.cost.is_some(), "an explicitly reported zero fresh input can be fully cached");
        separate.usage_reported = false;
        separate.price(&config);
        assert!(separate.cost.is_none(), "an omitted primary counter must not be priced as zero");
        config.models.get_mut("fallback").unwrap().input_usd_per_million = Some(999.0);
        assert_eq!(
            Receipt::read(&bytes).unwrap().cost.unwrap().input_usd_per_million,
            2.0,
            "historical rates stay recorded"
        );
        config.models.get_mut("fallback").unwrap().cache_read_usd_per_million = None;
        let mut unknown = inclusive.clone();
        unknown.cost = None;
        unknown.price(&config);
        assert!(unknown.cost.is_none(), "missing cache price is unknown, not free");
        config.models.get_mut("fallback").unwrap().cache_read_usd_per_million = Some(0.2);
        unknown.complete = false;
        unknown.price(&config);
        assert!(unknown.cost.is_none());
        config.models.get_mut("fallback").unwrap().output_usd_per_million = Some(f64::INFINITY);
        assert!(validate_prices(config.models.get("fallback").unwrap()).is_err());
        config.models.get_mut("fallback").unwrap().output_usd_per_million = Some(-1.0);
        assert!(validate_prices(config.models.get("fallback").unwrap()).is_err());
    }

    #[test]
    fn response_receipt_is_atomic_with_visible_and_opaque_state_and_survives_only_its_fork_prefix() {
        let home = tempfile::tempdir().unwrap();
        let rook = engine(home.path());
        let session = rook.start_session("routing report").unwrap();
        let mut response = response();
        response.message.reasoning.push(serde_json::json!({"type":"thinking","signature":"unchanged"}));
        let receipt = Receipt::new(
            "analysis",
            "implementation",
            Dispatch::bounded("fallback", "physical-fast", true),
            &response,
            &Vault::empty(),
        );
        let state =
            crate::provider_history::record(&rook, session, &response, &Vault::empty(), Some(&receipt))
                .unwrap()
                .unwrap();
        let events = rook.store.events(session, state, 4).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].record.label, crate::provider_history::LABEL);
        assert_eq!(events[1].record.kind, EventKind::AssistantMessage);
        assert_eq!(events[2].record.label, LABEL);
        let early = rook.fork_session(session, events[2].seq).unwrap().id;
        assert!(rook.context_usage(early, Some(65536)).unwrap().last_response.is_none());
        let late = rook.fork_session(session, events[2].seq + 1).unwrap().id;
        let saved = rook.context_usage(late, Some(65536)).unwrap().last_response.unwrap();
        assert_eq!(saved.receipt.dispatch.unwrap().model, "physical-fast");
        assert_eq!(saved.receipt.reported_model.as_deref(), Some("server-echo"));
        assert_eq!(saved.receipt.usage.cache_write_tokens, 10);
        let messages = crate::agent::history::replay(&rook, session).unwrap();
        assert!(!serde_json::to_string(&messages).unwrap().contains("physical-fast"));
        drop(rook);
        let rook = engine(home.path());
        assert_eq!(
            rook.context_usage(session, Some(65536))
                .unwrap()
                .last_response
                .unwrap()
                .receipt
                .usage
                .cache_read_tokens,
            80
        );
    }
    #[test]
    fn oversized_saved_receipts_are_refused_before_context_replay_copies_them() {
        let home = tempfile::tempdir().unwrap();
        let rook = engine(home.path());
        let session = rook.start_session("bounded report").unwrap();
        let receipt = Receipt::new("analysis", "analysis", None, &response(), &Vault::empty());
        let mut bytes = serde_json::to_vec(&receipt).unwrap();
        bytes.resize(MAX_BYTES + 1, b' ');
        assert!(bytes.len() > MAX_BYTES);
        assert!(serde_json::from_slice::<Receipt>(&bytes).is_ok());
        rook.log(session, EventKind::Note, LABEL, std::str::from_utf8(&bytes).unwrap()).unwrap();
        let before = rook.store.get_session(session).unwrap().unwrap().next_seq;
        assert!(
            rook.context_usage(session, Some(65536)).unwrap_err().to_string().contains("exceeds 4096 bytes")
        );
        assert_eq!(rook.store.get_session(session).unwrap().unwrap().next_seq, before);
        let huge = "🙂".repeat(65);
        assert!(huge.len() > 256);
        assert!(Dispatch::bounded("source", &huge, true).is_none());
        let mut assembler = rook_llm::Assembler::default();
        assert!(
            assembler
                .push(rook_llm::Delta::Dispatch(Dispatch {
                    provider: "source".into(),
                    model: huge,
                    input_includes_cache: true
                }))
                .is_err()
        );
        assert!(assembler.dispatch().is_none());
    }
}
