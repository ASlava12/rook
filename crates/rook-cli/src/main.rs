//! `rook` — the command line and TUI front end.

mod approve;
mod chat;
mod fmt;
mod source;
mod tui;

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use crate::source::Source;
use rook_core::SessionSummary;
use rook_core::agent::Progress;
use rook_core::{AGENT_VERSION, Rook};
use rook_skills::SkillCard;
use rook_store::StoreStats;
use rook_store::{Kind, ObjectId};

#[derive(Parser)]
#[command(
    name = "rook",
    version = AGENT_VERSION,
    about = "A compact autonomous agent with an inspectable memory",
    long_about = "Rook keeps everything it does in a content-addressed local store.\n\
                  Every subcommand under `store`, `session` and `skills` exists so that\n\
                  memory is something you can read, diff and roll back — not a black box."
)]
struct Cli {
    /// Workspace root. Defaults to the current directory.
    #[arg(long, short = 'C', global = true)]
    workspace: Option<PathBuf>,
    /// Emit JSON instead of tables.
    #[arg(long, global = true)]
    json: bool,
    /// Approve everything the deny list does not forbid, without asking.
    #[arg(long, short = 'y', global = true)]
    yes: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Create the store and the config file, and say where skills go.
    Init,
    /// Report what Rook detected about this machine and what it means for skills.
    Doctor,
    /// Talk to the agent interactively.
    Chat {
        /// Continue an existing session instead of starting one.
        #[arg(long)]
        session: Option<String>,
    },
    /// Run a single turn against the configured model.
    Run {
        prompt: Vec<String>,
        /// Continue an existing session instead of starting one.
        #[arg(long)]
        session: Option<String>,
    },
    /// Browse the store, sessions and skills in a read-only terminal UI.
    Tui,
    /// List the models the configured provider says it can serve.
    Models,
    /// Speak the Agent Client Protocol on stdio, for editors.
    Acp,
    /// Start the HTTP backend and web UI.
    Serve {
        #[arg(long)]
        port: Option<u16>,
    },
    /// Inspect and maintain the object store.
    #[command(subcommand)]
    Store(StoreCmd),
    /// Inspect session transcripts.
    #[command(subcommand)]
    Session(SessionCmd),
    /// List, author and version skills.
    #[command(subcommand)]
    Skills(SkillCmd),
    /// Snapshot and restore parts of the workspace.
    #[command(subcommand)]
    Checkpoint(CheckpointCmd),
    /// Inspect the external tool servers from `[[mcp]]` in config.toml.
    #[command(subcommand)]
    Mcp(McpCmd),
    /// Read, edit and audit what the agent remembers.
    #[command(subcommand)]
    Memory(MemoryCmd),
    /// Ask the language servers what the agent would ask them.
    #[command(subcommand)]
    Lsp(LspCmd),
    /// Search everything the agent has said, read and run.
    Search {
        query: Vec<String>,
        /// Only this session.
        #[arg(long)]
        session: Option<String>,
        /// Skip file contents, which are most of a store by size.
        #[arg(long)]
        conversation: bool,
        #[arg(long, default_value_t = 40)]
        limit: usize,
    },
}

#[derive(Subcommand)]
enum LspCmd {
    /// Which language servers apply here.
    Servers,
    /// What the type checker thinks is wrong with a file.
    Diagnostics { path: String },
    /// Where a name used in a file is defined.
    Definition { path: String, symbol: String },
    /// What refers to a name, as the type checker sees it.
    References { path: String, symbol: String },
    /// Find a symbol anywhere in the workspace.
    Symbol { query: String },
}

#[derive(Subcommand)]
enum MemoryCmd {
    /// Everything remembered that applies here.
    Ls {
        /// Include facts scoped to other workspaces.
        #[arg(long)]
        all: bool,
    },
    /// Rank memory against a query, showing why each result matched.
    Search { query: Vec<String> },
    /// Teach it something.
    Add {
        text: Vec<String>,
        #[arg(long)]
        tag: Vec<String>,
        /// Applies everywhere, not just this workspace.
        #[arg(long)]
        global: bool,
        /// Always keep in context, regardless of relevance.
        #[arg(long)]
        pin: bool,
    },
    /// Drop a fact by id or exact text.
    Rm { id: String },
    /// Every recorded state of memory.
    History,
    /// What changed between two recorded states.
    Diff { a: String, b: String },
    /// What has been learned or forgotten since a number of days ago.
    Since {
        #[arg(default_value_t = 1)]
        days: i64,
    },
}

#[derive(Subcommand)]
enum McpCmd {
    /// Connect every configured server and report what it offers.
    Ls,
    /// List one server's tools with their schemas.
    Tools { server: String },
    /// Call a tool directly, without a model in the loop.
    Call {
        server: String,
        tool: String,
        /// Arguments as JSON.
        #[arg(default_value = "{}")]
        args: String,
    },
}

