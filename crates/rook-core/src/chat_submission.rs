//! Durable identity for a socket prompt that starts a turn or controls a goal.
//!
//! The session claim and the prompt event are separate boundaries. A claim
//! survives a disconnected caller; admission is marked in the same transaction
//! as the UserMessage so a retry cannot run an already accepted prompt again.
//! Goal creation and continuation instead bind the same owner slot to their
//! generation in the transaction that saves the managed run.

use crate::{AGENT_VERSION, CoreError, Result, Rook};
use rook_proto::TurnOptions;
use rook_store::{SessionMeta, Store};
use sha2::{Digest, Sha256};
use std::io::Write;

const RECORD: usize = 16 + 1 + 32 + 26;
const PENDING_TURN: &[u8; 26] = b"00000000000000000000000000";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    Created,
    Pending,
    Admitted,
}

#[derive(Debug)]
pub struct Claim {
    pub session: u128,
    pub key: String,
    pub status: Status,
    pub turn: Option<String>,
}

struct HashWriter(Sha256);

impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn fingerprint(scope: &str, text: &str, options: &TurnOptions) -> Result<[u8; 32]> {
    let mut writer = HashWriter(Sha256::new());
    serde_json::to_writer(&mut writer, &(scope, text, options))?;
    Ok(writer.0.finalize().into())
}

fn record(session: u128, hash: &[u8; 32]) -> [u8; RECORD] {
    let mut bytes = [0u8; RECORD];
    bytes[..16].copy_from_slice(&session.to_be_bytes());
    bytes[17..49].copy_from_slice(hash);
    bytes[49..].copy_from_slice(PENDING_TURN);
    bytes
}

fn decode(key: String, bytes: &[u8], hash: &[u8; 32], fresh: bool) -> Result<Claim> {
    if bytes.len() != RECORD || !matches!(bytes[16], 0 | 1) {
        return Err(CoreError::Other("saved chat prompt receipt has an unsupported format".into()));
    }
    if &bytes[17..49] != hash {
        return Err(CoreError::Other("instruction ID already belongs to a different prompt".into()));
    }
    let session = u128::from_be_bytes(
        bytes[..16]
            .try_into()
            .map_err(|_| CoreError::Other("saved chat prompt receipt has an invalid session".into()))?,
    );
    let admitted = bytes[16] == 1;
    let turn = if bytes[49..] == *PENDING_TURN {
        None
    } else {
        let name = std::str::from_utf8(&bytes[49..])
            .map_err(|_| CoreError::Other("saved chat prompt receipt has an invalid turn".into()))?;
        name.parse::<ulid::Ulid>()
            .map_err(|_| CoreError::Other("saved chat prompt receipt has an invalid turn".into()))?;
        Some(name.to_owned())
    };
    if admitted && turn.is_none() {
        return Err(CoreError::Other("admitted chat prompt receipt has no turn".into()));
    }
    Ok(Claim {
        session,
        key,
        status: if fresh {
            Status::Created
        } else if admitted {
            Status::Admitted
        } else {
            Status::Pending
        },
        turn,
    })
}

/// Claim a socket prompt before the daemon starts its turn. The first prompt
/// creates its session and receipt atomically; a named session uses the same
/// bounded companion mutation as other queue receipts.
pub fn claim(
    rook: &Rook,
    session: Option<u128>,
    id: &str,
    text: &str,
    options: &TurnOptions,
) -> Result<Claim> {
    if id.is_empty()
        || id.len() > 64
        || !id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(CoreError::Other(
            "instruction ID must be 1–64 ASCII letters, digits, hyphens or underscores".into(),
        ));
    }
    match session {
        None => {
            let key = format!("chat-prompt/new/{id}");
            let hash = fingerprint(&rook.workspace.to_string_lossy(), text, options)?;
            let session = rook_store::new_session_id();
            let mut meta =
                SessionMeta::new(session, "", rook.workspace.display().to_string(), rook_store::now_unix());
            meta.model = rook.config.agent.model.clone();
            meta.agent = format!("rook {AGENT_VERSION}");
            let value = record(session, &hash);
            let existing = rook.store.create_session_with_value_once(&meta, &key, &value, RECORD)?;
            rook.store.flush()?;
            let claim = decode(key, existing.as_deref().unwrap_or(&value), &hash, existing.is_none())?;
            if rook.store.get_session(claim.session)?.is_none() {
                return Err(CoreError::Other("saved chat prompt session was deleted".into()));
            }
            Ok(claim)
        }
        Some(session) => {
            let prefix = format!("chat-prompt/session/{session:032x}/");
            let key = format!("{prefix}{id}");
            let hash = fingerprint(&rook_store::format_session_id(session), text, options)?;
            let value = record(session, &hash);
            let existing = rook.store.kv_claim_session_limited(
                session,
                &key,
                &prefix,
                &value,
                RECORD,
                rook.config.work.max_messages,
            )?;
            rook.store.flush()?;
            decode(key, existing.as_deref().unwrap_or(&value), &hash, existing.is_none())
        }
    }
}

