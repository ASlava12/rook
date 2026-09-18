//! Language-server discovery and installation.

use crate::args::LspCmd;
use crate::workspace_of;
use anyhow::{Result, bail};
use rook_core::AGENT_VERSION;
use std::path::PathBuf;

pub(crate) fn cmd_lsp(workspace: Option<PathBuf>, cmd: LspCmd, json: bool) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async move {
        // No store: a language server is a file under the state directory and
        // the configuration is a file too. Opening one meant `rook lsp install`
        // refused while `rookd` was up — which is when somebody is working and
        // notices a server missing.
        let here = workspace_of(&workspace);
        let config = rook_core::Config::load()?;
        let env = rook_skills::Environment::detect(AGENT_VERSION);
        if let LspCmd::Update = &cmd {
            let into = rook_core::paths::servers_dir();
            let installer = rook_core::install::Installer::new(into, &config.proxy.for_install())
                .map_err(anyhow::Error::msg)?;
            let report = installer.update(&env).await;
            if report.is_empty() {
                println!("nothing installed under {}", rook_core::paths::servers_dir().display());
            }
            for (command, said) in report {
                match said {
                    Ok(said) => println!("  ✓ {command:<28} {said}"),
                    Err(why) => println!("  ✗ {command:<28} {why}"),
                }
            }
            return anyhow::Ok(());
        }
        if let LspCmd::Install { name } = &cmd {
            let Some(recipe) = rook_core::install::recipe_for(name) else {
                anyhow::bail!(
                    "no recipe for {name:?} — this can install rust-analyzer, clangd, \
                     typescript-language-server, pyright-langserver and gopls, by those names or by \
                     language"
                );
            };
            let into = rook_core::paths::servers_dir();
            let installer = rook_core::install::Installer::new(into, &config.proxy.for_install())
                .map_err(anyhow::Error::msg)?;
            let done = installer.install(recipe, &env).await.map_err(anyhow::Error::msg)?;
            println!("installed {} {} at {}", done.command, done.tag, done.path.display());
            println!("verified:     {}", done.verified);
            println!("not verified: {}", done.unverified);
            return anyhow::Ok(());
        }
        // What the agent would have, not what is installed: the comment below
        // claims this cannot drift from a turn, and a copy of the expression it
        // was built from is how it did.
        let configs = rook_core::lsp::for_workspace(&config, &here);
        if configs.is_empty() {
            bail!(
                "no language server applies here — none is configured under [[lsp]], or none of \
                 the ones on PATH handles a file in {}",
                here.display()
            );
        }
        let servers = rook_core::lsp::Servers::new(configs, &here);

        // The tools are the same ones the agent calls, so this cannot drift
        // from what a turn would see.
        let mut tools = rook_tools::ToolBox::default();
        rook_core::lsp::register(&mut tools, servers.clone());
        let ctx = rook_tools::ToolContext::new(here.clone());

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
            // Answered above, before any server was started: installing one is
            // the one thing here that must not need one running.
            LspCmd::Install { .. } | LspCmd::Update => {
                unreachable!("install and update return before the servers are built")
            }
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
