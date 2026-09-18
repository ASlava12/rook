//! Initialization, configuration and model selection.

use super::doctor::offered;
use crate::args::ConfigCmd;
use crate::fmt;
use anyhow::{Context, Result};
use rook_core::Rook;
use std::path::PathBuf;

pub(crate) fn cmd_init(workspace: Option<PathBuf>) -> Result<()> {
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

pub(crate) fn cmd_models(
    workspace: Option<PathBuf>,
    json: bool,
    recheck: bool,
    source: Option<String>,
) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    if recheck {
        return rechecked(&runtime, json);
    }
    runtime.block_on(async move {
        let _ = workspace;
        let config = rook_core::Config::load()?;
        // Named, so the question is about that machine rather than about
        // whatever the agent happens to be pointed at. The list it comes back
        // with is what `[models.<name>] model` has to be one of, which is the
        // thing nobody can know before asking.
        if let Some(named) = source {
            let vault = rook_core::Vault::load().unwrap_or_else(|_| rook_core::Vault::empty());
            let provider = rook_core::models::provider_for(&config, &vault, &named)
                .with_context(|| format!("asking {named:?}"))?;
            let models = provider.models().await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&models)?);
                return anyhow::Ok(());
            }
            let wanted = rook_core::models::model_named(&config, &named);
            let rows: Vec<Vec<String>> = models
                .iter()
                .map(|m| {
                    vec![
                        if m.id == wanted { "▸".into() } else { " ".into() },
                        m.id.clone(),
                        m.context_window.map(|w| format!("{w}")).unwrap_or_default(),
                        m.quantization.clone().unwrap_or_default(),
                    ]
                })
                .collect();
            print!("{}", fmt::table(&["", "model", "context", "quant"], &rows));
            return anyhow::Ok(());
        }
        let configured = rook_core::models::model_named(&config, &config.agent.model);
        let configured = configured.as_str();
        let models = provider(&config)?.models().await?;
        if json {
            println!("{}", serde_json::to_string_pretty(&models)?);
            return anyhow::Ok(());
        }
        if models.is_empty() {
            println!("the endpoint does not list its models");
            return anyhow::Ok(());
        }
        // Resident and quantisation where the endpoint says, and blank where it
        // does not — which is every hosted one. They are what decides whether a
        // local model answers in a second or a minute, and neither is visible
        // in a name: three models were tried here in an evening, and each wall
        // was found only after switching to it.
        let rows: Vec<Vec<String>> = models
            .iter()
            .map(|m| {
                vec![
                    if m.id.eq_ignore_ascii_case(configured) { "▸".into() } else { " ".into() },
                    m.id.clone(),
                    m.context_window.map(|w| format!("{w}")).unwrap_or_default(),
                    m.quantization.clone().unwrap_or_default(),
                    match m.loaded {
                        Some(true) => "loaded".into(),
                        Some(false) => String::new(),
                        None => String::new(),
                    },
                    m.owned_by.clone().unwrap_or_default(),
                ]
            })
            .collect();
        print!("{}", fmt::table(&["", "model", "context", "quant", "", "owner"], &rows));
        if offered(&models, configured).is_none() {
            println!("\n{configured:?} is configured but not offered here");
        }
        anyhow::Ok(())
    })
}

/// Ask every configured endpoint whether it is there, and put back any that
/// were out.
///
/// Through the daemon where one is running, which is the opposite of what every
/// other command here does — and for a reason. The endpoints that are out live
/// in the memory of the process that talks to them, and that is the daemon: a
/// terminal that rechecked on its own would clear its own empty set, print a
/// perfectly true table, and leave the agent believing what it believed a
/// minute ago.
pub(crate) fn cmd_config(cmd: ConfigCmd, json: bool) -> Result<()> {
    match cmd {
        ConfigCmd::Show => {
            let config = rook_core::Config::load()?;
            match json {
                true => println!("{}", serde_json::to_string_pretty(&config)?),
                // The same shape the file has, so what is printed can be
                // pasted back into it — a listing in another format is one
                // somebody has to translate before they can act on it.
                false => print!("{}", config.as_written()?),
            }
            Ok(())
        }
        ConfigCmd::Set { key, value } => {
            let path = rook_core::paths::config_file();
            match rook_core::Config::set_in(&path, &key, &value) {
                Ok(_) => {
                    println!("{key} = {value}");
                    // Said because it is the one thing a person cannot see from
                    // here: a turn already running was built with the old one.
                    println!("in {} — the next turn reads it", path.display());
                    Ok(())
                }
                Err(why) => anyhow::bail!("{why}"),
            }
        }
        ConfigCmd::Check => {
            let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
            checked(&runtime, json)
        }
    }
}

