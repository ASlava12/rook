//! Negotiated ACP v1 previews. Child controls follow the runtime's allowlist.
use super::{Peer, protocol};
use rook_core::Rook;
use rook_core::agent::{
    Progress,
    observe::{Event, Observer},
};
use rook_llm::Delta;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const MAX_CHILDREN: usize = 1024;
const MAX_DISPLAY_BYTES: usize = 64 * 1024;

pub(super) fn restricted(rook: &Rook, session: &str) -> Result<bool, protocol::Error> {
    let Some(id) = rook_store::parse_session_id(session) else { return Ok(false) };
    let meta = rook.store.get_session(id).map_err(|error| protocol::Error::internal(error.to_string()))?;
    Ok(meta.is_some_and(|meta| meta.tags.iter().any(|tag| tag == "subtask")))
}

pub(super) fn prefix(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

struct Child {
    parent: u128,
    turn: String,
    message: u64,
    streaming: u8,
    started: u64,
    finished: u64,
    state: serde_json::Value,
}

pub(super) struct Preview {
    rook: Arc<Rook>,
    peer: Arc<Peer>,
    root: u128,
    subagents: bool,
    compaction: bool,
    children: Mutex<HashMap<u128, Child>>,
}
impl Preview {
    pub fn new(rook: Arc<Rook>, peer: Arc<Peer>, root: u128, subagents: bool, compaction: bool) -> Self {
        Self { rook, peer, root, subagents, compaction, children: Mutex::new(HashMap::new()) }
    }
    fn update(&self, session: u128, update: serde_json::Value) {
        self.peer.notify(
            "session/update",
            serde_json::json!({
                "sessionId": rook_store::format_session_id(session), "update": update,
            }),
        );
    }
    fn association(&self, parent: u128, child: u128, state: serde_json::Value) {
        self.update(parent, serde_json::json!({
            "sessionUpdate":"subagent_update", "sessionId":rook_store::format_session_id(child), "state":state,
        }));
    }
    fn announce(&self, parent: u128, child: u128, task: &str, running: bool) {
        if !self.subagents {
            return;
        }
        let mut children = self.children.lock().unwrap_or_else(|e| e.into_inner());
        if child == self.root || child == parent || (parent != self.root && !children.contains_key(&parent)) {
            self.peer.closed.notify_one();
            return;
        }
        if let Some(known) = children.get(&child) {
            if known.parent != parent {
                self.peer.closed.notify_one();
            }
            return;
        }
        if children.len() >= MAX_CHILDREN {
            self.peer.closed.notify_one();
            return;
        }
        children.insert(
            child,
            Child {
                parent,
                turn: rook_store::format_session_id(rook_store::new_session_id()),
                message: 0,
                streaming: 0,
                started: 0,
                finished: 0,
                state: serde_json::json!({"state":if running {"running"} else {"unknown"}}),
            },
        );
        // The association precedes every child-addressed event/request. The
        // runtime has no independently cancellable child handle, so no control
        // is advertised. Descriptions are display metadata, not copied prompts.
        self.update(
            parent,
            serde_json::json!({
                "sessionUpdate":"subagent_update", "sessionId":rook_store::format_session_id(child),
                "title":prefix(task.lines().next().unwrap_or(task), 256),
                "description":prefix(task, 1024),
                "capabilities":{},
                "state":{"state":if running {"running"} else {"unknown"}},
            }),
        );
        if running {
            let shown =
                prefix(task, MAX_DISPLAY_BYTES.min(self.peer.outbound.byte_limit().saturating_sub(1024) / 6));
            self.update(parent,serde_json::json!({
                "sessionUpdate":"session_message","messageId":format!("assign_{}",rook_store::format_session_id(child)),
                "senderSessionId":rook_store::format_session_id(parent),"recipientSessionId":rook_store::format_session_id(child),
                "content":[{"type":"text","text":shown}],
                "_meta":{"rook":{"taskTruncated":shown.len() != task.len(),"savedSession":rook_store::format_session_id(child)}},
            }));
        }
    }
    fn known(&self, session: u128) -> bool {
        session == self.root || self.children.lock().unwrap_or_else(|e| e.into_inner()).contains_key(&session)
    }
    pub fn action(&self, session: &str) -> Action<'_> {
        let child = rook_store::parse_session_id(session);
        if let Some(child) = child {
            self.child_state(child, "requires_action");
        }
        Action { preview: self, child }
    }
    pub fn approval_id(&self, session: &str, tool: &str) -> String {
        let children = self.children.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(child) = rook_store::parse_session_id(session).and_then(|id| children.get(&id))
            && child.finished < child.started
        {
            return format!("{}:call_{}", child.turn, child.finished);
        }
        format!("approval_{}", prefix(tool, 256))
    }
    fn child_state(&self, child: u128, state: &str) {
        let mut children = self.children.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(known) = children.get_mut(&child) {
            known.state = serde_json::json!({"state":state});
            self.association(known.parent, child, known.state.clone());
        }
    }
    fn child_progress(&self, session: u128, workspace: &std::path::Path, progress: &Progress<'_>) {
        let mut children = self.children.lock().unwrap_or_else(|e| e.into_inner());
        let Some(child) = children.get_mut(&session) else {
            return;
        };
        match progress {
            Progress::Turn { id } => {
                child.turn = prefix(id, 128).into();
                child.streaming = 0;
                child.message = 0;
                child.started = 0;
                child.finished = 0;
                child.state = serde_json::json!({"state":"running"});
                self.association(child.parent, session, child.state.clone());
            }
            Progress::Delta(Delta::Text(text) | Delta::Reasoning(text)) => {
                let kind = if matches!(progress, Progress::Delta(Delta::Text(_))) { 2 } else { 1 };
                if child.streaming != kind {
                    child.streaming = kind;
                    child.message += 1;
                }
                let message = format!("{}:msg_{}", child.turn, child.message);
                self.narrative_to(session, text, &message, kind == 1, (kind == 2).then_some(child.parent));
            }
            Progress::Delta(Delta::ToolCall(call)) => {
                child.streaming = 0;
                let id = format!("{}:call_{}", child.turn, child.started);
                child.started += 1;
                self.peer.notify(
                    "session/update",
                    protocol::tool_call(
                        &rook_store::format_session_id(session),
                        &id,
                        prefix(&call.name, 256),
                        prefix(&rook_core::calls::doing(&call.name, Some(&call.arguments), workspace), 1024),
                        protocol::tool_kind(&call.name),
                    ),
                );
            }
            Progress::ToolDone { failed, .. } => {
                let id = format!("{}:call_{}", child.turn, child.finished);
                child.finished += 1;
                self.peer.notify(
                    "session/update",
                    protocol::tool_call_done(&rook_store::format_session_id(session), &id, *failed),
                );
            }
            Progress::Context { used, size } => self.peer.notify(
                "session/update",
                protocol::usage_update(&rook_store::format_session_id(session), *used, *size),
            ),
            _ => {}
        }
    }
    pub fn narrative(&self, session: u128, text: &str, message: &str, thought: bool) {
        self.narrative_to(session, text, message, thought, None);
    }
    fn narrative_to(&self, session: u128, text: &str, message: &str, thought: bool, recipient: Option<u128>) {
        let id = rook_store::format_session_id(session);
        // Reserve JSON overhead before copying either root or child text.
        let limit = ((self.peer.outbound.byte_limit().saturating_sub(1024)) / 6).clamp(512, 32768);
        let mut left = text;
        while !left.is_empty() {
            let part = prefix(left, limit);
            let update = if let Some(parent) = recipient {
                serde_json::json!({"sessionId":id,"update":{
                    "sessionUpdate":"session_message_chunk","messageId":message,
                    "senderSessionId":id,"recipientSessionId":rook_store::format_session_id(parent),
                    "content":{"type":"text","text":part},
                }})
            } else if thought {
                protocol::agent_thought_chunk(&id, part, message)
            } else {
                protocol::agent_message_chunk(&id, part, message)
            };
            self.peer.notify("session/update", update);
            left = &left[part.len()..];
        }
    }
    pub fn snapshot(&self) {
        let children = self.children.lock().unwrap_or_else(|e| e.into_inner());
        let mut queue = std::collections::VecDeque::from([self.root]);
        while let Some(parent) = queue.pop_front() {
            for (&id, child) in children.iter().filter(|(_, child)| child.parent == parent) {
                self.update(parent,serde_json::json!({
                    "sessionUpdate":"subagent_update","sessionId":rook_store::format_session_id(id),"capabilities":{},
                    "state":child.state,
                }));
                queue.push_back(id);
            }
        }
    }
    fn compaction(&self, session: u128, id: &str, status: &str, summary: Option<&str>, error: Option<&str>) {
        if !self.known(session) {
            return;
        }
        if !self.compaction {
            self.peer.notify(
                "session/update",
                protocol::agent_thought_chunk(
                    &rook_store::format_session_id(session),
                    &format!("[context compaction: {status}]\n"),
                    &format!("compact_{id}"),
                ),
            );
            return;
        }
        let mut update =
            serde_json::json!({"sessionUpdate":"compaction_update", "compactionId":id, "status":status});
        if let Some(summary) = summary {
            let maximum = MAX_DISPLAY_BYTES.min(self.peer.outbound.byte_limit().saturating_sub(1024) / 6);
            let shown = prefix(summary, maximum);
            update["summary"] = serde_json::json!([{"type":"text", "text":shown}]);
            if shown.len() != summary.len() {
                update["_meta"] = serde_json::json!({"rook": {"summaryTruncated":true, "savedSession":rook_store::format_session_id(session)}});
            }
        }
        if let Some(error) = error {
            update["error"] = prefix(error, 512).into();
        }
        self.update(session, update);
    }

    pub fn recover(&self) -> rook_core::Result<bool> {
        // Restore only bounded metadata and completed compactions. Child history
        // replay is best-effort in this RFD; do not replay opaque provider state.
        let mut queue = std::collections::VecDeque::from([self.root]);
        let mut visited = std::collections::HashSet::new();
        let mut pages = 0;
        let mut truncated = false;
        let mut compactions = 0;
        while let Some(parent) = queue.pop_front() {
            if visited.contains(&parent) {
                continue;
            }
            if visited.len() >= 64 {
                truncated = true;
                break;
            }
            visited.insert(parent);
            if self.compaction {
                let meta = self.rook.store.get_session(parent)?.ok_or_else(|| {
                    rook_core::CoreError::Other("session disappeared during ACP recovery".into())
                })?;
                truncated |= meta.next_seq > 128;
                for event in self.rook.store.events(parent, meta.next_seq.saturating_sub(128), 128)? {
                    if event.record.kind != rook_store::EventKind::Compaction {
                        continue;
                    }
                    if compactions >= 32 {
                        truncated = true;
                        break;
                    }
                    compactions += 1;
                    let Some(stat) = self.rook.store.stat_object(&event.record.body)? else {
                        continue;
                    };
                    // The core's configured summary cap is at most 1 MiB. JSON
                    // escaping may take six bytes per admitted source byte.
                    const MAX_RECORD: usize = 6 * 1024 * 1024 + 8192;
                    if stat.size_raw > MAX_RECORD as u64 {
                        self.compaction(
                            parent,
                            &format!("saved_{}_{}", rook_store::format_session_id(parent), event.seq),
                            "completed",
                            None,
                            None,
                        );
                        continue;
                    }
                    let body = self.rook.store.get_range(&event.record.body, 0, MAX_RECORD)?;
                    let Ok(body) = serde_json::from_slice::<serde_json::Value>(&body) else {
                        continue;
                    };
                    let id = body["compaction_id"]
                        .as_str()
                        .filter(|id| rook_store::parse_session_id(id).is_some())
                        .map(str::to_owned)
                        .unwrap_or_else(|| {
                            format!("saved_{}_{}", rook_store::format_session_id(parent), event.seq)
                        });
                    self.compaction(parent, &id, "completed", body["summary"].as_str(), None);
                }
            }
            if !self.subagents {
                continue;
            }
            let mut query = rook_core::branches::Query::default();
            loop {
                if pages >= 128 {
                    truncated = true;
                    break;
                }
                pages += 1;
                let page = rook_core::branches::page(&self.rook, parent, &query)?;
                for node in page.children.iter().filter(|node| node.delegated) {
                    if self.children.lock().unwrap_or_else(|e| e.into_inner()).len() >= 63 {
                        truncated = true;
                        break;
                    }
                    let Some(child) = rook_store::parse_session_id(&node.id) else {
                        continue;
                    };
                    self.announce(parent, child, &node.title, false);
                    if let Some(outcome) = self.rook.completed_turn(child)? {
                        self.association(
                            parent,
                            child,
                            serde_json::json!({"state":"idle", "stopReason":child_stop(&outcome.stopped)}),
                        );
                    }
                    if queue.len() < 64 {
                        queue.push_back(child);
                    } else {
                        truncated = true;
                    }
                }
                let Some(next) = page.next else {
                    break;
                };
                query.after = Some(next);
            }
        }
        Ok(truncated)
    }
}

