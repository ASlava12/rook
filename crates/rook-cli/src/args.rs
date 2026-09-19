//! Command-line grammar; execution lives in `commands`.

use crate::turn_options::TurnArgs;
use clap::{Parser, Subcommand};
use rook_core::AGENT_VERSION;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "rook",
    version = AGENT_VERSION,
    about = "A compact autonomous agent with an inspectable memory",
    long_about = "Rook keeps everything it does in a content-addressed local store.\n\
                  Every subcommand under `store`, `session` and `skills` exists so that\n\
                  memory is something you can read, diff and roll back — not a black box."
)]
pub(crate) struct Cli {
    /// Workspace root. Defaults to the current directory.
    #[arg(long, short = 'C', global = true)]
    pub(crate) workspace: Option<PathBuf>,
    /// Emit JSON instead of tables.
    #[arg(long, global = true)]
    pub(crate) json: bool,
    /// Approve everything the deny list does not forbid, without asking.
    #[arg(long, short = 'y', global = true)]
    pub(crate) yes: bool,
    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Create the store and the config file, and say where skills go.
    Init,
    /// Report what Rook detected about this machine and what it means for skills.
    Doctor,
    /// Check GitHub for a newer rook, and fetch it.
    ///
    /// The previous version is kept beside the new one as `rook.previous`, so
    /// going back is a rename. `--check` only asks.
    Update {
        /// Say what is published and stop, without fetching anything.
        #[arg(long, conflicts_with_all = ["force", "rollback"])]
        check: bool,
        /// Fetch and replace even where the published version is not newer,
        /// which is how a broken install is repaired or a version undone.
        #[arg(long, conflicts_with = "rollback")]
        force: bool,
        /// Put back what the last update replaced, without asking GitHub
        /// anything. Running it again undoes it.
        #[arg(long)]
        rollback: bool,
        /// As JSON.
        #[arg(long)]
        json: bool,
    },
    /// Talk to the agent interactively.
    Chat {
        /// Continue an existing session instead of starting one. `last` is the
        /// most recent in this workspace.
        #[arg(long)]
        session: Option<String>,
    },
    /// Run a single turn against the configured model.
    Run {
        prompt: Vec<String>,
        #[command(flatten)]
        output: TurnArgs,
        /// Continue an existing session instead of starting one. `last` is the
        /// most recent in this workspace.
        #[arg(long)]
        session: Option<String>,
    },
    /// Chat, and browse the store, sessions and skills, in the terminal.
    Tui {
        /// Take the store for this window alone rather than sharing it through
        /// `rookd`. One process, and no second window while it runs.
        #[arg(long)]
        alone: bool,
    },
    /// Read the configuration, fill in what it does not say, and check it.
    #[command(subcommand)]
    Config(ConfigCmd),
    /// Work at one goal across many turns, with the checks run between them.
    ///
    /// A turn ends and something has to decide whether there is another one.
    /// Here that decision reads what the harness measured and what the
    /// filesystem says changed — never the turn's account of itself.
    Work {
        /// The standing goal, carried into every iteration; --resume uses the saved goal.
        #[arg(required_unless_present = "resume")]
        goal: Vec<String>,
        /// Iterations at most. Zero lifts it, and then the other two bound it.
        #[arg(long, default_value_t = 10)]
        most: u32,
        /// Stop once the run has spent this many tokens, counted between
        /// iterations — so the one that crosses the line finishes first. Zero
        /// lifts it.
        #[arg(long, default_value_t = 0)]
        tokens: u64,
        /// Keep going after the checks pass, looking for more to do, rather
        /// than stopping at the first clean evaluation.
        #[arg(long)]
        keep_going: bool,
        /// Approve everything the deny list does not forbid, for a run nobody
        /// is watching. Without it an unattended run refuses what it cannot get
        /// approved, which is the safe end of the wait and also a short run.
        #[arg(long)]
        yes: bool,
        /// Carry on the last run in this workspace rather than starting one.
        ///
        /// A run measured in days is one a machine reboots in the middle of,
        /// and what it has done by then — the iterations, what the checks said,
        /// what it spent — is worth more than starting over.
        #[arg(long)]
        resume: bool,
    },
    /// Run the checks this project is judged by, from `.rook/evaluation.toml`.
    ///
    /// The harness runs them, not the model — an agent that could run its own
    /// evaluation could run it until it passed.
    Eval {
        /// Print the report as JSON, for something else to read.
        #[arg(long)]
        json: bool,
    },
    /// List the models the configured provider says it can serve.
    Models {
        /// Put every configured endpoint back in the rotation and ask each one.
        /// For after topping up an account or starting a server, when the
        /// answer wanted is "does it work now" rather than "in a minute".
        #[arg(long)]
        recheck: bool,
        /// One endpoint from `[models]` rather than the configured one. For
        /// seeing what a machine serves before pointing anything at it.
        #[arg(long)]
        source: Option<String>,
    },
    /// Speak the Agent Client Protocol on stdio, for editors.
    Acp,
    /// Start the HTTP backend and web UI.
    Serve {
        #[arg(long)]
        port: Option<u16>,
    },
    /// Look after the `rookd` that holds the store for every window.
    #[command(subcommand)]
    Daemon(DaemonCmd),
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
    /// The documentation the agent has gathered and reads from.
    #[command(subcommand)]
    Docs(DocsCmd),
    /// Secrets the agent can use by name, and never see the value of.
    #[command(subcommand)]
    Secrets(SecretsCmd),
    /// Print a secret to whatever asked for a password. Set as `SSH_ASKPASS`
    /// for a command that names one; not useful by hand.
    #[command(hide = true)]
    Askpass { name: String },
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
pub(crate) enum DaemonCmd {
    /// Where it is, how long it has been up, and whether it is the build that
    /// is installed.
    Status,
    /// Start one, if none is answering.
    Start,
    /// Stop it. A turn in flight is named rather than dropped.
    Stop {
        /// End the turns that are running.
        #[arg(long)]
        force: bool,
    },
    /// Stop it and start the installed build in its place.
    Restart {
        /// End the turns that are running.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum LspCmd {
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
    /// Fetch a language server this machine does not have, checked against the
    /// digest its publisher lists, into the state directory.
    Install { name: String },
    /// Fetch again every server `install` put in place, and say which moved.
    Update,
}

#[derive(Subcommand)]
pub(crate) enum MemoryCmd {
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
pub(crate) enum DocsCmd {
    /// What is kept, newest first.
    Ls,
    /// Read a kept set: the passages that answer a question, or the pages it
    /// was made from.
    Show {
        topic: String,
        /// Which version. The current one when not said.
        #[arg(long)]
        version: Option<String>,
        /// Show the passages that answer this rather than the page list.
        #[arg(long)]
        question: Option<String>,
        /// Print a page whole, by its number in the list.
        #[arg(long)]
        page: Option<usize>,
    },
    /// Search the web for a topic's documentation, read it, and keep it here.
    Add {
        topic: String,
        #[arg(long)]
        version: Option<String>,
        /// Ask the sources what has changed and re-read only that. Age says
        /// nothing on its own — a pinned version does not go stale by getting
        /// older, and `latest` can be wrong the day after it was read.
        #[arg(long)]
        refresh: bool,
    },
    /// Drop a set, or every version of a topic.
    Rm {
        topic: String,
        #[arg(long)]
        version: Option<String>,
    },
}

#[derive(Subcommand)]
pub(crate) enum ConfigCmd {
    /// Every setting in force, with the ones the file does not name filled in
    /// from the defaults. What the agent reads, rather than what was written.
    Show,
    /// Read the file, name what is wrong in it, and ask every endpoint whether
    /// it is there.
    Check,
    /// Change one setting, leaving the rest of the file — comments included —
    /// exactly as it was.
    Set {
        /// Dotted, as `config show` prints it: `agent.model`, `agent.effort`,
        /// `models.home-llama.priority`.
        key: String,
        value: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum SecretsCmd {
    /// What is set, where each comes from, and whether it answers. Never a
    /// value: there is no command here that prints one.
    Ls,
    /// Keep a value on this machine, typed rather than passed — an argument
    /// would be in the shell's history before the agent ever saw it.
    Add {
        name: String,
        /// Where the value lives instead of here: `env:NAME`, `cmd:op read …`,
        /// `keychain:service/account`. Without it, the value is asked for.
        #[arg(long)]
        source: Option<String>,
    },
    /// Drop one.
    Rm { name: String },
}

#[derive(Subcommand)]
pub(crate) enum McpCmd {
    /// Offer Rook's own tools over stdio, so any MCP client can use them.
    Serve {
        /// Allow anything the deny list does not forbid. Without it a client
        /// reaching a tool that changes the machine is refused, because there
        /// is nobody at this end to ask.
        #[arg(long)]
        yes: bool,
    },
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
pub(crate) enum StoreCmd {
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
pub(crate) enum SessionCmd {
    /// Inspect interrupted execution, or acknowledge one reviewed operation.
    Recovery {
        id: String,
        #[arg(long, requires = "note")]
        acknowledge: Option<String>,
        /// What was checked and what happened; acknowledgement never retries it.
        #[arg(long, requires = "acknowledge")]
        note: Option<String>,
    },
    Ls {
        /// Every workspace, not just this one.
        #[arg(long)]
        all: bool,
    },
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
        /// Model context window to measure against. Defaults to the configured
        /// model's own, which is the number the agent budgets against.
        #[arg(long)]
        window: Option<usize>,
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
    /// Move a session to another workspace, so its next turn runs there.
    ///
    /// A session's turns run where it was started, on purpose. This is the way
    /// to change that when the directory was the wrong one — one project deep
    /// for work that spans the projects beside it — rather than losing the
    /// transcript and beginning again.
    Move {
        id: String,
        /// The workspace its turns should run in from now on.
        to: std::path::PathBuf,
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
pub(crate) enum SkillCmd {
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
    /// What the configured sources offer, best match first.
    Search {
        /// Words to match against a name or description. Empty lists everything.
        #[arg(default_value = "")]
        query: String,
        /// Fetch the sources again before looking.
        #[arg(long)]
        refresh: bool,
    },
    /// Install a skill a source offers, by name.
    Install { name: String },
    /// Where `search` and `install` look.
    Sources,
    /// Bring skills installed from a source up to what it offers now. Skills
    /// written here, and installed ones edited since, are left alone and said.
    Update,
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
pub(crate) enum CheckpointCmd {
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
