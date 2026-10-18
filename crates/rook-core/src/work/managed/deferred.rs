//! A wait is a committed stage boundary, never a sleeping executor.
use super::*;
use rook_proto::work::Waiting;
use serde_json::Value;

pub(crate) const TOOL: &str = "defer_work";
pub(crate) const MAX_SECONDS: u64 = 86_400;
pub(crate) const MAX_REASON: usize = 512;

pub(crate) struct Request {
    pub seconds: u64,
    pub reason: String,
}

pub(crate) struct Call<'a> {
    pub session: u128,
    pub turn: &'a str,
    pub id: &'a str,
    pub control_revision: u64,
}

pub(crate) fn parse(args: &Value) -> Result<Request> {
    let fields = args.as_object().ok_or_else(|| bad("defer_work expects an object"))?;
    if fields.keys().any(|key| !matches!(key.as_str(), "seconds" | "reason")) {
        return Err(bad("defer_work accepts only seconds and an optional reason"));
    }
    let seconds = fields
        .get("seconds")
        .and_then(Value::as_u64)
        .filter(|seconds| (1..=MAX_SECONDS).contains(seconds))
        .ok_or_else(|| bad("seconds must be an integer from 1 to 86400"))?;
    let reason = match fields.get("reason") {
        None => String::new(),
        Some(Value::String(reason))
            if reason.len() <= MAX_REASON && !reason.chars().any(char::is_control) =>
        {
            reason.clone()
        }
        _ => return Err(bad("reason must be text of at most 512 bytes without control characters")),
    };
    Ok(Request { seconds, reason })
}

