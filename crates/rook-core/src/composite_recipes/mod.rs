//! Bounded stage receipts for explicitly selected, frozen recipe compositions.
mod settings;
use crate::{CoreError, Result, Rook};
use rook_store::{EventKind, Kind, NewEvent};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
pub use settings::Settings;
static WRITING: std::sync::Mutex<()> = std::sync::Mutex::new(());
pub(crate) const TOOL: &str = "run_subrecipes";
fn bad(message: impl Into<String>) -> CoreError {
    CoreError::Other(message.into())
}
fn key(session: u128) -> String {
    format!("composite-recipes/{session:032x}")
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Stage {
    pub id: String,
    pub recipe: String,
    pub parameters: std::collections::BTreeMap<String, String>,
    pub inputs: Vec<Input>,
    pub child: Option<String>,
    pub state: String,
    pub output: Option<u64>,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Input {
    pub stage: String,
    pub parameter: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Call {
    pub version: u32,
    pub id: String,
    pub turn: String,
    pub tool_call: String,
    pub fingerprint: String,
    pub mode: String,
    pub state: String,
    pub stages: Vec<Stage>,
    pub definitions: Vec<u64>,
    pub sources: std::collections::BTreeMap<u64, String>,
    pub note: Option<String>,
}
#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Book {
    calls: Vec<Call>,
}
fn read(rook: &Rook, session: u128) -> Result<Book> {
    let raw = rook.store.kv_get_limited(&key(session), rook.config.composite_recipes.state_bytes)?;
    let book = raw.map(|b| serde_json::from_slice::<Book>(&b)).transpose()?.unwrap_or_default();
    if book.calls.len() > rook.config.composite_recipes.max_calls
        || book
            .calls
            .iter()
            .any(|c| c.version != 1 || c.stages.len() > rook.config.composite_recipes.max_stages)
    {
        return Err(bad(
            "Composite receipts exceed supported version/bounds; restore configuration before inspection",
        ));
    }
    Ok(book)
}
fn save(rook: &Rook, session: u128, book: &Book) -> Result<()> {
    let bytes = crate::persistence::encode_with_limit(book, rook.config.composite_recipes.state_bytes)?;
    rook.store.append_events_with_values(session, [], &[(&key(session), &bytes)])?;
    Ok(())
}
pub(crate) fn begin(
    rook: &Rook,
    session: u128,
    call: Call,
    definition: &crate::recipes::Frozen,
) -> Result<(Call, bool)> {
    let _lock = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let mut book = read(rook, session)?;
    if let Some(old) = book.calls.iter().find(|c| c.turn == call.turn && c.tool_call == call.tool_call) {
        if old.fingerprint != call.fingerprint {
            return Err(bad("Composite call identity already belongs to different arguments"));
        }
        return Ok((old.clone(), false));
    }
    if book.calls.len() >= rook.config.composite_recipes.max_calls {
        return Err(bad("Composite call capacity reached; inspect and forget resolved receipts"));
    }
    let mut call = call;
    call.state = "preparing".into();
    book.calls.push(call.clone());
    save(rook, session, &book)?;
    let mut sources = Vec::new();
    collect(definition, &mut sources);
    for source in sources {
        let label = format!("recipe definition: {}", source.path);
        let event = NewEvent::new(EventKind::Note, Kind::Message, source.source.as_bytes()).label(&label);
        rook.store.append_event_with_receipt(session, event, &key(session), |seq| {
            let saved = book.calls.last_mut().ok_or_else(|| {
                rook_store::StoreError::Encoding("Composite call disappeared during source capture".into())
            })?;
            saved.definitions.push(seq);
            saved.sources.insert(seq, rook_store::ObjectId::of(source.source.as_bytes()).to_hex());
            crate::persistence::encode_with_limit(&book, rook.config.composite_recipes.state_bytes)
                .map_err(|e| rook_store::StoreError::Encoding(e.to_string()))
        })?;
    }
    book.calls.last_mut().ok_or_else(|| bad("Composite call missing after definition capture"))?.state =
        "running".into();
    save(rook, session, &book)?;
    call = book.calls.pop().ok_or_else(|| bad("Composite call missing"))?;
    Ok((call, true))
}
fn collect<'a>(root: &'a crate::recipes::Frozen, items: &mut Vec<&'a crate::recipes::Frozen>) {
    items.push(root);
    for child in root.children.values() {
        collect(&child.definition, items);
    }
}
pub(crate) fn started(
    rook: &Rook,
    session: u128,
    id: &str,
    index: usize,
    parameters: std::collections::BTreeMap<String, String>,
) -> Result<u128> {
    let _lock = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let mut book = read(rook, session)?;
    let original = rook.store.kv_get_limited(&key(session), rook.config.composite_recipes.state_bytes)?;
    let parent = rook.store.get_session(session)?.ok_or_else(|| bad("Composite parent session missing"))?;
    let child = rook_store::new_session_id();
    let call = book.calls.iter_mut().find(|c| c.id == id).ok_or_else(|| bad("Composite call missing"))?;
    let stage = call.stages.get_mut(index).ok_or_else(|| bad("Composite stage index missing"))?;
    if stage.state != "pending" {
        return Err(bad("Composite stage already admitted; inspect its original child"));
    }
    stage.child = Some(rook_store::format_session_id(child));
    stage.parameters = parameters;
    stage.state = "running".into();
    let source = crate::persistence::encode_with_limit(
        &json!({"call":id,"stage":stage.id,"recipe":stage.recipe,"parameters":stage.parameters}),
        rook.config.composite_recipes.state_bytes,
    )?;
    let title = format!("Recipe stage {}", stage.id);
    let mut meta = rook_store::SessionMeta::new(
        child,
        title,
        rook.workspace.display().to_string(),
        rook_store::now_unix(),
    );
    meta.parent = Some(session);
    meta.model = rook.config.agent.model.clone();
    meta.agent = format!("rook {}", crate::AGENT_VERSION);
    meta.tags.push("subtask".into());
    if parent.tags.iter().any(|t| t == "rook:work") {
        meta.tags.push("rook:work".into());
    }
    let bytes = crate::persistence::encode_with_limit(&book, rook.config.composite_recipes.state_bytes)?;
    if !rook.store.create_session_with_companion_checked(
        &meta,
        NewEvent::new(EventKind::Note, Kind::Message, &source).label("recipe stage inputs"),
        &key(session),
        original.as_deref(),
        &bytes,
        rook.config.composite_recipes.state_bytes,
    )? {
        return Err(bad("Composite stage changed before atomic child admission"));
    }
    rook.store.kv_update_session_values(
        child,
        &[(&format!("fork-at/{child:032x}"), parent.next_seq.saturating_sub(1).to_string().as_bytes())],
    )?;
    Ok(child)
}
pub(crate) fn finish_stage(
    rook: &Rook,
    session: u128,
    id: &str,
    index: usize,
    state: &str,
    reply: &str,
    error: Option<String>,
) -> Result<()> {
    let _lock = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let mut book = read(rook, session)?;
    let call = book.calls.iter_mut().find(|c| c.id == id).ok_or_else(|| bad("Composite call missing"))?;
    let stage = call.stages.get_mut(index).ok_or_else(|| bad("Composite stage missing"))?;
    if !matches!(stage.state.as_str(), "pending" | "running") {
        return Err(bad("Composite stage already resolved"));
    }
    if reply.len() > rook.config.composite_recipes.result_bytes {
        return Err(bad(
            "Subrecipe output exceeds composite_recipes.result_bytes; full child history remains available",
        ));
    }
    stage.state = state.into();
    stage.error = error.map(|e| e.chars().take(512).collect());
    if !reply.is_empty() {
        let event =
            NewEvent::new(EventKind::Note, Kind::Message, reply.as_bytes()).label("recipe stage output");
        rook.store.append_event_with_receipt(session, event, &key(session), |seq| {
            let saved = book
                .calls
                .iter_mut()
                .find(|c| c.id == id)
                .and_then(|c| c.stages.get_mut(index))
                .ok_or_else(|| {
                    rook_store::StoreError::Encoding("Composite stage missing during output capture".into())
                })?;
            saved.output = Some(seq);
            book.calls
                .iter_mut()
                .find(|c| c.id == id)
                .ok_or_else(|| rook_store::StoreError::Encoding("Composite call missing".into()))?
                .sources
                .insert(seq, rook_store::ObjectId::of(reply.as_bytes()).to_hex());
            crate::persistence::encode_with_limit(&book, rook.config.composite_recipes.state_bytes)
                .map_err(|e| rook_store::StoreError::Encoding(e.to_string()))
        })?;
    } else {
        save(rook, session, &book)?;
    }
    Ok(())
}
pub(crate) fn finish(rook: &Rook, session: u128, id: &str, state: &str) -> Result<Call> {
    change(rook, session, id, |call| {
        call.state =
            if call.stages.iter().any(|s| s.state == "interrupted") { "interrupted" } else { state }.into();
        if state != "completed" {
            for stage in &mut call.stages {
                if stage.state == "pending" {
                    stage.state = "skipped".into();
                } else if stage.state == "running" {
                    stage.state = "interrupted".into();
                }
            }
        }
        Ok(call.clone())
    })
}
fn change<T>(rook: &Rook, session: u128, id: &str, f: impl FnOnce(&mut Call) -> Result<T>) -> Result<T> {
    let _lock = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let mut book = read(rook, session)?;
    let call =
        book.calls.iter_mut().find(|c| c.id == id).ok_or_else(|| bad("Composite call receipt missing"))?;
    let result = f(call)?;
    save(rook, session, &book)?;
    Ok(result)
}
pub(crate) fn output(rook: &Rook, session: u128, id: &str, stage: &str) -> Result<String> {
    let book = read(rook, session)?;
    let call = book.calls.iter().find(|c| c.id == id).ok_or_else(|| bad("Composite call missing"))?;
    let item =
        call.stages.iter().find(|s| s.id == stage).filter(|s| s.state == "completed").ok_or_else(|| {
            bad("Source stage did not complete; its partial output cannot authorize a dependent stage")
        })?;
    let seq = item.output.ok_or_else(|| bad("Source stage produced no output"))?;
    let event = rook
        .store
        .events(session, seq, 1)?
        .into_iter()
        .next()
        .ok_or_else(|| bad("Source output was deleted"))?;
    if event.seq != seq
        || call.sources.get(&seq).is_none_or(|hash| *hash != event.record.body.to_hex())
        || event.record.label != "recipe stage output"
    {
        return Err(bad("Source stage output anchor changed"));
    }
    let cap = rook.config.composite_recipes.result_bytes;
    if rook.store.stat_object(&event.record.body)?.is_none_or(|m| m.size_raw > cap as u64) {
        return Err(bad("Saved source output exceeds its configured bound"));
    }
    String::from_utf8(rook.store.get_range(&event.record.body, 0, cap)?)
        .map_err(|_| bad("Saved subrecipe output is not UTF-8"))
}
pub(crate) struct Guard<'a> {
    pub rook: &'a Rook,
    pub session: u128,
    pub id: String,
    pub open: bool,
}
impl Drop for Guard<'_> {
    fn drop(&mut self) {
        if self.open {
            let _ = finish(self.rook, self.session, &self.id, "interrupted");
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    List { after: Option<String> },
    Inspect { id: String },
    Parameters { id: String, stage: String, offset: u64 },
    Read { id: String, seq: u64, offset: u64 },
    Resolve { id: String, note: String },
    Forget { id: String },
}
pub fn arguments(text: &str) -> Result<Request> {
    if text.len() > 8192 {
        return Err(bad("Recipe receipt request exceeds 8 KiB"));
    }
    if text.trim().is_empty() {
        return Ok(Request::List { after: None });
    }
    Ok(serde_json::from_str(text)?)
}
impl Rook {
    pub fn composite_request(&self, session: u128, request: Request) -> Result<Value> {
        if self.store.get_session(session)?.is_none() {
            return Err(bad("Composite parent session does not exist"));
        }
        let _lock = WRITING.lock().unwrap_or_else(|e| e.into_inner());
        let mut book = read(self, session)?;
        match request {
            Request::List { after } => {
                let mut calls: Vec<_> =
                    book.calls.iter().filter(|c| after.as_ref().is_none_or(|a| c.id > *a)).collect();
                calls.sort_by(|a, b| a.id.cmp(&b.id));
                Ok(
                    json!({"calls":calls.iter().take(16).map(|c|json!({"id":c.id,"mode":c.mode,"state":state(self,session,c),"stages":c.stages.len()})).collect::<Vec<_>>(),"after":calls.get(16).and_then(|_|calls.get(15)).map(|c|&c.id)}),
                )
            }
            Request::Inspect { id } => {
                let call =
                    book.calls.iter().find(|c| c.id == id).ok_or_else(|| bad("Composite call missing"))?;
                let stages:Vec<_>=call.stages.iter().map(|s|{
    let parameters=crate::persistence::encode_with_limit(&s.parameters,4096).ok().and_then(|b|serde_json::from_slice::<Value>(&b).ok());
    json!({"id":s.id,"recipe":s.recipe,"state":s.state,"child":s.child,"output":s.output,"error":s.error,"inputs":s.inputs,"parameters_omitted":parameters.is_none(),"parameters":parameters})
   }).collect();
                Ok(
                    json!({"version":call.version,"id":call.id,"turn":call.turn,"tool_call":call.tool_call,"fingerprint":call.fingerprint,"mode":call.mode,"state":state(self,session,call),"definitions":call.definitions,"sources":call.sources,"note":call.note,"stages":stages}),
                )
            }
            Request::Parameters { id, stage, offset } => {
                let item = book
                    .calls
                    .iter()
                    .find(|c| c.id == id)
                    .and_then(|c| c.stages.iter().find(|s| s.id == stage))
                    .ok_or_else(|| bad("Composite stage missing"))?;
                let bytes = crate::persistence::encode_with_limit(
                    &item.parameters,
                    self.config.composite_recipes.state_bytes,
                )?;
                let start = usize::try_from(offset)
                    .ok()
                    .filter(|start| *start <= bytes.len())
                    .ok_or_else(|| bad("Parameter offset exceeds its source"))?;
                let end = start.saturating_add(4096).min(bytes.len());
                use base64::Engine;
                Ok(
                    json!({"id":id,"stage":stage,"offset":offset,"total_bytes":bytes.len(),"bytes_base64":base64::engine::general_purpose::STANDARD.encode(&bytes[start..end]),"next_offset":(end<bytes.len()).then_some(end)}),
                )
            }
            Request::Read { id, seq, offset } => {
                let call =
                    book.calls.iter().find(|c| c.id == id).ok_or_else(|| bad("Composite call missing"))?;
                if !call.definitions.contains(&seq) && !call.stages.iter().any(|s| s.output == Some(seq)) {
                    return Err(bad("Source event does not belong to this composite call"));
                }
                let event = self
                    .store
                    .events(session, seq, 1)?
                    .into_iter()
                    .next()
                    .filter(|e| e.seq == seq)
                    .ok_or_else(|| bad("Composite source event missing"))?;
                if call.sources.get(&seq).is_none_or(|hash| *hash != event.record.body.to_hex()) {
                    return Err(bad("Composite source anchor changed; do not read replacement events"));
                }
                let total = self
                    .store
                    .stat_object(&event.record.body)?
                    .ok_or_else(|| bad("Composite source object missing"))?
                    .size_raw;
                if offset > total {
                    return Err(bad("Source offset exceeds its size"));
                }
                let bytes = self.store.get_range(&event.record.body, offset, 4096)?;
                use base64::Engine;
                let next = offset.saturating_add(bytes.len() as u64);
                Ok(
                    json!({"id":id,"seq":seq,"offset":offset,"total_bytes":total,"bytes_base64":base64::engine::general_purpose::STANDARD.encode(&bytes),"next_offset":(next<total).then_some(next)}),
                )
            }
            Request::Resolve { id, note } => {
                if note.trim().is_empty() || note.len() > 512 {
                    return Err(bad("Explain the reviewed interruption in 1–512 bytes"));
                }
                let at = book
                    .calls
                    .iter()
                    .position(|c| c.id == id)
                    .ok_or_else(|| bad("Composite call missing"))?;
                if state(self, session, &book.calls[at]) != "interrupted"
                    || book.calls[at].stages.iter().any(|s| {
                        s.child
                            .as_deref()
                            .and_then(rook_store::parse_session_id)
                            .is_some_and(|child| crate::execution::is_active(self, child))
                    })
                {
                    return Err(bad("Composite call is not interrupted or a child is still active"));
                }
                book.calls[at].state = "reviewed".into();
                book.calls[at].note = Some(note);
                save(self, session, &book)?;
                Ok(
                    json!({"id":id,"state":"reviewed","replayed":false,"original_execution_acknowledgement":false}),
                )
            }
            Request::Forget { id } => {
                let call =
                    book.calls.iter().find(|c| c.id == id).ok_or_else(|| bad("Composite call missing"))?;
                if !matches!(call.state.as_str(), "completed" | "failed" | "reviewed") {
                    return Err(bad(
                        "Inspect and explicitly resolve interrupted stages before forgetting the receipt",
                    ));
                }
                book.calls.retain(|c| c.id != id);
                save(self, session, &book)?;
                Ok(json!({"id":id,"state":"forgotten","source_events_retained":true}))
            }
        }
    }
}
fn state<'a>(rook: &Rook, session: u128, call: &'a Call) -> &'a str {
    if matches!(call.state.as_str(), "running" | "preparing")
        && (!crate::execution::is_active(rook, session)
            || crate::execution::current(rook, session).ok().flatten().is_none_or(|e| e.turn != call.turn))
    {
        "interrupted"
    } else {
        &call.state
    }
}
