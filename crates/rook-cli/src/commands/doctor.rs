//! Machine and provider diagnostics that do not require opening the store.

use super::config::provider;
use crate::fmt;
use anyhow::Result;
use rook_core::AGENT_VERSION;
use std::path::Path;

/// Diagnostics, and so the one command that must answer when things are wrong
/// — including while `rookd` holds the store, which is exactly a time somebody
/// runs it. Nothing here needs the store: the configuration is a file, the
/// environment is the machine, and the store's path is a path.
pub(crate) fn cmd_doctor(workspace: &Path, json: bool) -> Result<()> {
    let config = rook_core::Config::load()?;
    let env = rook_skills::Environment::detect(AGENT_VERSION);
    if json {
        println!("{}", serde_json::to_string_pretty(&env)?);
        return Ok(());
    }
    let env = &env;
    println!("rook {AGENT_VERSION}");
    println!("os        {} ({} userland)", env.os, env.userland);
    println!("arch      {}", env.arch);
    println!("workspace {}", workspace.display());
    println!("store     {}", rook_core::paths::store_dir().display());

    // Where the provider keys came from, which is the first thing anybody
    // debugging a 401 wants and the last thing they can see: names only, and
    // never a value.
    let dotenv = rook_core::paths::home().join(".env");
    if dotenv.is_file() {
        // Read at startup, so by now they are in this process's environment;
        // what the file would have set is what it names.
        let names = std::fs::read_to_string(&dotenv)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.trim().strip_prefix("export ").unwrap_or(line.trim()).split_once('='))
            .map(|(name, _)| name.trim().to_string())
            .filter(|name| !name.is_empty() && !name.starts_with('#'))
            .collect::<Vec<_>>();
        println!("env       {} — {}", dotenv.display(), names.join(", "));
    }
    // Not read, and said rather than left silent: a workspace is somebody
    // else's repository as often as it is yours, and a `.env` in one can point
    // a provider's base URL at a host of its choosing.
    if workspace.join(".env").is_file() {
        println!(
            "env       {} is NOT read — a cloned repository must not be able to redirect a key.\n\
             \x20         Move what rook needs to {}, or export it.",
            workspace.join(".env").display(),
            dotenv.display()
        );
    }

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
    println!("state:");
    // Where it is, when that is not where a fresh install would put it. An
    // upgrade keeps an existing directory rather than moving gigabytes of
    // sessions, so on Windows the old `%USERPROFILE%\.rook` goes on being used
    // and nothing would otherwise say so — which is how somebody ends up
    // looking for their history under `%LOCALAPPDATA%` and not finding it.
    if let Some(note) = rook_core::paths::where_the_state_is() {
        println!("  {note}");
    }
    // What accumulates under here is every transcript the agent has written,
    // the files it read included — so who else can read it is a fact about
    // this machine worth one line.
    match rook_core::paths::readable_by_others() {
        loose if loose.is_empty() => println!("  {} — yours alone", rook_core::paths::home().display()),
        loose => {
            for (dir, mode) in loose {
                println!("  {} is mode {mode:o}: other accounts on this machine can read it", dir.display());
                println!("    chmod 700 {}", dir.display());
            }
        }
    }

    println!();
    println!("daemon:");
    // Where a person looks when something is odd, and "the daemon is older
    // than the binary you just installed" is exactly the kind of odd that
    // looks like a fix that did not take.
    match crate::source::Daemon::running() {
        None => println!("  none running — a window starts one when it needs one"),
        Some(daemon) => match daemon.health() {
            Ok(health) => {
                println!(
                    "  {} — rook {}, started {}, {} turn(s) running",
                    daemon.base,
                    health.version,
                    fmt::ago(rook_store::now_unix() - health.uptime_secs as i64),
                    health.turns_running
                );
                if daemon.replaced {
                    println!(
                        "  the `rookd` on disk was installed after this one started: it is running \n  \
                         the previous build. `rook daemon restart` picks up the installed one, on \n  \
                         the same port, so an open window keeps working."
                    );
                }
            }
            Err(e) => println!("  {} — but it did not answer: {e}", daemon.base),
        },
    }

    println!();
    println!("commands:");
    // Asked of the same context a turn runs with, so what doctor says is
    // what a command gets — and what its result would say it got.
    let ctx = rook_core::agent::tool_context(&config, workspace, &rook_core::paths::output_dir());
    match rook_tools::isolate::choose(ctx.isolate, &ctx.isolation) {
        Ok((Some(_), how)) => println!("  contained — {how}"),
        Ok((None, how)) => println!("  not contained — {how}"),
        Err(refused) => println!("  refused — {refused}"),
    }

    println!();
    println!("project reference sources (trust requires a content pin):");
    // Named, because the difference between a file the agent reads and one it
    // does not is invisible until something it says is ignored.
    let standing = rook_core::instructions::applying_in(workspace, config.agent.max_instructions_bytes);
    if standing.is_empty() {
        println!(
            "  (none — an {} here or in {} is read as reference data)",
            rook_core::instructions::FILENAME,
            rook_core::paths::home().display()
        );
    }
    for one in &standing {
        let cut = match one.elided {
            0 => String::new(),
            n => format!(", {n} bytes past `[agent] max_instructions_bytes` not read"),
        };
        let origin = one.from.canonicalize().unwrap_or_else(|_| one.from.clone());
        let hash = rook_store::ObjectId::of(one.text.as_bytes()).to_hex();
        let trusted = one.elided == 0
            && origin.is_absolute()
            && config.agent.trusted_sources.get(origin.to_string_lossy().as_ref()) == Some(&hash);
        let authority = if trusted { "scoped_instructions" } else { "data" };
        println!("  {} ({} bytes{cut}; {authority})", origin.display(), one.text.len());
        println!("    body_blake3: {hash}");
    }

    println!();
    // A setting nothing reads is a setting that did nothing and said nothing
    // about it, which is how an afternoon goes into measuring a limit that was
    // never raised. Said where a person looks when something did not take.
    let ignored = rook_core::Config::ignored_in(&rook_core::paths::config_file());
    if !ignored.is_empty() {
        println!();
        println!("settings nothing reads:");
        for key in &ignored {
            match rook_core::Config::nearest_to(key) {
                Some(near) => println!("  {key} — did you mean {near}?"),
                None => println!("  {key}"),
            }
        }
    }

    // What the project's own file asked for and did not get. It travels with
    // the repository, so it may say how to work here and not what the agent is
    // allowed to do — and a setting dropped in silence leaves whoever wrote it
    // certain they changed something.
    let refused = rook_core::Config::refused_from_workspace(workspace);
    if !refused.is_empty() {
        println!();
        println!(
            "{} sets these, and they are not a project's to set:",
            rook_core::paths::workspace_config_file(workspace).display()
        );
        for key in &refused {
            println!("  {key}");
        }
        println!("  A project may say how to work in it, never what the agent may do.");
        println!("  `sandbox.deny` and `sandbox.ask` are the exception: added to, never replaced.");
    }

    println!("model:");
    match probe_provider(&config) {
        Ok(note) => println!("  {note}"),
        Err(e) => {
            // Indented as one block: the advice belongs to the failure above it,
            // and doctor is read top to bottom.
            println!("  {}", config.agent.model);
            for line in e.to_string().lines() {
                println!("  {line}");
            }
        }
    }

    let servers = rook_core::lsp::configured(&config);
    println!();
    println!("language servers:");
    if servers.is_empty() {
        println!(
            "  none found on PATH (rust-analyzer, gopls, clangd, …) — `rook lsp install rust-analyzer` fetches one"
        );
    }
    let here: Vec<String> =
        rook_core::lsp::for_workspace(&config, workspace).into_iter().map(|c| c.language).collect();
    for (config, started) in probe_servers(&servers, workspace) {
        // Installed and working is one question; used in this workspace is
        // another, and a ✓ against a language with no files here reads as the
        // first answering the second.
        let used = match here.contains(&config.language) {
            true => String::new(),
            false => format!("  · no {} files here", config.extensions.join("/")),
        };
        // Whether it starts and whether it is wanted here are independent, so a
        // server that fails both says both.
        match started {
            Ok(()) => println!("  ✓ {:<10} {}{used}", config.language, config.command),
            Err(why) => {
                let why = why.strip_prefix(&format!("{}: ", config.language)).unwrap_or(&why);
                println!("  ✗ {:<10} {}{used} — {why}", config.language, config.command)
            }
        }
    }

    println!();
    println!("web:");
    match config.web.enabled {
        false => println!("  off — `[web] enabled = true` offers `web_fetch`"),
        true => {
            println!("  ✓ web_fetch");
            let engine = rook_tools::web::Engine::named(&config.web.search, &config.web.search_url);
            match (config.web.search.trim(), engine) {
                ("", _) => println!("  · no search engine — set `[web] search` to searxng or brave"),
                (named, None) => {
                    println!("  ✗ {named}: named but unusable — brave needs BRAVE_API_KEY in the environment")
                }
                // Reachability is not asked here: a search engine that is down
                // this second is a different failure from one misconfigured, and
                // doctor should not stall on a network round trip.
                (named, Some(_)) => println!("  ✓ web_search via {named}"),
            }
        }
    }

    let (_, unusable) = rook_tools::policy::Policy::compile(
        config.sandbox.stance,
        &config.sandbox.allow,
        &config.sandbox.ask,
        &config.sandbox.deny,
    );
    println!();
    println!("approvals: {} mode", config.sandbox.stance.as_str());
    // The rules themselves, not just their count: "what am I allowed to run"
    // is the question somebody has when they are being asked about every
    // command, and the answer was in a config file they had to go and read.
    for (what, rules) in [
        ("allowed without asking", &config.sandbox.allow),
        ("always asked about", &config.sandbox.ask),
        ("refused outright", &config.sandbox.deny),
    ] {
        // One a line: the shipped deny rules are regular expressions long
        // enough that three of them on one line is a paragraph nobody reads.
        match rules.is_empty() {
            true => println!("  {what} — none"),
            false => {
                println!("  {what}:");
                for rule in rules.iter() {
                    println!("    {rule}");
                }
            }
        }
    }
    println!("  in this mode, anything else is {}", asked_or_allowed(config.sandbox.stance));
    for error in &unusable {
        println!("  ✗ {error}");
    }
    if !unusable.is_empty() {
        println!("  a rule that does not compile is not applied — a broken deny rule stops the agent");
    }

    if !config.hooks.is_empty() {
        let (_, unusable) = rook_core::hooks::Hooks::compile(&config.hooks);
        println!();
        println!("hooks: {}", config.hooks.len());
        for hook in &config.hooks {
            println!("  {:<14} {}", hook.event.as_str(), hook.command);
        }
        for error in &unusable {
            // Not dropped: it fires on everything instead, which is louder than
            // never firing and easier to mistake for the hook simply misbehaving.
            println!("  ✗ {error} — this hook runs on every subject until the pattern parses");
        }
    }

    let (plugins, plugin_errors) = rook_core::plugins::discover(workspace);
    if !plugins.is_empty() {
        println!();
        println!("plugins:");
        for plugin in &plugins {
            println!(
                "  {} {} — {} skills, {} servers",
                plugin.name,
                plugin.version,
                std::fs::read_dir(plugin.skills_dir()).into_iter().flatten().count(),
                plugin.mcp.len()
            );
        }
    }

    let (skills, skill_errors) = rook_core::Rook::discover_skills(workspace, &plugins);
    let cards = skills.catalog(env);
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
    let failed: Vec<&String> = skill_errors.iter().chain(&plugin_errors).collect();
    if !failed.is_empty() {
        println!();
        println!("skills that failed to load:");
        for e in failed {
            println!("  {e}");
        }
    }
    Ok(())
}

