//! Durable frontend settings for session-owned follow-up workers.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub(crate) struct Saved {
    workspace: String,
    model: String,
    effort: String,
    stance: String,
    #[serde(default)]
    paused: bool,
    #[serde(default)]
    error: Option<String>,
}
static WRITING: std::sync::Mutex<()> = std::sync::Mutex::new(());
const MAX_DRIVER_BYTES: usize = 16 * 1024;

fn key(session: u128) -> String {
    format!("followup-driver/{session:032x}")
}

impl Settings {
    pub(super) fn save_followups(
        &self,
        rook: &rook_core::Rook,
        session: u128,
        resume: bool,
    ) -> Result<(), String> {
        let _lock = WRITING.lock().unwrap_or_else(|e| e.into_inner());
        let previous = match read(rook, session) {
            Ok(saved) => saved,
            Err(_) if resume => None,
            Err(error) => return Err(error),
        };
        let model = self.model.read().unwrap_or_else(|e| e.into_inner());
        let model = model.as_ref().unwrap_or(&self.config.agent.model);
        let workspace = rook.workspace.to_string_lossy();
        if model.len() > 4096 || workspace.len() > 4096 {
            return Err("follow-up model and workspace names must each fit 4096 bytes".into());
        }
        let saved = Saved {
            workspace: workspace.into_owned(),
            model: model.clone(),
            effort: self.effort().as_str().into(),
            stance: self.policy.stance().as_str().into(),
            paused: !resume && previous.as_ref().is_some_and(|s| s.paused),
            error: None,
        };
        save(rook, session, &saved)
    }
}
fn save(rook: &rook_core::Rook, session: u128, saved: &Saved) -> Result<(), String> {
    // A fixed writer bounds escaped JSON before allocating an encoded copy.
    // Leave room for a later diagnostic without making the pause itself fail.
    let mut buffer = [0u8; MAX_DRIVER_BYTES];
    let limit = if saved.error.is_some() { MAX_DRIVER_BYTES } else { MAX_DRIVER_BYTES / 2 };
    let mut encoded = std::io::Cursor::new(&mut buffer[..limit]);
    serde_json::to_writer(&mut encoded, saved).map_err(|_| {
        "follow-up settings exceed the recovery limit; shorten the model or workspace name".to_string()
    })?;
    let len = encoded.position() as usize;
    rook.store.kv_set(&key(session), &encoded.get_ref()[..len]).map_err(|e| e.to_string())?;
    rook.store.flush().map_err(|e| e.to_string())
}
fn read(rook: &rook_core::Rook, session: u128) -> Result<Option<Saved>, String> {
    rook.store
        .kv_get_limited(&key(session), MAX_DRIVER_BYTES)
        .map_err(|e| e.to_string())?
        .map(|bytes| serde_json::from_slice(&bytes).map_err(|e| e.to_string()))
        .transpose()
}
pub(crate) fn pause(rook: &rook_core::Rook, session: u128, reason: Option<String>) -> Result<(), String> {
    let _lock = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(mut saved) = read(rook, session)? {
        saved.paused = true;
        saved.error = reason.map(|r| r.chars().take(512).collect());
        save(rook, session, &saved)?;
    }
    Ok(())
}

/// Commit the ordinary Stop receipt and its recovery pause together. The
/// frontend settings writer stays held through the store transaction, so a
/// concurrent settings save cannot undo the pause with an older snapshot.
pub(crate) fn pause_with_stop_receipt(
    rook: &rook_core::Rook,
    session: u128,
    execution_key: &str,
    execution: &[u8],
) -> rook_core::Result<()> {
    let _lock = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let saved = read(rook, session).map_err(rook_core::CoreError::Other)?;
    let mut buffer = [0u8; 8192];
    let driver_key = key(session);
    if let Some(mut saved) = saved {
        saved.paused = true;
        saved.error = None;
        let mut encoded = std::io::Cursor::new(&mut buffer[..]);
        serde_json::to_writer(&mut encoded, &saved).map_err(|_| {
            rook_core::CoreError::Other("follow-up settings exceed the recovery limit".into())
        })?;
        let len = encoded.position() as usize;
        rook.store.kv_update_session_values(
            session,
            &[(execution_key, execution), (driver_key.as_str(), &buffer[..len])],
        )?;
    } else {
        rook.store.kv_update_session_values(session, &[(execution_key, execution)])?;
    }
    rook.store.flush()?;
    Ok(())
}

