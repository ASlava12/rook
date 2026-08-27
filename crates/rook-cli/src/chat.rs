//! The interactive session.
//!
//! Everything here is a thin front end over `rook-core`: slash commands call the
//! same methods the non-interactive subcommands do, so the two cannot disagree
//! about what the agent did.

use std::io::Write;

use anyhow::{Context, Result};
use rook_core::agent::{AgentLoop, Progress};
use rook_core::{Rook, TranscriptEntry};
use rook_llm::Delta;
use rook_store::EventKind;
use rustyline::error::ReadlineError;

use crate::fmt;

const HELP: &str = "  /context [window]   what this conversation costs, and of what
  /skills [name]      skills that apply here, or one skill's body
  /session            id, size and token totals
  /goal [text]        what this session is for; the agent is told
  /mode [name]        approvals: auto, ask or readonly
  /effort [name]      how much the model may think: low … max
  /memory [query]     what it remembers, or what matches
  /search <query>     find it in everything said, read and run
  /diff               what this session has changed on disk
  /btw <question>     ask about this conversation without joining it
  /mcp                connected tool servers
  /undo               rewind past the last exchange, files included
  /rewind <seq>       rewind to a specific point in the transcript
  /new [title]        start a fresh session
  /help  /quit        this, and leaving

Ctrl-C stops the turn in flight. Ctrl-D leaves.";

pub fn run(workspace: Option<std::path::PathBuf>, resume: Option<String>, yes: bool) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let rook = Rook::open(workspace)?;
    let provider = rook_llm::from_spec_with(
        &rook.config.agent.model,
        rook.config.agent.stream_idle(),
        rook.config.agent.context_window,
    )
    .with_context(|| format!("configuring model {:?}", rook.config.agent.model))?;

    let mut session = match resume {
        Some(id) => rook.session_named(&id)?,
        None => rook.start_session("")?,
    };

    let mcp = runtime.block_on(rook.connect_mcp());
    for (name, error) in &mcp.failures {
        eprintln!("mcp {name}: {error}");
    }

    println!(
        "rook {} · {} · {}",
        rook_core::AGENT_VERSION,
        rook.config.agent.model,
        rook.workspace.display()
    );
    let skills = rook.catalog().iter().filter(|c| c.applicable).count();
    println!(
        "{skills} skill(s), {} tool server(s) offering {} tool(s), session {}",
        mcp.servers.len(),
        mcp.tool_count(),
        rook_store::format_session_id(session)
    );
    println!("/help for commands\n");

    // One policy for the whole session, so "always this run" means the session
    // and not the single turn it was granted in.
    let shared = Session {
        mcp: std::sync::Arc::new(mcp),
        policy: rook_core::agent::policy_for(&rook),
        // Likewise the language servers: a pool dropped per turn restarts
        // rust-analyzer, and it indexes the workspace every time it starts.
        servers: rook_core::agent::servers_for(&rook),
        effort: std::cell::Cell::new(rook.config.agent.effort()),
        yes,
    };

    let mut editor = rustyline::DefaultEditor::new()?;
    let history = rook_core::paths::home().join("history");
    let _ = editor.load_history(&history);

    loop {
        match editor.readline("› ") {
            Ok(line) if line.trim().is_empty() => continue,
            Ok(line) => {
                let _ = editor.add_history_entry(line.as_str());
                let line = line.trim().to_string();
                if let Some(question) = line.strip_prefix("/btw ") {
                    let provider = rook_llm::from_spec_with(
                        &rook.config.agent.model,
                        rook.config.agent.stream_idle(),
                        rook.config.agent.context_window,
                    )?;
                    runtime.block_on(aside(&rook, provider, session, question.trim()));
                    continue;
                }
                if let Some(command) = line.strip_prefix('/') {
                    match runtime.block_on(dispatch(&rook, &mut session, &shared, command)) {
                        Ok(said) => {
                            print!("{}", said.text);
                            if said.quit {
                                break;
                            }
                        }
                        Err(e) => println!("{e}"),
                    }
                    continue;
                }
                let provider = rook_llm::from_spec_with(
                    &rook.config.agent.model,
                    rook.config.agent.stream_idle(),
                    rook.config.agent.context_window,
                )?;
                runtime.block_on(turn(&rook, provider, session, &shared, &line));
            }
            // Ctrl-C at the prompt clears the line rather than leaving; the
            // reflex from every other REPL is to press it to abandon input.
            Err(ReadlineError::Interrupted) => continue,
            Err(ReadlineError::Eof) => break,
            Err(e) => return Err(e.into()),
        }
    }

    let _ = editor.save_history(&history);
    runtime.block_on(shared.mcp.shutdown());
    runtime.block_on(shared.servers.shutdown());
    drop(provider);
    println!("session {}", rook_store::format_session_id(session));
    Ok(())
}