/// Check a previously claimed prompt without creating a receipt for a new
/// correction. Active turns use the ordinary steering queue when no claim is
/// found under this ID.
pub fn read(
    rook: &Rook,
    session: u128,
    id: &str,
    text: &str,
    options: &TurnOptions,
) -> Result<Option<Claim>> {
    let key = format!("chat-prompt/session/{session:032x}/{id}");
    let Some(value) = rook.store.kv_get_limited(&key, RECORD)? else { return Ok(None) };
    let hash = fingerprint(&rook_store::format_session_id(session), text, options)?;
    Ok(Some(decode(key, &value, &hash, false)?))
}

/// A receipt may outlive its turn. Only rejoin live output when the journal
/// still names the turn that admitted this exact prompt.
pub fn current_turn(rook: &Rook, claim: &Claim) -> Result<bool> {
    let Some(turn) = &claim.turn else { return Ok(false) };
    Ok(crate::execution::current(rook, claim.session)?.is_some_and(|state| state.turn == *turn))
}

/// Bind a pending receipt to the execution before its hooks or model run.
pub(crate) fn started_value(store: &Store, key: &str, session: u128, turn: &str) -> Result<Vec<u8>> {
    let mut value = store
        .kv_get_limited(key, RECORD)?
        .ok_or_else(|| CoreError::Other("chat prompt receipt disappeared before execution started".into()))?;
    if value.len() != RECORD || value[..16] != session.to_be_bytes() || value[16] != 0 || turn.len() != 26 {
        return Err(CoreError::Other("chat prompt receipt changed before execution started".into()));
    }
    value[49..].copy_from_slice(turn.as_bytes());
    Ok(value)
}

/// Prepare the admission marker for the same store transaction as UserMessage.
pub(crate) fn admitted_value(store: &Store, key: &str, session: u128, turn: &str) -> Result<Vec<u8>> {
    let mut value = store
        .kv_get_limited(key, RECORD)?
        .ok_or_else(|| CoreError::Other("chat prompt receipt disappeared before admission".into()))?;
    if value.len() != RECORD
        || value[..16] != session.to_be_bytes()
        || value[16] != 0
        || &value[49..] != turn.as_bytes()
    {
        return Err(CoreError::Other("chat prompt receipt changed before admission".into()));
    }
    value[16] = 1;
    value[49..].copy_from_slice(turn.as_bytes());
    Ok(value)
}

/// A conversation goal is admitted when its generation and saved goal become
/// durable, before its first model turn. Its generation occupies the same
/// fixed-size owner slot as an ordinary execution turn.
pub(crate) fn goal_admitted_value(
    store: &Store,
    key: &str,
    session: u128,
    generation: &str,
) -> Result<Vec<u8>> {
    goal_value(store, key, session, generation, false)
}

/// A repeated continuation can confirm its saved admission without changing
/// the current status of that generation.
pub(crate) fn continuation_admitted_value(
    store: &Store,
    key: &str,
    session: u128,
    generation: &str,
    already_applied: bool,
) -> Result<Vec<u8>> {
    goal_value(store, key, session, generation, already_applied)
}