/// Caller holds the same admission lock as manual prompts and managed goals.
pub(crate) async fn supervise(state: &Arc<AppState>, legacy_running: usize, cursor: &mut Option<u128>) {
    let (sessions, cap, scan) = {
        let rook = state.rook.read().await;
        let scan = rook.config.work.followup_scan_sessions;
        (rook.store.session_ids_after(*cursor, scan), rook.config.work.max_parallel_runs, scan)
    };
    let Ok(sessions) = sessions else { return };
    let at_end = sessions.len() < scan;
    let mut running =
        legacy_running.saturating_add(state.live.read().await.values().filter(|l| l.running()).count());
    for session in sessions {
        if running >= cap {
            return;
        }
        *cursor = Some(session);
        if state.live.read().await.get(&session).is_some_and(|l| l.running()) {
            continue;
        }
        let saved = {
            let rook = state.rook.read().await;
            match read(&rook, session) {
                Ok(Some(s)) if !s.paused => s,
                _ => continue,
            }
        };
        let ready = {
            let rook = state.rook.read().await;
            rook_core::message_queue::followups::ready(&rook, session)
        };
        if !matches!(ready, Ok(true)) {
            continue;
        }
        match resume(state, session, &saved).await {
            Ok(live) => {
                state.remember(session, live).await;
                running += 1;
            }
            Err(error) => {
                let rook = state.rook.read().await;
                if let Err(persist) = pause(&rook, session, Some(error.clone())) {
                    tracing::error!("cannot save follow-up restart failure: {persist}");
                }
                tracing::warn!("follow-up session {session} not resumed: {error}");
            }
        }
    }
    if at_end {
        *cursor = None;
    }
}
async fn resume(state: &Arc<AppState>, session: u128, saved: &Saved) -> Result<Arc<Live>, String> {
    let engine = state.engine_for(Some(std::path::Path::new(&saved.workspace))).await?;
    let shared = state.equipment_for(&engine).await;
    let settings = {
        let rook = engine.read().await;
        let meta = rook
            .store
            .get_session(session)
            .map_err(|e| e.to_string())?
            .ok_or("follow-up session disappeared")?;
        if std::path::Path::new(&meta.workspace) != std::path::Path::new(&saved.workspace) {
            return Err("follow-up workspace changed; open the session and select its settings".into());
        }
        let settings = Arc::new(Settings::new(&rook));
        settings.set("model", &saved.model)?;
        settings.set("effort", &saved.effort)?;
        settings.set("stance", &saved.stance)?;
        settings
    };
    Ok(begin(state, &engine, &shared, &settings, session, StartTurn::FollowUps, Default::default()).await)
}

pub(super) fn configure<'a>(agent: &mut AgentLoop<'a>, settings: Arc<Settings>) {
    let rook = agent.rook;
    let session = agent.session;
    agent.followup_model = Some(Arc::new(move || {
        settings.save_followups(rook, session, false).map_err(rook_core::CoreError::Other)?;
        let provider = rook_core::models::chosen(&rook.config, settings.model().as_deref())?;
        Ok((provider.into(), settings.effort()))
    }));
}

