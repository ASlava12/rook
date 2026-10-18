//! Keep current owners reachable while closing admission for an update.
use crate::AppState;
use axum::{
    Json,
    extract::{Request, State},
    http::{Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use rook_core::drain::{Action, Counts, RestartReceipt, View};
use std::sync::Arc;

pub(crate) async fn fence(State(state): State<Arc<AppState>>, request: Request, next: Next) -> Response {
    let path = request.uri().path();
    let control = path == "/api/daemon/drain"
        || path == "/api/shutdown"
        || (path.starts_with("/api/sessions/") && path.contains("/private-input/"))
        || (path.starts_with("/api/jobs/") && path.ends_with("/stop"))
        || (path.starts_with("/api/work/") && path.ends_with("/control"));
    if matches!(*request.method(), Method::GET | Method::HEAD | Method::OPTIONS)
        || path == "/api/daemon/drain"
        || path == "/api/shutdown"
    {
        return next.run(request).await;
    }
    let permit = if control { state.drain.interaction() } else { state.drain.request() };
    let _permit = match permit {
        Ok(permit) => permit,
        Err(error) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(rook_proto::ApiError::new("draining", error.to_string())),
            )
                .into_response();
        }
    };
    next.run(request).await
}

pub(crate) async fn view(state: &Arc<AppState>) -> View {
    let mut counts = Counts { turns: state.turns_running(), ..Default::default() };
    let mut owners = std::collections::BTreeSet::new();
    match state.work.0.try_lock() {
        Ok(tasks) => {
            owners.extend(tasks.iter().filter(|(_, task)| !task.is_finished()).map(|(id, _)| id.clone()))
        }
        Err(_) => counts.unknown += 1,
    }
    match state.rook.try_read() {
        Ok(rook) => {
            match rook_core::work::managed::list(&rook) {
                Ok(runs) => owners.extend(
                    runs.iter()
                        .filter(|run| run.status.runnable() && state.drain.allows_run(&run.id))
                        .map(|run| run.id.clone()),
                ),
                Err(_) => counts.unknown += 1,
            }
            counts.writers += rook.being_written().len();
        }
        Err(_) => counts.unknown += 1,
    }
    counts.managed = owners.len();
    for project in state.elsewhere.read().await.values() {
        match project.result.borrow().as_ref() {
            Some(Ok(engine)) => match engine.try_read() {
                Ok(rook) => counts.writers += rook.being_written().len(),
                Err(_) => counts.unknown += 1,
            },
            None => counts.unknown += 1,
            Some(Err(_)) => {}
        }
    }
    for equipment in state.equipment.read().await.values() {
        if let Some(shared) = equipment.get() {
            counts.jobs += shared.jobs.running_count();
        }
    }
    let mut view = state.drain.observe(counts);
    match RestartReceipt::read() {
        Ok(handoff) => view.handoff = handoff,
        Err(error) => view.reason = Some(format!("restart receipt unavailable: {error}")),
    }
    view
}

pub(crate) async fn status(State(state): State<Arc<AppState>>) -> Json<View> {
    Json(view(&state).await)
}

pub(crate) async fn change(
    State(state): State<Arc<AppState>>,
    Json(action): Json<Action>,
) -> Result<Json<View>, (StatusCode, Json<rook_proto::ApiError>)> {
    // Same lock as socket admission and worker publication: no accepted root
    // can fall between collecting grandfathered owners and closing the fence.
    if matches!(action, Action::Cancel { .. }) {
        state.drain.change(action, Default::default()).map_err(|error| {
            (StatusCode::CONFLICT, Json(rook_proto::ApiError::new("draining", error.to_string())))
        })?;
        return Ok(Json(view(&state).await));
    }
    let tasks =
        tokio::time::timeout(std::time::Duration::from_secs(5), state.work.0.lock()).await.map_err(|_| {
            (
                StatusCode::CONFLICT,
                Json(rook_proto::ApiError::new(
                    "busy",
                    "work admission is busy; no drain was started. Retry the same ID.",
                )),
            )
        })?;
    let mut allowed = std::collections::BTreeSet::new();
    if matches!(action, Action::Start { .. }) {
        allowed.extend(tasks.iter().filter(|(_, task)| !task.is_finished()).map(|(id, _)| id.clone()));
        let live = state.live.read().await;
        let rook = state.rook.read().await;
        let runs = rook_core::work::managed::list(&rook).map_err(|error| {
            (StatusCode::CONFLICT, Json(rook_proto::ApiError::new("draining", error.to_string())))
        })?;
        {
            for run in runs {
                if run.status.runnable()
                    && run
                        .conversation
                        .as_ref()
                        .and_then(|c| rook_store::parse_session_id(&c.session))
                        .is_some_and(|session| live.get(&session).is_some_and(|owner| owner.running()))
                {
                    allowed.insert(run.id);
                }
            }
        }
    }
    state.drain.change(action, allowed).map_err(|error| {
        (StatusCode::CONFLICT, Json(rook_proto::ApiError::new("draining", error.to_string())))
    })?;
    drop(tasks);
    Ok(Json(view(&state).await))
}

pub(crate) async fn supervise(state: Arc<AppState>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(250));
    loop {
        tick.tick().await;
        if !state.drain.active() {
            continue;
        }
        let view = view(&state).await;
        let Some(id) = view.id.as_deref() else { continue };
        if view.phase != "ready" || !view.restart || !state.drain.handoff(id) {
            continue;
        }
        let receipt = RestartReceipt { version: 1, id: id.into(), phase: "restarting".into(), reason: None };
        if let Err(error) = receipt.save() {
            state.drain.failed(error.to_string());
            continue;
        }
        // The installed CLI already owns stop/address release/start recovery.
        // It survives the old daemon and publishes a bounded result receipt.
        let mut command = std::process::Command::new(&state.restart_cli);
        rook_contain::quietly(&mut command);
        let child = tokio::process::Command::from(command)
            .args(["daemon", "restart", "--drain-receipt", id])
            .current_dir(&state.about.workspace)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(false)
            .spawn();
        match child {
            Ok(mut child) => {
                let state = state.clone();
                let id = id.to_owned();
                tokio::spawn(async move {
                    let error = match child.wait().await {
                        Ok(status) if status.success() => return,
                        Ok(status) => format!(
                            "restart controller exited {status}; old daemon retained if shutdown had not begun; inspect the handoff receipt"
                        ),
                        Err(error) => error.to_string(),
                    };
                    state.drain.failed(error.clone());
                    if !RestartReceipt::read()
                        .ok()
                        .flatten()
                        .is_some_and(|saved| saved.id == id && saved.phase == "failed")
                    {
                        let _ = RestartReceipt {
                            version: 1,
                            id,
                            phase: "failed".into(),
                            reason: Some(error.chars().take(512).collect()),
                        }
                        .save();
                    }
                });
            }
            Err(error) => {
                state.drain.failed(error.to_string());
                let _ = RestartReceipt {
                    phase: "failed".into(),
                    reason: Some(error.to_string().chars().take(512).collect()),
                    ..receipt
                }
                .save();
            }
        }
    }
}
