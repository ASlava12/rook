//! Immutable turn results and a constant-size ledger, committed with recovery.
use crate::{CoreError, Result, Rook, agent::TurnOutcome, execution::Execution};
use rook_store::{EventKind, Store};
use serde::{Deserialize, Serialize};

pub(crate) const LABEL: &str = "turn-summary";
pub(crate) const RESULT: &str = "turn-result";
const RECORD_BYTES: usize = 8192;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Totals {
    pub turns: u64,
    pub completed: u64,
    pub steps: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub elapsed_seconds: u64,
    pub saturated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Summary {
    pub session: String,
    pub turn: String,
    pub follow_up: Option<String>,
    pub continuation: Option<String>,
    pub prompt_seq: Option<u64>,
    pub started_at: i64,
    pub ended_at: i64,
    pub stopped: String,
    pub steps: u32,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cached_tokens: u32,
    pub files_changed: usize,
    pub reply: String,
    pub reply_truncated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    /// The complete outcome is an ordinary, byte-pageable history event.
    pub result_seq: u64,
    pub summary: Summary,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Query {
    pub before: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page {
    pub items: Vec<Entry>,
    pub before: Option<u64>,
    pub scanned_events: usize,
    pub totals: Totals,
    /// First recorded outcome's prompt; earlier turns are not reconstructed.
    pub coverage_from: Option<u64>,
}

#[derive(Default, Serialize, Deserialize)]
pub(crate) struct Ledger {
    pub last_turn: Option<String>,
    pub totals: Totals,
    pub coverage_from: Option<u64>,
}

pub(crate) fn key(session: u128) -> String {
    format!("turn-ledger/{session:032x}")
}

pub(crate) fn ledger(store: &Store, session: u128) -> Result<Ledger> {
    store
        .kv_get(&key(session))?
        .map(|bytes| serde_json::from_slice(&bytes).map_err(Into::into))
        .transpose()
        .map(Option::unwrap_or_default)
}

pub(crate) fn prepare(state: &Execution, outcome: &TurnOutcome, ledger: &mut Ledger) -> Result<Vec<u8>> {
    let ended_at = rook_store::now_unix();
    let mut reply_end = outcome.reply.len().min(256);
    while !outcome.reply.is_char_boundary(reply_end) {
        reply_end -= 1;
    }
    let summary = Summary {
        session: state.session.clone(),
        turn: state.turn.clone(),
        follow_up: state.follow_up.clone(),
        continuation: state.continuation.clone(),
        prompt_seq: state.prompt.as_ref().map(|p| p.seq),
        started_at: state.started_at,
        ended_at,
        stopped: outcome.stopped.chars().take(64).collect(),
        steps: outcome.steps,
        input_tokens: outcome.input_tokens,
        output_tokens: outcome.output_tokens,
        cached_tokens: outcome.cached_tokens,
        files_changed: outcome.files_changed.len(),
        reply: outcome.reply[..reply_end].into(),
        reply_truncated: reply_end < outcome.reply.len(),
    };
    let totals = &mut ledger.totals;
    for (count, increment) in [
        (&mut totals.turns, 1),
        (&mut totals.completed, u64::from(crate::agent::finished(&outcome.stopped))),
        (&mut totals.steps, u64::from(outcome.steps)),
        (&mut totals.input_tokens, u64::from(outcome.input_tokens)),
        (&mut totals.output_tokens, u64::from(outcome.output_tokens)),
        (&mut totals.cached_tokens, u64::from(outcome.cached_tokens)),
        (&mut totals.elapsed_seconds, ended_at.saturating_sub(state.started_at).max(0) as u64),
    ] {
        match count.checked_add(increment) {
            Some(value) => *count = value,
            None => {
                *count = u64::MAX;
                totals.saturated = true;
            }
        }
    }
    ledger.last_turn = Some(state.turn.clone());
    ledger.coverage_from.get_or_insert(state.prompt.as_ref().map_or(state.start_seq, |p| p.seq));
    crate::persistence::encode_with_limit(&summary, RECORD_BYTES)
}

/// Newest first. Each call scans a bounded event page, including pages with no
/// results; following `before` makes progress through long tool-heavy turns.
pub fn page(rook: &Rook, session: u128, query: &Query) -> Result<Page> {
    let session_name = rook_store::format_session_id(session);
    let meta = rook.store.get_session(session)?.ok_or_else(|| CoreError::NoSession(session_name.clone()))?;
    let limits = rook.config.transcript.bounded();
    let ledger = ledger(&rook.store, session)?;
    let before = query.before.unwrap_or(meta.next_seq).min(meta.next_seq);
    let events = rook.store.events_before(session, before, limits.search_events)?;
    let mut page = Page {
        items: Vec::new(),
        before: None,
        scanned_events: 0,
        totals: ledger.totals,
        coverage_from: ledger.coverage_from,
    };
    let mut bytes_left = limits.page_bytes.saturating_sub(1024);
    for event in events.iter().rev() {
        if event.record.kind == EventKind::Note && event.record.label == LABEL {
            let size = rook.store.stat_object(&event.record.body)?.map_or(0, |m| m.size_raw);
            if size > RECORD_BYTES as u64 {
                return Err(CoreError::Other("oversized turn summary".into()));
            }
            let summary: Summary = serde_json::from_slice(&rook.store.get(&event.record.body)?)?;
            // Forked history remains readable, but is not another execution in
            // the child. Its token totals must not be charged a second time.
            if summary.session == session_name {
                let entry = Entry { result_seq: event.seq + 1, summary };
                let encoded = crate::persistence::encode_with_limit(&entry, RECORD_BYTES)?;
                if encoded.len() + 1 > bytes_left && page.items.is_empty() {
                    return Err(CoreError::Other("turn summary exceeds transcript.page_bytes; preserve the store and inspect the summary".into()));
                }
                if encoded.len() + 1 > bytes_left || page.items.len() == limits.page_entries {
                    page.before = Some(event.seq + 1);
                    break;
                }
                bytes_left -= encoded.len() + 1;
                page.items.push(entry);
            }
        }
        page.scanned_events += 1;
        page.before = (event.seq > 0).then_some(event.seq);
    }
    Ok(page)
}

/// A shared text view for command-line and terminal history readers.
pub fn describe(page: &Page) -> String {
    let t = &page.totals;
    format!(
        "Recorded turns: {} ({} completed) · tokens in/out/cache: {}/{}/{} · {} steps · {}s{}\nCoverage starts at {}; inherited branch turns and attempts without a saved outcome are excluded.",
        t.turns,
        t.completed,
        t.input_tokens,
        t.output_tokens,
        t.cached_tokens,
        t.steps,
        t.elapsed_seconds,
        if t.saturated { " · counters saturated" } else { "" },
        page.coverage_from.map_or_else(|| "no recorded outcome".into(), |seq| format!("event #{seq}"))
    )
}

pub fn entry_text(entry: &Entry) -> String {
    let s = &entry.summary;
    format!(
        "{} · {}\n{} steps · tokens in/out/cache: {}/{}/{} · {} files\nPrompt #{} · result #{}{}{}\n\n{}{}",
        s.turn,
        s.stopped,
        s.steps,
        s.input_tokens,
        s.output_tokens,
        s.cached_tokens,
        s.files_changed,
        s.prompt_seq.map_or_else(|| "unknown".into(), |n| n.to_string()),
        entry.result_seq,
        s.follow_up.as_ref().map(|id| format!(" · follow-up {id}")).unwrap_or_default(),
        s.continuation.as_ref().map(|id| format!(" · continues {id}")).unwrap_or_default(),
        s.reply,
        if s.reply_truncated { "\n[preview; open result for full outcome]" } else { "" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::Journal;

    fn engine(path: &std::path::Path) -> Rook {
        Rook::from_parts(
            Store::open(path.join("store")).unwrap(),
            crate::Config::default(),
            rook_skills::Environment::bare("linux", "x86_64", "0.10.0"),
            rook_skills::SkillIndex::default(),
            path.into(),
        )
    }
    fn outcome(text: &str) -> TurnOutcome {
        serde_json::from_value(serde_json::json!({
            "steps":2,"stopped":"end_turn","reply":text,"input_tokens":11,"output_tokens":7,"cached_tokens":3,
            "tools_called":[],"skills_loaded":[],"skills_written":[],"facts_learned":[],"facts_forgotten":[],"delegated":[],"compactions":0
        })).unwrap()
    }
    fn record(rook: &Rook, session: u128, outcome: &TurnOutcome) -> std::sync::Arc<Journal> {
        let journal = Journal::start(rook, session, None, false).unwrap();
        journal.admit_prompt(rook, "prompt", "prompt", "", Some(""), None).unwrap();
        journal.record_outcome(outcome).unwrap();
        journal
    }

    #[test]
    fn results_and_totals_survive_restart_once_without_entering_model_context() {
        let dir = tempfile::tempdir().unwrap();
        let rook = engine(dir.path());
        let session = rook.start_session("turn ledger").unwrap();
        let reply = "Я\"\n".repeat(300);
        let journal = record(&rook, session, &outcome(&reply));
        journal.record_outcome(&outcome("a retry must not replace the committed result")).unwrap();
        journal.finish("end_turn", None).unwrap();
        let first = page(&rook, session, &Query::default()).unwrap();
        assert_eq!(first.items.len(), 1);
        assert_eq!(first.totals.turns, 1);
        assert_eq!(first.totals.input_tokens, 11);
        assert!(first.items[0].summary.reply_truncated);
        assert!(reply.len() > 256);
        assert!(first.items[0].summary.reply.len() <= 256);
        let result = rook.store.events(session, first.items[0].result_seq, 1).unwrap().remove(0);
        assert_eq!(result.record.label, RESULT);
        let (_, saved): (String, TurnOutcome) =
            serde_json::from_slice(&rook.store.get(&result.record.body).unwrap()).unwrap();
        assert_eq!(saved.reply, reply);
        assert!(!crate::context::reaches_the_model(result.record.kind));
        drop(journal);
        drop(rook);
        let rook = engine(dir.path());
        let mut limited = outcome("limit reached");
        limited.stopped = "max_steps".into();
        let journal = record(&rook, session, &limited);
        journal.finish("max_steps", None).unwrap();
        let report = page(&rook, session, &Query::default()).unwrap();
        assert_eq!(report.items.len(), 2);
        assert_eq!(report.items[0].summary.stopped, "max_steps");
        assert_eq!(report.items[1].summary.turn, first.items[0].summary.turn);
        assert_eq!(report.totals.turns, 2);
        assert_eq!(report.totals.completed, 1);
        assert_eq!(report.totals.input_tokens, 22);
        assert_eq!(report.totals.output_tokens, 14);
        assert_eq!(report.totals.cached_tokens, 6);
        assert_eq!(report.coverage_from, first.coverage_from);
        let fork =
            rook.fork_session(session, rook.store.get_session(session).unwrap().unwrap().next_seq).unwrap();
        let inherited = page(&rook, fork.id, &Query::default()).unwrap();
        assert!(inherited.items.is_empty());
        assert_eq!(inherited.totals.turns, 0);
        assert!(inherited.coverage_from.is_none());
        record(&rook, fork.id, &outcome("child result")).finish("end_turn", None).unwrap();
        assert_eq!(page(&rook, fork.id, &Query::default()).unwrap().totals.turns, 1);
        assert_eq!(page(&rook, session, &Query::default()).unwrap().totals.turns, 2);
        rook.delete_session(session).unwrap();
        assert!(rook.store.kv_get(&key(session)).unwrap().is_none());
        assert!(page(&rook, session, &Query::default()).is_err());
    }

    #[test]
    fn result_pages_reach_byte_entry_and_scan_bounds_without_losing_a_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let mut rook = engine(dir.path());
        rook.config.transcript.page_bytes = 4096;
        rook.config.transcript.page_entries = 2;
        rook.config.transcript.search_events = 4;
        let session = rook.start_session("bounded results").unwrap();
        for _ in 0..5 {
            record(&rook, session, &outcome(&"\u{0001}".repeat(500))).finish("end_turn", None).unwrap();
        }
        for _ in 0..5 {
            rook.log(session, EventKind::Note, "noise", "not a result").unwrap();
        }
        let first = page(&rook, session, &Query::default()).unwrap();
        assert_eq!(first.scanned_events, 4);
        assert!(first.items.is_empty());
        assert!(first.before.is_some());
        let mut query = Query::default();
        let mut ids = std::collections::BTreeSet::new();
        for scan in 0..30 {
            let report = page(&rook, session, &query).unwrap();
            assert!(report.items.len() <= 2);
            assert!(report.scanned_events <= 4);
            assert!(serde_json::to_vec(&report).unwrap().len() <= 4096);
            for entry in report.items {
                assert!(ids.insert(entry.summary.turn));
            }
            if report.before.is_none() {
                break;
            }
            assert!(query.before.is_none_or(|before| report.before.unwrap() < before));
            query.before = report.before;
            assert!(scan < 29, "cursor did not finish");
        }
        assert_eq!(ids.len(), 5);
        rook.config.transcript.search_events = 256;
        rook.config.transcript.page_entries = 256;
        let report = page(&rook, session, &Query::default()).unwrap();
        assert!(report.items.len() < 5, "escaped bodies must reach the byte cap");
        assert!(report.before.is_some());
        assert!(serde_json::to_vec(&report).unwrap().len() <= 4096);
        rook.config.transcript.page_bytes = 262144;
        rook.config.transcript.page_entries = 1;
        assert_eq!(page(&rook, session, &Query::default()).unwrap().items.len(), 1);
    }

    #[test]
    fn an_oversized_outcome_cannot_commit_a_summary_or_increase_totals() {
        let dir = tempfile::tempdir().unwrap();
        let rook = engine(dir.path());
        let session = rook.start_session("atomic result").unwrap();
        let journal = Journal::start(&rook, session, None, false).unwrap();
        journal.admit_prompt(&rook, "prompt", "prompt", "", Some(""), None).unwrap();
        let large = outcome(&"x".repeat(8 * 1024 * 1024 + 1));
        assert!(large.reply.len() > 8 * 1024 * 1024);
        assert!(journal.record_outcome(&large).is_err());
        let report = page(&rook, session, &Query::default()).unwrap();
        assert!(report.items.is_empty());
        assert_eq!(report.totals.turns, 0);
        assert!(rook.store.kv_get(&format!("execution-outcome/{session:032x}")).unwrap().is_none());
        journal.record_outcome(&outcome("fits")).unwrap();
        assert_eq!(page(&rook, session, &Query::default()).unwrap().totals.turns, 1);
    }
}
