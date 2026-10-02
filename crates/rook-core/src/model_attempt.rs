//! Physical generation attempts. Notes carry evidence, never a second token charge.
use std::sync::Arc;
use std::time::Instant;

use rook_llm::{AttemptFacts, AttemptStatus, Dispatch};
use rook_store::{EventKind, Kind, NewEvent, Store};
use serde::{Deserialize, Serialize};

use crate::model_route::{MAX_BYTES, Purpose};
use crate::{CoreError, Result, Rook, Vault};

pub(crate) const LABEL: &str = "rook:model-attempt:v1";

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum State {
    Started,
    Completed,
    Failed,
    Incomplete,
    Interrupted,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Record {
    pub id: String,
    pub purpose: Purpose,
    pub state: State,
    pub dispatch: Option<Dispatch>,
    pub usage: Option<rook_llm::Usage>,
    pub usage_reported: bool,
    pub completion_confirmed: bool,
    pub reported_model: Option<String>,
    pub elapsed_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<crate::model_route::Cost>,
}

impl Record {
    fn save(&self, store: &Store, session: u128) -> rook_llm::Result<()> {
        let bytes = crate::persistence::encode_with_limit(self, MAX_BYTES)
            .map_err(|error| rook_llm::LlmError::Other(format!("cannot encode model attempt: {error}")))?;
        // Admission must survive process loss once the leaf has opened HTTP.
        // Endings must also survive a later crash before the turn's usual flush.
        store
            .append_event_durable(session, NewEvent::new(EventKind::Note, Kind::Message, &bytes).label(LABEL))
            .map_err(|error| rook_llm::LlmError::Other(format!("cannot save model attempt: {error}")))?;
        Ok(())
    }

    pub(crate) fn read(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_BYTES {
            return Err(CoreError::Other(
                "saved model attempt exceeds 4096 bytes; preserve the store and inspect it".into(),
            ));
        }
        let record: Self = serde_json::from_slice(bytes)?;
        rook_store::parse_session_id(&record.id)
            .ok_or_else(|| CoreError::Other("unsupported saved model attempt ID".into()))?;
        if record
            .dispatch
            .as_ref()
            .is_some_and(|d| Dispatch::bounded(&d.provider, &d.model, d.input_includes_cache).is_none())
            || record
                .reported_model
                .as_ref()
                .is_some_and(|name| name.len() > 256 || name.chars().any(char::is_control))
        {
            return Err(CoreError::Other("unsupported saved model attempt identity".into()));
        }
        if record.cost.is_some() {
            crate::model_route::Receipt {
                selected: None,
                phase: "ordinary".into(),
                dispatch: record.dispatch.clone(),
                reported_model: record.reported_model.clone(),
                usage: record
                    .usage
                    .clone()
                    .ok_or_else(|| CoreError::Other("priced model attempt has no usage".into()))?,
                complete: record.completion_confirmed,
                usage_reported: record.usage_reported,
                elapsed_ms: record.elapsed_ms,
                cost: record.cost.clone(),
            }
            .validate()?;
            if matches!(record.state, State::Started) {
                return Err(CoreError::Other("model admission cannot contain a cost estimate".into()));
            }
        }
        Ok(record)
    }
}

pub(crate) fn observer(
    rook: &Rook,
    session: u128,
    vault: Arc<Vault>,
    purpose: Purpose,
) -> Arc<dyn rook_llm::AttemptObserver> {
    observer_with_prices(
        rook,
        session,
        vault,
        purpose,
        Arc::new(crate::model_route::Prices::new(&rook.config)),
    )
}

pub(crate) fn observer_with_prices(
    rook: &Rook,
    session: u128,
    vault: Arc<Vault>,
    purpose: Purpose,
    prices: Arc<crate::model_route::Prices>,
) -> Arc<dyn rook_llm::AttemptObserver> {
    Arc::new(Observer { store: rook.store.clone(), session, vault, purpose, prices })
}

struct Observer {
    store: Arc<Store>,
    session: u128,
    vault: Arc<Vault>,
    purpose: Purpose,
    prices: Arc<crate::model_route::Prices>,
}

impl rook_llm::AttemptObserver for Observer {
    fn start(&self, dispatch: Option<&Dispatch>) -> rook_llm::Result<Box<dyn rook_llm::Attempt>> {
        let dispatch = dispatch.and_then(|d| {
            crate::model_route::identity(&d.provider, &self.vault)?;
            crate::model_route::identity(&d.model, &self.vault)?;
            Dispatch::bounded(&d.provider, &d.model, d.input_includes_cache)
        });
        let record = Record {
            id: rook_store::format_session_id(rook_store::new_session_id()),
            purpose: self.purpose,
            state: State::Started,
            dispatch,
            usage: None,
            usage_reported: false,
            completion_confirmed: false,
            reported_model: None,
            elapsed_ms: 0,
            cost: None,
        };
        // A failed admission prevents the leaf from opening its HTTP request.
        record.save(&self.store, self.session)?;
        Ok(Box::new(Active {
            store: self.store.clone(),
            session: self.session,
            vault: self.vault.clone(),
            record,
            started: Instant::now(),
            prices: self.prices.clone(),
        }))
    }
}

struct Active {
    store: Arc<Store>,
    session: u128,
    vault: Arc<Vault>,
    record: Record,
    started: Instant,
    prices: Arc<crate::model_route::Prices>,
}

impl rook_llm::Attempt for Active {
    fn finish(&mut self, status: AttemptStatus, facts: &AttemptFacts) -> rook_llm::Result<()> {
        self.record.state = match status {
            AttemptStatus::Completed => State::Completed,
            AttemptStatus::Failed => State::Failed,
            AttemptStatus::Incomplete => State::Incomplete,
            AttemptStatus::Interrupted => State::Interrupted,
        };
        self.record.usage = facts.usage.clone();
        self.record.usage_reported = facts.usage_reported;
        self.record.completion_confirmed = facts.completion_confirmed;
        self.record.reported_model =
            facts.reported_model.as_deref().and_then(|name| crate::model_route::identity(name, &self.vault));
        self.record.elapsed_ms = self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        self.record.cost = self.prices.estimate(self.record.dispatch.as_ref(), facts);
        self.record.save(&self.store, self.session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rook_llm::AttemptObserver;

    fn engine(path: &std::path::Path) -> Rook {
        Rook::from_parts(
            Store::open(path.join("store")).unwrap(),
            crate::Config::default(),
            rook_skills::Environment::bare("windows", "x86_64", "0.1.0"),
            rook_skills::SkillIndex::default(),
            path.to_path_buf(),
        )
    }

    #[test]
    fn priced_attempts_survive_without_a_response_receipt_and_cannot_be_repriced_or_forged() {
        let home = tempfile::tempdir().unwrap();
        let mut rook = engine(home.path());
        rook.config.models.insert(
            "physical".into(),
            crate::ModelSource {
                model: "model".into(),
                input_usd_per_million: Some(2.0),
                output_usd_per_million: Some(6.0),
                ..Default::default()
            },
        );
        let session = rook.start_session("priced interruption").unwrap();
        let observed = observer(&rook, session, Arc::new(Vault::empty()), Purpose::Main);
        let mut active = observed.start(Dispatch::bounded("physical", "model", true).as_ref()).unwrap();
        rook.config.models.get_mut("physical").unwrap().input_usd_per_million = Some(99.0);
        active
            .finish(
                AttemptStatus::Interrupted,
                &AttemptFacts {
                    usage: Some(rook_llm::Usage { input_tokens: 10, output_tokens: 3, ..Default::default() }),
                    usage_reported: true,
                    completion_confirmed: true,
                    reported_model: Some("server".into()),
                },
            )
            .unwrap();
        let end = rook.store.get_session(session).unwrap().unwrap().next_seq;
        let fork = rook.fork_session(session, end).unwrap().id;
        let event = rook.store.events(session, 0, 8).unwrap().pop().unwrap();
        let bytes = rook.store.get(&event.record.body).unwrap();
        let mut record = Record::read(&bytes).unwrap();
        assert_eq!(record.cost.as_ref().unwrap().input_usd_per_million, 2.0);
        record.cost.as_mut().unwrap().estimated_usd += 1.0;
        assert!(Record::read(&serde_json::to_vec(&record).unwrap()).is_err());
        let mut old: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        old.as_object_mut().unwrap().remove("cost");
        assert!(Record::read(&serde_json::to_vec(&old).unwrap()).unwrap().cost.is_none());
        drop(active);
        drop(observed);
        drop(rook);
        let rook = engine(home.path());
        for id in [session, fork] {
            let context = rook.context_usage(id, Some(65536)).unwrap();
            assert!(context.last_response.is_none());
            let coverage = context.cost_coverage.unwrap();
            assert_eq!(coverage.priced_attempts, 1);
            assert_eq!(coverage.unpriced_attempts, 0);
            assert!(coverage.known_subtotal_usd.is_none(), "no response receipt was saved");
            assert!((coverage.attempt_known_subtotal_usd.unwrap() - 0.000038).abs() < 1e-15);
            assert!(!coverage.complete_accounting);
            let meta = rook.store.get_session(id).unwrap().unwrap();
            assert_eq!(
                (meta.tokens_in, meta.tokens_out),
                (0, 0),
                "physical receipts add no second token charge"
            );
        }
    }

    #[test]
    fn admissions_reopen_and_fork_as_pending_until_their_saved_boundary_contains_an_ending() {
        let home = tempfile::tempdir().unwrap();
        let rook = engine(home.path());
        let session = rook.start_session("attempt history").unwrap();
        let vault = Arc::new(Vault::empty());
        let observer = Observer {
            store: rook.store.clone(),
            session,
            vault: vault.clone(),
            purpose: Purpose::Main,
            prices: Arc::new(crate::model_route::Prices::new(&rook.config)),
        };
        let dispatch = Dispatch::bounded("physical", "model", true).unwrap();
        let mut first = observer.start(Some(&dispatch)).unwrap();
        let boundary = rook.store.get_session(session).unwrap().unwrap().next_seq;
        let mut second = observer.start(Some(&dispatch)).unwrap();
        let facts = AttemptFacts {
            usage: Some(rook_llm::Usage { input_tokens: 10, output_tokens: 3, ..Default::default() }),
            usage_reported: true,
            completion_confirmed: false,
            reported_model: Some("echo".into()),
        };
        second.finish(AttemptStatus::Failed, &facts).unwrap();
        let coverage = rook.context_usage(session, Some(65536)).unwrap().cost_coverage.unwrap();
        assert_eq!(
            (coverage.attempts_started, coverage.attempts_failed, coverage.attempts_pending),
            (2, 1, 1)
        );
        assert!(coverage.known_subtotal_usd.is_none() && !coverage.complete_accounting);
        let before = rook.fork_session(session, boundary).unwrap().id;
        first.finish(AttemptStatus::Interrupted, &AttemptFacts::default()).unwrap();
        let end = rook.store.get_session(session).unwrap().unwrap().next_seq;
        let after = rook.fork_session(session, end).unwrap().id;
        let meta = rook.store.get_session(session).unwrap().unwrap();
        assert_eq!((meta.tokens_in, meta.tokens_out), (0, 0), "attempt evidence never charges usage twice");
        assert!(crate::agent::history::replay(&rook, session).unwrap().is_empty());
        drop(first);
        drop(second);
        drop(observer);
        drop(rook);
        let rook = engine(home.path());
        let historical = rook.context_usage(before, Some(65536)).unwrap().cost_coverage.unwrap();
        assert_eq!((historical.attempts_started, historical.attempts_pending), (1, 1));
        assert_eq!(historical.attempts_interrupted, 0, "a later ending cannot cross the fork boundary");
        for id in [session, after] {
            let coverage = rook.context_usage(id, Some(65536)).unwrap().cost_coverage.unwrap();
            assert_eq!(
                (
                    coverage.attempts_started,
                    coverage.attempts_failed,
                    coverage.attempts_interrupted,
                    coverage.attempts_pending
                ),
                (2, 1, 1, 0)
            );
        }
    }

    #[test]
    fn attempt_identity_and_copy_limits_hold_without_reading_oversized_bookkeeping_into_history() {
        let home = tempfile::tempdir().unwrap();
        let rook = engine(home.path());
        let session = rook.start_session("attempt bounds").unwrap();
        let vault = Arc::new(Vault::empty());
        vault.also_hide("private-token");
        let recorder = Observer {
            store: rook.store.clone(),
            session,
            vault,
            purpose: Purpose::Aside,
            prices: Arc::new(crate::model_route::Prices::new(&rook.config)),
        };
        let secret = Dispatch::bounded("physical", "private-token", true).unwrap();
        let mut active = recorder.start(Some(&secret)).unwrap();
        active
            .finish(
                AttemptStatus::Failed,
                &AttemptFacts { reported_model: Some("private-token".into()), ..Default::default() },
            )
            .unwrap();
        let events = rook.store.events(session, 0, 8).unwrap();
        for event in &events {
            let body = rook.store.get(&event.record.body).unwrap();
            assert!(!String::from_utf8_lossy(&body).contains("private-token"));
            let record = Record::read(&body).unwrap();
            assert!(record.dispatch.is_none() && record.reported_model.is_none());
        }
        let mut bytes = rook.store.get(&events[0].record.body).unwrap();
        bytes.resize(MAX_BYTES + 1, b' ');
        assert!(bytes.len() > MAX_BYTES && serde_json::from_slice::<Record>(&bytes).is_ok());
        rook.log(session, EventKind::Note, LABEL, std::str::from_utf8(&bytes).unwrap()).unwrap();
        assert!(crate::agent::history::replay(&rook, session).unwrap().is_empty());
        assert!(
            rook.context_usage(session, Some(65536)).unwrap_err().to_string().contains("exceeds 4096 bytes")
        );
    }
}