/// Ask the endpoint what it serves, which answers "is it up" and "is the model
/// configured actually there" in one round trip.
/// Start each one and shut it down again. Being on `PATH` is not the same as
/// working — rustup installs a `rust-analyzer` shim whether or not the component
/// is, and it fails on the first request — and doctor is where that difference
/// has to show up rather than in the middle of a turn.
fn probe_servers(
    configs: &[rook_lsp::ServerConfig],
    root: &std::path::Path,
) -> Vec<(rook_lsp::ServerConfig, std::result::Result<(), String>)> {
    let Ok(runtime) = tokio::runtime::Builder::new_multi_thread().enable_all().build() else {
        return Vec::new();
    };
    runtime.block_on(async {
        let mut out = Vec::new();
        for config in configs {
            let started = match rook_lsp::Server::start(config, root).await {
                Ok(server) => {
                    server.shutdown().await;
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            };
            out.push((config.clone(), started));
        }
        out
    })
}

/// The offered model a configuration names, and whether it named it exactly.
///
/// Case first, because two models that differ only by case are two models — a
/// catalogue that has both is the reason to be strict. Then without it,
/// because an endpoint that answers in a spelling of its own is the ordinary
/// case: LM Studio lowercases what it serves, so a model configured as its
/// publisher writes it works on every request and was reported here as one the
/// endpoint does not have.
pub(crate) fn offered<'a>(
    models: &'a [rook_llm::ModelInfo],
    configured: &str,
) -> Option<(&'a rook_llm::ModelInfo, bool)> {
    if let Some(exact) = models.iter().find(|m| m.id == configured) {
        return Some((exact, true));
    }
    models.iter().find(|m| m.id.eq_ignore_ascii_case(configured)).map(|near| (near, false))
}