async fn aside(rook: &Rook, provider: Box<dyn rook_llm::Provider>, session: u128, question: &str) {
    let agent = AgentLoop::new(rook, provider.into(), session);
    let mut out = std::io::stdout();
    print!("\x1b[2m");
    let _ = out.flush();
    let result = agent
        .aside(question, |delta| {
            if let Delta::Text(text) = delta {
                print!("{text}");
                let _ = out.flush();
            }
        })
        .await;
    println!("\x1b[0m");
    if let Err(e) = result {
        println!("{e}");
    }
}

/// What a session keeps between turns, so a turn does not rebuild it.
pub struct Session {
    pub mcp: std::sync::Arc<rook_core::McpSession>,
    pub policy: std::sync::Arc<rook_tools::policy::Policy>,
    pub servers: std::sync::Arc<rook_core::lsp::Servers>,
    /// The policy holds the mode; effort has nowhere else to live and is worth
    /// changing per task, so it sits beside it for the whole session.
    pub effort: std::cell::Cell<rook_llm::Effort>,
    pub yes: bool,
}

async fn turn(
    rook: &Rook,
    provider: Box<dyn rook_llm::Provider>,
    session: u128,
    shared: &Session,
    prompt: &str,
) {
    let mut agent = AgentLoop::new(rook, provider.into(), session);
    // Even under `--yes`: approving every command is not the same as never
    // wanting to be asked which one to run.
    agent.ask_via(std::sync::Arc::new(crate::approve::Terminal));
    if shared.yes {
        agent.allow_everything_not_denied();
    } else {
        agent.policy = shared.policy.clone();
        agent.approver = std::sync::Arc::new(crate::approve::Terminal);
    }
    rook_core::agent::equip(&mut agent, shared.servers.clone(), &shared.mcp);
    agent.effort = shared.effort.get();

    let mut out = std::io::stdout();
    let running = agent.run_with(prompt, |progress| match progress {
        Progress::Delta(Delta::Text(text)) => {
            print!("{text}");
            let _ = out.flush();
        }
        Progress::Delta(Delta::ToolCall(call)) => {
            print!("\n  · {}", call.name);
            let _ = out.flush();
        }
        Progress::Delegated { task, done, total } => {
            let _ = writeln!(out, "\n  [{done}/{total}] {task}");
            let _ = out.flush();
        }
        Progress::ToolDone { failed, .. } => {
            println!("{}", if failed { " ✗" } else { " ✓" });
            let _ = out.flush();
        }
        _ => {}
    });

    // Dropping the future cancels the turn. Whatever the loop already logged
    // stays in the session, so an interrupted turn is still readable afterwards.
    tokio::select! {
        result = running => match result {
            Ok(outcome) => {
                if let Ok(changes) = rook.changes(session, false)
                    && changes.touched() > 0
                {
                    println!("\n\x1b[2m  {} — /diff\x1b[0m", changes.summary());
                }
                for id in &outcome.delegated {
                    println!("\x1b[2m  sub-agent {id}\x1b[0m");
                }
                println!(
                "\n\x1b[2m[{} steps · {} in / {} out{}]\x1b[0m",
                outcome.steps,
                outcome.input_tokens,
                outcome.output_tokens,
                if outcome.compactions > 0 { format!(" · {} compactions", outcome.compactions) } else { String::new() }
                )
            }
            Err(e) => println!("\n{e}"),
        },
        _ = tokio::signal::ctrl_c() => {
            rook.log(session, EventKind::Note, "interrupted", "the user stopped this turn").ok();
            println!("\n\x1b[2m[stopped]\x1b[0m");
        }
    }
}

/// What a slash command produced, and whether it ends the session.
///
/// Text rather than printing, so every front end renders it its own way instead
/// of growing a copy of the command set — which is how the TUI came to have one
/// of the fourteen.
#[derive(Default)]
pub struct Said {
    pub text: String,
    pub quit: bool,
}

