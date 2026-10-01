//! Thin clients of the daemon-owned durable scheduler.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rook_proto::work::{Action, ControlOutcome, IdentifiedControl, Run, Start, Steer, Steering};

use crate::{
    args::TaskCmd,
    source::{Daemon, Source},
};

fn daemon() -> Result<Daemon> {
    if let Some(daemon) = Daemon::running() {
        return Ok(daemon);
    }
    Source::start_a_daemon(None)?;
    Daemon::running().context("rookd did not become available")
}

pub(crate) fn run(cmd: TaskCmd, workspace: Option<PathBuf>, yes: bool, json: bool) -> Result<()> {
    let value = execute(cmd, workspace, yes)?;
    println!("{}", if json { serde_json::to_string_pretty(&value)? } else { describe(&value) });
    Ok(())
}

fn execute(cmd: TaskCmd, workspace: Option<PathBuf>, yes: bool) -> Result<serde_json::Value> {
    let daemon = daemon()?;
    let value = match cmd {
        TaskCmd::Schedule { goal, when, timezone, stance, seconds, tokens, max_iterations, id } => {
            let workspace = workspace.unwrap_or(std::env::current_dir()?).canonicalize()?;
            let id = id.unwrap_or_else(|| rook_store::format_session_id(rook_store::new_session_id()));
            eprintln!("schedule {id}");
            daemon.post(
                "/api/tasks",
                &serde_json::to_value(rook_proto::schedule::Create {
                    id,
                    spec: rook_proto::schedule::Spec {
                        goal: goal.join(" "),
                        workspace: workspace.display().to_string(),
                        timing: when,
                        timezone,
                        stance: if yes { "autonomous".into() } else { stance },
                        max_seconds: seconds,
                        max_tokens: tokens,
                        max_iterations,
                    },
                })?,
            )?
        }
        TaskCmd::Enable { id } => schedule_control(&daemon, &id, rook_proto::schedule::Action::Enable)?,
        TaskCmd::Disable { id } => schedule_control(&daemon, &id, rook_proto::schedule::Action::Disable)?,
        TaskCmd::Run { id } => schedule_control(&daemon, &id, rook_proto::schedule::Action::RunNow)?,
        TaskCmd::CancelRun { id } => schedule_control(&daemon, &id, rook_proto::schedule::Action::CancelRun)?,
        TaskCmd::Delete { id } => daemon.delete(&schedule_path(&id)?)?,
        TaskCmd::Start { goal, max_iterations, tokens, seconds } => {
            let workspace = workspace.unwrap_or(std::env::current_dir()?).canonicalize()?;
            daemon.post(
                "/api/work",
                &serde_json::to_value(Start {
                    conversation: None,
                    goal: goal.join(" "),
                    workspace: Some(workspace.display().to_string()),
                    autonomous: yes,
                    max_iterations,
                    max_tokens: tokens,
                    max_seconds: seconds,
                })?,
            )?
        }
        TaskCmd::List => daemon.get("/api/tasks")?,
        TaskCmd::Show { id } => {
            let tasks: Vec<rook_proto::schedule::Task> = daemon.get("/api/tasks")?;
            if let Some(task) = tasks.into_iter().find(|t| t.id == id) {
                serde_json::to_value(task)?
            } else {
                daemon.get(&path(&id)?)?
            }
        }
        TaskCmd::Steer { id, text, message_id, wait_secs } => {
            let path = path(&id)?;
            let message_id =
                message_id.unwrap_or_else(|| rook_store::format_session_id(rook_store::new_session_id()));
            // Print the retry key before sending: a disconnect must not erase it.
            eprintln!("instruction {message_id}");
            let mut receipt: Steering = daemon.post(
                &format!("{path}/steer"),
                &serde_json::to_value(Steer { id: message_id.clone(), text: text.join(" ") })?,
            )?;
            let until = std::time::Instant::now() + std::time::Duration::from_secs(wait_secs.min(86_400));
            while receipt.queued() && std::time::Instant::now() < until {
                std::thread::sleep(std::time::Duration::from_millis(500));
                let run: Run = daemon.get(&path)?;
                if let Some(found) = run.instructions.into_iter().find(|m| m.id == message_id) {
                    receipt = found;
                }
                if !run.status.runnable() {
                    break;
                }
            }
            serde_json::to_value(receipt)?
        }
        TaskCmd::Pause { id, control_id, generation } => {
            control(&daemon, &id, Action::Pause, control_id, generation)?
        }
        TaskCmd::Resume { id, control_id, generation } => {
            control(&daemon, &id, Action::Resume, control_id, generation)?
        }
        TaskCmd::Cancel { id, control_id, generation } => {
            control(&daemon, &id, Action::Cancel, control_id, generation)?
        }
        TaskCmd::Forget { id } => daemon.delete(&path(&id)?)?,
    };
    Ok(value)
}

fn path(id: &str) -> Result<String> {
    if rook_store::parse_session_id(id).is_none() {
        bail!("invalid task id");
    }
    Ok(format!("/api/work/{id}"))
}

fn control(
    daemon: &Daemon,
    id: &str,
    action: Action,
    control_id: Option<String>,
    generation: Option<String>,
) -> Result<serde_json::Value> {
    let (control_id, generation) = match (control_id, generation) {
        (Some(control_id), Some(generation)) => (control_id, generation),
        (None, None) => {
            let run: Run = daemon.get(&path(id)?)?;
            if run.generation.is_empty() {
                eprintln!("legacy run has no generation; inspect its state after an uncertain response");
                return daemon.post(&format!("{}/control", path(id)?), &serde_json::to_value(action)?);
            }
            (rook_store::format_session_id(rook_store::new_session_id()), run.generation)
        }
        _ => bail!("retry a control with both --control-id and --generation"),
    };
    // Both values reach stderr before the write so a lost response is retryable.
    eprintln!("control {control_id} generation {generation}");
    daemon.post(
        &format!("{}/control", path(id)?),
        &serde_json::to_value(IdentifiedControl { id: control_id, generation, action })?,
    )
}