#[derive(Subcommand)]
enum StoreCmd {
    /// Size, compression ratio and per-kind breakdown.
    Stat,
    /// List objects.
    Ls {
        #[arg(long)]
        kind: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Print one object by hash prefix.
    Cat { id: String },
    /// List refs, optionally under a prefix.
    Refs { prefix: Option<String> },
    /// Collect unreachable objects.
    Gc {
        #[arg(long)]
        dry_run: bool,
    },
    /// Apply the retention policy.
    Prune {
        #[arg(long)]
        dry_run: bool,
    },
    /// Prune, collect, enforce the size budget and retrain dictionaries.
    Maintain {
        #[arg(long)]
        dry_run: bool,
    },
    /// Re-read and re-hash every object.
    Verify,
    /// Train compression dictionaries from what is already stored.
    Train,
}

#[derive(Subcommand)]
enum SessionCmd {
    Ls,
    /// Print a session transcript.
    Show {
        id: String,
        #[arg(long, default_value_t = 0)]
        from: u64,
        #[arg(long, default_value_t = 200)]
        limit: usize,
        /// Bytes of each payload to show before eliding.
        #[arg(long, default_value_t = 4096)]
        max_body: usize,
    },
    Rm {
        id: String,
    },
    /// Show what a session is costing in context, and of what.
    Context {
        id: String,
        /// Model context window to measure against.
        #[arg(long, default_value_t = 128_000)]
        window: usize,
    },
    /// What a session changed on disk, from its own checkpoints.
    Diff {
        id: String,
        /// Names and counts only.
        #[arg(long)]
        stat: bool,
    },
    /// Show or set what a session is for.
    Goal {
        id: String,
        /// Leave empty to show the current goal.
        goal: Vec<String>,
    },
    /// Fork a session at a sequence number, keeping the original intact.
    Fork {
        id: String,
        #[arg(long)]
        at: u64,
    },
    /// Fork at a sequence number and put the workspace files back with it.
    Rewind {
        id: String,
        #[arg(long)]
        to: u64,
        /// Rewind the conversation only, leaving files as they are.
        #[arg(long)]
        keep_files: bool,
    },
}

#[derive(Subcommand)]
enum SkillCmd {
    /// List skills that apply here.
    Ls {
        /// Include skills whose requirements do not match this machine.
        #[arg(long)]
        all: bool,
    },
    /// Print a skill's resolved body for this environment.
    Show { name: String },
    /// Explain which version was chosen and why the others were not.
    Why { name: String },
    /// Scaffold a new skill.
    New {
        name: String,
        #[arg(long, short)]
        description: String,
    },
    /// Record the skill's current content as a new version in the store.
    Capture {
        name: String,
        #[arg(long, short)]
        message: Option<String>,
    },
    /// Show a skill's captured versions.
    History { name: String },
    /// Diff two captures by object id.
    Diff { a: String, b: String },
    /// Restore a captured version over the skill's directory.
    Rollback { name: String, object: String },
}

#[derive(Subcommand)]
enum CheckpointCmd {
    Create {
        name: String,
        #[arg(long)]
        path: Option<PathBuf>,
    },
    Ls,
    Restore {
        object: String,
        #[arg(long)]
        to: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // Defaults if the config is unreadable: logging must not be what reports a
    // broken config, and the command about to run will report it properly.
    rook_core::telemetry::init(&rook_core::Config::load().unwrap_or_default().telemetry);

    match cli.command {
        // Bare `rook` opens a conversation: talking to the agent is the point,
        // and every comparable tool starts there.
        None => chat::run(cli.workspace, None, cli.yes),
        Some(Command::Tui) => tui::run(Rook::open(cli.workspace)?, cli.yes),
        Some(Command::Init) => cmd_init(cli.workspace),
        Some(Command::Doctor) => cmd_doctor(&Rook::open(cli.workspace)?, cli.json),
        Some(Command::Chat { session }) => chat::run(cli.workspace, session, cli.yes),
        Some(Command::Run { prompt, session }) => cmd_run(cli.workspace, prompt, session, cli.yes),
        Some(Command::Models) => cmd_models(cli.workspace, cli.json),
        Some(Command::Acp) => cmd_acp(cli.workspace),
        Some(Command::Serve { port }) => cmd_serve(port),
        Some(Command::Store(c)) => cmd_store(&Source::open(cli.workspace)?, c, cli.json),
        Some(Command::Session(c)) => cmd_session(&Source::open(cli.workspace)?, c, cli.json),
        Some(Command::Skills(c)) => cmd_skills(&Source::open(cli.workspace)?, c, cli.json),
        Some(Command::Checkpoint(c)) => cmd_checkpoint(&Rook::open(cli.workspace)?, c, cli.json),
        Some(Command::Mcp(c)) => cmd_mcp(cli.workspace, c, cli.json),
        Some(Command::Memory(c)) => cmd_memory(&Rook::open(cli.workspace)?, c, cli.json),
        Some(Command::Lsp(c)) => cmd_lsp(cli.workspace, c, cli.json),
        Some(Command::Search { query, session, conversation, limit }) => {
            cmd_search(&Rook::open(cli.workspace)?, &query.join(" "), session, conversation, limit, cli.json)
        }
    }
}

fn cmd_init(workspace: Option<PathBuf>) -> Result<()> {
    let rook = Rook::open(workspace)?;
    rook.config.save().ok();
    println!("store       {}", rook.store.root().display());
    println!("config      {}", rook_core::paths::config_file().display());
    println!("skills      {}", rook_core::paths::user_skills_dir().display());
    println!("model       {}", rook.config.agent.model);
    println!();
    println!("Next: `rook skills new my-skill -d \"...\"`, then `rook doctor`.");
    Ok(())
}

fn cmd_doctor(rook: &Rook, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(rook.env())?);
        return Ok(());
    }
    let env = rook.env();
    println!("rook {AGENT_VERSION}");
    println!("os        {} ({} userland)", env.os, env.userland);
    println!("arch      {}", env.arch);
    println!("workspace {}", rook.workspace.display());
    println!("store     {}", rook.store.root().display());
    println!();
    println!("toolchains detected:");
    if env.languages.is_empty() {
        println!("  (none)");
    }
    for (k, v) in &env.languages {
        println!("  {k:<10} {v}");
    }
    println!();
    println!("tools detected:");
    if env.tools.is_empty() {
        println!("  (none)");
    }
    for (k, v) in &env.tools {
        println!("  {k:<10} {v}");
    }

    println!();
    println!("model:");
    match probe_provider(rook) {
        Ok(note) => println!("  {note}"),
        Err(e) => {
            // Indented as one block: the advice belongs to the failure above it,
            // and doctor is read top to bottom.
            println!("  {}", rook.config.agent.model);
            for line in e.to_string().lines() {
                println!("  {line}");
            }
        }
    }