/// The execution writer atomically closes its pending operation together with
/// this generation-bound checkpoint. No snapshot is held across an await.
pub(crate) fn commit(
    rook: &Rook,
    identity: &RunIdentity,
    call: Call<'_>,
    request: Request,
    outcome: &crate::agent::TurnOutcome,
    write: impl FnOnce(&str, &[u8], &str) -> Result<u64>,
) -> Result<(u64, String)> {
    if !(1..=MAX_SECONDS).contains(&request.seconds)
        || request.reason.len() > MAX_REASON
        || request.reason.chars().any(char::is_control)
    {
        return Err(bad("wait must be 1..86400 seconds with at most 512 bytes of reason text"));
    }
    if call.id.is_empty() || call.id.len() > 128 || call.id.chars().any(char::is_control) {
        return Err(bad("defer_work requires a bounded tool call identity"));
    }
    let _writing = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let mut saved = read_identity(rook, identity)?.ok_or_else(|| bad("the managed goal changed"))?;
    if saved.control_revision != call.control_revision {
        return Err(bad(
            "user controls changed after this stage was admitted; the old wait was not committed",
        ));
    }
    if !saved.run.status.runnable() || !owns_step(rook, &identity.id) {
        return Err(bad("defer_work requires an admitted runnable managed stage"));
    }
    let session = rook_store::format_session_id(call.session);
    let active = saved.active.as_ref().ok_or_else(|| bad("the managed stage has no checkpoint"))?;
    if active.session != session {
        return Err(bad("defer_work cannot schedule another session"));
    }
    if saved.run.instructions.iter().any(|message| message.queued() && message.follow_up.is_none())
        || !crate::message_queue::pending(rook, call.session)?.is_empty()
    {
        return Err(bad("new guidance is queued; read it before deferring the goal"));
    }
    let at = now();
    let waiting = if let Some(waiting) = &saved.run.waiting {
        if waiting.turn != call.turn || waiting.call != call.id || waiting.session != session {
            return Err(bad("this stage already committed a different wait"));
        }
        if waiting.seconds != request.seconds || waiting.reason != request.reason {
            return Err(bad("the same wait identity cannot change its arguments"));
        }
        waiting.clone()
    } else {
        let spent = crate::work::spent(rook, call.session)?;
        saved.run.tokens = saved.run.tokens.saturating_add(spent.saturating_sub(active.accounted_tokens));
        saved.active.as_mut().ok_or_else(|| bad("the managed checkpoint disappeared"))?.accounted_tokens =
            spent;
        if let Some(reason) = limit(&saved.run, at) {
            return Err(bad(reason));
        }
        if saved.run.max_iterations > 0 && saved.run.iterations.saturating_add(1) >= saved.run.max_iterations
        {
            return Err(bad("no iteration allowance remains for a deferred continuation"));
        }
        let until =
            at.checked_add(request.seconds).ok_or_else(|| bad("wait deadline exceeds the clock range"))?;
        if saved.run.max_seconds > 0 && until >= saved.run.created_at.saturating_add(saved.run.max_seconds) {
            return Err(bad("wait exceeds the remaining goal lifetime; it cannot extend the saved budget"));
        }
        saved.run.iterations = saved.run.iterations.saturating_add(1);
        saved.run.recent.push(rook_proto::work::Iteration {
            number: saved.run.iterations,
            session: session.clone(),
            stopped: "work_deferred".into(),
            changed: outcome.files_changed.iter().take(64).cloned().collect(),
            summary: clipped(
                &format!("Checkpoint deferred until unix {until}: {}; verification pending", request.reason),
                2048,
            ),
        });
        let excess = saved.run.recent.len().saturating_sub(rook.config.work.retained_iterations);
        saved.run.recent.drain(..excess);
        Waiting {
            until,
            reason: request.reason,
            seconds: request.seconds,
            session,
            turn: call.turn.into(),
            call: call.id.into(),
        }
    };
    let text = format!(
        "Work checkpoint saved; continue in this same transcript after unix {}. {}",
        waiting.until, waiting.reason
    );
    saved.run.status = Status::Queued;
    saved.run.reason = format!("waiting until unix {}: {}", waiting.until, waiting.reason);
    saved.run.next_attempt_at = Some(waiting.until);
    saved.run.waiting = Some(waiting);
    saved.run.updated_at = at;
    saved.run.restart_guard.unfinished_stage = false;
    saved.run.restart_guard.consecutive_interruptions = 0;
    saved.idle = 0;
    let encoded = crate::persistence::encode(&saved)?;
    let seq = write(&key(&identity.id)?, &encoded, &text)?;
    Ok((seq, text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_wait_retries_keep_the_admitted_deadline_and_conflicting_sources_cannot_replace_it() {
        let workspace = tempfile::tempdir().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let rook = Rook::from_parts(
            rook_store::Store::open(directory.path()).unwrap(),
            crate::Config::default(),
            rook_skills::Environment::bare("linux", "x86_64", "0.19.0"),
            rook_skills::SkillIndex::default(),
            workspace.path().into(),
        );
        let run = start(
            &rook,
            Start {
                goal: "wait retry fixture".into(),
                workspace: None,
                conversation: None,
                autonomous: true,
                max_iterations: Some(10),
                max_tokens: Some(0),
                max_seconds: Some(300),
            },
        )
        .unwrap();
        let session = rook.start_session("wait receipt").unwrap();
        update(&rook, &run.id, |saved| {
            saved.run.session = Some(rook_store::format_session_id(session));
            saved.active = Some(Active {
                session: rook_store::format_session_id(session),
                start_seq: 0,
                before: crate::evaluation::witness(&rook.workspace, &Default::default()),
                answer: None,
                accounted_tokens: 0,
            });
            saved.run.restart_guard.unfinished_stage = true;
            Ok(())
        })
        .unwrap();
        let _step = {
            let _lock = WRITING.lock().unwrap();
            Step::claim(&rook, &run.id).unwrap()
        };
        let journal = crate::execution::Journal::start(&rook, session, None, false).unwrap();
        let outcome = crate::agent::TurnOutcome {
            steps: 1,
            stopped: "work_deferred".into(),
            reply: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
            tools_called: vec![TOOL.into()],
            skills_loaded: vec![],
            skills_written: vec![],
            facts_learned: vec![],
            facts_forgotten: vec![],
            files_changed: vec![],
            delegated: vec![],
            compactions: 0,
            decisions: vec![],
            open_questions: vec![],
        };
        let call = |id| Call { session, turn: journal.turn(), id, control_revision: 0 };
        let request = || Request { seconds: 60, reason: "original request".into() };
        journal.begin(TOOL, "{\"seconds\":60}", false, false, None).unwrap();
        let original = rook.store.kv_get(&key(&run.id).unwrap()).unwrap().unwrap();
        let failed = commit(&rook, &run.identity(), call("same-call"), request(), &outcome, |_, _, _| {
            Err(bad("injected checkpoint writer failure"))
        });
        assert!(failed.is_err());
        assert_eq!(rook.store.kv_get(&key(&run.id).unwrap()).unwrap().unwrap(), original);
        assert!(
            !rook
                .store
                .events(session, 0, 128)
                .unwrap()
                .iter()
                .any(|event| event.record.kind == rook_store::EventKind::ToolResult)
        );
        let (seq, _) =
            commit(&rook, &run.identity(), call("same-call"), request(), &outcome, |key, bytes, text| {
                journal.complete_checkpoint(text, key, bytes)
            })
            .unwrap();
        let saved = read(&rook, &run.id).unwrap();
        let first_at = saved.run.updated_at;
        let until = saved.run.waiting.as_ref().unwrap().until;
        assert_eq!(saved.run.iterations, 1);
        let execution = crate::execution::current(&rook, session).unwrap().unwrap();
        assert!(execution.pending.is_none());
        assert_eq!(execution.last_result_seq, Some(seq));
        let clock_limit = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while now() == first_at && std::time::Instant::now() < clock_limit {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(now() > first_at, "retry fixture must cross a persisted clock second");
        assert!(
            now().saturating_add(60) > until,
            "recomputing a relative deadline must differ from the admitted one"
        );
        journal.begin(TOOL, "{\"seconds\":60}", false, false, None).unwrap();
        commit(&rook, &run.identity(), call("same-call"), request(), &outcome, |key, bytes, text| {
            journal.complete_checkpoint(text, key, bytes)
        })
        .unwrap();
        let retried = read(&rook, &run.id).unwrap();
        assert_eq!(retried.run.waiting.unwrap().until, until, "a later retry cannot move the deadline");
        assert_eq!(retried.run.iterations, 1);
        assert_eq!(retried.run.recent.len(), 1);
        for (id, seconds) in [("other-call", 60), ("same-call", 61)] {
            let result = commit(
                &rook,
                &run.identity(),
                call(id),
                Request { seconds, reason: "original request".into() },
                &outcome,
                |_, _, _| panic!("a conflicting source cannot reach the store writer"),
            );
            assert!(result.is_err());
        }
        control(&rook, &run.id, Action::Resume).unwrap();
        assert!(
            commit(&rook, &run.identity(), call("same-call"), request(), &outcome, |_, _, _| panic!(
                "an old controlled stage cannot recreate its wait"
            ))
            .is_err()
        );
        assert!(read(&rook, &run.id).unwrap().run.waiting.is_none());
        journal.finish("stop", None).unwrap();
    }
}