fn probe_provider(config: &rook_core::Config) -> Result<String> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let configured = rook_core::models::model_named(config, &config.agent.model);
    let configured = configured.as_str();
    let provider = provider(config)?;
    let models = runtime.block_on(provider.models())?;

    let spec = &config.agent.model;
    let window = provider.context_window();

    if models.is_empty() {
        return Ok(format!("{spec} — reachable, {window} token window assumed"));
    }
    let Some((serving, exactly)) = offered(&models, configured) else {
        return Ok(format!(
            "{spec} — reachable, but {configured:?} is not among the {} it offers (`rook models`)",
            models.len()
        ));
    };
    let reported = serving.context_window;

    let mut note = format!("{spec} — reachable, {} model(s) offered, {window} token window", models.len());
    if !exactly {
        note.push_str(&format!("\n  the endpoint spells it {:?}, differing only in case", serving.id));
    }
    // The endpoint knowing better than our default is common for self-hosted
    // models, and silently budgeting against the wrong number wastes most of
    // the window or overruns it.
    if let Some(reported) = reported.filter(|r| *r != window) {
        note.push_str(&format!(
            "\n  the endpoint reports {reported}; set `context_window = {reported}` under [agent] to use it"
        ));
    }
    // A knob connected to nothing is worse than no knob: every front end shows
    // the effort beside the stance, and on a model with no reasoning to spend
    // it was being shown a setting that reached no request.
    if !provider.takes_effort() {
        note.push_str(
            "\n  `effort` is not sent to this model — it is not one of the families that reason, \
             and an unknown field is refused by a strict endpoint rather than ignored",
        );
    }
    Ok(note)
}