    let servers =
        if rook.config.lsp.is_empty() { rook_core::lsp::detected() } else { rook.config.lsp.clone() };
    println!();
    print!("language servers: ");
    if servers.is_empty() {
        println!("none found on PATH (rust-analyzer, gopls, clangd, …)");
    } else {
        let names: Vec<String> = servers.iter().map(|s| format!("{} ({})", s.language, s.command)).collect();
        println!("{}", names.join(", "));
    }

    let cards = rook.catalog();
    let (ok, blocked): (Vec<_>, Vec<_>) = cards.iter().partition(|c| c.applicable);
    println!();
    println!("skills: {} usable, {} blocked here", ok.len(), blocked.len());
    // The built-in ones live next to the binary, which a plain `cargo build`
    // does not put them there — the commonest reason a fresh install has none,
    // and invisible from a count of zero.
    if cards.is_empty() && rook_core::paths::builtin_skills_dir().is_none() {
        println!("  none are installed next to {}", std::env::current_exe().unwrap_or_default().display());
        println!("  `cargo xtask dist` packages them there, or set ROOK_BUILTIN_SKILLS");
        println!("  your own go in {}", rook_core::paths::user_skills_dir().display());
    }
    for c in blocked {
        println!("  {} — {}", c.name, c.mismatches.join("; "));
    }
    if !rook.skill_errors.is_empty() {
        println!();
        println!("skills that failed to load:");
        for e in &rook.skill_errors {
            println!("  {e}");
        }
    }
    Ok(())
}

/// Ask the endpoint what it serves, which answers "is it up" and "is the model
/// configured actually there" in one round trip.
fn probe_provider(rook: &Rook) -> Result<String> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let (_, configured) = rook_llm::split_spec(&rook.config.agent.model);
    let provider = provider(rook)?;
    let models = runtime.block_on(provider.models())?;

    let spec = &rook.config.agent.model;
    let window = provider.context_window();
    let reported = models.iter().find(|m| m.id == configured).and_then(|m| m.context_window);

    if models.is_empty() {
        return Ok(format!("{spec} — reachable, {window} token window assumed"));
    }
    if !models.iter().any(|m| m.id == configured) {
        return Ok(format!(
            "{spec} — reachable, but {configured:?} is not among the {} it offers (`rook models`)",
            models.len()
        ));
    }

    let mut note = format!("{spec} — reachable, {} model(s) offered, {window} token window", models.len());
    // The endpoint knowing better than our default is common for self-hosted
    // models, and silently budgeting against the wrong number wastes most of
    // the window or overruns it.
    if let Some(reported) = reported.filter(|r| *r != window) {
        note.push_str(&format!(
            "\n  the endpoint reports {reported}; set `context_window = {reported}` under [agent] to use it"
        ));
    }
    Ok(note)
}

fn cmd_run(
    workspace: Option<PathBuf>,
    prompt: Vec<String>,
    session: Option<String>,
    yes: bool,
) -> Result<()> {
    let prompt = prompt.join(" ");
    if prompt.trim().is_empty() {
        bail!("nothing to do: pass a prompt");
    }
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async move {
        let rook = Rook::open(workspace)?;
        let provider = rook_llm::from_spec_with(
            &rook.config.agent.model,
            rook.config.agent.stream_idle(),
            rook.config.agent.context_window,
        )
        .with_context(|| format!("configuring model {:?}", rook.config.agent.model))?;
        let session = match session {
            Some(s) => session_id(&s)?,
            None => rook.start_session(&first_line(&prompt))?,
        };
        let mut agent = rook_core::agent::AgentLoop::new(&rook, provider.into(), session);
        // `run` is scripted more often than watched, so it refuses what it cannot
        // get approved rather than prompting into a pipe.
        if yes {
            agent.allow_everything_not_denied();
        }
        let mcp = rook.connect_mcp().await;
        for (server, tools) in &mcp.servers {
            agent.tools.register_server(server.clone(), tools.clone());
        }
        for (name, error) in &mcp.failures {
            eprintln!("mcp {name}: {error}");
        }
        let mut out = std::io::stdout();
        let outcome = agent
            .run_with(&prompt, |progress| match progress {
                Progress::Delta(rook_llm::Delta::Text(text)) => {
                    let _ = write!(out, "{text}");
                    let _ = out.flush();
                }
                Progress::Delta(rook_llm::Delta::ToolCall(call)) => {
                    let _ = write!(out, "\n  · {}({})", call.name, compact(&call.arguments));
                    let _ = out.flush();
                }
                Progress::Delegated { task, done, total } => {
                    let _ = writeln!(out, "\n  [{done}/{total}] {task}");
                    let _ = out.flush();
                }
                Progress::ToolDone { failed, .. } => {
                    let _ = writeln!(out, "{}", if failed { " ✗" } else { " ✓" });
                    let _ = out.flush();
                }
                _ => {}
            })
            .await?;
        println!();
        mcp.shutdown().await;
        for id in &outcome.delegated {
            eprintln!("sub-agent {id} — `rook session show {id}` for its detail");
        }
        if let Ok(changes) = rook.changes(session, false)
            && changes.touched() > 0
        {
            eprintln!(
                "{} — `rook session diff {}`",
                changes.summary(),
                rook_store::format_session_id(session)
            );
        }
        eprintln!(
            "\n[session {} · {} steps · {} in / {} out tokens{} · {} tool calls{}]",
            rook_store::format_session_id(session),
            outcome.steps,
            outcome.input_tokens,
            outcome.output_tokens,
            cached(outcome.cached_tokens),
            outcome.tools_called.len(),
            if outcome.compactions > 0 {
                format!(" · {} compactions", outcome.compactions)
            } else {
                String::new()
            }
        );
        anyhow::Ok(())
    })
}

/// One-line form of tool arguments, for the progress line.
fn compact(args: &serde_json::Value) -> String {
    let text = args.to_string();
    if text.len() <= 80 {
        return text;
    }
    let cut = (0..=80).rev().find(|i| text.is_char_boundary(*i)).unwrap_or(0);
    format!("{}…", &text[..cut])
}