pub async fn dispatch(rook: &Rook, session: &mut u128, shared: &Session, command: &str) -> Result<Said> {
    let mcp = &shared.mcp;
    let out = &mut String::new();
    // Blocks, so they can stand where a match arm expects an expression.
    macro_rules! put {
        ($($arg:tt)*) => {{ out.push_str(&format!($($arg)*)) }};
    }
    macro_rules! say {
        ($($arg:tt)*) => {{ put!($($arg)*); out.push('\n') }};
    }
    let (name, rest) = command.split_once(' ').unwrap_or((command, ""));
    let rest = rest.trim();

    match name {
        "quit" | "exit" | "q" => return Ok(Said { text: String::new(), quit: true }),
        "help" | "?" => say!("{HELP}"),

        "mode" if rest.is_empty() => say!("{}", shared.policy.mode().as_str()),
        "mode" => match rook_tools::policy::Mode::parse(rest) {
            Some(mode) => shared.policy.set_mode(mode),
            None => say!("no mode {rest:?} — auto, ask or readonly"),
        },

        "effort" if rest.is_empty() => say!("{}", shared.effort.get().as_str()),
        "effort" => match rook_llm::Effort::parse(rest) {
            Some(effort) => shared.effort.set(effort),
            None => say!("no effort {rest:?} — low, medium, high, xhigh or max"),
        },

        "context" => {
            let window = rest.parse().unwrap_or(128_000);
            let usage = rook.context_usage(*session, window)?;
            let pct = usage.live_tokens as f64 / usage.usable.max(1) as f64 * 100.0;
            say!("~{} of {} usable tokens ({pct:.0}%)", usage.live_tokens, usage.usable);
            if usage.needs_compaction {
                say!("over the compaction threshold — the next turn will compact");
            }
            let rows: Vec<Vec<String>> = usage
                .by_kind
                .iter()
                .map(|(kind, u)| vec![kind.clone(), u.events.to_string(), format!("~{}", u.tokens)])
                .collect();
            put!("{}", fmt::table(&["kind", "events", "tokens"], &rows));
        }

        "skills" if rest.is_empty() => {
            let rows: Vec<Vec<String>> = rook
                .catalog()
                .iter()
                .map(|c| {
                    vec![
                        if c.applicable { "✓".into() } else { "·".into() },
                        c.name.clone(),
                        c.version.clone(),
                        c.description.chars().take(60).collect(),
                    ]
                })
                .collect();
            put!("{}", fmt::table(&["", "name", "version", "description"], &rows));
        }
        "skills" => {
            let resolved = rook.skills().resolve(rest, rook.env())?;
            say!("{}", resolved.body);
        }

        "goal" if rest.is_empty() => match rook.goal(*session)? {
            Some(goal) => say!("{goal}"),
            None => say!("no goal set — /goal <text> to set one"),
        },
        "goal" => {
            rook.set_goal(*session, rest)?;
            say!("goal set");
        }

        "diff" => {
            let changes = rook.changes(*session, false)?;
            if changes.touched() == 0 {
                say!("nothing changed on disk yet");
            }
            for file in changes.files.iter().filter(|f| f.change != rook_core::changes::Change::Unchanged) {
                say!("{} {}  +{} -{}", file.change.sigil(), file.path, file.lines_added, file.lines_removed);
            }
        }

        "search" if !rest.is_empty() => {
            let found = rook.search(rest, &Default::default())?;
            if found.hits.is_empty() {
                say!("nothing matched in {} object(s)", found.objects_scanned);
            }
            for hit in found.hits.iter().take(10) {
                say!("\x1b[2m{} {}\x1b[0m", fmt::hit_where(hit), hit.kind);
                say!("  {}", hit.snippet);
            }
        }

        "memory" => {
            let book = rook.memory()?;
            let workspace = rook.workspace.display().to_string();
            let facts: Vec<_> = if rest.is_empty() {
                book.in_scope(&workspace).cloned().collect()
            } else {
                rook_core::memory::search(book.in_scope(&workspace), rest)
                    .into_iter()
                    .map(|h| h.fact)
                    .collect()
            };
            if facts.is_empty() {
                say!("nothing remembered yet");
            }
            for fact in facts {
                let pin = if fact.pinned { "* " } else { "  " };
                say!("{pin}[{}] {}", fact.id, fact.text);
            }
        }

        "session" => {
            let meta = rook.store.get_session(*session)?.context("session vanished")?;
            say!("{}", rook_store::format_session_id(meta.id));
            say!("{} events · {} in / {} out tokens", meta.event_count, meta.tokens_in, meta.tokens_out);
            if let Some(goal) = rook.goal(*session)? {
                say!("goal: {goal}");
            }
            if let Some(parent) = meta.parent {
                say!("forked from {}", rook_store::format_session_id(parent));
            }
        }

        "mcp" => {
            if mcp.servers.is_empty() {
                say!("no tool servers connected");
            }
            for (server, tools) in &mcp.servers {
                say!("{} — {} tool(s)", server.name(), tools.len());
            }
        }

        "undo" => {
            let seq = last_user_turn(rook, *session)?.context("nothing to undo yet")?;
            rewind_to(rook, session, seq)?;
        }
        "rewind" => {
            let seq = rest.parse().context("usage: /rewind <seq>, from `rook session show`")?;
            rewind_to(rook, session, seq)?;
        }

        "new" => {
            let title = if rest.is_empty() { "chat" } else { rest };
            *session = rook.start_session(title)?;
            say!("session {}", rook_store::format_session_id(*session));
        }

        other => say!("unknown command /{other} — /help lists them"),
    }
    Ok(Said { text: std::mem::take(out), quit: false })
}

fn rewind_to(rook: &Rook, session: &mut u128, seq: u64) -> Result<()> {
    let report = rook.rewind(*session, seq, true)?;
    *session = crate::session_id(&report.session)?;
    println!(
        "rewound to #{seq}: {} events kept, {} file(s) restored, {} removed",
        report.events_kept, report.files_restored, report.files_removed
    );
    Ok(())
}

/// The sequence number of the most recent user message, which is where an undo
/// should land: before the exchange the user is unhappy with, not inside it.
fn last_user_turn(rook: &Rook, session: u128) -> Result<Option<u64>> {
    let entries: Vec<TranscriptEntry> = rook.transcript(session, 0, usize::MAX, 1)?;
    Ok(entries.iter().rev().find(|e| e.kind == "user").map(|e| e.seq))
}
