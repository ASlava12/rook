//! Durable receipts distinguish an interrupted request from a known tool result.
//! Nothing here retries an operation. A missing receipt is a reason to inspect.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, LazyLock, Mutex};

use rook_store::{EventKind, Kind, NewEvent, Store};
use serde::{Deserialize, Serialize};

use crate::{CoreError, Result, Rook};

const MAX_PENDING: usize = 64;
const MAX_FAMILY: usize = 256;
const PREVIEW: usize = 2048;
static OWNER: LazyLock<String> = LazyLock::new(|| ulid::Ulid::generate().to_string());
type Active = BTreeMap<(std::path::PathBuf, u128), String>;
// Also serializes read/modify/write of receipts; never held across an await.
static ACTIVE: Mutex<Active> = Mutex::new(BTreeMap::new());

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Operation {
    pub session: String,
    pub id: String,
    pub tool: String,
    pub arguments: String,
    pub started_at: i64,
    pub call_seq: u64,
    pub may_have_effects: bool,
    pub job: Option<String>,
    pub registry: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Execution {
    pub version: u32,
    pub session: String,
    pub turn: String,
    pub owner: String,
    pub pid: u32,
    pub started_at: i64,
    pub updated_at: i64,
    pub status: String,
    pub task: String,
    pub start_seq: u64,
    /// Missing on older receipts and until the prompt hooks have admitted it.
    #[serde(default)]
    pub prompt: Option<PromptAdmission>,
    #[serde(default)]
    pub follow_up: Option<String>,
    /// Explicit continuations share a completion boundary, but retain their
    /// own execution and prompt receipts. Missing means this turn starts it.
    #[serde(default)]
    pub continuation: Option<String>,
    pub completed_operations: u64,
    pub last_result_seq: Option<u64>,
    pub pending: Option<Operation>,
    pub background: Vec<Operation>,
    pub unknown: Vec<Operation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PromptAdmission {
    pub seq: u64,
    pub body: String,
    pub label: String,
    /// Present for recoverable admission, including an empty hook context.
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub prompt_context: Option<String>,
}

impl Execution {
    pub(crate) fn completion_boundary(&self) -> &str {
        self.continuation.as_deref().unwrap_or(&self.turn)
    }
}

fn key(session: u128) -> String {
    format!("execution/{session:032x}")
}

pub(crate) fn current(rook: &Rook, session: u128) -> Result<Option<Execution>> {
    load(&rook.store, session)
}

pub(crate) fn is_active(rook: &Rook, session: u128) -> bool {
    ACTIVE.lock().unwrap_or_else(|e| e.into_inner()).contains_key(&(rook.store.root().to_path_buf(), session))
}

fn load(store: &Store, session: u128) -> Result<Option<Execution>> {
    let Some(bytes) = store.kv_get(&key(session))? else { return Ok(None) };
    let state: Execution = serde_json::from_slice(&bytes)?;
    if state.version != 1 || state.background.len() > MAX_PENDING || state.unknown.len() > MAX_PENDING + 1 {
        return Err(CoreError::Other(
            "unsupported or oversized execution receipt; preserve the store and inspect it before continuing"
                .into(),
        ));
    }
    Ok(Some(state))
}

pub(crate) fn diagnostic_state(store: &Store, session: u128) -> Result<serde_json::Value> {
    let Some(state) = load(store, session)? else { return Ok(serde_json::Value::Null) };
    let status = match state.status.as_str() {
        "running" | "interrupted" | "needs_review" | "end_turn" | "failed" | "cancelled" | "complete"
        | "stopped" | "max_tokens" | "blocked" | "incomplete" | "time" | "budget" | "steps" => {
            state.status.as_str()
        }
        _ => "other",
    };
    Ok(serde_json::json!({"status":status,"pid":state.pid,"started_at":state.started_at,
        "updated_at":state.updated_at,"completed_operations":state.completed_operations,
        "pending":state.pending.is_some(),"background":state.background.len(),"unknown":state.unknown.len()}))
}

fn save(store: &Store, session: u128, state: &mut Execution) -> Result<()> {
    state.updated_at = rook_store::now_unix();
    store.kv_set(&key(session), &crate::persistence::encode(state)?)?;
    let protect = state.status == "running" || !state.unknown.is_empty() || !state.background.is_empty();
    store.update_session(session, |meta| {
        meta.tags.retain(|tag| tag != "rook:execution");
        if protect {
            meta.tags.push("rook:execution".into());
        }
    })?;
    // A command must never start before the receipt saying it could have run.
    store.flush()?;
    Ok(())
}

fn uncertain(state: &mut Execution, operation: Operation) {
    if operation.may_have_effects && !state.unknown.iter().any(|p| p.id == operation.id) {
        state.unknown.push(operation);
    }
}

fn interrupt(state: &mut Execution, lost_owner: bool) {
    state.status = "interrupted".into();
    if let Some(operation) = state.pending.take() {
        uncertain(state, operation);
    }
    if lost_owner {
        for operation in std::mem::take(&mut state.background) {
            uncertain(state, operation);
        }
    }
}

/// Called only after acquiring the store's exclusive process lock.
pub(crate) fn recover(store: &Store) -> Result<()> {
    let _active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
    for meta in store.list_sessions()? {
        let Some(mut state) = load(store, meta.id)? else { continue };
        if state.status != "running" && state.background.is_empty() {
            continue;
        }
        interrupt(&mut state, true);
        let message = format!(
            "Execution {} lost owner {} (pid {}). {} operation(s) have an unknown result. Nothing was retried. Inspect `rook session recovery {}` before acknowledging any unknown operation.",
            state.turn,
            state.owner,
            state.pid,
            state.unknown.len(),
            state.session
        );
        store.append_event(
            meta.id,
            NewEvent::new(EventKind::Note, Kind::Message, message.as_bytes()).label("execution interrupted"),
        )?;
        save(store, meta.id, &mut state)?;
    }
    Ok(())
}

/// A fork changes the transcript, not whether an external side effect happened.
pub(crate) fn inherit(rook: &Rook, parent: u128, child: u128) -> Result<()> {
    let _active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
    let states = rook.execution(parent)?;
    let mut unknown = Vec::new();
    for mut state in states {
        interrupt(&mut state, true);
        for operation in state.unknown {
            if !unknown.iter().any(|p: &Operation| p.id == operation.id) {
                unknown.push(operation);
            }
        }
    }
    if unknown.is_empty() {
        rook.store.update_session(child, |meta| meta.tags.retain(|tag| tag != "rook:execution"))?;
        return Ok(());
    }
    if unknown.len() > MAX_PENDING + 1 {
        return Err(CoreError::Other(
            "too many unknown operations to fork; review the source session first".into(),
        ));
    }
    let mut state = Execution {
        version: 1,
        session: rook_store::format_session_id(child),
        turn: ulid::Ulid::generate().to_string(),
        owner: OWNER.clone(),
        pid: std::process::id(),
        started_at: rook_store::now_unix(),
        updated_at: 0,
        status: "interrupted".into(),
        task: "fork inherits unknown side effects from the source execution".into(),
        start_seq: 0,
        prompt: None,
        follow_up: None,
        continuation: None,
        completed_operations: 0,
        last_result_seq: None,
        pending: None,
        background: Vec::new(),
        unknown,
    };
    save(&rook.store, child, &mut state)
}

/// One guard per active turn; cancelling its future leaves an interrupted receipt.
pub(crate) struct Journal {
    output_dir: std::path::PathBuf,
    store: Arc<Store>,
    session: u128,
    turn: String,
    resumed: bool,
}

impl Journal {
    pub(crate) fn start(
        rook: &Rook,
        session: u128,
        jobs: Option<&rook_tools::jobs::Jobs>,
        continuing: bool,
    ) -> Result<Arc<Self>> {
        let _queue = crate::work::receipts::WRITING.lock().unwrap_or_else(|e| e.into_inner());
        let mut active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
        let identity = (rook.store.root().to_path_buf(), session);
        if active.contains_key(&identity) {
            return Err(CoreError::Other("this session already has an active execution".into()));
        }
        let meta = rook
            .store
            .get_session(session)?
            .ok_or_else(|| CoreError::NoSession(rook_store::format_session_id(session)))?;
        let mut previous = load(&rook.store, session)?;
        if let Some(state) = &mut previous {
            if state.status == "running" {
                let lost_owner = state.owner != *OWNER;
                interrupt(state, lost_owner);
            }
            refresh(&rook.store, &rook.output_dir, session, state, jobs)?;
        }
        let turn = ulid::Ulid::generate().to_string();
        let continuation = previous
            .as_ref()
            .filter(|state| continuing && state.status != "evaluated")
            .map(|state| state.completion_boundary().to_owned());
        let mut state = Execution {
            version: 1,
            session: rook_store::format_session_id(session),
            turn: turn.clone(),
            owner: OWNER.clone(),
            pid: std::process::id(),
            started_at: rook_store::now_unix(),
            updated_at: 0,
            status: "running".into(),
            task: "prompt awaiting admission by configured hooks".into(),
            start_seq: meta.next_seq,
            prompt: None,
            follow_up: None,
            continuation,
            completed_operations: 0,
            last_result_seq: None,
            pending: None,
            background: previous.as_ref().map(|s| s.background.clone()).unwrap_or_default(),
            unknown: previous.map(|s| s.unknown).unwrap_or_default(),
        };
        save(&rook.store, session, &mut state)?;
        active.insert(identity, turn.clone());
        Ok(Arc::new(Self {
            output_dir: rook.output_dir.clone(),
            store: rook.store.clone(),
            session,
            turn,
            resumed: false,
        }))
    }

    pub(crate) fn reserve_follow_up(
        rook: &Rook,
        session: u128,
    ) -> Result<Option<(Arc<Self>, rook_proto::work::Steering)>> {
        let _queue = crate::work::receipts::WRITING.lock().unwrap_or_else(|e| e.into_inner());
        let mut active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
        let identity = (rook.store.root().to_path_buf(), session);
        if active.contains_key(&identity) {
            return Ok(None);
        }
        let mut messages = crate::message_queue::list(rook, session)?;
        if let Some(at) = crate::message_queue::followups::resumable(rook, session, &messages)? {
            let mut state = load(&rook.store, session)?
                .ok_or_else(|| CoreError::Other("reserved execution disappeared".into()))?;
            state.owner = OWNER.clone();
            state.pid = std::process::id();
            state.status = "running".into();
            save(&rook.store, session, &mut state)?;
            active.insert(identity, state.turn.clone());
            return Ok(Some((
                Arc::new(Self {
                    output_dir: rook.output_dir.clone(),
                    store: rook.store.clone(),
                    session,
                    turn: state.turn,
                    resumed: true,
                }),
                messages[at].clone(),
            )));
        }
        let Some(at) = crate::message_queue::followups::next(rook, session, &messages)? else {
            return Ok(None);
        };
        let previous = load(&rook.store, session)?;
        let Some(follow) = messages[at].follow_up.as_ref() else { return Ok(None) };
        let after = follow.after.clone();
        let goal = follow.goal.clone();
        crate::message_queue::followups::arm(&mut messages, &after, &goal);
        let turn = ulid::Ulid::generate().to_string();
        if let Some(follow) = &mut messages[at].follow_up {
            follow.reserved = Some(turn.clone());
        }
        let message = messages[at].clone();
        let meta = rook
            .store
            .get_session(session)?
            .ok_or_else(|| CoreError::NoSession(rook_store::format_session_id(session)))?;
        let state = Execution {
            version: 1,
            session: rook_store::format_session_id(session),
            turn: turn.clone(),
            owner: OWNER.clone(),
            pid: std::process::id(),
            started_at: rook_store::now_unix(),
            updated_at: rook_store::now_unix(),
            status: "running".into(),
            task: "follow-up awaiting admission by configured hooks".into(),
            start_seq: meta.next_seq,
            prompt: None,
            follow_up: Some(message.id.clone()),
            continuation: None,
            completed_operations: 0,
            last_result_seq: None,
            pending: None,
            background: previous.as_ref().map(|s| s.background.clone()).unwrap_or_default(),
            unknown: previous.map(|s| s.unknown).unwrap_or_default(),
        };
        // Protect before reserving. A crash here can leave a harmless protection
        // tag, but cannot leave a reserved execution eligible for retention.
        rook.store.update_session(session, |meta| {
            if !meta.tags.iter().any(|tag| tag == "rook:execution") {
                meta.tags.push("rook:execution".into());
            }
        })?;
        rook.store.append_events_with_values(
            session,
            [],
            &[
                (&key(session), &crate::persistence::encode(&state)?),
                (&crate::message_queue::key(session), &crate::persistence::encode(&messages)?),
            ],
        )?;
        active.insert(identity, turn.clone());
        Ok(Some((
            Arc::new(Self {
                output_dir: rook.output_dir.clone(),
                store: rook.store.clone(),
                session,
                turn,
                resumed: false,
            }),
            message,
        )))
    }

    pub(crate) fn recovered_outcome(&self) -> Result<Option<crate::agent::TurnOutcome>> {
        if !self.resumed {
            return Ok(None);
        }
        let Some(bytes) = self.store.kv_get(&format!("execution-outcome/{:032x}", self.session))? else {
            return Ok(None);
        };
        let (turn, outcome): (String, crate::agent::TurnOutcome) = serde_json::from_slice(&bytes)?;
        Ok((turn == self.turn).then_some(outcome))
    }

    pub(crate) fn recovered_prompt(&self, body: &str) -> Result<Option<PromptAdmission>> {
        if !self.resumed {
            return Ok(None);
        }
        let state = load(&self.store, self.session)?
            .ok_or_else(|| CoreError::Other("reserved execution disappeared".into()))?;
        let Some(prompt) = state.prompt else { return Ok(None) };
        if prompt.body != rook_store::ObjectId::of(body.as_bytes()).to_string()
            || !prompt.label.is_empty()
            || prompt.context.is_none()
        {
            return Err(CoreError::Other("saved follow-up admission is incompatible; inspect session recovery before continuing explicitly".into()));
        }
        Ok(Some(prompt))
    }

    fn update(&self, change: impl FnOnce(&mut Execution) -> Result<()>) -> Result<()> {
        let _active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = load(&self.store, self.session)?
            .ok_or_else(|| CoreError::Other("execution receipt disappeared".into()))?;
        if state.turn != self.turn || state.status != "running" {
            return Err(CoreError::Other(
                "execution ownership changed; the operation was not started".into(),
            ));
        }
        change(&mut state)?;
        save(&self.store, self.session, &mut state)
    }

    pub(crate) fn admit(&self, task: &str) -> Result<()> {
        self.update(|state| {
            state.task = rook_tools::elide_middle(task, PREVIEW);
            Ok(())
        })
    }

    /// Hooks run before admission. Recording the prompt and its identity must
    /// commit together: recovery cannot infer admission from a task preview or
    /// mistake a hook's intervening event for the prompt's sequence.
    pub(crate) fn admit_prompt(
        &self,
        rook: &Rook,
        task: &str,
        body: &str,
        label: &str,
        context: Option<&str>,
        prompt_context: Option<&str>,
    ) -> Result<(u64, Option<rook_proto::queue::Notice>)> {
        let _queue = crate::work::receipts::WRITING.lock().unwrap_or_else(|e| e.into_inner());
        let _active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = load(&self.store, self.session)?
            .ok_or_else(|| CoreError::Other("execution receipt disappeared".into()))?;
        if state.turn != self.turn || state.status != "running" {
            return Err(CoreError::Other("execution ownership changed; the prompt was not admitted".into()));
        }
        if context.map_or(0, str::len).saturating_add(prompt_context.map_or(0, str::len)) > 1024 * 1024 {
            return Err(CoreError::Other(
                "session hook context exceeds the 1 MiB recovery limit; reduce hook output".into(),
            ));
        }
        let identity = rook_store::ObjectId::of(body.as_bytes()).to_string();
        if let Some(prompt) = &state.prompt {
            if prompt.body == identity && prompt.label == label {
                return Ok((prompt.seq, None));
            }
            return Err(CoreError::Other("this execution already admitted a different prompt".into()));
        }
        state.task = rook_tools::elide_middle(task, PREVIEW);
        state.updated_at = rook_store::now_unix();
        let mut notice = None;
        let mut values = Vec::new();
        if let Some(id) = &state.follow_up {
            let mut messages = crate::message_queue::read_from(&self.store, self.session)?;
            let message = messages
                .iter_mut()
                .find(|m| &m.id == id)
                .ok_or_else(|| CoreError::Other("follow-up reservation disappeared".into()))?;
            let current_goal = crate::work::managed::for_session(rook, self.session)?;
            let scope_changed = message.follow_up.as_ref().is_none_or(|follow| {
                follow.goal != current_goal.as_ref().map(|run| run.identity())
                    || current_goal.as_ref().is_some_and(|run| {
                        !run.status.terminal()
                            || (follow.after.starts_with("goal.")
                                && run.status != rook_proto::work::Status::Completed)
                    })
            });
            if scope_changed
                || !message.queued()
                || message.text != body
                || message.follow_up.as_ref().and_then(|f| f.reserved.as_deref()) != Some(self.turn.as_str())
            {
                return Err(CoreError::Other(
                    "follow-up reservation changed; the prompt was not admitted".into(),
                ));
            }
            message.applied_at = Some(crate::work::managed::now());
            message.session = Some(rook_store::format_session_id(self.session));
            notice = Some(rook_proto::queue::Notice::new(
                rook_store::format_session_id(self.session),
                format!("session.{}", id),
                message,
            ));
            values.push((crate::message_queue::key(self.session), crate::persistence::encode(&messages)?));
            values.push((format!("goal/{:032x}", self.session), body.as_bytes().to_vec()));
        }
        let references: Vec<_> = values.iter().map(|(key, bytes)| (key.as_str(), bytes.as_slice())).collect();
        let encode = |seq| {
            state.prompt = Some(PromptAdmission {
                seq,
                body: identity,
                label: label.into(),
                context: context.map(str::to_owned),
                prompt_context: prompt_context.map(str::to_owned),
            });
            crate::persistence::encode(&state)
                .map_err(|error| rook_store::StoreError::Encoding(error.to_string()))
        };
        let prompt = NewEvent::new(EventKind::UserMessage, Kind::Message, body.as_bytes()).label(label);
        let seq = if notice.is_some() {
            let [_, seq] = self.store.append_events_with_receipt(
                self.session,
                [NewEvent::new(EventKind::Note, Kind::Message, body.as_bytes()).label("goal"), prompt],
                &key(self.session),
                &references,
                |[_, seq]| encode(seq),
            )?;
            seq
        } else {
            self.store.append_event_with_receipt(self.session, prompt, &key(self.session), encode)?
        };
        Ok((seq, notice))
    }

    pub(crate) fn begin(
        &self,
        tool: &str,
        arguments: &str,
        effects: bool,
        background: bool,
        jobs: Option<&rook_tools::jobs::Jobs>,
    ) -> Result<()> {
        self.update(|state| {
            refresh(&self.store, &self.output_dir, self.session, state, jobs)?;
            if effects && !state.unknown.is_empty() {
                return Err(CoreError::Other(
                    "unknown operation results require inspection before changes resume".into(),
                ));
            }
            if state.pending.is_some() {
                return Err(CoreError::Other("another operation has no completion receipt".into()));
            }
            if background && state.background.len() >= MAX_PENDING {
                return Err(CoreError::Other(
                    "64 background operations await receipts; inspect them before starting another".into(),
                ));
            }
            let kind = if tool.starts_with("harness:") { EventKind::Note } else { EventKind::ToolCall };
            let seq = self.store.append_event(
                self.session,
                NewEvent::new(kind, Kind::Message, arguments.as_bytes()).label(tool),
            )?;
            state.pending = Some(Operation {
                session: state.session.clone(),
                id: ulid::Ulid::generate().to_string(),
                tool: tool.into(),
                arguments: rook_tools::elide_middle(arguments, PREVIEW),
                started_at: rook_store::now_unix(),
                call_seq: seq,
                may_have_effects: effects,
                job: None,
                registry: None,
            });
            Ok(())
        })
    }

    pub(crate) fn complete(
        &self,
        result: &str,
        jobs: Option<&rook_tools::jobs::Jobs>,
        new_job: Option<&str>,
    ) -> Result<()> {
        self.update(|state| {
            let operation = state
                .pending
                .take()
                .ok_or_else(|| CoreError::Other("operation receipt disappeared".into()))?;
            // Reuse a tool's own final log entry. Some refused built-ins log an
            // Error or no result, so retain their actual answer here as well.
            let latest = self.store.get_session(self.session)?.and_then(|m| m.next_seq.checked_sub(1));
            let result_kind =
                if operation.tool.starts_with("harness:") { EventKind::Note } else { EventKind::ToolResult };
            let recorded = latest
                .and_then(|seq| self.store.events(self.session, seq, 1).ok())
                .and_then(|v| v.into_iter().next())
                .filter(|e| {
                    e.record.kind == result_kind
                        && e.record.label == operation.tool
                        && e.record.body == rook_store::ObjectId::of(result.as_bytes())
                });
            let seq = match recorded {
                Some(event) => event.seq,
                None => self.store.append_event(
                    self.session,
                    NewEvent::new(result_kind, Kind::Message, result.as_bytes()).label(&operation.tool),
                )?,
            };
            state.last_result_seq = Some(seq);
            state.completed_operations += 1;
            if let Some(id) = new_job {
                if state.background.len() >= MAX_PENDING {
                    return Err(CoreError::Other("background receipt limit reached".into()));
                }
                let mut pending = operation.clone();
                pending.job = Some(id.into());
                pending.registry = jobs.map(rook_tools::jobs::Jobs::identity);
                state.background.push(pending);
            }
            Ok(())
        })
    }

    pub(crate) fn background(&self, tool: &str, arguments: &str) -> Result<Background> {
        let id = ulid::Ulid::generate().to_string();
        self.update(|state| {
            if state.background.len() >= MAX_PENDING {
                return Err(CoreError::Other("background receipt limit reached".into()));
            }
            let seq = self.store.append_event(
                self.session,
                NewEvent::new(EventKind::Note, Kind::Message, arguments.as_bytes()).label(tool),
            )?;
            state.background.push(Operation {
                session: state.session.clone(),
                id: id.clone(),
                tool: tool.into(),
                arguments: rook_tools::elide_middle(arguments, PREVIEW),
                started_at: rook_store::now_unix(),
                call_seq: seq,
                may_have_effects: true,
                registry: None,
                job: Some(format!("harness:{id}")),
            });
            Ok(())
        })?;
        Ok(Background { store: self.store.clone(), session: self.session, id, done: false })
    }

    pub(crate) fn record_outcome(&self, outcome: &crate::agent::TurnOutcome) -> Result<()> {
        let _queue = crate::work::receipts::WRITING.lock().unwrap_or_else(|e| e.into_inner());
        self.update(|state| {
            let mut messages = crate::message_queue::read_from(&self.store, self.session)?;
            if crate::agent::finished(&outcome.stopped) && state.unknown.is_empty() {
                for message in &mut messages {
                    if let Some(follow) = &mut message.follow_up
                        && follow.after == format!("turn.{}", state.completion_boundary())
                        && follow.reserved.is_none()
                    {
                        follow.ready = true;
                    }
                }
            }
            self.store.append_events_with_values(
                self.session,
                [],
                &[
                    (
                        &format!("execution-outcome/{:032x}", self.session),
                        &crate::persistence::encode(&(self.turn.as_str(), outcome))?,
                    ),
                    (&crate::message_queue::key(self.session), &crate::persistence::encode(&messages)?),
                ],
            )?;
            Ok(())
        })
    }

    pub(crate) fn finish(&self, status: &str, jobs: Option<&rook_tools::jobs::Jobs>) -> Result<bool> {
        let mut review = false;
        self.update(|state| {
            refresh(&self.store, &self.output_dir, self.session, state, jobs)?;
            if state.pending.is_some() {
                interrupt(state, false);
            } else {
                state.status = status.into();
            }
            review = !state.unknown.is_empty();
            if review {
                state.status = "needs_review".into();
            }
            Ok(())
        })?;
        ACTIVE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&(self.store.root().to_path_buf(), self.session));
        crate::message_queue::followups::stopped(
            &self.store,
            self.session,
            &self.turn,
            if review { "needs_review" } else { status },
        )?;
        Ok(review)
    }
}

pub(crate) struct Background {
    store: Arc<Store>,
    session: u128,
    id: String,
    done: bool,
}

impl Background {
    pub(crate) fn finish(mut self, result: &str) -> Result<()> {
        let _active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = load(&self.store, self.session)?
            .ok_or_else(|| CoreError::Other("execution receipt disappeared".into()))?;
        let Some(at) = state.background.iter().position(|p| p.id == self.id) else {
            return Err(CoreError::Other("background operation no longer owned by this process".into()));
        };
        let operation = state.background.remove(at);
        self.store.append_event(
            self.session,
            NewEvent::new(EventKind::Note, Kind::Message, result.as_bytes()).label(&operation.tool),
        )?;
        save(&self.store, self.session, &mut state)?;
        self.done = true;
        Ok(())
    }
}

impl Drop for Background {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let _active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
        if let Ok(Some(mut state)) = load(&self.store, self.session)
            && let Some(at) = state.background.iter().position(|p| p.id == self.id)
        {
            let operation = state.background.remove(at);
            uncertain(&mut state, operation);
            if let Err(error) = save(&self.store, self.session, &mut state) {
                tracing::error!("background recovery receipt failed: {error}");
            }
        }
    }
}