/// Cache hits only matter when there are any; a constant "0 cached" is noise.
pub fn cached(tokens: u32) -> String {
    if tokens == 0 { String::new() } else { format!(" ({tokens} cached)") }
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("session").chars().take(72).collect()
}

fn cmd_models(workspace: Option<PathBuf>, json: bool) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async move {
        let rook = Rook::open(workspace)?;
        let (_, configured) = rook_llm::split_spec(&rook.config.agent.model);
        let models = provider(&rook)?.models().await?;
        if json {
            println!("{}", serde_json::to_string_pretty(&models)?);
            return anyhow::Ok(());
        }
        if models.is_empty() {
            println!("the endpoint does not list its models");
            return anyhow::Ok(());
        }
        let rows: Vec<Vec<String>> = models
            .iter()
            .map(|m| {
                vec![
                    if m.id == configured { "▸".into() } else { " ".into() },
                    m.id.clone(),
                    m.context_window.map(|w| format!("{w}")).unwrap_or_default(),
                    m.owned_by.clone().unwrap_or_default(),
                ]
            })
            .collect();
        print!("{}", fmt::table(&["", "model", "context", "owner"], &rows));
        if !models.iter().any(|m| m.id == configured) {
            println!("\n{configured:?} is configured but not offered here");
        }
        anyhow::Ok(())
    })
}

fn provider(rook: &Rook) -> Result<Box<dyn rook_llm::Provider>> {
    rook_llm::from_spec_with(
        &rook.config.agent.model,
        rook.config.agent.stream_idle(),
        rook.config.agent.context_window,
    )
    .with_context(|| format!("configuring model {:?}", rook.config.agent.model))
}

fn cmd_acp(workspace: Option<PathBuf>) -> Result<()> {
    // stdout carries the protocol, so logs must not: a stray line there is an
    // unparsable message to the editor.
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async move {
        let rook = Rook::open(workspace)?;
        rook_acp::serve_stdio(rook).await?;
        anyhow::Ok(())
    })
}

fn cmd_serve(port: Option<u16>) -> Result<()> {
    let mut config = rook_core::Config::load()?;
    if let Some(p) = port {
        config.server.port = p;
    }
    bail!(
        "the daemon lives in its own binary: run `rookd --port {}`.\n\
         It is separate so an editor integration or a headless box can run the backend \
         without pulling in the TUI.",
        config.server.port
    )
}

fn show_stats(s: &StoreStats, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&s)?);
        return Ok(());
    }
    println!(
        "objects        {:>12}  ({} inline, {} external)",
        s.objects, s.inline_objects, s.external_objects
    );
    println!("logical size   {:>12}", fmt::bytes(s.bytes_raw));
    println!(
        "stored size    {:>12}  ({:.1}x compression)",
        fmt::bytes(s.bytes_stored),
        s.compression_ratio()
    );
    println!("saved by dedup {:>12}", fmt::bytes(s.dedup_saved_hint));
    println!(
        "on disk        {:>12}  (index {}, objects {})",
        fmt::bytes(s.disk_bytes()),
        fmt::bytes(s.index_bytes),
        fmt::bytes(s.external_bytes)
    );
    println!("sessions       {:>12}", s.sessions);
    println!("events         {:>12}", s.events);
    println!("refs           {:>12}", s.refs);
    if !s.dictionaries.is_empty() {
        let d: Vec<String> = s.dictionaries.iter().map(|(k, v)| format!("{k} {}", fmt::bytes(*v))).collect();
        println!("dictionaries   {}", d.join(", "));
    } else {
        println!("dictionaries   none yet — run `rook store train` once you have some history");
    }
    println!();
    let max = s.per_kind.iter().map(|k| k.bytes_stored).max().unwrap_or(0);
    let rows: Vec<Vec<String>> = s
        .per_kind
        .iter()
        .map(|k| {
            vec![
                k.kind.clone(),
                k.objects.to_string(),
                fmt::bytes(k.bytes_raw),
                fmt::bytes(k.bytes_stored),
                format!("{:.1}x", k.ratio()),
                fmt::bar(k.bytes_stored, max, 20),
            ]
        })
        .collect();
    print!("{}", fmt::table(&["kind", "objects", "logical", "stored", "ratio", ""], &rows));
    Ok(())
}

fn show_sessions(sessions: &[SessionSummary], json: bool) -> Result<()> {
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
                    s.meta.title.chars().take(40).collect::<String>()
                ),
                s.meta.event_count.to_string(),
                format!("{}/{}", s.meta.tokens_in, s.meta.tokens_out),
                fmt::ago(s.meta.updated_at),
                s.goal.clone().unwrap_or_else(|| s.meta.workspace.clone()),
            ]
        })
        .collect();
    print!("{}", fmt::table(&["id", "title", "events", "tok in/out", "updated", "goal / workspace"], &rows));
    Ok(())
}

