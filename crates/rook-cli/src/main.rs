//! `rook` — the command line and TUI front end.

mod approve;
mod args;
mod chat;
mod commands;
mod fmt;
mod notify;
mod remote;
mod source;
mod tui;
mod turn_options;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use args::{Cli, Command};
use clap::Parser;

use crate::source::Source;

/// Whether the command about to run owns the terminal.
///
/// Only the window does. A log line written to stderr while it is drawing lands
/// in the middle of the interface and stays until something redraws over it —
/// and the lines that would land there are exactly the ones worth reading: a
/// sandbox rule that will not compile, a lock read through after a panic, a
/// note that the log itself could not be opened.
fn draws_the_screen(command: &Option<Command>) -> bool {
    matches!(command, Some(Command::Tui { .. }))
}

fn main() -> Result<()> {
    // Before anything else: started as the launcher, this process lowers
    // itself and runs a command instead of being rook.
    rook_contain::launcher_entry();
    // Before anything reads the environment and before this process has a
    // second thread: provider keys live there, and a shell that has them and a
    // desktop launcher that does not should not behave differently.
    rook_core::config::load_env_file();
    let cli = Cli::parse();
    // Defaults if the config is unreadable: logging must not be what reports a
    // broken config, and the command about to run will report it properly.
    rook_core::telemetry::init(
        &rook_core::Config::load().unwrap_or_default().telemetry,
        !draws_the_screen(&cli.command),
    );

    match cli.command {
        // Bare `rook` opens a conversation: talking to the agent is the point,
        // and every comparable tool starts there.
        None => chat::run(cli.workspace, None, cli.yes),
        Some(Command::Tui { alone }) => {
            // Shared by default: the store takes one writer, so a window that
            // takes it is a window nobody can open a second of — which is what
            // somebody working in two projects meets first.
            let (source, started) = match alone {
                true => (Source::open(cli.workspace)?, None),
                false => Source::shared(cli.workspace)?,
            };
            tui::run(source, cli.yes, started)
        }
        Some(Command::Init) => commands::config::cmd_init(cli.workspace),
        Some(Command::Doctor) => commands::doctor::cmd_doctor(&workspace_of(&cli.workspace), cli.json),
        Some(Command::Chat { session }) => chat::run(cli.workspace, session, cli.yes),
        Some(Command::Run { prompt, session, output }) => {
            commands::run::cmd_run(cli.workspace, prompt, session, cli.yes, cli.json, output.load()?)
        }
        Some(Command::Models { recheck, source }) => {
            commands::config::cmd_models(cli.workspace, cli.json, recheck, source)
        }
        Some(Command::Config(cmd)) => commands::config::cmd_config(cmd, cli.json),
        Some(Command::Eval { json }) => commands::work::cmd_eval(cli.workspace, json || cli.json),
        Some(Command::Work { goal, most, tokens, keep_going, yes, resume }) => commands::work::cmd_work(
            cli.workspace,
            goal.join(" "),
            rook_core::work::Plan { goal: String::new(), most, tokens, until_clean: !keep_going },
            yes || cli.yes,
            cli.json,
            resume,
        ),
        Some(Command::Acp) => commands::daemon::cmd_acp(cli.workspace),
        Some(Command::Serve { port }) => commands::daemon::cmd_serve(port),
        Some(Command::Daemon(c)) => commands::daemon::cmd_daemon(c, cli.json),
        Some(Command::Store(c)) => commands::store::cmd_store(&Source::open(cli.workspace)?, c, cli.json),
        Some(Command::Session(c)) => {
            let here = workspace_of(&cli.workspace);
            commands::sessions::cmd_session(&Source::open(cli.workspace)?, c, &here, cli.json)
        }
        Some(Command::Skills(c)) => {
            let here = workspace_of(&cli.workspace);
            commands::skills::cmd_skills(&Source::open(cli.workspace)?, c, &here, cli.json)
        }
        Some(Command::Checkpoint(c)) => {
            commands::store::cmd_checkpoint(&Source::open(cli.workspace)?, c, cli.json)
        }
        Some(Command::Mcp(c)) => commands::mcp::cmd_mcp(cli.workspace, c, cli.json),
        Some(Command::Memory(c)) => {
            let here = workspace_of(&cli.workspace);
            commands::knowledge::cmd_memory(&Source::open(cli.workspace)?, c, &here, cli.json)
        }
        Some(Command::Docs(c)) => commands::knowledge::cmd_docs(&Source::open(cli.workspace)?, c, cli.json),
        Some(Command::Secrets(c)) => {
            commands::secrets::cmd_secrets(&Source::open(cli.workspace)?, c, cli.json)
        }
        // Answered before anything else is built: it is spawned by `ssh` in the
        // middle of a command, and everything this binary does on the way to a
        // subcommand — opening a store, starting a daemon — is time a password
        // prompt is waiting on.
        Some(Command::Askpass { name }) => {
            let variable = format!("ROOK_SECRET_{}", name.to_uppercase().replace('-', "_"));
            match std::env::var(&variable) {
                Ok(value) => {
                    println!("{value}");
                    Ok(())
                }
                Err(_) => bail!("{variable} is not in this environment — nothing asked for {name:?}"),
            }
        }
        Some(Command::Lsp(c)) => commands::lsp::cmd_lsp(cli.workspace, c, cli.json),
        Some(Command::Search { query, session, conversation, limit }) => commands::sessions::cmd_search(
            &Source::open(cli.workspace)?,
            &query.join(" "),
            session,
            conversation,
            limit,
            cli.json,
        ),
    }
}

fn workspace_of(given: &Option<PathBuf>) -> PathBuf {
    given.clone().or_else(|| std::env::current_dir().ok()).unwrap_or_else(|| PathBuf::from("."))
}

pub fn session_id(s: &str) -> Result<u128> {
    rook_store::parse_session_id(s).with_context(|| format!("{s:?} is not a session id"))
}

#[cfg(test)]
mod tests {
    use super::{Command, draws_the_screen};

    /// Only the window owns the terminal, so only the window keeps the log off
    /// it.
    ///
    /// Every other command prints its own output and is read as it runs; a
    /// warning on stderr belongs with that output. The window draws instead,
    /// and a line written under the alternate screen lands in the middle of the
    /// interface and stays there.
    #[test]
    fn only_the_window_owns_the_terminal_it_is_drawing_on() {
        assert!(draws_the_screen(&Some(Command::Tui { alone: false })));
        assert!(draws_the_screen(&Some(Command::Tui { alone: true })));

        // Bare `rook` is a conversation printed line by line, not a drawn
        // screen: its warnings belong on stderr with the rest of what it says.
        assert!(!draws_the_screen(&None));
        assert!(!draws_the_screen(&Some(Command::Doctor {})));
    }
}