fn refresh(
    store: &Store,
    output_dir: &std::path::Path,
    session: u128,
    state: &mut Execution,
    jobs: Option<&rook_tools::jobs::Jobs>,
) -> Result<()> {
    for operation in std::mem::take(&mut state.background) {
        if state.owner == *OWNER && operation.job.as_deref().is_some_and(|id| id.starts_with("harness:")) {
            state.background.push(operation);
            continue;
        }
        let job = operation.job.as_deref().and_then(|id| jobs.and_then(|jobs| jobs.get(id)));
        match job {
            Some(job)
                if state.owner == *OWNER
                    && operation.registry == jobs.map(rook_tools::jobs::Jobs::identity) =>
            {
                if job.exit_code.is_none() {
                    state.background.push(operation);
                } else {
                    let body = serde_json::json!({"job":job.id,"exit_code":job.exit_code,"output":job.output,
                        "output_file":job.output_file,"output_complete":job.output_complete});
                    let seq = store.append_event(
                        session,
                        NewEvent::new(EventKind::ToolResult, Kind::Message, &serde_json::to_vec(&body)?)
                            .label("job"),
                    )?;
                    let meta = BTreeMap::from([
                        ("output_file".into(), body["output_file"].clone()),
                        ("output_complete".into(), body["output_complete"].clone()),
                    ]);
                    crate::results::register_capture(store, output_dir, session, seq, &meta)?;
                    state.last_result_seq = Some(seq);
                }
            }
            _ => uncertain(state, operation),
        }
    }
    Ok(())
}

