//! A bounded historical receipt for one main-conversation response. It is not
//! total session spend or a claim about today's configuration/workspace.
use crate::{Result, Vault};
use rook_llm::{Dispatch, Response, Usage};
use serde::{Deserialize, Serialize};

pub(crate) const LABEL: &str = "rook:model-route:v1";
pub(crate) const MAX_BYTES: usize = 4096;
pub(crate) const AUX_LABEL: &str = "rook:model-aux:v1";

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Purpose {
    Main,
    Checking,
    CompletionCheck,
    OutputRepair,
    Compaction,
    Aside,
    FinalAnswer,
    BranchSummary,
}

pub(crate) fn identity(value: &str, vault: &Vault) -> Option<String> {
    (!value.is_empty()
        && value.len() <= 256
        && !value.chars().any(char::is_control)
        && vault.redact(value) == value)
        .then(|| value.to_owned())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Auxiliary {
    pub purpose: Purpose,
    pub receipt: Receipt,
}

impl Auxiliary {
    pub(crate) fn new(
        config: &crate::Config,
        vault: &crate::Vault,
        purpose: Purpose,
        selected: &str,
        completed: &rook_llm::Completion,
        started: std::time::Instant,
    ) -> Self {
        let dispatch = completed
            .dispatch
            .as_ref()
            .and_then(|d| Dispatch::bounded(&d.provider, &d.model, d.input_includes_cache));
        let mut receipt = Receipt::new(selected, "ordinary", dispatch, &completed.response, vault);
        receipt.complete = completed.completion_confirmed;
        receipt.usage_reported = completed.usage_reported;
        receipt.elapsed_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        receipt.price(config);
        Self { purpose, receipt }
    }

    pub(crate) fn record(
        &self,
        rook: &crate::Rook,
        session: u128,
        carrier: rook_store::NewEvent<'_>,
    ) -> Result<()> {
        let bytes = crate::persistence::encode_with_limit(self, MAX_BYTES)?;
        rook.store.append_event_pair(
            session,
            carrier,
            rook_store::NewEvent::new(rook_store::EventKind::Note, rook_store::Kind::Message, &bytes)
                .label(AUX_LABEL),
        )?;
        Ok(())
    }

    pub(crate) fn read(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_BYTES {
            return Err(crate::CoreError::Other("saved auxiliary model receipt exceeds 4096 bytes".into()));
        }
        let auxiliary: Self = serde_json::from_slice(bytes)?;
        auxiliary.receipt.validate()?;
        Ok(auxiliary)
    }
}

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
    fn rates(source: &crate::ModelSource) -> Option<Self> {
        validate_prices(source).ok()?;
        Some(Self {
            estimated_usd: 0.0,
            input_usd_per_million: source.input_usd_per_million?,
            output_usd_per_million: source.output_usd_per_million?,
            cache_read_usd_per_million: source.cache_read_usd_per_million,
            cache_write_usd_per_million: source.cache_write_usd_per_million,
        })
    }

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

/// Only bounded identity hashes and numeric rates survive preparation. Provider
/// construction may copy credentials; this snapshot must never copy Config.
#[derive(Default)]
pub(crate) struct Prices {
    entries: std::collections::BTreeMap<[u8; 32], Cost>,
}

impl Prices {
    pub(crate) const MAX_SOURCES: usize = 512;

    fn key(provider: &str, model: &str) -> Option<[u8; 32]> {
        use sha2::{Digest, Sha256};
        if provider.is_empty()
            || model.is_empty()
            || provider.len() > 256
            || model.len() > 256
            || provider.chars().chain(model.chars()).any(char::is_control)
        {
            return None;
        }
        let mut hash = Sha256::new();
        hash.update(b"rook-price-source-v1");
        hash.update((provider.len() as u64).to_le_bytes());
        hash.update(provider.as_bytes());
        hash.update(model.as_bytes());
        Some(hash.finalize().into())
    }

    pub(crate) fn new(config: &crate::Config) -> Self {
        let mut prices = Self::default();
        for (name, source) in &config.models {
            // Remaining sources stay explicitly unpriced, never USD zero.
            if prices.entries.len() == Self::MAX_SOURCES {
                break;
            }
            let Some(key) = Self::key(name, source.model.trim()) else { continue };
            let Some(rates) = Cost::rates(source) else { continue };
            prices.entries.insert(key, rates);
        }
        prices
    }

    pub(crate) fn estimate(
        &self,
        dispatch: Option<&Dispatch>,
        facts: &rook_llm::AttemptFacts,
    ) -> Option<Cost> {
        if !facts.completion_confirmed || !facts.usage_reported {
            return None;
        }
        let usage = facts.usage.as_ref()?;
        if usage.input_tokens == 0
            && usage.output_tokens == 0
            && usage.cache_read_tokens == 0
            && usage.cache_write_tokens == 0
        {
            return None;
        }
        let dispatch = dispatch?;
        let mut cost = self.entries.get(&Self::key(&dispatch.provider, &dispatch.model)?)?.clone();
        cost.estimated_usd = cost.calculate(usage, dispatch)?;
        Some(cost)
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
        let dispatch = dispatch
            .filter(|d| identity(&d.provider, vault).is_some() && identity(&d.model, vault).is_some());
        Self {
            selected: identity(selected, vault),
            phase: phase.into(),
            dispatch,
            reported_model: identity(&response.model, vault),
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
        let Some(mut cost) = Cost::rates(source) else { return };
        let Some(usd) = cost.calculate(&self.usage, dispatch) else { return };
        cost.estimated_usd = usd;
        self.cost = Some(cost);
    }

    pub(crate) fn read(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_BYTES {
            return Err(crate::CoreError::Other("saved model route receipt exceeds 4096 bytes".into()));
        }
        let receipt: Self = serde_json::from_slice(bytes)?;
        receipt.validate()?;
        Ok(receipt)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        let invalid_cost = self.cost.as_ref().is_some_and(|cost| {
            !self.complete
                || !self.usage_reported
                || !cost.estimated_usd.is_finite()
                || self.dispatch.as_ref().and_then(|d| cost.calculate(&self.usage, d))
                    != Some(cost.estimated_usd)
        });
        if invalid_cost
            || !["ordinary", "analysis", "implementation", "implementation_held"]
                .contains(&self.phase.as_str())
            || self
                .selected
                .iter()
                .chain(self.reported_model.iter())
                .any(|s| s.len() > 256 || s.chars().any(char::is_control))
            || self
                .dispatch
                .as_ref()
                .is_some_and(|d| Dispatch::bounded(&d.provider, &d.model, d.input_includes_cache).is_none())
        {
            return Err(crate::CoreError::Other(
                "unsupported model route receipt; preserve the store and inspect it".into(),
            ));
        }
        Ok(())
    }
}

/// A subset of estimates in saved branch history, including inherited receipts.
/// Omitted native usage and legacy history remain unknown, never zero charges.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CostCoverage {
    pub main_receipts: u64,
    pub auxiliary_receipts: u64,
    pub priced_receipts: u64,
    pub unpriced_receipts: u64,
    pub usage_events_without_receipt: u64,
    pub known_subtotal_usd: Option<f64>,
    pub complete_accounting: bool,
    pub attempts_started: u64,
    pub attempts_completed: u64,
    pub attempts_failed: u64,
    pub attempts_incomplete: u64,
    pub attempts_interrupted: u64,
    pub attempts_pending: u64,
    pub priced_attempts: u64,
    pub unpriced_attempts: u64,
    pub attempt_known_subtotal_usd: Option<f64>,
    pub delegated: DelegatedCosts,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DelegatedCosts {
    pub started: u64,
    pub completed: u64,
    pub failed: u64,
    pub interrupted: u64,
    pub pending: u64,
    pub captured_sessions: u64,
    pub missing_snapshots: u64,
    pub unfinished_descendants: u64,
    pub priced_receipts: u64,
    pub unpriced_receipts: u64,
    pub usage_events_without_receipt: u64,
    pub attempts_started: u64,
    pub attempts_pending: u64,
    pub priced_attempts: u64,
    pub unpriced_attempts: u64,
    pub known_receipt_subtotal_usd: Option<f64>,
    pub known_attempt_subtotal_usd: Option<f64>,
}

impl DelegatedCosts {
    pub(crate) fn include(&mut self, record: &crate::model_delegation::Record) {
        match record.state {
            crate::model_delegation::State::Started => {
                self.started += 1;
                return;
            }
            crate::model_delegation::State::Completed => self.completed += 1,
            crate::model_delegation::State::Failed => self.failed += 1,
            crate::model_delegation::State::Interrupted => self.interrupted += 1,
        }
        if record.snapshot_missing {
            self.missing_snapshots = self.missing_snapshots.saturating_add(1);
            return;
        }
        self.captured_sessions = self.captured_sessions.saturating_add(1);
        if let Some(c) = &record.coverage {
            let nested = &c.delegated;
            self.captured_sessions = self.captured_sessions.saturating_add(nested.captured_sessions);
            self.missing_snapshots = self.missing_snapshots.saturating_add(nested.missing_snapshots);
            self.unfinished_descendants = self
                .unfinished_descendants
                .saturating_add(nested.pending)
                .saturating_add(nested.unfinished_descendants);
            self.priced_receipts =
                self.priced_receipts.saturating_add(c.priced_receipts).saturating_add(nested.priced_receipts);
            self.unpriced_receipts = self
                .unpriced_receipts
                .saturating_add(c.unpriced_receipts)
                .saturating_add(nested.unpriced_receipts);
            self.usage_events_without_receipt = self
                .usage_events_without_receipt
                .saturating_add(c.usage_events_without_receipt)
                .saturating_add(nested.usage_events_without_receipt);
            self.attempts_started = self
                .attempts_started
                .saturating_add(c.attempts_started)
                .saturating_add(nested.attempts_started);
            self.attempts_pending = self
                .attempts_pending
                .saturating_add(c.attempts_pending)
                .saturating_add(nested.attempts_pending);
            self.priced_attempts =
                self.priced_attempts.saturating_add(c.priced_attempts).saturating_add(nested.priced_attempts);
            self.unpriced_attempts = self
                .unpriced_attempts
                .saturating_add(c.unpriced_attempts)
                .saturating_add(nested.unpriced_attempts);
            self.known_receipt_subtotal_usd = crate::model_accounting::add(
                self.known_receipt_subtotal_usd,
                crate::model_accounting::add(c.known_subtotal_usd, nested.known_receipt_subtotal_usd),
            );
            self.known_attempt_subtotal_usd = crate::model_accounting::add(
                self.known_attempt_subtotal_usd,
                crate::model_accounting::add(c.attempt_known_subtotal_usd, nested.known_attempt_subtotal_usd),
            );
        }
    }
}

impl CostCoverage {
    pub(crate) fn include(&mut self, receipt: &Receipt, auxiliary: bool) {
        if auxiliary {
            self.auxiliary_receipts += 1;
        } else {
            self.main_receipts += 1;
        }
        match &receipt.cost {
            Some(cost) => {
                self.priced_receipts += 1;
                self.known_subtotal_usd = Some(self.known_subtotal_usd.unwrap_or(0.0) + cost.estimated_usd);
            }
            None => self.unpriced_receipts += 1,
        }
    }

    pub fn describe(&self) -> String {
        let subtotal = self
            .known_subtotal_usd
            .map(|usd| format!("USD {} configured-rate estimate", amount(usd)))
            .unwrap_or_else(|| "unknown (no priced receipts)".into());
        let attempt_subtotal = self
            .attempt_known_subtotal_usd
            .map(|usd| format!("USD {} configured-rate estimate", amount(usd)))
            .unwrap_or_else(|| "unknown (no priced attempts)".into());
        let mut text = format!(
            "Cost coverage · saved branch history\nKnown subtotal: {subtotal}\nPriced receipts: {} · unpriced receipts: {} · usage events without receipt: {}\nRecorded physical attempts: {} started · {} completed · {} failed · {} incomplete · {} interrupted · {} pending\nAttempt subtotal: {attempt_subtotal} · {} priced · {} unpriced endings\nReceipt and attempt subtotals overlap; do not add them.\nTotal cost is unknown: retry/failure attempts may lack complete usage; legacy history and delegated-session costs can remain uncovered. Inherited receipts are historical, not new charges.\n",
            self.priced_receipts,
            self.unpriced_receipts,
            self.usage_events_without_receipt,
            self.attempts_started,
            self.attempts_completed,
            self.attempts_failed,
            self.attempts_incomplete,
            self.attempts_interrupted,
            self.attempts_pending,
            self.priced_attempts,
            self.unpriced_attempts
        );
        if self.delegated.started > 0 {
            let d = &self.delegated;
            let money = |value: Option<f64>| {
                value
                    .map(|v| format!("USD {} configured-rate estimate", amount(v)))
                    .unwrap_or_else(|| "unknown".into())
            };
            text.push_str(&format!("Recorded delegated sessions: {} started · {} completed · {} failed · {} interrupted · {} pending\nCaptured child history: {} sessions · {} missing snapshots · {} unfinished descendants · {} pending attempts\nChild receipts: {} priced · {} unpriced · {} usage events without receipt\nParent and captured children, response subtotal: {}\nParent and captured children, attempt subtotal: {}\nChild snapshots retain their recorded boundaries; later child activity is excluded. Response and attempt subtotals overlap; do not add them.\n",
                d.started, d.completed, d.failed, d.interrupted, d.pending, d.captured_sessions, d.missing_snapshots, d.unfinished_descendants, d.attempts_pending, d.priced_receipts, d.unpriced_receipts, d.usage_events_without_receipt,
                money(crate::model_accounting::add(self.known_subtotal_usd, d.known_receipt_subtotal_usd)),
                money(crate::model_accounting::add(self.attempt_known_subtotal_usd, d.known_attempt_subtotal_usd))));
        }
        text
    }
}

fn amount(usd: f64) -> String {
    if usd > 0.0 && usd < 1e-8 { format!("{usd:.3e}") } else { format!("{usd:.8}") }
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
            let amount = amount(c.estimated_usd);
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
    fn price_snapshots_retain_bounded_numeric_rates_and_leave_excess_or_unverified_sources_unknown() {
        let mut config = Config::default();
        let source = crate::ModelSource {
            model: "physical-model".into(),
            input_usd_per_million: Some(2.0),
            output_usd_per_million: Some(6.0),
            cache_read_usd_per_million: Some(0.2),
            cache_write_usd_per_million: Some(0.5),
            ..Default::default()
        };
        for index in 0..=Prices::MAX_SOURCES {
            config.models.insert(format!("source-{index:04}"), source.clone());
        }
        assert!(config.models.len() > Prices::MAX_SOURCES, "the snapshot cap must actually be exceeded");
        let prices = Prices::new(&config);
        assert_eq!(prices.entries.len(), Prices::MAX_SOURCES);
        let dispatch = Dispatch::bounded("source-0000", "physical-model", true).unwrap();
        let mut facts = rook_llm::AttemptFacts {
            usage: Some(response().usage),
            usage_reported: true,
            completion_confirmed: true,
            reported_model: Some("server-version".into()),
        };
        let frozen = prices.estimate(Some(&dispatch), &facts).unwrap();
        assert!((frozen.estimated_usd - 0.000123).abs() < 1e-15);
        config.models.get_mut("source-0000").unwrap().input_usd_per_million = Some(99.0);
        assert_eq!(prices.estimate(Some(&dispatch), &facts).unwrap().estimated_usd, frozen.estimated_usd);
        let excess =
            Dispatch::bounded(&format!("source-{:04}", Prices::MAX_SOURCES), "physical-model", true).unwrap();
        assert!(prices.estimate(Some(&excess), &facts).is_none());
        assert!(Prices::key("source", &"🙂".repeat(65)).is_none());
        facts.completion_confirmed = false;
        assert!(prices.estimate(Some(&dispatch), &facts).is_none());
        facts.completion_confirmed = true;
        facts.usage_reported = false;
        assert!(prices.estimate(Some(&dispatch), &facts).is_none());
    }

    #[test]
    fn auxiliary_costs_survive_reopen_and_forks_without_becoming_context_or_recharging_usage() {
        let home = tempfile::tempdir().unwrap();
        let mut rook = engine(home.path());
        rook.config.models.insert(
            "physical-source".into(),
            crate::ModelSource {
                model: "physical-model".into(),
                input_usd_per_million: Some(2.0),
                output_usd_per_million: Some(6.0),
                cache_read_usd_per_million: Some(0.2),
                cache_write_usd_per_million: Some(3.0),
                ..Default::default()
            },
        );
        let session = rook.start_session("auxiliary accounting").unwrap();
        // Force the context metadata reader to cross its bounded page boundary.
        for _ in 0..260 {
            rook.log(session, EventKind::Note, "diagnostic", "not a generation").unwrap();
        }
        assert!(rook.store.get_session(session).unwrap().unwrap().next_seq > 256);
        let mut completed = rook_llm::Completion {
            response: response(),
            dispatch: Dispatch::bounded("physical-source", "physical-model", true),
            usage_reported: true,
            completion_confirmed: true,
        };
        let vault = Vault::empty();
        let auxiliary = Auxiliary::new(
            &rook.config,
            &vault,
            Purpose::CompletionCheck,
            "preferred",
            &completed,
            std::time::Instant::now(),
        );
        let estimate = auxiliary.receipt.cost.as_ref().unwrap().estimated_usd;
        auxiliary
            .record(
                &rook,
                session,
                rook_store::NewEvent::new(EventKind::Note, rook_store::Kind::Message, b"checked")
                    .label("completion check")
                    .usage(120, 7),
            )
            .unwrap();
        let usage = rook.context_usage(session, Some(65536)).unwrap();
        assert!(usage.last_response.is_none(), "an auxiliary call cannot replace the main response");
        assert!(crate::agent::history::replay(&rook, session).unwrap().is_empty());
        let before = rook.store.get_session(session).unwrap().unwrap();
        assert_eq!((before.tokens_in, before.tokens_out), (120, 7), "receipt carries no second token charge");
        completed.dispatch = None;
        Auxiliary::new(
            &rook.config,
            &vault,
            Purpose::OutputRepair,
            "custom",
            &completed,
            std::time::Instant::now(),
        )
        .record(
            &rook,
            session,
            rook_store::NewEvent::new(EventKind::Note, rook_store::Kind::Message, b"repair usage")
                .label("usage")
                .usage(120, 7),
        )
        .unwrap();
        rook.store
            .append_event(
                session,
                rook_store::NewEvent::new(
                    EventKind::AssistantMessage,
                    rook_store::Kind::Message,
                    b"legacy answer",
                )
                .usage(9, 4),
            )
            .unwrap();
        let coverage = rook.context_usage(session, Some(65536)).unwrap().cost_coverage.unwrap();
        assert_eq!(
            (
                coverage.main_receipts,
                coverage.auxiliary_receipts,
                coverage.priced_receipts,
                coverage.unpriced_receipts,
                coverage.usage_events_without_receipt
            ),
            (0, 2, 1, 1, 1)
        );
        assert_eq!(coverage.known_subtotal_usd, Some(estimate));
        assert!(!coverage.complete_accounting, "retry/failure/child/branch-summary costs remain uncovered");
        assert!(coverage.describe().contains("Total cost is unknown"));
        let end = rook.store.get_session(session).unwrap().unwrap().next_seq;
        let child = rook.fork_session(session, end).unwrap().id;
        drop(rook);
        // Current rates/models are absent. Saved estimates retain their snapshot.
        let rook = engine(home.path());
        for id in [session, child] {
            let coverage = rook.context_usage(id, Some(65536)).unwrap().cost_coverage.unwrap();
            assert_eq!(coverage.known_subtotal_usd, Some(estimate));
            assert_eq!(coverage.usage_events_without_receipt, 1);
        }
    }

    #[test]
    fn auxiliary_receipt_limits_and_secret_redaction_apply_before_copying_and_replay() {
        let home = tempfile::tempdir().unwrap();
        let rook = engine(home.path());
        let session = rook.start_session("bounded auxiliary report").unwrap();
        let vault = Vault::empty();
        vault.also_hide("private-access-token");
        let mut completed = rook_llm::Completion {
            response: response(),
            dispatch: Dispatch::bounded("source", "private-access-token", true),
            usage_reported: true,
            completion_confirmed: true,
        };
        completed.response.model = "private-access-token".into();
        let auxiliary = Auxiliary::new(
            &rook.config,
            &vault,
            Purpose::Aside,
            "private-access-token",
            &completed,
            std::time::Instant::now(),
        );
        let mut bytes = serde_json::to_vec(&auxiliary).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("private-access-token"));
        bytes.resize(MAX_BYTES + 1, b' ');
        assert!(bytes.len() > MAX_BYTES && serde_json::from_slice::<Auxiliary>(&bytes).is_ok());
        rook.log(session, EventKind::Note, AUX_LABEL, std::str::from_utf8(&bytes).unwrap()).unwrap();
        assert!(crate::agent::history::replay(&rook, session).unwrap().is_empty());
        let before = rook.store.get_session(session).unwrap().unwrap().next_seq;
        assert!(
            rook.context_usage(session, Some(65536)).unwrap_err().to_string().contains("exceeds 4096 bytes")
        );
        assert_eq!(rook.store.get_session(session).unwrap().unwrap().next_seq, before);
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