fn goal_value(store: &Store, key: &str, session: u128, generation: &str, repeat: bool) -> Result<Vec<u8>> {
    let mut value = store
        .kv_get_limited(key, RECORD)?
        .ok_or_else(|| CoreError::Other("chat goal receipt disappeared before admission".into()))?;
    if value.len() != RECORD
        || value[..16] != session.to_be_bytes()
        || !(value[16] == 0 && value[49..] == *PENDING_TURN
            || repeat && value[16] == 1 && &value[49..] == generation.as_bytes())
        || generation.parse::<ulid::Ulid>().is_err()
    {
        return Err(CoreError::Other("chat goal receipt changed before admission".into()));
    }
    value[16] = 1;
    value[49..].copy_from_slice(generation.as_bytes());
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::Journal;
    use rook_proto::work::{Conversation, Start};

    fn engine(root: &std::path::Path) -> Rook {
        Rook::from_parts(
            Store::open(root.join("store")).unwrap(),
            crate::Config::default(),
            rook_skills::Environment::bare("linux", "x86_64", "0.10.0"),
            rook_skills::SkillIndex::discover(&[]).0,
            root.into(),
        )
    }

    #[test]
    fn first_prompt_claim_and_admission_survive_retry_without_another_session_or_message() {
        let dir = tempfile::tempdir().unwrap();
        let rook = engine(dir.path());
        let options = TurnOptions::default();
        let first = claim(&rook, None, "caller-one", "build a table", &options).unwrap();
        assert_eq!(first.status, Status::Created);
        let pending = claim(&rook, None, "caller-one", "build a table", &options).unwrap();
        assert_eq!((pending.session, pending.status), (first.session, Status::Pending));
        assert_eq!(rook.store.list_sessions().unwrap().len(), 1);
        assert!(claim(&rook, None, "caller-one", "different", &options).is_err());
        assert!(
            claim(
                &rook,
                None,
                "caller-one",
                "build a table",
                &TurnOptions { output: Some("different.txt".into()), ..Default::default() }
            )
            .is_err()
        );

        let journal = Journal::start_with_claim(&rook, first.session, None, false, Some(&first.key)).unwrap();
        let started = claim(&rook, None, "caller-one", "build a table", &options).unwrap();
        assert_eq!(started.status, Status::Pending);
        assert!(started.turn.is_some(), "the pending receipt names the turn that owns it");
        assert!(current_turn(&rook, &started).unwrap());
        let (seq, _) =
            journal.admit_prompt(&rook, "build a table", "build a table", "", Some(""), None).unwrap();
        let admitted = claim(&rook, None, "caller-one", "build a table", &options).unwrap();
        assert_eq!(admitted.status, Status::Admitted);
        assert!(admitted.turn.is_some());
        assert!(current_turn(&rook, &admitted).unwrap());
        assert_eq!(
            rook.store.events(first.session, seq, 1).unwrap()[0].record.kind,
            rook_store::EventKind::UserMessage
        );
        assert_eq!(rook.store.list_sessions().unwrap().len(), 1);

        drop(journal);
        rook.delete_session(first.session).unwrap();
        assert!(rook.store.kv_update_session_values(first.session, &[("late", b"receipt")]).is_err());
        assert!(rook.store.kv_get("late").unwrap().is_none());
        let later = claim(&rook, None, "caller-one", "build a table", &options).unwrap();
        assert_eq!(later.status, Status::Created, "retention removes the first-session lookup too");
        assert_ne!(later.session, first.session);
    }

    #[test]
    fn named_session_claim_does_not_cross_into_another_session() {
        let dir = tempfile::tempdir().unwrap();
        let mut rook = engine(dir.path());
        rook.config.work.max_messages = 1;
        let one = rook.start_session("one").unwrap();
        let two = rook.start_session("two").unwrap();
        let options = TurnOptions::default();
        let first = claim(&rook, Some(one), "same-id", "first", &options).unwrap();
        assert_eq!(first.status, Status::Created);
        assert_eq!(read(&rook, one, "same-id", "first", &options).unwrap().unwrap().status, Status::Pending);
        assert!(read(&rook, one, "same-id", "other", &options).is_err());
        assert!(read(&rook, two, "same-id", "first", &options).unwrap().is_none());
        assert_eq!(claim(&rook, Some(two), "same-id", "second", &options).unwrap().status, Status::Created);
        assert!(claim(&rook, Some(one), "another", "third", &options).is_err());
        assert_eq!(claim(&rook, Some(one), "same-id", "first", &options).unwrap().status, Status::Pending);
        rook.delete_session(one).unwrap();
        assert!(read(&rook, one, "same-id", "first", &options).unwrap().is_none());
    }

    #[test]
    fn goal_creation_admits_the_claim_with_its_generation_and_goal_event() {
        let dir = tempfile::tempdir().unwrap();
        let rook = engine(dir.path());
        let options = TurnOptions::default();
        let first = claim(&rook, None, "goal-caller", "/goal inspect", &options).unwrap();
        let request = Start {
            goal: "inspect".into(),
            workspace: None,
            autonomous: false,
            max_iterations: None,
            max_tokens: None,
            max_seconds: None,
            conversation: Some(Conversation {
                session: rook_store::format_session_id(first.session),
                model: None,
                effort: "high".into(),
                stance: "assist".into(),
                options: options.clone(),
            }),
        };
        let run = crate::work::managed::start_with_claim(&rook, request.clone(), Some(&first.key)).unwrap();
        let admitted = claim(&rook, None, "goal-caller", "/goal inspect", &options).unwrap();
        assert_eq!(admitted.status, Status::Admitted);
        assert_eq!(admitted.turn.as_deref(), Some(run.generation.as_str()));
        assert_eq!(rook.goal(first.session).unwrap().as_deref(), Some("inspect"));
        assert!(crate::work::managed::start_with_claim(&rook, request, Some(&first.key)).is_err());
        assert_eq!(rook.store.list_sessions().unwrap().len(), 1);
        assert_eq!(
            rook.store
                .events(first.session, 0, 100)
                .unwrap()
                .iter()
                .filter(|e| e.record.label == "goal")
                .count(),
            1
        );
    }
}