impl Drop for Journal {
    fn drop(&mut self) {
        let mut active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
        let identity = (self.store.root().to_path_buf(), self.session);
        if active.get(&identity) != Some(&self.turn) {
            return;
        }
        if let Ok(Some(mut state)) = load(&self.store, self.session)
            && state.turn == self.turn
            && state.status == "running"
        {
            interrupt(&mut state, false);
            if let Err(error) = save(&self.store, self.session, &mut state) {
                tracing::error!("could not persist interrupted execution: {error}");
            }
        }
        active.remove(&identity);
        drop(active);
        if let Err(error) = crate::message_queue::followups::stopped(
            &self.store,
            self.session,
            &self.turn,
            "interrupted; inspect session recovery before continuing",
        ) {
            tracing::warn!("could not record interrupted follow-up: {error}");
        }
    }
}

#[derive(Serialize, Deserialize)]
struct EvaluationCache {
    contract: String,
    entries: Vec<CachedCheck>,
}

#[derive(Serialize, Deserialize)]
struct CachedCheck {
    result: Option<crate::evaluation::Scored>,
    witness: String,
}

fn witness_hash(workspace: &std::path::Path, card: &crate::evaluation::Scorecard) -> Result<String> {
    Ok(rook_store::ObjectId::of(&crate::persistence::encode(&crate::evaluation::witness(workspace, card))?)
        .to_string())
}