/// What is wrong with the file, and what the endpoints in it say.
///
/// Two questions rather than one, because they fail apart: a setting nobody
/// reads is a typo that changes nothing, and an endpoint that will not answer
/// is a machine that is off. Reporting them together is what makes the answer
/// worth reading — the file is the only place both are decided.
fn checked(runtime: &tokio::runtime::Runtime, json: bool) -> Result<()> {
    let path = rook_core::paths::config_file();
    let config = rook_core::Config::load()?;
    let ignored = rook_core::Config::ignored_in(&path);
    let unpointed = rook_core::models::unpointed(&config);
    let answers = {
        let vault = rook_core::Vault::load().unwrap_or_else(|_| rook_core::Vault::empty());
        runtime.block_on(rook_core::models::recheck(&config, &vault))
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "config": path,
                "ignored": ignored,
                "unpointed": unpointed,
                "models": answers,
            }))?
        );
        return Ok(());
    }

    println!("{}", path.display());
    match ignored.is_empty() {
        true => println!("  ✓ every setting in it is one this reads"),
        false => {
            for name in &ignored {
                // The nearest known name where there is an obvious one, because
                // "not a setting" sends somebody to the documentation and "did
                // you mean" sends them to the line they typed.
                match rook_core::Config::nearest_to(name) {
                    Some(meant) => println!("  ✗ {name} is not a setting — did you mean {meant}?"),
                    None => println!("  ✗ {name} is not a setting"),
                }
            }
        }
    }

    for name in &unpointed {
        // Inert rather than wrong, so it is a line and not a failure — but an
        // address nothing uses looks exactly like one something does.
        println!("  · [endpoints.{name}] is described and no `[models]` source asks for it");
    }

    println!();
    if answers.is_empty() {
        println!("no models are named under `[models]`");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = answers
        .iter()
        .map(|answer| {
            vec![
                if answer.answering() { "✓".into() } else { "✗".into() },
                answer.name.clone(),
                format!("{} ms", answer.took_ms),
                match &answer.refused {
                    Some(why) => why.lines().next().unwrap_or(why).to_string(),
                    None => match answer.serving.len() {
                        1 => answer.serving[0].clone(),
                        n => format!("{n} models"),
                    },
                },
            ]
        })
        .collect();
    // "model" rather than "endpoint": the rows are `[models]` names, and since
    // `[endpoints]` became a table of its own the older word named the wrong
    // one of the two.
    print!("{}", fmt::table(&["", "model", "answered in", "what it says"], &rows));
    Ok(())
}

