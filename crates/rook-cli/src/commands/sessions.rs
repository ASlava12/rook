//! Session inspection, recovery, rewind and search.

use crate::args::SessionCmd;
use crate::session_id;
use crate::{fmt, source::Source};
use anyhow::Result;
use rook_core::SessionSummary;
use std::path::Path;

pub(crate) fn cmd_session(source: &Source, cmd: SessionCmd, workspace: &Path, json: bool) -> Result<()> {
    // Both read, and both are what somebody wants while the daemon is up.
    if let SessionCmd::Recovery { id, acknowledge, note } = &cmd {
        let session = source.session_named(id, workspace)?;
        if let Some(operation) = acknowledge {
            source.acknowledge_operation(session, operation, note.as_deref().unwrap_or_default())?;
        }
        println!("{}", serde_json::to_string_pretty(&source.execution(session)?)?);
        return Ok(());
    }
    if let SessionCmd::Ls { all } = cmd {
        return show_sessions(&source.sessions()?, workspace, all, json);
    }
    if let SessionCmd::Show { id, from, limit, max_body } = &cmd {
        let session = source.session_named(id, workspace)?;
        let entries = source.transcript(session, *from, *limit, *max_body)?;
        return show_transcript(&entries, json);
    }
    if let SessionCmd::Diff { id, stat } = &cmd {
        let session = source.session_named(id, workspace)?;
        return show_changes(&source.changes(session, !stat)?, json);
    }
    if let SessionCmd::Context { id, window } = &cmd {
        let session = source.session_named(id, workspace)?;
        return show_context(&source.context_usage(session, *window, workspace)?, json);
    }
    // Writes the daemon's API serves, routed for the same reason the reads
    // are: the store takes one writer, and stopping the daemon to set a goal
    // is not an answer.
    if let SessionCmd::Goal { id, goal } = &cmd {
        let session = source.session_named(id, workspace)?;
        if goal.is_empty() {
            match source.goal(session)? {
                Some(goal) => println!("{goal}"),
                None => println!("no goal set for this session"),
            }
            return Ok(());
        }
        source.set_goal(session, &goal.join(" "))?;
        println!("goal set");
        return Ok(());
    }
    if let SessionCmd::Rewind { id, to, keep_files } = &cmd {
        let session = source.session_named(id, workspace)?;
        return show_rewind(&source.rewind(session, *to, !keep_files)?, *keep_files, json);
    }
    match cmd {
        SessionCmd::Recovery { .. }
        | SessionCmd::Ls { .. }
        | SessionCmd::Show { .. }
        | SessionCmd::Diff { .. }
        | SessionCmd::Context { .. }
        | SessionCmd::Goal { .. }
        | SessionCmd::Rewind { .. } => unreachable!("routed above"),
        SessionCmd::Move { id, to } => {
            let session = source.session_named(&id, workspace)?;
            let home = source.move_session(session, &to)?;
            println!("this session's turns now run in {home}");
        }
        SessionCmd::Fork { id, at } => {
            let (forked, events) = source.fork_session(source.session_named(&id, workspace)?, at)?;
            println!("forked {events} events into {forked}");
        }
        SessionCmd::Rm { id } => {
            let removed = source.delete_session(source.session_named(&id, workspace)?)?;
            println!("removed session with {removed} events; run `rook store gc` to reclaim space");
        }
    }
    Ok(())
}

fn show_sessions(sessions: &[SessionSummary], workspace: &Path, all: bool, json: bool) -> Result<()> {
    let here = workspace.display().to_string();
    let shown: Vec<&SessionSummary> = sessions.iter().filter(|s| all || s.meta.workspace == here).collect();
    let elsewhere = sessions.len() - shown.len();
    let sessions = shown;
    if json {
        println!("{}", serde_json::to_string_pretty(&sessions)?);
        return Ok(());
    }
    let rows: Vec<Vec<String>> = sessions
        .iter()
        .map(|s| {
            vec![
                rook_store::format_session_id(s.meta.id),
                // Sub-tasks and forks are listed alongside what they came
                // from; the marker is what tells them apart at a glance, and a
                // fork says where in the parent it diverged.
                format!(
                    "{}{}",
                    match (s.meta.parent, s.forked_at) {
                        (Some(_), Some(at)) => format!("↳@{at} "),
                        (Some(_), None) => "↳ ".into(),
                        _ => String::new(),
                    },
                    match s.meta.title.trim().is_empty() {
                        true => "(untitled)".into(),
                        false => s.meta.title.chars().take(40).collect::<String>(),
                    }
                ),
                s.meta.event_count.to_string(),
                format!("{}/{}", s.meta.tokens_in, s.meta.tokens_out),
                fmt::ago(s.meta.updated_at),
                s.goal.clone().unwrap_or_else(|| s.meta.workspace.clone()),
            ]
        })
        .collect();
    print!("{}", fmt::table(&["id", "title", "events", "tok in/out", "updated", "goal / workspace"], &rows));
    if elsewhere > 0 {
        println!("\n({elsewhere} more in other workspaces — `rook session ls --all`)");
    }
    Ok(())
}