impl Rook {
    /// Work's evaluator uses the same durable boundary as an agent tool call.
    pub fn evaluate_recorded(
        &self,
        session: u128,
        card: &crate::evaluation::Scorecard,
        before: &crate::evaluation::Witness,
        jobs: Option<&rook_tools::jobs::Jobs>,
    ) -> Result<crate::evaluation::Report> {
        if let Some(reason) = self.recovery_block(session)? {
            return Err(CoreError::Other(reason));
        }
        let journal = Journal::start(self, session, jobs, false)?;
        journal.admit("Evaluate the work scorecard")?;
        if card.checks.len() > 256 {
            return Err(CoreError::Other("work recovery supports at most 256 checks".into()));
        }
        let key = format!("evaluation/{session:032x}");
        let contract = rook_store::ObjectId::of(&crate::persistence::encode(&(card, before))?).to_string();
        let mut cache: EvaluationCache = match self.store.kv_get(&key)? {
            Some(bytes) => serde_json::from_slice(&bytes)?,
            None => EvaluationCache { contract: contract.clone(), entries: Vec::new() },
        };
        if cache.contract != contract || cache.entries.len() > card.checks.len() {
            return Err(CoreError::Other("the saved evaluation uses a different scorecard or baseline; start a new work iteration to evaluate again".into()));
        }
        if cache.entries.iter().any(|entry| entry.result.is_none()) {
            return Err(CoreError::Other("a selected evaluation check has no saved result; resuming will not repeat it. Inspect the operation and start a new work run to evaluate again".into()));
        }
        let current_witness = witness_hash(&self.workspace, card)?;
        let previous: Vec<_> = cache
            .entries
            .iter()
            .filter_map(|entry| {
                let mut scored = entry.result.clone()?;
                if entry.witness != current_witness {
                    scored.touched.push(
                        "guarded files changed since this recorded check; the command was not repeated"
                            .into(),
                    );
                }
                Some(scored)
            })
            .collect();
        let result = crate::evaluation::run_observed(&self.workspace, card, before, &previous, |progress| {
            match progress {
                crate::evaluation::CheckProgress::Starting(check) => {
                    // Persist the fact that this check was selected before its intent.
                    // Even a crash in this gap must not cause an automatic replay.
                    cache
                        .entries
                        .push(CachedCheck { result: None, witness: witness_hash(&self.workspace, card)? });
                    crate::persistence::save_json(&self.store, &key, &cache)?;
                    journal.begin("harness:evaluation", &serde_json::to_string(check)?, true, false, jobs)
                }
                crate::evaluation::CheckProgress::Completed(scored) => {
                    if let Some(entry) = cache.entries.last_mut() {
                        entry.result = Some(scored.clone());
                        entry.witness = witness_hash(&self.workspace, card)?;
                    }
                    // A crash after this flush may leave an unknown operation receipt,
                    // but after inspection the known check result is reused, never rerun.
                    crate::persistence::save_json(&self.store, &key, &cache)?;
                    journal.complete(&serde_json::to_string(scored)?, jobs, None)
                }
            }
        });
        journal.finish(if result.is_ok() { "evaluated" } else { "failed" }, jobs)?;
        result
    }

