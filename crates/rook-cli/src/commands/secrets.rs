//! Secret entry and vault management.

use crate::args::SecretsCmd;
use crate::source::Source;
use anyhow::{Context, Result, bail};

pub(crate) fn cmd_secrets(source: &Source, cmd: SecretsCmd, json: bool) -> Result<()> {
    match cmd {
        SecretsCmd::Ls => {
            let kept = source.secrets()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&kept)?);
                return Ok(());
            }
            if kept.is_empty() {
                println!(
                    "nothing set. `rook secrets add <name>` keeps a value here, or names where it \
                     already lives: --source env:NAME, cmd:… or keychain:service/account.\n\
                     A command uses one by name: run_command {{\"secrets\": [\"<name>\"]}}."
                );
                return Ok(());
            }
            for secret in &kept {
                let answers = match secret.resolves {
                    true => "",
                    false => "  — does not answer right now",
                };
                println!("{:<24} {}{answers}", secret.name, secret.source);
            }
        }
        SecretsCmd::Add { name, source: from } => match from {
            Some(from) => {
                let parsed = rook_core::Source::parse(&from).with_context(|| {
                    format!("{from:?} is not a source — env:NAME, cmd:<command>, keychain:service/account")
                })?;
                source.refer_secret(&name, &parsed)?;
                println!("{name} reads from {}", parsed.as_str());
            }
            None => {
                // Off the terminal with the echo off: an argument would be in
                // the shell's history, and a pipe would be in whatever wrote it.
                let value = read_hidden("value (not echoed): ")?;
                if value.trim().is_empty() {
                    bail!("nothing was typed, so nothing was kept");
                }
                source.keep_secret(&name, &value)?;
                println!("kept as {name} — a command uses it with `secrets: [\"{name}\"]`");
            }
        },
        SecretsCmd::Rm { name } => match source.forget_secret(&name)? {
            true => println!("dropped {name}"),
            false => bail!("no secret {name:?}"),
        },
    }
    Ok(())
}

/// A line off the terminal with nothing shown for it.
///
/// Through crossterm, which this binary already carries for the TUI, rather
/// than a crate for the one function: raw mode, characters until Enter, and
/// nothing echoed. Ctrl-C leaves without keeping anything, because a password
/// half-typed and abandoned should not become a secret.
fn read_hidden(prompt: &str) -> Result<String> {
    use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
    use std::io::{IsTerminal, Write};

    if !std::io::stdin().is_terminal() {
        bail!(
            "there is no terminal to type into. Run this where you can type, or name where the \
             value lives instead: --source env:NAME"
        );
    }
    print!("{prompt}");
    std::io::stdout().flush().ok();
    crossterm::terminal::enable_raw_mode().context("taking the terminal to read without echo")?;
    let mut typed = String::new();
    let read = loop {
        match crossterm::event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Enter => break Ok(()),
                KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => {
                    break Err(anyhow::anyhow!("cancelled, and nothing was kept"));
                }
                KeyCode::Backspace => {
                    typed.pop();
                }
                KeyCode::Char(c) => typed.push(c),
                _ => {}
            },
            Ok(_) => {}
            Err(e) => break Err(anyhow::anyhow!("reading the terminal: {e}")),
        }
    };
    // Always, whatever happened: a terminal left in raw mode is a shell that
    // stops echoing anything the user types next.
    crossterm::terminal::disable_raw_mode().ok();
    println!();
    read.map(|()| typed)
}