/// What happens to a command no rule mentions, which is the half of the answer
/// the rules do not give.
fn asked_or_allowed(stance: rook_tools::policy::Stance) -> &'static str {
    use rook_tools::policy::Stance::*;
    match stance {
        ReadOnly => "refused: nothing may change the machine",
        Autonomous | Free => "allowed without asking",
        _ => "asked about, once per command — or once per kind, if you answer `k`",
    }
}

#[cfg(test)]
mod tests {
    use super::offered;

    fn model(id: &str) -> rook_llm::ModelInfo {
        rook_llm::ModelInfo {
            id: id.into(),
            owned_by: None,
            context_window: Some(262_144),
            loaded: None,
            quantization: None,
        }
    }

    /// An endpoint that answers in a spelling of its own is the ordinary case
    /// — LM Studio lowercases what it serves — so a model configured the way
    /// its publisher writes it worked on every request and was reported by
    /// `doctor` and `models` as one the endpoint does not have.
    #[test]
    fn a_model_the_endpoint_spells_differently_is_still_the_one_configured() {
        let serving = [model("qwen/qwen3.8-27b"), model("google/gemma-4-31b-qat")];

        let (found, exactly) = offered(&serving, "qwen/qwen3.8-27b").expect("named exactly");
        assert_eq!((found.id.as_str(), exactly), ("qwen/qwen3.8-27b", true));

        let (found, exactly) = offered(&serving, "Qwen/Qwen3.8-27B").expect("and by case alone");
        assert_eq!((found.id.as_str(), exactly), ("qwen/qwen3.8-27b", false), "which is worth saying");

        assert!(offered(&serving, "somebody/else").is_none(), "and a name it does not serve is not a match");
    }

    /// Two models that differ only by case are two models, so the one asked
    /// for wins over the one that merely matches loosely.
    #[test]
    fn an_exact_name_wins_over_one_that_differs_by_case() {
        let serving = [model("Mixtral-8x7B"), model("mixtral-8x7b")];

        let (found, exactly) = offered(&serving, "mixtral-8x7b").expect("both are there");
        assert_eq!((found.id.as_str(), exactly), ("mixtral-8x7b", true));
    }
}
