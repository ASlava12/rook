//! Bot input uses the same admission, steering queue and live request registry as sockets.
use super::*;
use rook_core::telegram::{Input, Route};
use rook_proto::work::Steer;

pub(crate) async fn submit(state: &Arc<AppState>, input: &Input) -> Result<String, String> {
    let _admission = state.work.0.lock().await;
    let workspace = state.rook.read().await.config.telegram.workspace.clone();
    let engine = state.engine_for(Some(std::path::Path::new(&workspace))).await?;
    let session = rook_store::parse_session_id(&input.session).ok_or("Invalid Telegram session")?;
    let live = state.live.read().await.get(&session).filter(|l| l.running()).cloned();
    let rook = engine.read().await;
    // Routing records the canonical directory, while the daemon may have been
    // started through an alias (for example /var on macOS).
    let workspace =
        rook.workspace.canonicalize().map_err(|e| format!("Telegram workspace unavailable: {e}"))?;
    if rook
        .store
        .get_session(session)
        .map_err(|e| e.to_string())?
        .is_none_or(|m| std::path::Path::new(&m.workspace) != workspace)
    {
        return Err("Telegram session workspace changed; inspect routing".into());
    }
    let cfg = &rook.config.telegram;
    if !cfg.enabled
        || !cfg.errors().is_empty()
        || !cfg.allowed_users.contains(&input.user)
        || !cfg.allowed_chats.contains(&input.chat)
    {
        return Err("Telegram sender/chat authorization was revoked before dispatch".into());
    }
    let id = format!("tg-{}", input.update);
    let options = rook_proto::TurnOptions::default();
    let prior = rook_core::chat_submission::read(&rook, session, &id, &input.text, &options)
        .map_err(|e| e.to_string())?;
    let queued = rook_core::message_queue::list(&rook, session)
        .map_err(|e| e.to_string())?
        .into_iter()
        .any(|m| m.id == id);
    let mode = input.mode.as_deref().unwrap_or_else(|| {
        if input.callback.is_some() {
            "callback"
        } else if input.text.trim() == "/stop" {
            "stop"
        } else if input.text.trim() == "/status" {
            "status"
        } else if prior.is_some() {
            "prompt"
        } else if queued || live.is_some() {
            "steer"
        } else {
            "prompt"
        }
    });
    let _permit = match mode {
        "prompt" | "steer" => Some(state.drain.request().map_err(|error| error.to_string())?),
        "callback" | "stop" => Some(state.drain.interaction().map_err(|error| error.to_string())?),
        _ => None,
    };
    let target = input.target.clone().or_else(|| live.as_ref().map(|l| l.epoch.clone()));
    let captured =
        rook.telegram_dispatch(input.update, mode, target.as_deref()).map_err(|e| e.to_string())?;
    match mode {
        "status" => {
            Ok(format!("Session {} · {}", input.session, if live.is_some() { "running" } else { "idle" }))
        }
        "callback" => {
            let live = live.ok_or("This button has no live request; open the current session")?;
            let callback = rook.telegram_take_callback(&captured, &live.epoch).map_err(|e| e.to_string())?;
            if matches!(callback.action.as_str(), "once" | "deny") {
                if !live.approver.current().iter().any(|r| r.id == callback.request) {
                    return Err("This approval is no longer pending".into());
                }
                live.approver.answer(
                    &callback.request,
                    if callback.action == "once" {
                        rook_tools::policy::Approval::Once
                    } else {
                        rook_tools::policy::Approval::declined()
                    },
                );
            } else {
                let answers: Vec<Vec<String>> =
                    serde_json::from_str(&callback.action).map_err(|_| "Invalid question answer")?;
                if !live.asker.current().iter().any(|r| r.id == callback.request) {
                    return Err("This question is no longer pending".into());
                }
                live.asker.answer(&callback.request, answers);
            }
            Ok("Decision submitted to the live request".into())
        }
        "stop" => {
            let live = live.ok_or("No active turn; no later turn was stopped")?;
            if captured.target.as_deref() != Some(live.epoch.as_str()) {
                return Err("This stop belongs to an earlier turn; inspect /status".into());
            }
            if let Some(run) = session_goal(&rook, session)?.filter(|r| !r.status.terminal()) {
                managed::control_identified(
                    &rook,
                    &run.id,
                    rook_proto::work::IdentifiedControl {
                        id,
                        generation: run.generation,
                        action: rook_proto::work::Action::Pause,
                    },
                )
                .map_err(|e| e.to_string())?;
                return Ok("Goal pause requested at the next safe boundary".into());
            }
            followups::pause(&rook, session, None)?;
            live.stop();
            Ok("Turn stopped; history and recovery records remain in this session".into())
        }
        "steer" => {
            if queued {
                return Ok("Follow-up already retained; no duplicate input was submitted".into());
            }
            let live =
                live.ok_or("The captured turn ended before this follow-up; inspect the pending input")?;
            if captured.target.as_deref() != Some(live.epoch.as_str()) {
                return Err("Follow-up belongs to an earlier turn; no later turn received it".into());
            }
            let request = Steer { id, text: input.text.clone() };
            if let Some(run) = session_goal(&rook, session)?.filter(|r| !r.status.terminal()) {
                managed::steer_noticed(&rook, &run.id, request).map_err(|e| e.to_string())?;
            } else {
                rook_core::message_queue::submit_noticed(&rook, session, request)
                    .map_err(|e| e.to_string())?;
            }
            Ok("Follow-up retained for the current turn".into())
        }
        "prompt" => {
            if state.drain.active() {
                return Err("Daemon is draining; this prompt was not admitted. Existing question callbacks and /stop remain available.".into());
            }
            let claim = rook_core::chat_submission::claim(&rook, Some(session), &id, &input.text, &options)
                .map_err(|e| e.to_string())?;
            if claim.status == rook_core::chat_submission::Status::Admitted {
                return Ok("Prompt already admitted; results remain in the session".into());
            }
            if claim.turn.is_some() {
                return Err(
                    "Earlier execution has a receipt; inspect recovery before sending a new prompt".into()
                );
            }
            if live.is_some() {
                return Err("Session has another active turn; this captured prompt remains separate".into());
            }
            if crate::work::occupied(state, _admission.values().filter(|task| !task.is_finished()).count())
                .await
                >= rook.config.work.max_parallel_runs
            {
                return Err(
                    "Daemon turn capacity reached; inspect this input and resend after a turn finishes"
                        .into(),
                );
            }
            let settings = Arc::new(Settings::new(&rook));
            let ceiling = rook_tools::policy::Stance::parse(&rook.config.telegram.stance)
                .ok_or("Invalid Telegram stance")?;
            settings.policy.set_stance(settings.policy.stance().min(ceiling));
            drop(rook);
            let shared = state.equipment_for(&engine).await;
            let live = begin(
                state,
                &engine,
                &shared,
                &settings,
                session,
                StartTurn::Prompt(input.text.clone(), Some(claim.key)),
                options,
            )
            .await;
            state.remember(session, live).await;
            Ok(format!("Accepted in session {}", input.session))
        }
        _ => Err("Unsupported saved Telegram dispatch mode".into()),
    }
}

