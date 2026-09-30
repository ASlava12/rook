//! The daemon owns durable work; HTTP clients submit and observe it.
use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
};
use rook_core::work::managed;
use rook_proto::work::{Action, EditInstruction, Run, Start, Status, Steer, Steering, WithdrawInstruction};
use tokio::sync::Mutex;

use crate::AppState;

#[derive(Default)]
pub struct Tasks(pub(crate) Mutex<HashMap<String, tokio::task::JoinHandle<()>>>);

/// Starting and recovery use the same lock: a scheduler tick must not launch a
/// second writer while the socket that submitted the goal is attaching to it.
pub async fn join_conversation(state: &Arc<AppState>, run: &Run) -> Result<Arc<crate::chat::Live>, String> {
    let _tasks = state.work.0.lock().await;
    let session = rook_store::parse_session_id(&run.id).ok_or("invalid conversation")?;
    if let Some(live) = state.live.read().await.get(&session).filter(|l| l.running()).cloned() {
        return Ok(live);
    }
    let current = managed::read(&*state.rook.read().await, &run.id).map_err(|e| e.to_string())?.run;
    if !current.status.runnable() {
        return Err(current.reason);
    }
    let live = crate::chat::resume_goal(state, &current).await?;
    state.remember(session, live.clone()).await;
    Ok(live)
}

type Failure = (StatusCode, Json<rook_proto::ApiError>);
fn failure(error: impl std::fmt::Display) -> Failure {
    (StatusCode::BAD_REQUEST, Json(rook_proto::ApiError::new("work", error.to_string())))
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/sessions/{id}/queue", get(queue_page).post(queue_change))
        .route("/api/sessions/{id}/queue/{reference}", get(queue_read))
        .route("/api/sessions/{id}/instructions", get(session_instructions).post(session_submit))
        .route(
            "/api/sessions/{id}/instructions/{message}",
            axum::routing::put(session_edit).delete(session_withdraw),
        )
        .route("/api/tasks", get(schedule_list).post(schedule_create))
        .route("/api/tasks/{id}", axum::routing::delete(schedule_delete))
        .route("/api/tasks/{id}/control", post(schedule_control))
        .route("/api/work", get(list).post(start))
        .route("/api/work/{id}", get(show).delete(forget))
        .route("/api/work/{id}/steer", post(steer))
        .route(
            "/api/work/{id}/instructions/{message}",
            axum::routing::put(edit_instruction).delete(withdraw_instruction),
        )
        .route("/api/work/{id}/control", post(control))
}

fn session_id(id: &str) -> Result<u128, Failure> {
    rook_store::parse_session_id(id).ok_or_else(|| failure("invalid session id"))
}

async fn publish_receipt(
    state: &Arc<AppState>,
    session: u128,
    receipt: &Steering,
    notice: rook_proto::queue::Notice,
) {
    if let Some(live) = state.live.read().await.get(&session).filter(|live| live.running()).cloned() {
        live.queue_notice(notice, receipt.text.clone());
    }
}

async fn queue_page(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<rook_proto::queue::Query>,
) -> Result<Json<rook_proto::queue::Page>, Failure> {
    let rook = state.rook.read().await;
    let session = session_id(&id)?;
    let mut page = rook_core::message_queue::view::page(&rook, session, &query).map_err(failure)?;
    if page.follow_up_status.is_none() {
        page.follow_up_status = crate::chat::followups::status(&rook, session);
    }
    Ok(Json(page))
}
async fn queue_read(
    State(state): State<Arc<AppState>>,
    Path((id, reference)): Path<(String, String)>,
) -> Result<Json<rook_proto::queue::Entry>, Failure> {
    Ok(Json(
        rook_core::message_queue::view::read(&*state.rook.read().await, session_id(&id)?, &reference)
            .map_err(failure)?,
    ))
}
async fn queue_change(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(change): Json<rook_proto::queue::Change>,
) -> Result<Json<rook_proto::queue::Entry>, Failure> {
    let session = session_id(&id)?;
    let entry = rook_core::message_queue::view::change(&*state.rook.read().await, session, change)
        .map_err(failure)?;
    publish_receipt(
        &state,
        session,
        &entry.receipt,
        rook_proto::queue::Notice::new(
            rook_store::format_session_id(session),
            entry.reference.clone(),
            &entry.receipt,
        ),
    )
    .await;
    Ok(Json(entry))
}