fn show_context(usage: &rook_core::ContextUsage, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&usage)?);
        return Ok(());
    }
    let pct = usage.live_tokens as f64 / usage.usable.max(1) as f64 * 100.0;
    println!("window       {:>9}  (usable {}, compacts at {})", usage.window, usage.usable, usage.compact_at);
    println!(
        "in context   {:>9}  {:.0}% of usable {}",
        usage.live_tokens,
        pct,
        if usage.needs_compaction { "— over the compaction threshold" } else { "" }
    );
    println!("ever logged  {:>9}  ({} compactions so far)", usage.logged_tokens, usage.compactions);
    if usage.replay_from > 0 {
        println!("replay from  {:>9}  everything before it is the last summary", usage.replay_from);
    }
    println!();
    let max = usage.by_kind.iter().map(|(_, u)| u.tokens).max().unwrap_or(0) as u64;
    let rows: Vec<Vec<String>> = usage
        .by_kind
        .iter()
        .map(|(kind, u)| {
            vec![
                kind.clone(),
                u.events.to_string(),
                fmt::bytes(u.bytes),
                format!("~{}", u.tokens),
                fmt::bar(u.tokens as u64, max, 20),
            ]
        })
        .collect();
    print!("{}", fmt::table(&["kind", "events", "bytes", "tokens", ""], &rows));
    Ok(())
}

fn show_changes(changes: &rook_core::changes::Changes, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&changes)?);
        return Ok(());
    }
    if changes.touched() == 0 {
        println!("this session changed nothing on disk");
        return Ok(());
    }
    for file in changes.files.iter().filter(|f| f.change != rook_core::changes::Change::Unchanged) {
        println!("{} {}  +{} -{}", file.change.sigil(), file.path, file.lines_added, file.lines_removed);
        if let Some(diff) = &file.diff {
            for line in diff.lines() {
                let colour = match line.chars().next() {
                    Some('+') => "\x1b[32m",
                    Some('-') => "\x1b[31m",
                    Some('@') => "\x1b[36m",
                    _ => "",
                };
                println!("  {colour}{line}\x1b[0m");
            }
        }
    }
    // After the diffs, because there is nothing to show for them: a command
    // declares no paths, so nothing holds what these were before.
    if !changes.written_by_commands.is_empty() {
        println!("\nwritten by commands, with nothing kept to diff or restore:");
        for path in &changes.written_by_commands {
            println!("  ? {path}");
        }
    }
    if !changes.watched {
        println!("\nthe workspace was too large to walk, so more may have been written");
    }
    println!("\n{}", changes.summary());
    Ok(())
}

fn show_transcript(entries: &[rook_core::TranscriptEntry], json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(entries)?);
        return Ok(());
    }
    for e in entries {
        println!(
            "── #{:<4} {:<12} {:<20} {} → {} {}",
            e.seq,
            e.kind,
            e.label.chars().take(20).collect::<String>(),
            fmt::bytes(e.bytes),
            fmt::bytes(e.stored_bytes),
            if e.truncated { "(elided)" } else { "" }
        );
        println!("{}\n", e.body);
    }
    Ok(())
}

fn show_rewind(report: &rook_core::Rewind, keep_files: bool, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    println!("rewound to session {}", report.session);
    println!("  {} events kept, parent {} left intact", report.events_kept, report.parent);
    if !keep_files {
        println!(
            "  {} checkpoint(s) applied: {} file(s) restored, {} removed",
            report.checkpoints_applied, report.files_restored, report.files_removed
        );
        if report.files_kept > 0 {
            println!(
                "  {} file(s) captured as they were first: `rook session rewind {} {}` puts them back",
                report.files_kept, report.session, report.events_kept
            );
        }
    }
    Ok(())
}

pub(crate) fn cmd_search(
    source: &Source,
    query: &str,
    session: Option<String>,
    conversation: bool,
    limit: usize,
    json: bool,
) -> Result<()> {
    let options = rook_core::search::Search {
        limit,
        session: session.as_deref().map(session_id).transpose()?,
        conversation_only: conversation,
        ..Default::default()
    };
    let found = source.search(query, &options)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&found)?);
        return Ok(());
    }
    if found.hits.is_empty() {
        println!("nothing matched in {} object(s)", found.objects_scanned);
        return Ok(());
    }
    for hit in &found.hits {
        println!("\x1b[2m{}  {:<12} {}\x1b[0m", fmt::hit_where(hit), hit.kind, fmt::ago(hit.when));
        println!("  {}", hit.snippet);
    }
    println!(
        "\n{} hit(s) across {} object(s){}",
        found.hits.len(),
        found.objects_scanned,
        if found.truncated { " — scan hit its budget, narrow with --session" } else { "" }
    );
    Ok(())
}