    /// A final answer saved before the caller could publish its own run summary.
    pub fn completed_turn(&self, session: u128) -> Result<Option<crate::agent::TurnOutcome>> {
        let Some(state) = load(&self.store, session)? else { return Ok(None) };
        if state.status == "running" {
            return Ok(None);
        }
        let Some(bytes) = self.store.kv_get(&format!("execution-outcome/{session:032x}"))? else {
            return Ok(None);
        };
        let (turn, outcome): (String, crate::agent::TurnOutcome) = serde_json::from_slice(&bytes)?;
        Ok((turn == state.turn).then_some(outcome))
    }

    /// Latest execution receipts for this session and its delegated descendants.
    pub fn execution(&self, session: u128) -> Result<Vec<Execution>> {
        let all = self.store.list_sessions()?;
        if !all.iter().any(|m| m.id == session) {
            return Err(CoreError::NoSession(rook_store::format_session_id(session)));
        }
        let mut family = BTreeSet::from([session]);
        let mut queue = vec![session];
        while let Some(parent) = queue.pop() {
            for child in
                all.iter().filter(|m| m.parent == Some(parent) && m.tags.iter().any(|t| t == "subtask"))
            {
                if family.insert(child.id) {
                    queue.push(child.id);
                }
            }
        }
        let mut receipts = Vec::new();
        for id in family {
            if let Some(state) = load(&self.store, id)?
                && (id == session
                    || state.status == "running"
                    || state.status == "interrupted"
                    || !state.background.is_empty()
                    || !state.unknown.is_empty())
            {
                receipts.push(state);
                if receipts.len() > MAX_FAMILY {
                    return Err(CoreError::Other(
                        "more than 256 unresolved related executions; inspect child sessions separately"
                            .into(),
                    ));
                }
            }
        }
        Ok(receipts)
    }

