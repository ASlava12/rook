//! Session inspection, recovery, rewind and search.

use crate::args::SessionCmd;
use crate::session_id;
use crate::{fmt, source::Source};
use anyhow::Result;
use rook_core::SessionSummary;
use std::path::Path;

pub(crate) fn describe_turns(page: &rook_core::turns::Page) -> String {
    let mut text = rook_core::turns::describe(page);
    for entry in &page.items {
        text.push_str("\n\n");
        text.push_str(&rook_core::turns::entry_text(entry));
    }
    if let Some(before) = page.before {
        text.push_str(&format!("\n\nOlder results: --before {before} (REPL: /turns {before})"));
    }
    text
}

pub(crate) fn diagnostic_arguments(arguments: &str) -> Result<(bool, std::path::PathBuf)> {
    let arguments = arguments.trim();
    let (logs, rest) = if arguments == "--logs" {
        (true, "")
    } else if let Some(rest) = arguments.strip_prefix("--logs ") {
        (true, rest.trim())
    } else {
        (false, arguments)
    };
    anyhow::ensure!(!rest.starts_with("--"), "use /diagnostics [--logs] [new-file-path]");
    let path = if rest.is_empty() {
        let directory = rook_core::paths::home().join("diagnostics");
        std::fs::create_dir_all(&directory)?;
        directory.join(format!(
            "rook-diagnostics-{}.json",
            rook_store::format_session_id(rook_store::new_session_id())
        ))
    } else {
        rest.into()
    };
    Ok((logs, path))
}

pub(crate) fn cmd_session(source: &Source, cmd: SessionCmd, workspace: &Path, json: bool) -> Result<()> {
    if let SessionCmd::Rename { id, title } = &cmd {
        let renamed = source.rename_branch(source.session_named(id, workspace)?, title)?;
        if json {
            println!("{}", serde_json::to_string_pretty(&renamed)?);
        } else {
            println!("renamed {} to {}", renamed.id, renamed.title);
        }
        return Ok(());
    }
    if let SessionCmd::Bookmarks { id } = &cmd {
        let page = source.bookmarks(source.session_named(id, workspace)?)?;
        if json {
            println!("{}", serde_json::to_string_pretty(&page)?);
        } else if page.items.is_empty() {
            println!("no bookmarks");
        } else {
            for bookmark in page.items {
                println!(
                    "#{} {}{}",
                    bookmark.seq,
                    bookmark.label,
                    if bookmark.available { "" } else { " [event unavailable]" }
                );
            }
        }
        return Ok(());
    }
    if let SessionCmd::Bookmark { id, event, label } = &cmd {
        let page = source.mark_bookmark(source.session_named(id, workspace)?, *event, label)?;
        if json {
            println!("{}", serde_json::to_string_pretty(&page)?);
        } else {
            println!("bookmarked event #{event}");
        }
        return Ok(());
    }
    if let SessionCmd::Unbookmark { id, event } = &cmd {
        let page = source.mark_bookmark(source.session_named(id, workspace)?, *event, "")?;
        if json {
            println!("{}", serde_json::to_string_pretty(&page)?);
        } else {
            println!("removed bookmark for event #{event}");
        }
        return Ok(());
    }
    if let SessionCmd::Branch { id, event } = &cmd {
        let forked = source.branch_from_event(source.session_named(id, workspace)?, *event)?;
        // Always structured: the complete draft and attachments must survive
        // when this command is piped into another editor or client.
        println!("{}", serde_json::to_string_pretty(&forked)?);
        return Ok(());
    }
    if let SessionCmd::Tree { id, after } = &cmd {
        let page = source.branch_page(source.session_named(id, workspace)?, after.as_deref())?;
        println!(
            "{}",
            if json { serde_json::to_string_pretty(&page)? } else { rook_core::branches::describe(&page) }
        );
        return Ok(());
    }
    if let SessionCmd::Turns { id, before } = &cmd {
        let page = source.turn_results(source.session_named(id, workspace)?, *before)?;
        if json {
            println!("{}", serde_json::to_string_pretty(&page)?);
        } else {
            println!("{}", describe_turns(&page));
        }
        return Ok(());
    }
    if let SessionCmd::Queue { id, action } = &cmd {
        let session = source.session_named(id, workspace)?;
        let value = super::queue::execute(source, session, action.as_ref())?;
        println!(
            "{}",
            if json { serde_json::to_string_pretty(&value)? } else { super::queue::describe(&value)? }
        );
        return Ok(());
    }
    if let SessionCmd::Diagnostics { id, output, logs } = &cmd {
        let report = source.diagnostics(source.session_named(id, workspace)?, *logs)?;
        match output {
            Some(path) => {
                println!("{}", report.save(path)?.display());
            }
            None => println!("{}", report.json()?),
        }
        return Ok(());
    }
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
    match &cmd {
        SessionCmd::History { id, from, before, limit } => {
            let page = source.transcript_page(
                source.session_named(id, workspace)?,
                &rook_core::transcript::PageRequest { from: *from, before: *before, limit: *limit },
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&page)?);
            } else {
                show_transcript(&page.items, false)?;
                if let Some(before) = page.previous {
                    println!("earlier: --before {before}");
                }
                if let Some(from) = page.next {
                    println!("later: --from {from}");
                }
            }
            return Ok(());
        }
        SessionCmd::Find { id, query, from, offset, through } => {
            let result = source.transcript_search(
                source.session_named(id, workspace)?,
                query,
                rook_core::transcript::Cursor { seq: *from, offset: *offset, through: *through },
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                for hit in &result.hits {
                    println!(
                        "#{} {} {} (byte {})\n{}",
                        hit.seq, hit.kind, hit.label, hit.offset, hit.snippet
                    );
                }
                if let Some(next) = result.next {
                    println!(
                        "continue search: --from {} --offset {} --through {}",
                        next.seq,
                        next.offset,
                        next.through.unwrap_or(0)
                    );
                } else if result.hits.is_empty() {
                    println!("no matches in the remaining history");
                }
            }
            return Ok(());
        }
        SessionCmd::Entry { id, seq, offset } => {
            let page = source.transcript_entry(source.session_named(id, workspace)?, *seq, *offset)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&page)?);
            } else {
                println!(
                    "#{} {} byte {} / {}\n{}",
                    page.entry.seq, page.entry.kind, page.offset, page.total_bytes, page.entry.body
                );
                if let Some(offset) = page.next_offset {
                    println!("next: --offset {offset}");
                }
            }
            return Ok(());
        }
        SessionCmd::Quote { id, seq, offset } => {
            let quote = source.transcript_quote(source.session_named(id, workspace)?, *seq, *offset)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&quote)?);
            } else {
                println!("{}", quote.text);
            }
            return Ok(());
        }
        _ => {}
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
        SessionCmd::Rename { .. }
        | SessionCmd::Bookmarks { .. }
        | SessionCmd::Bookmark { .. }
        | SessionCmd::Unbookmark { .. }
        | SessionCmd::Branch { .. }
        | SessionCmd::Tree { .. }
        | SessionCmd::Turns { .. }
        | SessionCmd::Queue { .. }
        | SessionCmd::History { .. }
        | SessionCmd::Find { .. }
        | SessionCmd::Entry { .. }
        | SessionCmd::Quote { .. }
        | SessionCmd::Diagnostics { .. }
        | SessionCmd::Recovery { .. }
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
