//! Owned long polling, ordinary chat admission and a durable outbound sender.
use crate::AppState;
use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use rook_core::telegram::{Client, Settings};
use std::sync::Arc;

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/api/telegram", post(inspect).layer(axum::extract::DefaultBodyLimit::max(8192)))
}
async fn inspect(
    State(state): State<Arc<AppState>>,
    Json(request): Json<rook_core::telegram::Request>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<rook_proto::ApiError>)> {
    state
        .rook
        .read()
        .await
        .telegram_request(request)
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, Json(rook_proto::ApiError::new("telegram", e.to_string()))))
}
pub(crate) async fn run(state: Arc<AppState>) {
    loop {
        if let Err(e) = state.rook.read().await.telegram_recover() {
            tracing::warn!("Telegram recovery requires inspection: {e}");
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            continue;
        }
        let settings = state.rook.read().await.config.telegram.clone();
        if !settings.enabled {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            continue;
        }
        let result = async {
            let vault = rook_core::Vault::load().map_err(|_| "Telegram vault is unavailable".to_string())?;
            let client = Client::new(settings.clone(), &vault).map_err(|e| e.to_string())?;
            let bot = client.identity().await.map_err(|e| e.to_string())?;
            let offset = state.rook.read().await.telegram_bot(bot).map_err(|e| e.to_string())?;
            tokio::try_join!(
                poll(&state, &client, &settings, offset, bot),
                dispatch(&state, &client, &settings),
                send(&state, &client, &settings)
            )?;
            Ok::<(), String>(())
        }
        .await;
        if let Err(e) = result {
            let _ = state.rook.read().await.telegram_health(Some(&e));
            tracing::warn!("Telegram adapter paused; retained receipts remain inspectable: {e}");
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}
async fn current(state: &Arc<AppState>, settings: &Settings) -> Result<(), String> {
    if state.rook.read().await.config.telegram != *settings {
        return Err("Trusted Telegram settings changed; rebuilding the adapter".into());
    }
    Ok(())
}
async fn poll(
    state: &Arc<AppState>,
    client: &Client,
    settings: &Settings,
    mut offset: i64,
    bot: i64,
) -> Result<(), String> {
    loop {
        current(state, settings).await?;
        let updates = client.updates(offset).await.map_err(|e| e.to_string())?;
        state.rook.read().await.telegram_health(None).map_err(|e| e.to_string())?;
        for update in updates {
            current(state, settings).await?;
            let rook = state.rook.read().await;
            rook.telegram_update(&update).map_err(|e| e.to_string())?;
            offset = rook.telegram_bot(bot).map_err(|e| e.to_string())?;
        }
    }
}
async fn dispatch(state: &Arc<AppState>, client: &Client, settings: &Settings) -> Result<(), String> {
    let mut cursor = 0usize;
    loop {
        current(state, settings).await?;
        let inputs = state.rook.read().await.telegram_pending().map_err(|e| e.to_string())?;
        for input in inputs.into_iter().take(16) {
            let result = crate::chat::telegram::submit(state, &input).await;
            let route = state
                .rook
                .read()
                .await
                .telegram_routes()
                .map_err(|e| e.to_string())?
                .into_iter()
                .find(|r| r.session == input.session)
                .ok_or("Input route disappeared")?;
            let (text, error) = match result {
                Ok(text) => (text, None),
                Err(e) => (format!("Not applied: {e}"), Some(e)),
            };
            // Retain acknowledgement before resolving the source receipt.
            {
                let rook = state.rook.read().await;
                rook.telegram_input_done(input.update, &route, &text, error).map_err(|e| e.to_string())?;
            }
            if let Some(query) = input.callback_query {
                let _ = client.answer_callback(&query, "Decision processed; see the retained session").await;
            }
        }
        let routes = state.rook.read().await.telegram_routes().map_err(|e| e.to_string())?;
        let start = cursor % routes.len().max(1);
        cursor = cursor.wrapping_add(16);
        for route in routes.iter().skip(start).chain(routes.iter().take(start)).take(16) {
            if let Err(e) = crate::chat::telegram::notices(state, route).await {
                tracing::warn!("Telegram request publication: {e}");
            }
            let rook = state.rook.read().await;
            let session =
                rook_store::parse_session_id(&route.session).ok_or("Invalid Telegram route session")?;
            for event in rook
                .store
                .events(session, route.result_seq.saturating_add(1), 16)
                .map_err(|e| e.to_string())?
            {
                let mut text = String::new();
                if matches!(
                    event.record.kind,
                    rook_store::EventKind::AssistantMessage | rook_store::EventKind::Error
                ) {
                    let cap = settings.message_bytes.saturating_sub(512);
                    let bytes =
                        rook.store.get_range(&event.record.body, 0, cap).map_err(|e| e.to_string())?;
                    text = String::from_utf8_lossy(&bytes).into_owned();
                    if rook
                        .store
                        .stat_object(&event.record.body)
                        .map_err(|e| e.to_string())?
                        .is_some_and(|m| m.size_raw > cap as u64)
                    {
                        text.push_str(&format!(
                            "\n[Output shortened; full source: session {} event {}]",
                            route.session, event.seq
                        ));
                    }
                }
                rook.telegram_publish(route, event.seq, &text).map_err(|e| e.to_string())?;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}
async fn send(state: &Arc<AppState>, client: &Client, settings: &Settings) -> Result<(), String> {
    loop {
        current(state, settings).await?;
        let outbox = state.rook.read().await.telegram_outbox().map_err(|e| e.to_string())?;
        if let Some(item) =
            outbox.into_iter().find(|i| i.state == "pending" && i.next_at <= rook_core::work::managed::now())
        {
            let item = state.rook.read().await.telegram_begin_send(&item.id).map_err(|e| e.to_string())?;
            let result = client.send(&item).await;
            state.rook.read().await.telegram_finish_send(&item.id, result).map_err(|e| e.to_string())?;
        } else {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }
}