fn cmd_store(source: &Source, cmd: StoreCmd, json: bool) -> Result<()> {
    // Routed before the store is opened, because the daemon may be holding it.
    if let StoreCmd::Stat = cmd {
        return show_stats(&source.stats()?, json);
    }
    let rook = source.local()?;
    match cmd {
        StoreCmd::Stat => unreachable!("routed above"),
        StoreCmd::Ls { kind, limit } => {
            let want = kind.as_deref().map(parse_kind).transpose()?;
            let objects = rook.store.list_objects(want, limit)?;
            if json {
                let items: Vec<_> = objects
                    .iter()
                    .map(|(id, m)| {
                        serde_json::json!({
                            "id": id.to_hex(), "kind": Kind::from_u8(m.kind).as_str(),
                            "raw": m.size_raw, "stored": m.size_stored, "created_at": m.created_at,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&items)?);
                return Ok(());
            }
            let rows: Vec<Vec<String>> = objects
                .iter()
                .map(|(id, m)| {
                    vec![
                        id.short(),
                        Kind::from_u8(m.kind).as_str().to_string(),
                        fmt::bytes(m.size_raw),
                        fmt::bytes(m.size_stored),
                        if m.external { "file".into() } else { "inline".into() },
                        fmt::timestamp(m.created_at),
                    ]
                })
                .collect();
            print!("{}", fmt::table(&["id", "kind", "logical", "stored", "where", "created"], &rows));
        }
        StoreCmd::Cat { id } => {
            let object = rook
                .store
                .resolve_prefix(&id)?
                .with_context(|| format!("no object matches {id:?} (or the prefix is ambiguous)"))?;
            let data = rook.store.get(&object)?;
            use std::io::Write;
            std::io::stdout().write_all(&data)?;
        }
        StoreCmd::Refs { prefix } => {
            let refs = rook.store.list_refs(prefix.as_deref().unwrap_or(""))?;
            if json {
                let items: Vec<_> =
                    refs.iter().map(|(n, id)| serde_json::json!({"ref": n, "object": id.to_hex()})).collect();
                println!("{}", serde_json::to_string_pretty(&items)?);
                return Ok(());
            }
            let rows: Vec<Vec<String>> = refs.iter().map(|(n, id)| vec![n.clone(), id.short()]).collect();
            print!("{}", fmt::table(&["ref", "object"], &rows));
        }
        StoreCmd::Gc { dry_run } => {
            let report = rook.store.gc(&rook_store::GcOptions {
                expand: Some(&rook_core::fileset::gc_expander),
                dry_run,
                ..Default::default()
            })?;
            println!(
                "{}scanned {}, reachable {}, collected {} ({} freed), orphan files {}",
                if dry_run { "[dry run] " } else { "" },
                report.scanned,
                report.reachable,
                report.collected,
                fmt::bytes(report.bytes_freed),
                report.orphan_files_removed
            );
        }
        StoreCmd::Prune { dry_run } => {
            let report = rook.store.prune(&rook.config.storage.retention, dry_run)?;
            println!(
                "{}sessions deleted {}, events deleted {}, protected {}",
                if dry_run { "[dry run] " } else { "" },
                report.sessions_deleted,
                report.events_deleted,
                report.protected
            );
            if !dry_run {
                println!("run `rook store gc` to reclaim the space");
            }
            if rook.config.storage.retention.max_total_bytes.is_some() {
                println!("`store maintain` also enforces the size budget, which needs gc to measure");
            }
        }
        StoreCmd::Maintain { dry_run } => {
            let report = rook.maintenance(dry_run)?;
            let tag = if dry_run { "[dry run] " } else { "" };
            println!(
                "{tag}sessions deleted {}, events deleted {}, protected {}",
                report.prune.sessions_deleted, report.prune.events_deleted, report.prune.protected
            );
            println!("{tag}collected {} ({} freed)", report.gc.collected, fmt::bytes(report.gc.bytes_freed));
            for (kind, samples) in &report.dictionaries_trained {
                println!("trained {kind} dictionary from {samples} objects");
            }
            match report.over_budget_by {
                0 => println!("stored {}", fmt::bytes(rook.content_bytes()?)),
                over => {
                    let policy = &rook.config.storage.retention;
                    let left = rook.store.list_sessions()?;
                    let protected = left
                        .iter()
                        .filter(|s| s.tags.iter().any(|t| policy.protect_tags.contains(t)))
                        .count();
                    println!(
                        "still {} over the {} budget",
                        fmt::bytes(over),
                        fmt::bytes(policy.max_total_bytes.unwrap_or(0))
                    );
                    println!(
                        "  {} session(s) remain, {protected} protected; the rest is checkpoints, \
                         skill versions and memory, which retention does not delete",
                        left.len()
                    );
                }
            }
        }
        StoreCmd::Verify => {
            let bad = rook.store.verify()?;
            if bad.is_empty() {
                println!("all objects verified");
            } else {
                for (id, reason) in &bad {
                    println!("{}  {reason}", id.short());
                }
                bail!("{} object(s) failed verification", bad.len());
            }
        }
        StoreCmd::Train => {
            let trained = rook.store.train_dictionaries(
                rook.config.storage.train_dictionaries_after,
                rook.config.storage.dictionary_bytes,
            )?;
            if trained.is_empty() {
                println!("not enough samples yet — dictionaries need at least 32 objects of a kind");
            }
            for (kind, size) in trained {
                println!("trained {kind}: {}", fmt::bytes(size as u64));
            }
            println!("existing objects keep their old encoding; new ones use the dictionary");
        }
    }
    Ok(())
}

fn parse_kind(s: &str) -> Result<Kind> {
    Ok(match s {
        "message" => Kind::Message,
        "tool-result" | "tool_result" => Kind::ToolResult,
        "file" => Kind::FileBlob,
        "skill" => Kind::Skill,
        "memory" => Kind::Memory,
        "snapshot" => Kind::Snapshot,
        "other" => Kind::Other,
        other => bail!(
            "unknown kind {other:?}; expected one of message, tool-result, file, skill, memory, snapshot, other"
        ),
    })
}

fn cmd_session(source: &Source, cmd: SessionCmd, json: bool) -> Result<()> {
    if let SessionCmd::Ls = cmd {
        return show_sessions(&source.sessions()?, json);
    }
    let rook = source.local()?;
    match cmd {
        SessionCmd::Ls => unreachable!("routed above"),
        SessionCmd::Show { id, from, limit, max_body } => {
            let entries = rook.transcript(session_id(&id)?, from, limit, max_body)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&entries)?);
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
        }
        SessionCmd::Context { id, window } => {
            let usage = rook.context_usage(session_id(&id)?, window)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&usage)?);
                return Ok(());
            }
            let pct = usage.live_tokens as f64 / usage.usable.max(1) as f64 * 100.0;
            println!(
                "window       {:>9}  (usable {}, compacts at {})",
                usage.window, usage.usable, usage.compact_at
            );
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
        }
        SessionCmd::Diff { id, stat } => {
            let changes = rook.changes(session_id(&id)?, !stat)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&changes)?);
                return Ok(());
            }
            if changes.touched() == 0 {
                println!("this session changed nothing on disk");
                return Ok(());
            }
            for file in changes.files.iter().filter(|f| f.change != rook_core::changes::Change::Unchanged) {
                println!(
                    "{} {}  +{} -{}",
                    file.change.sigil(),
                    file.path,
                    file.lines_added,
                    file.lines_removed
                );
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
            println!("\n{}", changes.summary());
        }
        SessionCmd::Goal { id, goal } => {
            let session = session_id(&id)?;
            if goal.is_empty() {
                match rook.goal(session)? {
                    Some(goal) => println!("{goal}"),
                    None => println!("no goal set for this session"),
                }
            } else {
                rook.set_goal(session, &goal.join(" "))?;
                println!("goal set");
            }
        }
        SessionCmd::Fork { id, at } => {
            let source = session_id(&id)?;
            let meta = rook.store.get_session(source)?.context("no such session")?;
            let forked = rook.store.fork_session(
                source,
                rook_store::new_session_id(),
                at,
                &format!("{} @{at}", meta.title),
            )?;
            println!(
                "forked {} events into {}",
                forked.event_count,
                rook_store::format_session_id(forked.id)
            );
        }
        SessionCmd::Rewind { id, to, keep_files } => {
            let report = rook.rewind(session_id(&id)?, to, !keep_files)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
                return Ok(());
            }
            println!("rewound to #{to} as session {}", report.session);
            println!("  {} events kept, parent {} left intact", report.events_kept, report.parent);
            if !keep_files {
                println!(
                    "  {} checkpoint(s) applied: {} file(s) restored, {} removed",
                    report.checkpoints_applied, report.files_restored, report.files_removed
                );
            }
        }
        SessionCmd::Rm { id } => {
            let removed = rook.store.delete_session(session_id(&id)?)?;
            println!("removed session with {removed} events; run `rook store gc` to reclaim space");
        }
    }
    Ok(())
}

