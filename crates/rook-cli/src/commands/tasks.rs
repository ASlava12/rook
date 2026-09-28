//! Thin clients of the daemon-owned durable scheduler.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rook_proto::work::{Action, Run, Start, Steer, Steering};

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
        TaskCmd::List => daemon.get("/api/work")?,
        TaskCmd::Show { id } => daemon.get(&path(&id)?)?,
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
            while receipt.applied_at.is_none() && std::time::Instant::now() < until {
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
        TaskCmd::Pause { id } => control(&daemon, &id, Action::Pause)?,
        TaskCmd::Resume { id } => control(&daemon, &id, Action::Resume)?,
        TaskCmd::Cancel { id } => control(&daemon, &id, Action::Cancel)?,
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

fn control(daemon: &Daemon, id: &str, action: Action) -> Result<serde_json::Value> {
    daemon.post(&format!("{}/control", path(id)?), &serde_json::to_value(action)?)
}

fn describe(value: &serde_json::Value) -> String {
    if let Some(items) = value.as_array() {
        return if items.is_empty() {
            "No durable tasks.".into()
        } else {
            items.iter().map(describe).collect::<Vec<_>>().join("\n\n")
        };
    }
    if let Ok(run) = serde_json::from_value::<Run>(value.clone()) {
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
        "pause" => TaskCmd::Pause { id },
        "resume" => TaskCmd::Resume { id },
        "cancel" => TaskCmd::Cancel { id },
        "forget" => TaskCmd::Forget { id },
        "steer" => {
            let (id, text) = rest.trim().split_once(' ').context("/task steer <id> <correction>")?;
            TaskCmd::Steer { id: id.into(), text: vec![text.into()], message_id: None, wait_secs: 0 }
        }
        _ => bail!(
            "/task list | start <goal> | start-autonomous <goal> | show <id> | steer <id> <text> | pause/resume/cancel/forget <id>"
        ),
    };
    Ok(describe(&execute(cmd, Some(workspace.into()), command == "start-autonomous")?))
}