pub(crate) async fn notices(state: &Arc<AppState>, route: &Route) -> Result<(), String> {
    let session = rook_store::parse_session_id(&route.session).ok_or("Invalid Telegram route")?;
    let Some(live) = state.live.read().await.get(&session).filter(|l| l.running()).cloned() else {
        return Ok(());
    };
    let rook = state.rook.read().await;
    for request in live.approver.current() {
        let text = compact_text(
            ["Approval", request.tool.as_str(), request.action.as_str()]
                .into_iter()
                .chain(request.preview.as_deref()),
            rook.config.telegram.message_bytes,
        );
        rook.telegram_notice(
            route,
            &request.id,
            &live.epoch,
            &text,
            &[("Allow once", "once"), ("Deny", "deny")],
        )
        .map_err(|e| e.to_string())?;
    }
    for request in live.asker.current() {
        // Multiple/free-form questions remain answerable in the shared session UI.
        let heading = format!("Question in session {}", route.session);
        let text = compact_text(
            std::iter::once(heading.as_str())
                .chain(request.questions.iter().map(|q| q.question.as_str()))
                .chain(std::iter::once("Open this session in rook for free-form or multiple answers.")),
            rook.config.telegram.message_bytes,
        );
        let actions: Vec<(String, String)> = if request.questions.len() == 1 && !request.questions[0].multi {
            request.questions[0]
                .choices
                .iter()
                .filter(|choice| choice.len() <= 128)
                .take(16)
                .map(|choice| {
                    (
                        choice.chars().take(30).collect(),
                        serde_json::to_string(&vec![vec![choice]]).unwrap_or_default(),
                    )
                })
                .filter(|(_, a)| a.len() <= 256)
                .collect()
        } else {
            Vec::new()
        };
        if actions.is_empty() {
            rook.telegram_reply(&format!("ask-{}-{}", live.epoch, request.id), route, &text)
                .map_err(|e| e.to_string())?;
        } else {
            let borrowed: Vec<_> = actions.iter().map(|(l, a)| (l.as_str(), a.as_str())).collect();
            rook.telegram_notice(route, &request.id, &live.epoch, &text, &borrowed)
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

pub(super) fn configure(agent: &mut AgentLoop<'_>) -> rook_core::Result<()> {
    let rook = agent.rook;
    if rook.store.get_session(agent.session)?.is_none_or(|m| !m.tags.iter().any(|t| t == "rook:telegram")) {
        return Ok(());
    }
    let cfg = &rook.config.telegram;
    if !cfg.enabled || !cfg.errors().is_empty() {
        return Err(rook_core::CoreError::Other(
            "Telegram session disabled or invalid; repair trusted configuration".into(),
        ));
    }
    let ceiling =
        rook_tools::policy::Stance::parse(&cfg.stance).unwrap_or(rook_tools::policy::Stance::ReadOnly);
    agent.policy.set_stance(
        agent.policy.stance().min(ceiling).min(rook_core::agent::policy_for(&rook.config).stance()),
    );
    agent.max_turn_tokens = if agent.max_turn_tokens == 0 {
        cfg.max_turn_tokens
    } else {
        agent.max_turn_tokens.min(cfg.max_turn_tokens)
    };
    agent.max_turn_secs =
        if agent.max_turn_secs == 0 { cfg.max_turn_secs } else { agent.max_turn_secs.min(cfg.max_turn_secs) };
    Ok(())
}

fn compact_text<'a>(pieces: impl Iterator<Item = &'a str>, cap: usize) -> String {
    let limit = cap.saturating_sub(80).min(3800);
    let mut text = String::new();
    let mut omitted = false;
    'pieces: for piece in pieces {
        for c in piece.chars() {
            if text.len() + c.len_utf8() > limit {
                omitted = true;
                break 'pieces;
            }
            text.push(c);
        }
        if text.len() < limit {
            text.push('\n');
        }
    }
    if omitted {
        text.push_str("[Shortened; open the session for the full live request.]");
    }
    text
}