/// Takes the runtime rather than running inside one, which running it found:
/// the daemon client blocks for its answer, and blocking a thread that is
/// driving the runtime it blocks on is a panic rather than a wait. So the
/// choice between the two is made out here, and only the local half is given
/// to the runtime.
fn rechecked(runtime: &tokio::runtime::Runtime, json: bool) -> Result<()> {
    let daemon = crate::source::Daemon::running();
    let answers: Vec<rook_core::models::Answered> = match &daemon {
        Some(daemon) => {
            // Which process was put back, because that is the whole of what
            // this command does and the table alone does not say it: the same
            // rows would be printed by a terminal that had cleared nothing the
            // agent can see. On stderr, so `--json` stays machine-readable.
            eprintln!("putting them back for the running rookd at {}", daemon.base);
            daemon.recheck_models()?
        }
        None => {
            eprintln!("no rookd is running, so this asks the endpoints without putting any agent's back");
            let config = rook_core::Config::load()?;
            let vault = rook_core::Vault::load().unwrap_or_else(|_| rook_core::Vault::empty());
            runtime.block_on(rook_core::models::recheck(&config, &vault))
        }
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&answers)?);
        if let Some(daemon) = &daemon {
            second_opinion(runtime, daemon, &answers);
        }
        return Ok(());
    }
    if answers.is_empty() {
        println!("nothing is configured under `[models]`, so there is nothing to ask");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = answers
        .iter()
        .map(|answer| {
            vec![
                if answer.answering() { "✓".into() } else { "✗".into() },
                answer.name.clone(),
                format!("{} ms", answer.took_ms),
                match &answer.refused {
                    // The reason, cut to one line: a 401's body can be a page,
                    // and what a person needs here is which of the four it was.
                    Some(why) => why.lines().next().unwrap_or(why).to_string(),
                    None => match answer.serving.len() {
                        1 => answer.serving[0].clone(),
                        n => format!("{n} models"),
                    },
                },
            ]
        })
        .collect();
    // "model" rather than "endpoint": the rows are `[models]` names, and since
    // `[endpoints]` became a table of its own the older word named the wrong
    // one of the two.
    print!("{}", fmt::table(&["", "model", "answered in", "what it says"], &rows));
    if let Some(daemon) = &daemon {
        second_opinion(runtime, daemon, &answers);
    }
    Ok(())
}

/// Ask from here whatever the daemon says it cannot reach, and say so where
/// the two disagree.
///
/// This command is what somebody runs to answer "why can the agent not see my
/// server", and it answers it from inside the daemon — which is the one place
/// that cannot notice the daemon being the problem. It happened: a rookd that
/// had never reached the machines on its own network answered `No route to
/// host` in nought milliseconds for both of them, while a terminal beside it
/// reached one in ten and the other in ninety, with the same configuration,
/// the same environment and the same binary. Everything the table showed was
/// true and every word of it pointed at the network.
///
/// Only the ones it refused, so a recheck where everything answers costs
/// nothing. `answering_again` has already run in that process and in this one,
/// so neither answer is an exclusion being repeated back.
fn second_opinion(
    runtime: &tokio::runtime::Runtime,
    daemon: &crate::source::Daemon,
    answers: &[rook_core::models::Answered],
) {
    let refused: Vec<&str> = answers.iter().filter(|a| !a.answering()).map(|a| a.name.as_str()).collect();
    if refused.is_empty() {
        return;
    }
    let Ok(mut config) = rook_core::Config::load() else { return };
    // The same function the daemon just ran, narrowed to what it could not
    // reach: two ways of asking would be two answers to keep in step.
    config.models.retain(|name, _| refused.contains(&name.as_str()));
    let vault = rook_core::Vault::load().unwrap_or_else(|_| rook_core::Vault::empty());
    let here = runtime.block_on(rook_core::models::recheck(&config, &vault));

    let reached: Vec<String> =
        here.iter().filter(|a| a.answering()).map(|a| format!("{} in {} ms", a.name, a.took_ms)).collect();
    if reached.is_empty() {
        return;
    }
    eprintln!(
        "\nbut this terminal reaches {} — so it is that process and not the network.",
        reached.join(", ")
    );
    // How long it has been running, where it will say: a daemon older than the
    // network it is on is the shape this takes, and the age is what makes that
    // readable rather than a guess.
    let age = daemon
        .health()
        .map(|h| format!(" It has been up {}.", fmt::ago(rook_store::now_unix() - h.uptime_secs as i64)))
        .unwrap_or_default();
    eprintln!("`rook daemon restart` starts the installed build in its place.{age}");
}

/// Configured from the configuration and nothing else, so the two commands
/// that only want to ask a provider a question — `doctor` and `models` — do
/// not have to open a store to do it, and answer while `rookd` holds one.
pub(crate) fn provider(config: &rook_core::Config) -> Result<Box<dyn rook_llm::Provider>> {
    rook_core::models::configured(config)
        .with_context(|| format!("configuring model {:?}", config.agent.model))
}