pub(super) struct Action<'a> {
    preview: &'a Preview,
    child: Option<u128>,
}
impl Drop for Action<'_> {
    fn drop(&mut self) {
        if let Some(child) = self.child {
            self.preview.child_state(child, "running");
        }
    }
}

fn child_stop(reason: &str) -> &str {
    match reason {
        "max_steps" => "max_turn_requests",
        "stop" => "end_turn",
        other => other,
    }
}
impl Observer for Preview {
    fn observe(&self, event: Event<'_>) {
        match event {
            Event::ChildStarted { parent, child, task } => self.announce(parent, child, task, true),
            Event::ChildEnded { child, result } => {
                let mut children = self.children.lock().unwrap_or_else(|e| e.into_inner());
                let Some(known) = children.get_mut(&child) else {
                    return;
                };
                let state = match result {
                    None => serde_json::json!({"state":"unknown"}),
                    Some(Ok(reason)) => serde_json::json!({"state":"idle", "stopReason":child_stop(reason)}),
                    Some(Err(_)) => serde_json::json!({"state":"idle", "stopReason":"error",
                        "error":{"code":-32603,"message":"Child turn failed; inspect its saved session."}}),
                };
                known.state = state;
                self.association(known.parent, child, known.state.clone());
            }
            Event::Progress { session, workspace, progress } if session != self.root => {
                self.child_progress(session, workspace, progress)
            }
            Event::Progress { .. } => {}
            Event::CompactionStarted { session, id } => {
                self.compaction(session, &rook_store::format_session_id(id), "in_progress", None, None)
            }
            Event::CompactionEnded { session, id, result } => match result {
                None => self.compaction(session, &rook_store::format_session_id(id), "cancelled", None, None),
                Some(Ok((_, summary))) => self.compaction(
                    session,
                    &rook_store::format_session_id(id),
                    "completed",
                    Some(summary),
                    None,
                ),
                Some(Err(_)) => self.compaction(
                    session,
                    &rook_store::format_session_id(id),
                    "failed",
                    None,
                    Some("Compaction failed; inspect the saved session."),
                ),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        preview: Preview,
        frames: rook_core::delivery::Receiver,
        _dirs: (tempfile::TempDir, tempfile::TempDir),
    }
    fn fixture() -> Fixture {
        let store = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let rook = Rook::from_parts(
            rook_store::Store::open(store.path()).unwrap(),
            Default::default(),
            rook_skills::Environment::bare("linux", "x86_64", "0.1.0"),
            Default::default(),
            workspace.path().to_owned(),
        );
        let (sender, frames) = rook_core::delivery::channel(4096, 4 * 1024 * 1024);
        let peer = Arc::new(Peer::new(sender, Arc::new(tokio::sync::Notify::new())));
        Fixture {
            preview: Preview::new(Arc::new(rook), peer, 1, true, true),
            frames,
            _dirs: (store, workspace),
        }
    }
    #[tokio::test]
    async fn duplicate_child_announcements_are_idempotent_but_reparenting_and_cycles_close_observation() {
        for invalid in [(1, 3), (3, 1), (2, 2), (99, 4)] {
            let mut f = fixture();
            f.preview.announce(1, 2, "child", false);
            f.preview.announce(2, 3, "grandchild", false);
            f.frames.recv().await.unwrap();
            f.frames.recv().await.unwrap();
            f.preview.announce(1, 2, "duplicate", true);
            tokio::select! {
                biased;
                event = f.frames.recv() => panic!("duplicate announcement: {}",event.unwrap().text),
                _ = std::future::ready(()) => {}
            }
            f.preview.announce(invalid.0, invalid.1, "invalid association", true);
            tokio::select! {
                biased;
                _ = f.preview.peer.closed.notified() => {},
                _ = std::future::ready(()) => panic!("invalid relation was accepted: {invalid:?}")
            }
            assert_eq!(f.preview.children.lock().unwrap().len(), 2);
        }
    }
    #[tokio::test]
    async fn a_live_child_bound_closes_the_view_and_future_stop_reasons_survive() {
        let mut f = fixture();
        for child in 2..=MAX_CHILDREN as u128 + 1 {
            f.preview.announce(1, child, "child", true);
        }
        assert_eq!(f.preview.children.lock().unwrap().len(), MAX_CHILDREN);
        f.preview.announce(1, MAX_CHILDREN as u128 + 2, "excess", true);
        tokio::select! {
            biased;
            _ = f.preview.peer.closed.notified() => {},
            _ = std::future::ready(()) => panic!("child capacity was not enforced")
        }
        for _ in 0..MAX_CHILDREN * 2 {
            f.frames.recv().await.unwrap();
        }
        f.preview.observe(Event::ChildEnded { child: 2, result: Some(Ok("future_stop_reason")) });
        let frame: serde_json::Value = serde_json::from_str(&f.frames.recv().await.unwrap().text).unwrap();
        assert_eq!(frame["params"]["update"]["state"]["stopReason"], "future_stop_reason");
    }
}
