//! A child's costs are frozen in its parent's history, never reread from a
//! different branch's later state. Billing notes add no token charge.
use crate::{CoreError, Result, Rook};
use rook_store::{EventKind, Kind, NewEvent, Store};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub(crate) const LABEL: &str = "rook:model-delegation:v1";

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum State {
    Started,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Record {
    pub parent_session: String,
    pub child_session: String,
    pub state: State,
    pub child_through: Option<u64>,
    pub snapshot_missing: bool,
    pub coverage: Option<crate::model_route::CostCoverage>,
}

impl Record {
    fn save(&self, store: &Store, parent: u128) -> Result<()> {
        let bytes = crate::persistence::encode_with_limit(self, crate::model_route::MAX_BYTES)?;
        store.append_event_durable(
            parent,
            NewEvent::new(EventKind::Note, Kind::Message, &bytes).label(LABEL),
        )?;
        Ok(())
    }

    pub fn read(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > crate::model_route::MAX_BYTES {
            return Err(CoreError::Other("saved delegated model costs exceed 4096 bytes".into()));
        }
        let record: Self = serde_json::from_slice(bytes)?;
        if rook_store::parse_session_id(&record.parent_session).is_none()
            || rook_store::parse_session_id(&record.child_session).is_none()
            || record.parent_session == record.child_session
        {
            return Err(CoreError::Other("unsupported delegated model cost identity".into()));
        }
        let admission = matches!(record.state, State::Started);
        if (admission
            && (record.child_through.is_some() || record.snapshot_missing || record.coverage.is_some()))
            || (!admission && record.child_through.is_none() && !record.snapshot_missing)
            || (record.snapshot_missing && record.coverage.is_some())
        {
            return Err(CoreError::Other("unsupported delegated model cost snapshot".into()));
        }
        if let Some(c) = &record.coverage
            && [
                c.known_subtotal_usd,
                c.attempt_known_subtotal_usd,
                c.delegated.known_receipt_subtotal_usd,
                c.delegated.known_attempt_subtotal_usd,
            ]
            .into_iter()
            .flatten()
            .any(|value| !value.is_finite() || value < 0.0)
        {
            return Err(CoreError::Other("invalid saved delegated cost estimate".into()));
        }
        Ok(record)
    }
}

pub(crate) struct Guard {
    store: Arc<Store>,
    parent: u128,
    child: u128,
    open: bool,
}

impl Guard {
    pub fn start(rook: &Rook, parent: u128, child: u128) -> Result<Self> {
        let meta = rook
            .store
            .get_session(child)?
            .ok_or_else(|| CoreError::NoSession(rook_store::format_session_id(child)))?;
        if meta.parent != Some(parent) {
            return Err(CoreError::Other("delegated cost child is not owned by this parent".into()));
        }
        Record {
            parent_session: rook_store::format_session_id(parent),
            child_session: rook_store::format_session_id(child),
            state: State::Started,
            child_through: None,
            snapshot_missing: false,
            coverage: None,
        }
        .save(&rook.store, parent)?;
        Ok(Self { store: rook.store.clone(), parent, child, open: true })
    }

    pub fn finish(&mut self, state: State) -> Result<()> {
        if !std::mem::replace(&mut self.open, false) {
            return Ok(());
        }
        let through = self.store.get_session(self.child)?.map(|meta| meta.next_seq);
        let snapshot = through
            .map(|boundary| crate::model_accounting::saved(&self.store, self.child, boundary))
            .transpose();
        let (coverage, missing) = match snapshot {
            Ok(value) => (value.flatten(), through.is_none()),
            Err(_) => (None, true),
        };
        Record {
            parent_session: rook_store::format_session_id(self.parent),
            child_session: rook_store::format_session_id(self.child),
            state,
            child_through: through,
            snapshot_missing: missing,
            coverage,
        }
        .save(&self.store, self.parent)
    }

    pub fn returned<T>(&mut self, result: Result<T>) -> Result<T> {
        if result.is_err() {
            self.finish(State::Failed)?;
        }
        result
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.finish(State::Interrupted);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_route::{Auxiliary, CostCoverage, Purpose};
    use rook_llm::{AttemptFacts, AttemptStatus, Completion, Dispatch, Message, Response, StopReason, Usage};

    fn engine(path: &std::path::Path) -> Rook {
        let mut config = crate::Config::default();
        config.models.insert(
            "physical".into(),
            crate::ModelSource {
                model: "model".into(),
                input_usd_per_million: Some(2.0),
                output_usd_per_million: Some(6.0),
                ..Default::default()
            },
        );
        Rook::from_parts(
            Store::open(path.join("store")).unwrap(),
            config,
            rook_skills::Environment::bare("windows", "x86_64", "0.1.0"),
            rook_skills::SkillIndex::default(),
            path.to_path_buf(),
        )
    }

    fn charged(rook: &Rook, session: u128) {
        let dispatch = Dispatch::bounded("physical", "model", true);
        let usage = Usage { input_tokens: 10, output_tokens: 3, ..Default::default() };
        let vault = Arc::new(crate::Vault::empty());
        let observer = crate::model_attempt::observer(rook, session, vault.clone(), Purpose::Aside);
        let mut attempt = observer.start(dispatch.as_ref()).unwrap();
        attempt
            .finish(
                AttemptStatus::Completed,
                &AttemptFacts {
                    usage: Some(usage.clone()),
                    usage_reported: true,
                    completion_confirmed: true,
                    reported_model: Some("server".into()),
                },
            )
            .unwrap();
        let completion = Completion {
            dispatch,
            usage_reported: true,
            completion_confirmed: true,
            response: Response {
                message: Message::assistant("bill"),
                stop_reason: StopReason::EndTurn,
                usage,
                model: "server".into(),
            },
        };
        Auxiliary::new(
            &rook.config,
            &vault,
            Purpose::Aside,
            "physical",
            &completion,
            std::time::Instant::now(),
        )
        .record(
            rook,
            session,
            NewEvent::new(EventKind::Note, Kind::Message, b"bill").label("btw").usage(10, 3),
        )
        .unwrap();
    }

    fn coverage(rook: &Rook, session: u128) -> CostCoverage {
        rook.context_usage(session, Some(65536)).unwrap().cost_coverage.unwrap()
    }

    #[test]
    fn nested_costs_are_frozen_once_and_forks_cannot_follow_later_child_activity_or_rates() {
        let home = tempfile::tempdir().unwrap();
        let rook = engine(home.path());
        let parent = rook.start_session("parent").unwrap();
        charged(&rook, parent);
        let child = rook.fork_for_subtask(parent, "child").unwrap();
        let mut outer = Guard::start(&rook, parent, child).unwrap();
        let before =
            rook.fork_session(parent, rook.store.get_session(parent).unwrap().unwrap().next_seq).unwrap().id;
        for _ in 0..260 {
            rook.log(child, EventKind::Note, "diagnostic", "not a generation").unwrap();
        }
        assert!(rook.store.get_session(child).unwrap().unwrap().next_seq > 256);
        charged(&rook, child);
        let grandchild = rook.fork_for_subtask(child, "grandchild").unwrap();
        let mut inner = Guard::start(&rook, child, grandchild).unwrap();
        charged(&rook, grandchild);
        inner.finish(State::Completed).unwrap();
        outer.finish(State::Completed).unwrap();
        let boundary = rook.store.get_session(parent).unwrap().unwrap().next_seq;
        outer.finish(State::Completed).unwrap();
        assert_eq!(
            rook.store.get_session(parent).unwrap().unwrap().next_seq,
            boundary,
            "collection cannot charge twice"
        );
        let after = rook.fork_session(parent, boundary).unwrap().id;
        charged(&rook, child);
        charged(&rook, grandchild);
        drop(inner);
        drop(outer);
        drop(rook);
        let mut rook = engine(home.path());
        rook.config.models.get_mut("physical").unwrap().input_usd_per_million = Some(99.0);
        assert_eq!(
            coverage(&rook, before).delegated.pending,
            1,
            "a later child ending cannot cross this prefix"
        );
        for id in [parent, after] {
            let c = coverage(&rook, id);
            assert_eq!((c.delegated.started, c.delegated.completed, c.delegated.pending), (1, 1, 0));
            assert_eq!(
                (c.delegated.captured_sessions, c.delegated.priced_receipts, c.delegated.priced_attempts),
                (2, 2, 2)
            );
            assert_eq!(c.delegated.unfinished_descendants, 0);
            assert!((c.delegated.known_receipt_subtotal_usd.unwrap() - 0.000076).abs() < 1e-15);
            assert_eq!(c.delegated.known_attempt_subtotal_usd, c.delegated.known_receipt_subtotal_usd);
            assert!((c.known_subtotal_usd.unwrap() - 0.000038).abs() < 1e-15);
            let meta = rook.store.get_session(id).unwrap().unwrap();
            assert_eq!(
                (meta.tokens_in, meta.tokens_out),
                (10, 3),
                "the parent does not recharge children's carriers"
            );
            assert!(c.describe().contains("response subtotal: USD 0.00011400"));
            assert!(c.describe().contains("later child activity is excluded"));
            assert!(crate::agent::history::replay(&rook, id).unwrap().is_empty());
        }
        assert_eq!(coverage(&rook, child).auxiliary_receipts, 2, "the child really performed later work");
        let old: CostCoverage = serde_json::from_str(r#"{"main_receipts":1}"#).unwrap();
        assert_eq!(old.delegated.started, 0);
    }

    #[test]
    fn failed_cancelled_unfinished_and_missing_children_remain_visible_without_invented_zero_bills() {
        let home = tempfile::tempdir().unwrap();
        let rook = engine(home.path());
        let parent = rook.start_session("parent").unwrap();
        let failed = rook.fork_for_subtask(parent, "failed").unwrap();
        let mut ledger = Guard::start(&rook, parent, failed).unwrap();
        charged(&rook, failed);
        ledger.finish(State::Failed).unwrap();
        let cancelled = rook.fork_for_subtask(parent, "cancelled").unwrap();
        let ledger = Guard::start(&rook, parent, cancelled).unwrap();
        let observer =
            crate::model_attempt::observer(&rook, cancelled, Arc::new(crate::Vault::empty()), Purpose::Main);
        let _pending = observer.start(None).unwrap();
        drop(ledger);
        let missing = rook.fork_for_subtask(parent, "unreadable").unwrap();
        let mut ledger = Guard::start(&rook, parent, missing).unwrap();
        let too_big = vec![b'x'; crate::model_route::MAX_BYTES + 1];
        assert!(too_big.len() > crate::model_route::MAX_BYTES);
        rook.store
            .append_event(
                missing,
                NewEvent::new(EventKind::Note, Kind::Message, &too_big).label(crate::model_route::LABEL),
            )
            .unwrap();
        ledger.finish(State::Failed).unwrap();
        let running = rook.fork_for_subtask(parent, "process lost").unwrap();
        Record {
            parent_session: rook_store::format_session_id(parent),
            child_session: rook_store::format_session_id(running),
            state: State::Started,
            child_through: None,
            snapshot_missing: false,
            coverage: None,
        }
        .save(&rook.store, parent)
        .unwrap();
        let d = coverage(&rook, parent).delegated;
        assert_eq!((d.started, d.failed, d.interrupted, d.pending), (4, 2, 1, 1));
        assert_eq!((d.captured_sessions, d.missing_snapshots, d.attempts_pending), (2, 1, 1));
        assert!((d.known_attempt_subtotal_usd.unwrap() - 0.000038).abs() < 1e-15);
        assert!(!crate::agent::note_is_for_a_person(LABEL));
        assert!(crate::agent::history::replay(&rook, parent).unwrap().is_empty());
        let empty = rook.start_session("unknown").unwrap();
        let child = rook.fork_for_subtask(empty, "no response").unwrap();
        drop(Guard::start(&rook, empty, child).unwrap());
        let c = coverage(&rook, empty);
        assert!(c.delegated.known_attempt_subtotal_usd.is_none());
        assert!(!c.describe().contains("USD 0.00000000"));
    }

    #[test]
    fn invalid_child_ownership_and_oversized_or_malformed_snapshots_are_rejected_before_copying() {
        let home = tempfile::tempdir().unwrap();
        let rook = engine(home.path());
        let parent = rook.start_session("parent").unwrap();
        let unrelated = rook.start_session("unrelated").unwrap();
        assert!(Guard::start(&rook, parent, unrelated).is_err());
        assert_eq!(rook.store.get_session(parent).unwrap().unwrap().next_seq, 0);
        let child = rook.fork_for_subtask(parent, "child").unwrap();
        let mut record = Record {
            parent_session: rook_store::format_session_id(parent),
            child_session: rook_store::format_session_id(child),
            state: State::Started,
            child_through: None,
            snapshot_missing: false,
            coverage: None,
        };
        record.child_through = Some(0);
        assert!(Record::read(&serde_json::to_vec(&record).unwrap()).is_err());
        record.state = State::Completed;
        record.coverage = Some(CostCoverage { known_subtotal_usd: Some(-1.0), ..Default::default() });
        assert!(Record::read(&serde_json::to_vec(&record).unwrap()).is_err());
        let oversized = vec![b'x'; crate::model_route::MAX_BYTES + 1];
        assert!(oversized.len() > crate::model_route::MAX_BYTES);
        assert!(Record::read(&oversized).unwrap_err().to_string().contains("4096"));
        rook.store
            .append_event(parent, NewEvent::new(EventKind::Note, Kind::Message, &oversized).label(LABEL))
            .unwrap();
        assert!(
            rook.context_usage(parent, Some(65536)).unwrap_err().to_string().contains("exceeds 4096 bytes")
        );
        assert!(
            crate::agent::history::replay(&rook, parent).unwrap().is_empty(),
            "bookkeeping must not be read into model history"
        );
    }
}