    /// A user records their inspection; this never claims the operation succeeded.
    pub fn acknowledge_operation(&self, session: u128, operation: &str, note: &str) -> Result<()> {
        if note.trim().is_empty() || note.len() > 4096 {
            return Err(CoreError::Other("record an inspection note of 1..4096 bytes".into()));
        }
        let _active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = load(&self.store, session)?
            .ok_or_else(|| CoreError::Other("no execution receipt for this session".into()))?;
        if state.status == "running" {
            return Err(CoreError::Other(
                "stop the active turn before acknowledging an unknown operation".into(),
            ));
        }
        let Some(at) = state.unknown.iter().position(|p| p.id == operation) else {
            return Err(CoreError::Other(
                "that operation no longer needs acknowledgement; refresh the recovery report".into(),
            ));
        };
        let body = serde_json::json!({"operation":operation,"note":note,"meaning":"user reviewed the unknown result; this is not a success receipt"});
        self.log(session, EventKind::Note, "execution reviewed", &body.to_string())?;
        state.unknown.remove(at);
        if state.unknown.is_empty() && state.status == "needs_review" {
            state.status = "reviewed".into();
        }
        save(&self.store, session, &mut state)
    }

    pub(crate) fn recovery_block(&self, session: u128) -> Result<Option<String>> {
        // A resumed child also inherits unresolved work in its ancestor's family.
        let mut root = session;
        for _ in 0..MAX_FAMILY {
            let Some(meta) = self.store.get_session(root)? else { break };
            if !meta.tags.iter().any(|t| t == "subtask") {
                break;
            }
            match meta.parent {
                Some(parent) => root = parent,
                None => break,
            }
        }
        let states = self.execution(root)?;
        let unknown: Vec<_> = states
            .iter()
            .filter(|s| !s.unknown.is_empty())
            .map(|s| format!("{}: {} unknown operation(s)", s.session, s.unknown.len()))
            .collect();
        Ok((!unknown.is_empty()).then(|| format!("Changes are paused because an interrupted execution has unknown results ({}). Read the files and recorded output to inspect what happened. A user must inspect `rook session recovery <session>` and acknowledge each operation with an inspection note before changes resume. Do not retry commands or delegate around this restriction.", unknown.join("; "))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine(dir: &std::path::Path) -> Rook {
        Rook::from_parts(
            Store::open(dir.join("store")).unwrap(),
            crate::Config::default(),
            rook_skills::Environment::bare("linux", "x86_64", "0.10.0"),
            rook_skills::SkillIndex::default(),
            dir.into(),
        )
    }

    #[test]
    fn concurrent_prompt_admission_has_one_event_and_a_durable_matching_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let rook = engine(dir.path());
        let session = rook.start_session("prompt reservation").unwrap();
        let journal = Journal::start(&rook, session, None, false).unwrap();
        assert!(load(&rook.store, session).unwrap().unwrap().prompt.is_none());
        rook.log(session, EventKind::Note, "hook", "setup before admission").unwrap();
        let barrier = std::sync::Barrier::new(2);
        let (one, two) = std::thread::scope(|scope| {
            let admit = || {
                barrier.wait();
                journal
                    .admit_prompt(&rook, "redacted preview", "original 🙂 prompt", "", None, None)
                    .unwrap()
                    .0
            };
            let one = scope.spawn(admit);
            let two = scope.spawn(admit);
            (one.join().unwrap(), two.join().unwrap())
        });
        assert_eq!(one, two);
        assert_eq!(one, 1, "a hook note occupies the reservation's original sequence");
        assert!(journal.admit_prompt(&rook, "other", "different prompt", "", None, None).is_err());
        assert!(
            journal.admit_prompt(&rook, "other", "original 🙂 prompt", "other label", None, None).is_err()
        );
        let state = load(&rook.store, session).unwrap().unwrap();
        assert_eq!(state.task, "redacted preview");
        assert_eq!(state.prompt.unwrap().seq, one);
        assert_eq!(rook.store.not_on_disk_yet(), 0);
        drop(journal);
        drop(rook);
        let rook = engine(dir.path());
        let state = load(&rook.store, session).unwrap().unwrap();
        assert_eq!(state.status, "interrupted");
        let admission = state.prompt.unwrap();
        let events = rook.store.events(session, 0, 100).unwrap();
        assert_eq!(events.iter().filter(|e| e.record.kind == EventKind::UserMessage).count(), 1);
        let event = events.iter().find(|e| e.seq == admission.seq).unwrap();
        assert_eq!(admission.body, event.record.body.to_string());
        assert_eq!(rook.store.get(&event.record.body).unwrap(), "original 🙂 prompt".as_bytes());
    }

    #[test]
    fn an_old_execution_cannot_admit_into_its_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let rook = engine(dir.path());
        let session = rook.start_session("ownership").unwrap();
        let old = Journal::start(&rook, session, None, false).unwrap();
        old.finish("failed", None).unwrap();
        let current = Journal::start(&rook, session, None, false).unwrap();
        assert!(old.admit_prompt(&rook, "stale", "stale", "", None, None).is_err());
        assert!(load(&rook.store, session).unwrap().unwrap().prompt.is_none());
        assert!(rook.store.events(session, 0, 100).unwrap().is_empty());
        current.admit_prompt(&rook, "current", "current", "", None, None).unwrap();
        let state = load(&rook.store, session).unwrap().unwrap();
        assert_eq!(state.turn, current.turn);
        let mut legacy = serde_json::to_value(state).unwrap();
        legacy.as_object_mut().unwrap().remove("prompt");
        legacy.as_object_mut().unwrap().remove("continuation");
        let legacy = serde_json::from_value::<Execution>(legacy).unwrap();
        assert!(legacy.prompt.is_none());
        assert!(legacy.continuation.is_none());
        assert_eq!(legacy.completion_boundary(), legacy.turn);
    }
}