async fn session_instructions(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Vec<Steering>>, Failure> {
    Ok(Json(rook_core::message_queue::list(&*state.rook.read().await, session_id(&id)?).map_err(failure)?))
}

async fn session_submit(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<Steer>,
) -> Result<Json<Steering>, Failure> {
    let session = session_id(&id)?;
    let (receipt, notice) =
        rook_core::message_queue::submit_noticed(&*state.rook.read().await, session, request)
            .map_err(failure)?;
    publish_receipt(&state, session, &receipt, notice).await;
    Ok(Json(receipt))
}

async fn session_edit(
    State(state): State<Arc<AppState>>,
    Path((id, message)): Path<(String, String)>,
    Json(request): Json<EditInstruction>,
) -> Result<Json<Steering>, Failure> {
    let session = session_id(&id)?;
    let (receipt, notice) =
        rook_core::message_queue::edit_noticed(&*state.rook.read().await, session, &message, request)
            .map_err(failure)?;
    publish_receipt(&state, session, &receipt, notice).await;
    Ok(Json(receipt))
}

async fn session_withdraw(
    State(state): State<Arc<AppState>>,
    Path((id, message)): Path<(String, String)>,
    Json(request): Json<WithdrawInstruction>,
) -> Result<Json<Steering>, Failure> {
    let session = session_id(&id)?;
    let (receipt, notice) =
        rook_core::message_queue::withdraw_noticed(&*state.rook.read().await, session, &message, request)
            .map_err(failure)?;
    publish_receipt(&state, session, &receipt, notice).await;
    Ok(Json(receipt))
}

async fn list(State(state): State<Arc<AppState>>) -> Result<Json<Vec<Run>>, Failure> {
    let mut runs = managed::list(&*state.rook.read().await).map_err(failure)?;
    // Full instructions and history are fetched for one run, never all of them.
    for run in &mut runs {
        run.instructions.clear();
        run.recent.clear();
        run.reply.clear();
        run.verification.clear();
        if let Some(c) = &mut run.conversation {
            c.options = Default::default();
        }
    }
    Ok(Json(runs))
}

async fn show(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Result<Json<Run>, Failure> {
    Ok(Json(managed::read(&*state.rook.read().await, &id).map_err(failure)?.run))
}

async fn start(State(state): State<Arc<AppState>>, Json(request): Json<Start>) -> Result<Json<Run>, Failure> {
    let engine =
        state.engine_for(request.workspace.as_deref().map(std::path::Path::new)).await.map_err(failure)?;
    let run = managed::start(&*engine.read().await, request).map_err(failure)?;
    Ok(Json(run))
}

async fn steer(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<Steer>,
) -> Result<Json<Steering>, Failure> {
    let session = session_id(&id)?;
    let (receipt, notice) =
        managed::steer_noticed(&*state.rook.read().await, &id, request).map_err(failure)?;
    publish_receipt(&state, session, &receipt, notice).await;
    Ok(Json(receipt))
}

async fn control(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(action): Json<Action>,
) -> Result<Json<Run>, Failure> {
    Ok(Json(managed::control(&*state.rook.read().await, &id, action).map_err(failure)?))
}

async fn edit_instruction(
    State(state): State<Arc<AppState>>,
    Path((id, message)): Path<(String, String)>,
    Json(request): Json<EditInstruction>,
) -> Result<Json<Steering>, Failure> {
    let session = session_id(&id)?;
    let (receipt, notice) =
        managed::edit_instruction_noticed(&*state.rook.read().await, &id, &message, request)
            .map_err(failure)?;
    publish_receipt(&state, session, &receipt, notice).await;
    Ok(Json(receipt))
}

async fn withdraw_instruction(
    State(state): State<Arc<AppState>>,
    Path((id, message)): Path<(String, String)>,
    Json(request): Json<WithdrawInstruction>,
) -> Result<Json<Steering>, Failure> {
    let session = session_id(&id)?;
    let (receipt, notice) =
        managed::withdraw_instruction_noticed(&*state.rook.read().await, &id, &message, request)
            .map_err(failure)?;
    publish_receipt(&state, session, &receipt, notice).await;
    Ok(Json(receipt))
}

async fn forget(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, Failure> {
    if state.work.0.lock().await.get(&id).is_some_and(|task| !task.is_finished()) {
        return Err(failure("this run is still finishing its active operation"));
    }
    managed::forget(&*state.rook.read().await, &id).map_err(failure)?;
    Ok(Json(serde_json::json!({"forgotten": id})))
}

/// No socket owns a worker. Restart reads the registry and resumes runnable
/// states; unknown operation receipts block mutation in the core before replay.
pub async fn supervise(state: Arc<AppState>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
    let mut followup_cursor = None;
    loop {
        tick.tick().await;
        state.config_if_changed().await;
        schedule_tick(&state).await;
        // Session goals use the existing chat registry, streaming, approvals
        // and switching. A disconnected window is only a missing observer.
        let conversations = managed::list(&*state.rook.read().await);
        if let Ok(runs) = conversations {
            for run in runs.into_iter().filter(|r| r.conversation.is_some() && r.status.runnable()) {
                if let Err(error) = join_conversation(&state, &run).await {
                    block(&state, &run.id, &error).await;
                }
            }
        }
        let mut tasks = state.work.0.lock().await;
        crate::chat::followups::supervise(
            &state,
            tasks.values().filter(|h| !h.is_finished()).count(),
            &mut followup_cursor,
        )
        .await;
        let finished: Vec<_> =
            tasks.iter().filter(|(_, h)| h.is_finished()).map(|(id, _)| id.clone()).collect();
        for id in finished {
            if let Some(handle) = tasks.remove(&id)
                && let Err(error) = handle.await
            {
                block(&state, &id, &format!("worker stopped unexpectedly: {error}")).await;
            }
        }
        let (runs, cap) = {
            let rook = state.rook.read().await;
            (managed::list(&rook), rook.config.work.max_parallel_runs)
        };
        let mut runs = match runs {
            Ok(runs) => runs,
            Err(error) => {
                tracing::error!("cannot read durable work: {error}");
                continue;
            }
        };
        runs.sort_by_key(|run| run.updated_at);
        for run in runs {
            let live = if let Some(c) = &run.conversation {
                let session = rook_store::parse_session_id(&c.session);
                state.live.read().await.iter().any(|(id, live)| Some(*id) == session && live.running())
            } else {
                false
            };
            if run.status == Status::Cancelled && !tasks.contains_key(&run.id) && !live {
                let rook = state.rook.read().await;
                if let Ok(saved) = managed::read(&rook, &run.id)
                    && saved.active.is_some()
                {
                    let _ = managed::update(&rook, &run.id, |s| {
                        s.active = None;
                        Ok(())
                    });
                }
            }
            if run.conversation.is_some() {
                continue;
            }
            if tasks.len() >= cap {
                continue;
            }
            if tasks.contains_key(&run.id)
                || !run.status.runnable()
                || run.next_attempt_at.is_some_and(|at| at > managed::now())
            {
                continue;
            }
            let shared = state.clone();
            let id = run.id.clone();
            tasks.insert(
                id,
                tokio::spawn(async move {
                    if let Err(error) = iteration(&shared, &run).await {
                        block(&shared, &run.id, &error).await;
                    }
                }),
            );
        }
    }
}

async fn block(state: &AppState, id: &str, why: &str) {
    let rook = state.rook.read().await;
    if let Err(error) = managed::update(&rook, id, |saved| {
        if saved.run.status.runnable() {
            saved.run.status = Status::Blocked;
            saved.run.reason = why.chars().take(2048).collect();
        }
        Ok(())
    }) {
        tracing::error!("cannot persist work failure: {error}");
    }
}

async fn iteration(state: &Arc<AppState>, run: &Run) -> Result<(), String> {
    let engine = state.engine_for(Some(std::path::Path::new(&run.workspace))).await?;
    let equipment = state.equipment_for(&engine).await;
    let rook = engine.read().await;
    let shared = equipment.get_or_init(|| crate::chat::Shared::for_project(&rook)).await;
    let _counted = state.turn_started();
    managed::advance(
        &rook,
        &run.id,
        |session| {
            let provider = rook_core::models::configured(&rook.config)?;
            let mut agent = rook_core::agent::AgentLoop::new(&rook, provider.into(), session);
            rook_core::agent::equip(&mut agent, shared.servers.clone(), &shared.mcp, shared.jobs.clone());
            Ok(agent)
        },
        |_| {},
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Drop worker futures before releasing the store and its published address.
/// Their operation journals retain any interrupted effects for recovery.
pub async fn stop(state: &AppState) {
    let tasks: Vec<_> = state.work.0.lock().await.drain().map(|(_, task)| task).collect();
    for task in &tasks {
        task.abort();
    }
    for task in tasks {
        let _ = task.await;
    }
    let conversations: Vec<_> = state.live.write().await.drain().map(|(_, live)| live).collect();
    for live in conversations {
        live.shutdown().await;
    }
}

async fn schedule_list(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<rook_proto::schedule::Task>>, Failure> {
    let mut tasks = rook_core::schedules::list(&*state.rook.read().await).map_err(failure)?;
    let live = state.live.read().await;
    for task in &mut tasks {
        if let Some(run) = task.history.last_mut()
            && let Some(session) = rook_store::parse_session_id(&run.session)
            && live.get(&session).is_some_and(|l| l.needs_input())
        {
            run.status = "Needs input".into();
            run.reason = "Open this session to answer the pending question or approval".into();
        }
    }
    Ok(Json(tasks))
}
async fn schedule_create(
    State(state): State<Arc<AppState>>,
    Json(request): Json<rook_proto::schedule::Create>,
) -> Result<Json<rook_proto::schedule::Task>, Failure> {
    Ok(Json(
        rook_core::schedules::create(&*state.rook.read().await, request, managed::now()).map_err(failure)?,
    ))
}
async fn schedule_control(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(action): Json<rook_proto::schedule::Action>,
) -> Result<Json<rook_proto::schedule::Task>, Failure> {
    Ok(Json(
        rook_core::schedules::control(&*state.rook.read().await, &id, action, managed::now())
            .map_err(failure)?,
    ))
}
async fn schedule_delete(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, Failure> {
    rook_core::schedules::delete(&*state.rook.read().await, &id).map_err(failure)?;
    Ok(Json(serde_json::json!({"deleted": id})))
}
async fn schedule_tick(state: &Arc<AppState>) {
    let pending = rook_core::schedules::due(&*state.rook.read().await, managed::now());
    let pending = match pending {
        Ok(tasks) => tasks,
        Err(error) => {
            tracing::error!("cannot read schedules: {error}");
            return;
        }
    };
    for task in pending {
        let capacity = {
            let rook = state.rook.read().await;
            managed::list(&rook)
                .map(|runs| {
                    task.pending.as_ref().is_some_and(|id| runs.iter().any(|r| &r.id == id))
                        || runs.iter().filter(|r| r.status.runnable()).count()
                            < rook.config.work.max_parallel_runs
                })
                .unwrap_or(false)
        };
        if !capacity {
            break;
        }
        match state.engine_for(Some(std::path::Path::new(&task.spec.workspace))).await {
            Ok(engine) => {
                if let Err(error) = rook_core::schedules::launch(&*engine.read().await, &task.id) {
                    tracing::error!("cannot launch schedule {}: {error}", task.id);
                }
            }
            Err(error) => {
                if let Err(save_error) =
                    rook_core::schedules::fail_pending(&*state.rook.read().await, &task.id, &error)
                {
                    tracing::error!("cannot record schedule failure: {save_error}");
                }
            }
        }
    }
}