fn describe(value: &serde_json::Value) -> String {
    if let Some(items) = value.as_array() {
        return if items.is_empty() {
            "No scheduled tasks.".into()
        } else {
            items.iter().map(describe).collect::<Vec<_>>().join("\n\n")
        };
    }
    if let Ok(task) = serde_json::from_value::<rook_proto::schedule::Task>(value.clone()) {
        let mut text = format!(
            "{} · {} · {} · {}\n{}\n{}\nNext: {}",
            task.id,
            if task.enabled { "enabled" } else { "disabled" },
            task.spec.timing,
            task.spec.timezone,
            task.spec.goal,
            task.note,
            task.next_at
                .filter(|_| task.enabled)
                .map(|n| rook_core::schedules::display_time(n, &task.spec.timezone))
                .unwrap_or_else(|| "—".into())
        );
        for run in task.history.iter().rev() {
            text.push_str(&format!(
                "\n{} · {} · session {}\n{}",
                rook_core::schedules::display_time(run.at, &task.spec.timezone),
                run.status,
                run.session,
                run.reason
            ));
        }
        text
    } else if let Ok(outcome) = serde_json::from_value::<ControlOutcome>(value.clone()) {
        format!(
            "control {} · generation {} · {}\n{} · {:?}\n{}",
            outcome.id,
            outcome.generation,
            if outcome.already_applied { "already applied" } else { "applied" },
            outcome.run.id,
            outcome.run.status,
            outcome.run.reason
        )
    } else if let Ok(run) = serde_json::from_value::<Run>(value.clone()) {
        let mut text = format!(
            "{} · {:?} · {} iterations · {} tokens\n{}\n{}\nworkspace: {}",
            run.id, run.status, run.iterations, run.tokens, run.goal, run.reason, run.workspace
        );
        if let Some(at) = run.next_attempt_at {
            text.push_str(&format!("\nnext retry: unix {at}"));
        }
        if let Some(session) = run.session {
            text.push_str(&format!("\nsession: {session}"));
        }
        for message in run.instructions {
            text.push_str(&format!("\n{}: {} — {}", message.id, receipt_state(&message), message.text));
        }
        if !run.reply.is_empty() {
            text.push_str(&format!("\n\nLatest answer:\n{}", run.reply));
        }
        if !run.verification.is_empty() {
            text.push_str(&format!("\n\nVerification:\n{}", run.verification));
        }
        text
    } else if let Ok(receipt) = serde_json::from_value::<Steering>(value.clone()) {
        format!("{}: {}\n{}", receipt.id, receipt_state(&receipt), receipt.text)
    } else {
        value.to_string()
    }
}

fn receipt_state(receipt: &Steering) -> &'static str {
    if receipt.applied_at.is_some() {
        "taken into the agent's context (execution is not yet confirmed)"
    } else if receipt.withdrawn_at.is_some() {
        "withdrawn before acceptance"
    } else {
        "saved; awaiting the agent's next safe boundary (use task show to check)"
    }
}

/// The same operations in the TUI; preserve spaces in goals and corrections.
pub(crate) fn slash(rest: &str, workspace: &Path) -> Result<String> {
    let (command, rest) = rest.trim().split_once(' ').unwrap_or((rest.trim(), ""));
    let id = rest.trim().to_string();
    let cmd = match command {
        "" | "list" => TaskCmd::List,
        "start" | "start-autonomous" => {
            TaskCmd::Start { goal: vec![rest.into()], max_iterations: None, tokens: None, seconds: None }
        }
        "show" => TaskCmd::Show { id },
        "enable" => TaskCmd::Enable { id },
        "disable" => TaskCmd::Disable { id },
        "run" => TaskCmd::Run { id },
        "delete" => TaskCmd::Delete { id },
        "cancel-run" => TaskCmd::CancelRun { id },
        "pause" => TaskCmd::Pause { id, control_id: None, generation: None },
        "resume" => TaskCmd::Resume { id, control_id: None, generation: None },
        "cancel" => TaskCmd::Cancel { id, control_id: None, generation: None },
        "forget" => TaskCmd::Forget { id },
        "steer" => {
            let (id, text) = rest.trim().split_once(' ').context("/task steer <id> <correction>")?;
            TaskCmd::Steer { id: id.into(), text: vec![text.into()], message_id: None, wait_secs: 0 }
        }
        _ => bail!(
            "F4 creates schedules; /task list | show <id> | run/enable/disable/cancel-run/delete <id>. Use /goal for work in this session."
        ),
    };
    Ok(describe(&execute(cmd, Some(workspace.into()), command == "start-autonomous")?))
}

fn schedule_path(id: &str) -> Result<String> {
    if rook_store::parse_session_id(id).is_none() {
        bail!("invalid schedule id");
    }
    Ok(format!("/api/tasks/{id}"))
}
fn schedule_control(
    daemon: &Daemon,
    id: &str,
    action: rook_proto::schedule::Action,
) -> Result<serde_json::Value> {
    daemon.post(&format!("{}/control", schedule_path(id)?), &serde_json::to_value(action)?)
}