fn show_skills(catalog: &[SkillCard], all: bool, json: bool) -> Result<()> {
    let cards: Vec<_> = catalog.iter().filter(|c| all || c.applicable).collect();
    if json {
        println!("{}", serde_json::to_string_pretty(&cards)?);
        return Ok(());
    }
    let rows: Vec<Vec<String>> = cards
        .iter()
        .map(|c| {
            vec![
                if c.applicable { "✓".into() } else { "·".into() },
                c.name.clone(),
                c.version.clone(),
                c.source.clone(),
                format!("~{}", c.body_tokens),
                c.description.chars().take(60).collect(),
            ]
        })
        .collect();
    print!("{}", fmt::table(&["", "name", "version", "source", "tokens", "description"], &rows));
    if !all {
        println!(
            "\n(`--all` also shows skills blocked by this environment; `rook skills why <name>` explains one)"
        );
    }
    Ok(())
}

fn cmd_skills(source: &Source, cmd: SkillCmd, json: bool) -> Result<()> {
    if let SkillCmd::Ls { all } = cmd {
        return show_skills(&source.catalog()?, all, json);
    }
    let rook = source.local()?;
    match cmd {
        SkillCmd::Ls { .. } => unreachable!("routed above"),
        SkillCmd::Show { name } => {
            let resolved = rook.skills().resolve(&name, rook.env())?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "name": resolved.skill.manifest.name,
                        "version": resolved.skill.version().to_string(),
                        "source": resolved.skill.source.label(),
                        "dir": resolved.skill.dir,
                        "variant": resolved.variant.as_ref().map(|v| v.body.clone()),
                        "body": resolved.body,
                    }))?
                );
                return Ok(());
            }
            println!(
                "# {} {} ({})",
                resolved.skill.manifest.name,
                resolved.skill.version(),
                resolved.skill.source.label()
            );
            if let Some(v) = &resolved.variant {
                println!("variant: {} — selected for this environment", v.body.display());
            }
            println!("dir: {}\n", resolved.skill.dir.display());
            println!("{}", resolved.body);
        }
        SkillCmd::Why { name } => {
            let skills = rook.skills();
            let versions = skills.versions_of(&name);
            if versions.is_empty() {
                bail!("no skill named {name:?}");
            }
            println!(
                "environment: {} / {} / {} userland",
                rook.env().os,
                rook.env().arch,
                rook.env().userland
            );
            println!();
            for skill in &versions {
                let mismatches = skill.manifest.requires.check(rook.env());
                if mismatches.is_empty() {
                    println!("  ✓ {} [{}] applies", skill.id(), skill.source.label());
                } else {
                    println!("  ✗ {} [{}]", skill.id(), skill.source.label());
                    for m in mismatches {
                        println!("      {m}");
                    }
                }
            }
            println!();
            match rook.skills().resolve(&name, rook.env()) {
                Ok(r) => println!("chosen: {} [{}]", r.skill.id(), r.skill.source.label()),
                Err(e) => println!("chosen: none — {e}"),
            }
        }
        SkillCmd::New { name, description } => {
            let dir = rook.new_skill(&name, &description)?;
            println!("created {}", dir.join("SKILL.md").display());
            println!("edit it, then `rook skills capture {name} -m \"first version\"`");
        }
        SkillCmd::Capture { name, message } => {
            let (set, id) = rook.capture_skill(&name, message)?;
            println!(
                "captured {} v{} — {} file{}, {} → object {}",
                set.name,
                set.version,
                set.files.len(),
                if set.files.len() == 1 { "" } else { "s" },
                fmt::bytes(set.total_bytes),
                id.short()
            );
        }
        SkillCmd::History { name } => {
            let history = rook.skill_history(&name)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&history)?);
                return Ok(());
            }
            if history.is_empty() {
                println!("no captures yet — `rook skills capture {name}`");
                return Ok(());
            }
            let rows: Vec<Vec<String>> = history
                .iter()
                .map(|h| {
                    vec![
                        h.object.chars().take(12).collect(),
                        h.version.clone(),
                        fmt::timestamp(h.captured_at),
                        h.files.to_string(),
                        fmt::bytes(h.bytes),
                        h.note.clone().unwrap_or_default(),
                    ]
                })
                .collect();
            print!("{}", fmt::table(&["object", "version", "captured", "files", "size", "note"], &rows));
        }
        SkillCmd::Diff { a, b } => {
            let a = resolve_object(rook, &a)?;
            let b = resolve_object(rook, &b)?;
            let changes = rook.skill_diff(&a, &b)?;
            if changes.is_empty() {
                println!("identical");
            }
            for (path, change) in changes {
                println!("{} {path}", change.sigil());
            }
        }
        SkillCmd::Rollback { name, object } => {
            let id = resolve_object(rook, &object)?;
            let result = rook.rollback_skill(&name, &id)?;
            println!("restored {} file(s) for {name} from {}", result.restored, id.short());
            println!("the previous state was captured first, so this is undoable");
            if !result.left_behind.is_empty() {
                println!("\nnot in that capture, left on disk in {}:", result.dir.display());
                for f in &result.left_behind {
                    println!("  {f}");
                }
            }
        }
    }
    Ok(())
}

