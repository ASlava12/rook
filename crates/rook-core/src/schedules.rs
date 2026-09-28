//! Calendar tasks, durable launch reservations and bounded execution history.
//! Clock input is explicit so restart, missed dates and DST are testable.
use crate::{CoreError, Result, Rook, work::managed};
use chrono::{Datelike, NaiveDateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use rook_proto::schedule::{Action, Create, Launch, Spec, Task};
use rook_proto::work::{Conversation, Start};
use std::sync::Mutex;

const KEY: &str = "schedule/tasks-v1";
const HISTORY: usize = 16;
static WRITING: Mutex<()> = Mutex::new(());
fn bad(text: impl Into<String>) -> CoreError {
    CoreError::Other(text.into())
}

/// Return the first occurrence strictly after `after`. Ambiguous local times
/// run once (the earlier occurrence); nonexistent times are skipped.
pub fn next(timing: &str, zone: &str, after: u64) -> Result<Option<u64>> {
    let tz: Tz = zone.parse().map_err(|_| bad("use an IANA timezone, e.g. Europe/Moscow"))?;
    let utc = i64::try_from(after)
        .ok()
        .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
        .ok_or_else(|| bad("timestamp out of range"))?;
    let parts: Vec<_> = timing.split_whitespace().collect();
    if let ["once", date, time] = parts.as_slice() {
        let dt = NaiveDateTime::parse_from_str(&format!("{date} {time}"), "%Y-%m-%d %H:%M")
            .map_err(|_| bad("once requires YYYY-MM-DD HH:MM"))?;
        let at = tz
            .from_local_datetime(&dt)
            .single()
            .ok_or_else(|| bad("one-time date is ambiguous or missing in this timezone"))?
            .timestamp();
        return Ok(u64::try_from(at).ok().filter(|at| *at > after));
    }
    if let ["every", period] = parts.as_slice() {
        let split = period
            .len()
            .checked_sub(1)
            .filter(|i| period.is_char_boundary(*i))
            .ok_or_else(|| bad("invalid interval"))?;
        let (n, unit) = period.split_at(split);
        let multiplier = match unit {
            "m" => 60,
            "h" => 3600,
            "d" => 86400,
            _ => return Err(bad("interval uses m, h or d")),
        };
        let seconds = n
            .parse::<u64>()
            .ok()
            .and_then(|n| n.checked_mul(multiplier))
            .filter(|n| (60..=31_536_000).contains(n))
            .ok_or_else(|| bad("interval must be between one minute and one year"))?;
        return after.checked_add(seconds).map(Some).ok_or_else(|| bad("interval overflow"));
    }
    let (time, days): (&str, Vec<u32>) = match parts.as_slice() {
        ["daily", time] => (time, (0..7).collect()),
        ["weekdays", time] => (time, (0..5).collect()),
        ["weekly", day, time] => {
            let d = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"]
                .iter()
                .position(|v| v == day)
                .ok_or_else(|| bad("weekly day must be mon..sun"))?;
            (time, vec![d as u32])
        }
        _ => {
            return Err(bad(
                "use once YYYY-MM-DD HH:MM, every 30m, daily HH:MM, weekdays HH:MM or weekly fri HH:MM",
            ));
        }
    };
    let time = NaiveTime::parse_from_str(time, "%H:%M").map_err(|_| bad("time must be HH:MM"))?;
    let mut date = utc.with_timezone(&tz).date_naive();
    for _ in 0..15 {
        if days.contains(&date.weekday().num_days_from_monday())
            && let Some(at) = tz.from_local_datetime(&date.and_time(time)).earliest()
            && at.timestamp() > utc.timestamp()
        {
            return Ok(Some(at.timestamp() as u64));
        }
        date = date.succ_opt().ok_or_else(|| bad("date out of range"))?;
    }
    Err(bad("no occurrence in the next two weeks"))
}

pub fn display_time(at: u64, zone: &str) -> String {
    let parsed =
        zone.parse::<Tz>().ok().zip(i64::try_from(at).ok().and_then(|n| Utc.timestamp_opt(n, 0).single()));
    parsed
        .map(|(tz, dt)| dt.with_timezone(&tz).format("%Y-%m-%d %H:%M %Z").to_string())
        .unwrap_or_else(|| at.to_string())
}
fn load(rook: &Rook) -> Result<Vec<Task>> {
    rook.store.kv_get(KEY)?.map(|b| serde_json::from_slice(&b).map_err(Into::into)).unwrap_or(Ok(vec![]))
}
fn save(rook: &Rook, tasks: &[Task]) -> Result<()> {
    crate::persistence::save_json(&rook.store, KEY, &tasks)
}
fn validate(rook: &Rook, spec: &Spec, at: u64) -> Result<Option<u64>> {
    if spec.goal.trim().is_empty() || spec.goal.len() > rook.config.work.max_goal_bytes {
        return Err(bad("goal is empty or too long"));
    }
    if spec.timing.len() > 80 || spec.timezone.len() > 80 {
        return Err(bad("schedule is too long"));
    }
    if !matches!(spec.stance.as_str(), "readonly" | "assist" | "autonomous") {
        return Err(bad("stance must be readonly, assist or autonomous"));
    }
    if spec.max_seconds == 0
        || spec.max_seconds > 604800
        || spec.max_tokens == 0
        || spec.max_tokens > 100_000_000
        || spec.max_iterations == 0
        || spec.max_iterations > 10000
    {
        return Err(bad(
            "scheduled runs require positive budgets: at most 7 days, 100M tokens and 10000 iterations",
        ));
    }
    if spec.workspace.len() > 4096
        || !std::path::Path::new(&spec.workspace).is_absolute()
        || !std::path::Path::new(&spec.workspace).is_dir()
    {
        return Err(bad("workspace must be an existing absolute directory"));
    }
    next(&spec.timing, &spec.timezone, at)
}
pub fn create(rook: &Rook, request: Create, at: u64) -> Result<Task> {
    let _guard = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    if rook_store::parse_session_id(&request.id).is_none() {
        return Err(bad("invalid schedule id"));
    }
    let mut tasks = load(rook)?;
    if let Some(old) = tasks.iter().find(|t| t.id == request.id) {
        return if old.spec == request.spec {
            Ok(old.clone())
        } else {
            Err(bad("schedule id already has different settings"))
        };
    }
    if tasks.len() >= 64 {
        return Err(bad("64 schedules already exist; delete one first"));
    }
    let next_at =
        validate(rook, &request.spec, at)?.ok_or_else(|| bad("one-time date must be in the future"))?;
    let task = Task {
        id: request.id,
        spec: request.spec,
        enabled: true,
        next_at: Some(next_at),
        note: "Scheduled".into(),
        pending: None,
        history: vec![],
    };
    tasks.push(task.clone());
    save(rook, &tasks)?;
    Ok(task)
}
fn refresh(rook: &Rook, task: &mut Task) -> Result<()> {
    for launch in &mut task.history {
        if let Some(bytes) = rook.store.kv_get(&format!("work/managed/{}", launch.session))? {
            let saved: managed::Saved = serde_json::from_slice(&bytes)?;
            launch.status = format!("{:?}", saved.run.status);
            launch.reason = saved.run.reason;
        }
    }
    Ok(())
}
pub fn list(rook: &Rook) -> Result<Vec<Task>> {
    let mut tasks = load(rook)?;
    for task in &mut tasks {
        refresh(rook, task)?;
    }
    Ok(tasks)
}
fn busy(rook: &Rook, task: &Task) -> Result<bool> {
    if task.pending.is_some() {
        return Ok(true);
    }
    if let Some(last) = task.history.last()
        && let Some(bytes) = rook.store.kv_get(&format!("work/managed/{}", last.session))?
    {
        let saved: managed::Saved = serde_json::from_slice(&bytes)?;
        return Ok(!saved.run.status.terminal() || saved.active.is_some());
    }
    Ok(false)
}
fn reserve(task: &mut Task, at: u64) {
    let session = rook_store::format_session_id(rook_store::new_session_id());
    task.pending = Some(session.clone());
    task.history.push(Launch { session, at, status: "Queued".into(), reason: "Waiting to start".into() });
    task.note = "Session queued".into();
}
/// History remains bounded, while session transcripts follow normal retention.
fn trim(rook: &Rook, task: &mut Task) -> Result<()> {
    while task.history.len() >= HISTORY {
        let oldest = &task.history[0].session;
        if rook.store.kv_get(&format!("work/managed/{oldest}"))?.is_some() {
            managed::forget(rook, oldest)?;
        }
        task.history.remove(0);
    }
    Ok(())
}
pub fn control(rook: &Rook, id: &str, action: Action, at: u64) -> Result<Task> {
    let _guard = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let mut tasks = load(rook)?;
    let task = tasks.iter_mut().find(|t| t.id == id).ok_or_else(|| bad("no such schedule"))?;
    match action {
        Action::Enable => {
            task.next_at = validate(rook, &task.spec, at)?;
            if task.next_at.is_none() {
                return Err(bad("one-time date has passed; use Run now"));
            }
            task.enabled = true;
            task.note = "Enabled".into();
        }
        Action::Disable => {
            task.enabled = false;
            task.note = "Disabled; existing session continues".into();
        }
        Action::CancelRun => {
            if task.pending.is_none()
                && task.history.last().is_some_and(|r| matches!(r.status.as_str(), "Completed" | "Cancelled"))
            {
                return Err(bad("latest session has already ended"));
            }
            let session = task
                .pending
                .clone()
                .or_else(|| task.history.last().map(|r| r.session.clone()))
                .ok_or_else(|| bad("no run to cancel"))?;
            if rook.store.kv_get(&format!("work/managed/{session}"))?.is_some() {
                managed::control(rook, &session, rook_proto::work::Action::Cancel)?;
            } else if let Some(last) = task.history.last_mut() {
                last.status = "Cancelled".into();
                last.reason = "Cancelled before execution".into();
            }
            task.pending = None;
            task.note = "Latest session cancelled; schedule unchanged".into();
        }
        Action::RunNow => {
            if busy(rook, task)? {
                return Err(bad("previous session is unfinished; open it to continue or cancel"));
            }
            trim(rook, task)?;
            reserve(task, at);
        }
    }
    let result = task.clone();
    save(rook, &tasks)?;
    Ok(result)
}
pub fn delete(rook: &Rook, id: &str) -> Result<()> {
    let _guard = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let mut tasks = load(rook)?;
    let task = tasks.iter().find(|t| t.id == id).ok_or_else(|| bad("no such schedule"))?;
    if busy(rook, task)? {
        return Err(bad("disable this schedule and finish or cancel its session before deleting"));
    }
    for launch in &task.history {
        if rook.store.kv_get(&format!("work/managed/{}", launch.session))?.is_some() {
            managed::forget(rook, &launch.session)?;
        }
    }
    tasks.retain(|t| t.id != id);
    save(rook, &tasks)
}
/// On restart skip recurring occurrences missed by more than one minute. A
/// one-time job is performed once when the daemon returns. Never drain a backlog.
pub fn due(rook: &Rook, at: u64) -> Result<Vec<Task>> {
    let _guard = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let mut tasks = load(rook)?;
    let mut changed = false;
    // Preserve terminal receipts before freeing the bounded live-run registry.
    // Historical sessions themselves remain readable under normal retention.
    let mut retired = Vec::new();
    for task in &mut tasks {
        for launch in &mut task.history {
            if task.pending.as_deref() == Some(&launch.session) {
                continue;
            }
            if let Some(bytes) = rook.store.kv_get(&format!("work/managed/{}", launch.session))? {
                let saved: managed::Saved = serde_json::from_slice(&bytes)?;
                if saved.run.status.terminal() && saved.active.is_none() {
                    launch.status = format!("{:?}", saved.run.status);
                    launch.reason = saved.run.reason;
                    retired.push(launch.session.clone());
                }
            }
        }
    }
    if !retired.is_empty() {
        save(rook, &tasks)?;
        for id in retired {
            managed::forget(rook, &id)?;
        }
    }
    for task in &mut tasks {
        if task.enabled && task.next_at.is_some_and(|n| n <= at) {
            let scheduled = task.next_at.unwrap_or(at);
            let once = task.spec.timing.split_whitespace().next() == Some("once");
            task.next_at = next(&task.spec.timing, &task.spec.timezone, at)?;
            if once {
                task.enabled = false;
            }
            if busy(rook, task)? {
                task.note = "Skipped: previous session is unfinished".into();
            } else if !once && at.saturating_sub(scheduled) > 60 {
                task.note = "Missed occurrences skipped after downtime".into();
            } else {
                trim(rook, task)?;
                reserve(task, at);
            }
            changed = true;
        }
    }
    if changed {
        save(rook, &tasks)?;
    }
    Ok(tasks.into_iter().filter(|t| t.pending.is_some()).collect())
}
/// Start from a durable reservation. Holding this short lock fences delete and
/// manual launch; no model or network work occurs under it.
pub fn launch(rook: &Rook, id: &str) -> Result<()> {
    let _guard = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let mut tasks = load(rook)?;
    let task = tasks.iter_mut().find(|t| t.id == id).ok_or_else(|| bad("no such schedule"))?;
    let Some(session) = task.pending.clone() else {
        return Ok(());
    };
    let attempt = (|| -> Result<()> {
        let sid = rook_store::parse_session_id(&session).ok_or_else(|| bad("invalid reserved session"))?;
        if managed::for_session(rook, sid)?.is_some() {
            return Ok(());
        }
        // Reserved id survives a crash between these two durable writes.
        if rook.store.get_session(sid)?.is_none() {
            let mut meta = rook_store::SessionMeta::new(
                sid,
                format!("Scheduled: {}", task.spec.goal.chars().take(100).collect::<String>()),
                task.spec.workspace.clone(),
                at_i64(managed::now()),
            );
            meta.model = rook.config.agent.model.clone();
            meta.tags.push(format!("schedule:{}", task.id));
            rook.store.create_session(&meta)?;
            rook.store.flush()?;
        }
        managed::start(
            rook,
            Start {
                goal: task.spec.goal.clone(),
                workspace: Some(task.spec.workspace.clone()),
                conversation: Some(Conversation {
                    session: session.clone(),
                    model: None,
                    effort: "medium".into(),
                    stance: task.spec.stance.clone(),
                    options: Default::default(),
                }),
                autonomous: task.spec.stance == "autonomous",
                max_seconds: Some(task.spec.max_seconds),
                max_tokens: Some(task.spec.max_tokens),
                max_iterations: Some(task.spec.max_iterations),
            },
        )?;
        Ok(())
    })();
    match attempt {
        Ok(()) => {
            task.pending = None;
            task.note = "Session started".into();
        }
        Err(error) => {
            task.pending = None;
            task.enabled = false;
            task.note = format!("Needs attention: {error}").chars().take(2048).collect();
            if let Some(last) = task.history.last_mut() {
                last.status = "Blocked".into();
                last.reason = task.note.clone();
            }
        }
    }
    save(rook, &tasks)
}
fn at_i64(at: u64) -> i64 {
    i64::try_from(at).unwrap_or(i64::MAX)
}

/// Configuration or workspace failures need intervention, not an endless retry.
pub fn fail_pending(rook: &Rook, id: &str, reason: &str) -> Result<()> {
    let _guard = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let mut tasks = load(rook)?;
    if let Some(task) = tasks.iter_mut().find(|t| t.id == id && t.pending.is_some()) {
        task.pending = None;
        task.enabled = false;
        task.note = format!("Needs attention: {reason}").chars().take(2048).collect();
        if let Some(last) = task.history.last_mut() {
            last.status = "Blocked".into();
            last.reason = task.note.clone();
        }
        save(rook, &tasks)?;
    }
    Ok(())
}
