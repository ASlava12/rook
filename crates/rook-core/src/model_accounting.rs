//! Constant-space accounting over a fixed saved prefix, without replaying text.
use crate::model_route::{CostCoverage, SavedReceipt};
use crate::{CoreError, Result};
use rook_store::{Event, EventKind, Store};

#[derive(Default)]
pub(crate) struct Fold {
    pub coverage: CostCoverage,
    pub last_response: Option<SavedReceipt>,
    usage_events: u64,
}

impl Fold {
    pub fn include(&mut self, store: &Store, event: &Event, bytes: u64) -> Result<()> {
        let kind = event.record.kind;
        let label = event.record.label.as_str();
        if kind == EventKind::Note
            && matches!(
                label,
                crate::model_route::LABEL
                    | crate::model_route::AUX_LABEL
                    | crate::model_attempt::LABEL
                    | crate::model_delegation::LABEL
            )
        {
            if bytes > crate::model_route::MAX_BYTES as u64 {
                return Err(CoreError::Other(format!(
                    "saved model accounting record {label} exceeds 4096 bytes; preserve the store and inspect it"
                )));
            }
            let body = store.get_range(&event.record.body, 0, bytes as usize)?;
            match label {
                crate::model_route::LABEL => {
                    let receipt = crate::model_route::Receipt::read(&body)?;
                    self.coverage.include(&receipt, false);
                    self.last_response = Some(SavedReceipt { event_seq: event.seq, receipt });
                }
                crate::model_route::AUX_LABEL => {
                    self.coverage.include(&crate::model_route::Auxiliary::read(&body)?.receipt, true)
                }
                crate::model_attempt::LABEL => {
                    let attempt = crate::model_attempt::Record::read(&body)?;
                    match attempt.state {
                        crate::model_attempt::State::Started => self.coverage.attempts_started += 1,
                        crate::model_attempt::State::Completed => self.coverage.attempts_completed += 1,
                        crate::model_attempt::State::Failed => self.coverage.attempts_failed += 1,
                        crate::model_attempt::State::Incomplete => self.coverage.attempts_incomplete += 1,
                        crate::model_attempt::State::Interrupted => self.coverage.attempts_interrupted += 1,
                    }
                    if !matches!(attempt.state, crate::model_attempt::State::Started) {
                        if let Some(cost) = attempt.cost {
                            self.coverage.priced_attempts += 1;
                            self.coverage.attempt_known_subtotal_usd =
                                add(self.coverage.attempt_known_subtotal_usd, Some(cost.estimated_usd));
                        } else {
                            self.coverage.unpriced_attempts += 1;
                        }
                    }
                }
                crate::model_delegation::LABEL => {
                    self.coverage.delegated.include(&crate::model_delegation::Record::read(&body)?)
                }
                _ => {}
            }
        }
        if kind == EventKind::AssistantMessage
            || event.record.tokens_in > 0
            || event.record.tokens_out > 0
            || (kind == EventKind::Note
                && matches!(
                    label,
                    "usage" | "completion check" | "btw" | "compaction usage" | "branch summary usage"
                ))
        {
            self.usage_events += 1;
        }
        Ok(())
    }

    pub fn finish(mut self) -> (Option<CostCoverage>, Option<SavedReceipt>) {
        self.coverage.usage_events_without_receipt =
            self.usage_events.saturating_sub(self.coverage.main_receipts + self.coverage.auxiliary_receipts);
        self.coverage.attempts_pending = self.coverage.attempts_started.saturating_sub(
            self.coverage.attempts_completed
                + self.coverage.attempts_failed
                + self.coverage.attempts_incomplete
                + self.coverage.attempts_interrupted,
        );
        let delegated = &mut self.coverage.delegated;
        delegated.pending =
            delegated.started.saturating_sub(delegated.completed + delegated.failed + delegated.interrupted);
        let present = self.usage_events > 0
            || self.coverage.main_receipts + self.coverage.auxiliary_receipts > 0
            || self.coverage.attempts_started > 0
            || delegated.started > 0;
        (present.then_some(self.coverage), self.last_response)
    }
}

pub(crate) fn add(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (None, None) => None,
        _ => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)).filter(|v| v.is_finite()),
    }
}

pub(crate) fn saved(store: &Store, session: u128, through: u64) -> Result<Option<CostCoverage>> {
    let mut fold = Fold::default();
    let mut next = 0;
    while next < through {
        let page = store.events(session, next, 256)?;
        let Some(last) = page.last() else { break };
        next = last.seq.saturating_add(1);
        for event in page.into_iter().take_while(|event| event.seq < through) {
            let bytes = store.stat_object(&event.record.body)?.map(|m| m.size_raw).unwrap_or(0);
            fold.include(store, &event, bytes)?;
        }
    }
    Ok(fold.finish().0)
}