fn cmd_search(
    rook: &Rook,
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
    let found = rook.search(query, &options)?;
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

fn cmd_lsp(workspace: Option<PathBuf>, cmd: LspCmd, json: bool) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async move {
        let rook = Rook::open(workspace)?;
        let configs =
            if rook.config.lsp.is_empty() { rook_core::lsp::detected() } else { rook.config.lsp.clone() };
        if configs.is_empty() {
            bail!("no language server found on PATH, and none configured under [[lsp]]");
        }
        let servers = rook_core::lsp::Servers::new(configs, &rook.workspace);

        // The tools are the same ones the agent calls, so this cannot drift
        // from what a turn would see.
        let mut tools = rook_tools::ToolBox::default();
        rook_core::lsp::register(&mut tools, servers.clone());
        let ctx = rook_tools::ToolContext::new(rook.workspace.clone());

        let (tool, args) = match &cmd {
            LspCmd::Servers => {
                println!("{}", servers.languages().join(", "));
                servers.shutdown().await;
                return anyhow::Ok(());
            }
            LspCmd::Diagnostics { path } => ("diagnostics", serde_json::json!({ "path": path })),
            LspCmd::Definition { path, symbol } => {
                ("definition", serde_json::json!({ "path": path, "symbol": symbol }))
            }
            LspCmd::References { path, symbol } => {
                ("references", serde_json::json!({ "path": path, "symbol": symbol }))
            }
            LspCmd::Symbol { query } => ("find_symbol", serde_json::json!({ "query": query })),
        };

        let outcome = tools.call(&ctx, tool, &args).await?;
        servers.shutdown().await;
        if json {
            println!("{}", serde_json::to_string_pretty(&outcome)?);
        } else {
            println!("{}", outcome.content);
        }
        anyhow::Ok(())
    })
}

fn cmd_memory(rook: &Rook, cmd: MemoryCmd, json: bool) -> Result<()> {
    let workspace = rook.workspace.display().to_string();
    match cmd {
        MemoryCmd::Ls { all } => {
            let book = rook.memory()?;
            let facts: Vec<_> =
                if all { book.facts.iter().collect() } else { book.in_scope(&workspace).collect() };
            if json {
                println!("{}", serde_json::to_string_pretty(&facts)?);
                return Ok(());
            }
            let rows: Vec<Vec<String>> = facts
                .iter()
                .map(|f| {
                    vec![
                        f.id.clone(),
                        if f.pinned { "pin".into() } else { String::new() },
                        f.scope.label().rsplit('/').next().unwrap_or("global").to_string(),
                        f.tags.join(","),
                        f.text.chars().take(70).collect(),
                    ]
                })
                .collect();
            print!("{}", fmt::table(&["id", "", "scope", "tags", "fact"], &rows));
            if !all && facts.len() < book.facts.len() {
                println!("\n{} more scoped to other workspaces (--all)", book.facts.len() - facts.len());
            }
        }

        MemoryCmd::Search { query } => {
            let book = rook.memory()?;
            let hits = rook_core::memory::search(book.in_scope(&workspace), &query.join(" "));
            if json {
                println!("{}", serde_json::to_string_pretty(&hits)?);
                return Ok(());
            }
            if hits.is_empty() {
                println!("nothing matched");
            }
            for hit in hits {
                println!("[{}] {}", hit.fact.id, hit.fact.text);
                let why = if hit.matched.is_empty() { "pinned".into() } else { hit.matched.join(", ") };
                println!("      score {:.1} · {why}", hit.score);
            }
        }

        MemoryCmd::Add { text, tag, global, pin } => {
            let scope = if global { rook_core::Scope::Global } else { rook_core::Scope::Project(workspace) };
            let mut fact = rook_core::Fact::new(text.join(" "), scope).with_tags(tag);
            fact.pinned = pin;
            let id = fact.id.clone();
            use rook_core::memory::Learned;
            match rook.remember(fact, Some("added from the command line".into()))? {
                Learned::New | Learned::Merged => println!("remembered as [{id}]"),
                Learned::Unchanged => println!("already remembered as [{id}]"),
                Learned::ScopedElsewhere(scope) => println!(
                    "already remembered as [{id}], scoped to {} — pass --global to widen it",
                    scope.label()
                ),
            }
        }

        MemoryCmd::Rm { id } => match rook.forget(&id, Some("removed from the command line".into()))? {
            Some(fact) => println!("forgot [{}] {}", fact.id, fact.text),
            None => bail!("no fact {id:?}"),
        },

        MemoryCmd::History => {
            let history = rook.memory_history()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&history)?);
                return Ok(());
            }
            let rows: Vec<Vec<String>> = history
                .iter()
                .map(|v| {
                    vec![
                        v.object.chars().take(12).collect(),
                        fmt::timestamp(v.updated_at),
                        v.facts.to_string(),
                        v.note.clone().unwrap_or_default(),
                    ]
                })
                .collect();
            print!("{}", fmt::table(&["object", "when", "facts", "note"], &rows));
        }

        MemoryCmd::Diff { a, b } => {
            let changes = rook.memory_diff(&resolve_object(rook, &a)?, &resolve_object(rook, &b)?)?;
            if changes.is_empty() {
                println!("identical");
            }
            for (change, fact) in changes {
                let sigil = if change == rook_core::memory::Change::Learned { '+' } else { '-' };
                println!("{sigil} [{}] {}", fact.id, fact.text);
            }
        }

        MemoryCmd::Since { days } => {
            let changes = rook.memory_since(rook_store::now_unix() - days * 86_400)?;
            if changes.is_empty() {
                println!("nothing learned or forgotten in the last {days} day(s)");
            }
            for (change, fact) in changes {
                let sigil = if change == rook_core::memory::Change::Learned { '+' } else { '-' };
                println!("{sigil} [{}] {}", fact.id, fact.text);
            }
        }
    }
    Ok(())
}