pub(crate) fn status(rook: &rook_core::Rook, session: u128) -> Option<String> {
    match read(rook, session) {
        Ok(Some(saved)) if saved.paused => Some(match saved.error {
            Some(error) => format!("Follow-ups paused: {error}"),
            None => "Follow-ups paused; /continue resumes the session.".into(),
        }),
        Err(error) => Some(format!("Follow-up recovery settings invalid: {error}")),
        _ => None,
    }
    .map(|text| text.chars().take(64).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn engine(dir: &std::path::Path) -> rook_core::Rook {
        rook_core::Rook::from_parts(
            rook_store::Store::open(dir.join("store")).unwrap(),
            rook_core::Config::default(),
            rook_skills::Environment::bare("linux", "x86_64", "0.10.0"),
            rook_skills::SkillIndex::default(),
            dir.into(),
        )
    }
    #[test]
    fn saved_settings_keep_pause_across_changes_and_an_explicit_prompt_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let rook = engine(dir.path());
        let session = rook.start_session("settings").unwrap();
        let settings = Settings::new(&rook);
        settings.set("stance", "readonly").unwrap();
        settings.set("effort", "low").unwrap();
        settings.save_followups(&rook, session, true).unwrap();
        pause(&rook, session, None).unwrap();
        settings.set("effort", "high").unwrap();
        settings.save_followups(&rook, session, false).unwrap();
        let saved = read(&rook, session).unwrap().unwrap();
        assert!(saved.paused);
        assert_eq!(saved.stance, "readonly");
        assert_eq!(saved.effort, "high");
        assert!(status(&rook, session).unwrap().contains("/continue"));
        settings.save_followups(&rook, session, true).unwrap();
        assert!(!read(&rook, session).unwrap().unwrap().paused);
    }
    #[test]
    fn an_oversized_saved_driver_is_refused_without_unpausing_or_replacing_it() {
        let dir = tempfile::tempdir().unwrap();
        let rook = engine(dir.path());
        let session = rook.start_session("driver reader bound").unwrap();
        let legacy = serde_json::json!({
            "workspace":rook.workspace, "model":rook.config.agent.model,
            "effort":"high", "stance":"readonly"
        });
        let mut bytes = serde_json::to_vec(&legacy).unwrap();
        bytes.resize(MAX_DRIVER_BYTES, b' ');
        rook.store.kv_set(&key(session), &bytes).unwrap();
        let saved = read(&rook, session).unwrap().unwrap();
        assert!(!saved.paused);
        assert!(saved.error.is_none(), "legacy defaults still load at the cap");
        bytes.push(b' ');
        assert!(bytes.len() > MAX_DRIVER_BYTES);
        assert!(serde_json::from_slice::<Saved>(&bytes).is_ok());
        rook.store.kv_set(&key(session), &bytes).unwrap();
        let error = read(&rook, session).err().unwrap();
        assert!(error.contains("exceeds 16384 bytes"), "{error}");
        assert!(pause(&rook, session, None).is_err());
        let settings = Settings::new(&rook);
        assert!(settings.save_followups(&rook, session, false).is_err());
        assert!(status(&rook, session).unwrap().starts_with("Follow-up recovery settings invalid:"));
        assert_eq!(rook.store.kv_get(&key(session)).unwrap().unwrap(), bytes);
        // Explicit resume retains its existing repair behavior; background
        // supervision and unrelated settings changes cannot do this.
        settings.save_followups(&rook, session, true).unwrap();
        assert!(!read(&rook, session).unwrap().unwrap().paused);
    }

    #[test]
    fn recovery_settings_are_bounded_before_the_encoded_copy_and_leave_room_for_failure() {
        let dir = tempfile::tempdir().unwrap();
        let rook = engine(dir.path());
        let session = rook.start_session("bounded driver").unwrap();
        let settings = Settings::new(&rook);
        settings.save_followups(&rook, session, true).unwrap();
        let previous = rook.store.kv_get(&key(session)).unwrap().unwrap();
        let mut saved = read(&rook, session).unwrap().unwrap();
        saved.model = "\"".repeat(8192);
        assert!(serde_json::to_vec(&saved).unwrap().len() > 16384);
        assert!(save(&rook, session, &saved).is_err());
        assert_eq!(rook.store.kv_get(&key(session)).unwrap().unwrap(), previous);
        pause(&rook, session, Some("🙂".repeat(4096))).unwrap();
        let saved = read(&rook, session).unwrap().unwrap();
        assert!(saved.paused);
        assert_eq!(saved.error.unwrap().chars().count(), 512);
        assert!(status(&rook, session).unwrap().chars().count() <= 64);
    }
}
