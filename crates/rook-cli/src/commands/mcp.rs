//! MCP server configuration and probing.

use crate::args::McpCmd;
use crate::fmt;
use anyhow::{Context, Result};
use rook_core::McpSession;
use std::path::PathBuf;

/// What the agent would budget against: the override if there is one, else what
/// the provider says its model holds.
pub(crate) fn cmd_mcp(workspace: Option<PathBuf>, cmd: McpCmd, json: bool) -> Result<()> {
    if matches!(cmd, McpCmd::Status | McpCmd::Reconnect { .. }) {
        let daemon = crate::source::Daemon::running().context(
            "no rookd is running; start it with rook daemon start (for a standalone chat use /mcp)",
        )?;
        let workspace = workspace.unwrap_or(std::env::current_dir()?).canonicalize()?;
        let query = crate::remote::escaped(&workspace.to_string_lossy());
        let report: rook_core::mcp_connections::Report = match cmd {
            McpCmd::Reconnect { server } => daemon.post(
                &format!("/api/mcp/{}/reconnect?workspace={query}", crate::remote::escaped(&server)),
                &serde_json::json!({}),
            )?,
            _ => daemon.get(&format!("/api/mcp?workspace={query}"))?,
        };
        println!("{}", if json { serde_json::to_string_pretty(&report)? } else { describe(&report) });
        return Ok(());
    }
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async move {
        // Before the store is opened, and before the check that a server is
        // configured. Serving needs neither: the tools it offers read files and
        // run commands, so all it wants is the configuration and a directory —
        // and opening the store would stop it running beside a daemon holding
        // it, which is exactly when somebody wants both.
        if let McpCmd::Serve { yes } = cmd {
            let config = rook_core::Config::load()?;
            let here = workspace
                .clone()
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| PathBuf::from("."));
            let policy = rook_core::agent::policy_for(&config);
            if yes {
                policy.set_stance(rook_tools::policy::Stance::Autonomous);
            }
            let mut tools = rook_tools::ToolBox::standard();
            rook_core::lsp::register(&mut tools, rook_core::agent::servers_for(&config, &here));
            // Served for as long as stdin is open, which is a session like any
            // other, so a command left running is one this can keep.
            tools.register(std::sync::Arc::new(rook_tools::jobs::JobTool));
            let mut ctx = rook_core::agent::tool_context(&config, &here, &rook_core::paths::output_dir());
            ctx.jobs = Some(rook_core::agent::jobs_for(&config));
            eprintln!("rook mcp: offering {} tools from {}", tools.names().len(), here.display());
            return rook_core::mcp_server::serve(rook_core::mcp_server::Offered {
                tools,
                ctx,
                policy,
                approver: std::sync::Arc::new(rook_tools::policy::Unattended),
            })
            .await
            .map_err(Into::into);
        }

        let here = workspace.or_else(|| std::env::current_dir().ok()).unwrap_or_else(|| PathBuf::from("."));
        let config = rook_core::Config::load_for(&here)?;
        let (plugins, errors) = rook_core::plugins::discover(&here);
        for error in errors {
            eprintln!("{error}");
        }
        let servers = config.mcp.iter()
            .chain(plugins.iter().flat_map(|plugin| &plugin.mcp));

        if let McpCmd::Login { server } | McpCmd::Logout { server } = &cmd {
            let declarations: Vec<_> = servers.take(config.mcp_connections.max_servers.saturating_add(1)).collect();
            anyhow::ensure!(declarations.len() <= config.mcp_connections.max_servers, "too many MCP declarations");
            let matching: Vec<_> = declarations.iter().filter(|entry| &entry.name == server).collect();
            anyhow::ensure!(matching.len() == 1, "expected exactly one configured MCP server named {server:?}");
            let declaration = *matching[0];
            if matches!(cmd, McpCmd::Login { .. }) {
                rook_core::mcp_auth::login_native(declaration, &config.proxy.for_mcp(), config.mcp_connections, |url| {
                    eprintln!("Open this URL in a browser on this computer:\n{url}\nWaiting for sign-in…");
                }).await.map_err(anyhow::Error::msg)?;
                if json { println!("{}", serde_json::json!({"server":server,"authenticated":true,"reconnect_required":true})); }
                else { println!("Signed in. Use rook mcp reconnect {server} for a running daemon, or reopen the standalone session."); }
            } else {
                rook_core::mcp_auth::logout(declaration, config.mcp_connections).await.map_err(anyhow::Error::msg)?;
                if json { println!("{}", serde_json::json!({"server":server,"authenticated":false})); }
                else { println!("Stored OAuth credentials removed. Already dispatched requests may finish."); }
            }
            return anyhow::Ok(());
        }

        if servers.clone().next().is_none() && matches!(cmd, McpCmd::Ls) && !json {
            println!("no servers configured. Add one to {}:\n", rook_core::paths::config_file().display());
            println!("  [[mcp]]\n  name = \"filesystem\"\n  command = \"npx\"\n  args = [\"-y\", \"@modelcontextprotocol/server-filesystem\", \".\"]");
            return anyhow::Ok(());
        }
        let session = McpSession::connect(servers, &config.proxy.for_mcp(), config.mcp_connections).await;
        let connected_servers = session.servers();

        match cmd {
            McpCmd::Serve { .. } | McpCmd::Status | McpCmd::Reconnect { .. } | McpCmd::Login { .. } | McpCmd::Logout { .. } => unreachable!("handled above"),
            McpCmd::Ls => {
                if json {
                    let items: Vec<_> = connected_servers.iter().map(|(s, tools)| serde_json::json!({
                        "name": s.name(), "server": s.info().server, "tools": tools.len(),
                    })).collect();
                    println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                        "connected": items,
                        "failed": session.failures().iter().map(|(n, e)| serde_json::json!({"name": n, "error": e})).collect::<Vec<_>>(),
                    }))?);
                } else {
                    let rows: Vec<Vec<String>> = connected_servers.iter().map(|(s, tools)| vec![
                        s.name().to_string(),
                        format!("{} {}", s.info().server.name, s.info().server.version),
                        s.info().protocol_version.clone(),
                        tools.len().to_string(),
                    ]).collect();
                    print!("{}", fmt::table(&["name", "server", "protocol", "tools"], &rows));
                    for (name, error) in &session.failures() {
                        println!("\n✗ {name}: {error}");
                    }
                }
            }
            McpCmd::Tools { server } => {
                let (_, tools) = connected_servers.iter().find(|(s, _)| s.name() == server)
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
                let (connected, _) = connected_servers.iter().find(|(s, _)| s.name() == server)
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

/// CLI and the TUI use the same vocabulary for installed connection state.
pub(crate) fn describe(report: &rook_core::mcp_connections::Report) -> String {
    let mut text = String::new();
    if let Some(issue) = &report.issue {
        text.push_str(issue);
        text.push('\n');
    }
    if report.servers.is_empty() {
        text.push_str("no MCP servers installed; add one with rook config edit\n");
    }
    for server in &report.servers {
        use std::fmt::Write;
        let _ = writeln!(
            text,
            "{} — {}{} · {} tools · {} active requests · generation {}",
            server.name,
            server.state,
            if server.reconnecting { " (reconnecting)" } else { "" },
            server.tools,
            server.active_requests,
            server.generation
        );
        if let Some(error) = &server.last_request_error {
            let _ = writeln!(text, "  last request: {error}");
        }
        if let Some(error) = &server.error {
            let _ = writeln!(text, "  {error}");
        }
    }
    text
}