fn cmd_mcp(workspace: Option<PathBuf>, cmd: McpCmd, json: bool) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async move {
        let rook = Rook::open(workspace)?;
        if rook.config.mcp.is_empty() {
            println!("no servers configured. Add one to {}:\n", rook_core::paths::config_file().display());
            println!("  [[mcp]]\n  name = \"filesystem\"\n  command = \"npx\"\n  args = [\"-y\", \"@modelcontextprotocol/server-filesystem\", \".\"]");
            return anyhow::Ok(());
        }
        let session = rook.connect_mcp().await;

        match cmd {
            McpCmd::Ls => {
                if json {
                    let items: Vec<_> = session.servers.iter().map(|(s, tools)| serde_json::json!({
                        "name": s.name(), "server": s.info().server, "tools": tools.len(),
                    })).collect();
                    println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                        "connected": items,
                        "failed": session.failures.iter().map(|(n, e)| serde_json::json!({"name": n, "error": e})).collect::<Vec<_>>(),
                    }))?);
                } else {
                    let rows: Vec<Vec<String>> = session.servers.iter().map(|(s, tools)| vec![
                        s.name().to_string(),
                        format!("{} {}", s.info().server.name, s.info().server.version),
                        s.info().protocol_version.clone(),
                        tools.len().to_string(),
                    ]).collect();
                    print!("{}", fmt::table(&["name", "server", "protocol", "tools"], &rows));
                    for (name, error) in &session.failures {
                        println!("\n✗ {name}: {error}");
                    }
                }
            }
            McpCmd::Tools { server } => {
                let (_, tools) = session.servers.iter().find(|(s, _)| s.name() == server)
                    .with_context(|| format!("{server:?} is not connected"))?;
                if json {
                    println!("{}", serde_json::to_string_pretty(tools)?);
                } else {
                    for tool in tools {
                        println!("{}  ({})", rook_tools::mcp::namespaced(&server, &tool.name), tool.name);
                        println!("  {}", tool.description);
                        if let Some(props) = tool.input_schema.get("properties").and_then(|p| p.as_object()) {
                            let required = tool.input_schema.get("required").and_then(|r| r.as_array()).cloned().unwrap_or_default();
                            for (arg, schema) in props {
                                let mark = if required.iter().any(|r| r.as_str() == Some(arg)) { "*" } else { " " };
                                println!("   {mark}{arg}: {}", schema.get("type").and_then(|t| t.as_str()).unwrap_or("any"));
                            }
                        }
                        println!();
                    }
                }
            }
            McpCmd::Call { server, tool, args } => {
                let (connected, _) = session.servers.iter().find(|(s, _)| s.name() == server)
                    .with_context(|| format!("{server:?} is not connected"))?;
                let args: serde_json::Value = serde_json::from_str(&args).context("arguments must be JSON")?;
                let result = connected.call_tool(&tool, &args).await?;
                if result.is_error {
                    eprintln!("the server reported an error:");
                }
                println!("{}", result.to_text());
            }
        }
        session.shutdown().await;
        anyhow::Ok(())
    })
}

pub fn session_id(s: &str) -> Result<u128> {
    rook_store::parse_session_id(s).with_context(|| format!("{s:?} is not a session id"))
}

fn resolve_object(rook: &Rook, prefix: &str) -> Result<ObjectId> {
    rook.store
        .resolve_prefix(prefix)?
        .with_context(|| format!("no object matches {prefix:?} (or the prefix is ambiguous)"))
}

fn cmd_checkpoint(rook: &Rook, cmd: CheckpointCmd, json: bool) -> Result<()> {
    match cmd {
        CheckpointCmd::Create { name, path } => {
            let (set, id) = rook.checkpoint(&name, path.as_deref())?;
            println!(
                "checkpoint {name}: {} files, {} → {}",
                set.files.len(),
                fmt::bytes(set.total_bytes),
                id.short()
            );
        }
        CheckpointCmd::Ls => {
            let list = rook.checkpoints()?;
            if json {
                let items: Vec<_> =
                    list.iter().map(|(n, id)| serde_json::json!({"ref": n, "object": id.to_hex()})).collect();
                println!("{}", serde_json::to_string_pretty(&items)?);
                return Ok(());
            }
            let rows: Vec<Vec<String>> = list.iter().map(|(n, id)| vec![n.clone(), id.short()]).collect();
            print!("{}", fmt::table(&["ref", "object"], &rows));
        }
        CheckpointCmd::Restore { object, to } => {
            let id = resolve_object(rook, &object)?;
            let set = rook_core::FileSet::load(&rook.store, &id)?;
            let written = set.restore(&rook.store, &to)?;
            println!("restored {written} file(s) into {}", to.display());
        }
    }
    Ok(())
}
