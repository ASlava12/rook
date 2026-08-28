//! Every configurable field must be read by something.
//!
//! Four were not, and each was found by hand months apart: `sandbox.allow` did
//! nothing, `allow_outside_workspace` did nothing, `lazy_skills` did nothing,
//! and `lazy_tools` was read but its effect was broken. A knob that does nothing
//! is worse than a missing one, because it is documented and believed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// `upload` exists to make a promise checkable rather than to switch anything
/// on: telemetry has nowhere to go, and a reader looking for the answer finds
/// the field and its comment.
const DELIBERATELY_INERT: &[&str] = &["upload"];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// Field names as they are declared, which is what a reader would write.
fn declared_fields(config_rs: &str) -> BTreeSet<String> {
    config_rs
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_prefix("pub ")?.split(':').next())
        .filter(|name| !name.is_empty() && name.chars().all(|c| c.is_lowercase() || c == '_'))
        .map(str::to_string)
        .collect()
}

fn rust_sources(root: &Path) -> Vec<(PathBuf, String)> {
    ignore::WalkBuilder::new(root)
        .build()
        .flatten()
        .map(|e| e.into_path())
        .filter(|p| p.extension().is_some_and(|e| e == "rs"))
        .filter(|p| !p.starts_with(root.join("references")))
        .filter_map(|p| std::fs::read_to_string(&p).ok().map(|body| (p, body)))
        .collect()
}

#[test]
fn every_config_field_is_read_somewhere() {
    let root = repo_root();
    let config_rs = std::fs::read_to_string(root.join("crates/rook-core/src/config.rs")).unwrap();
    let fields = declared_fields(&config_rs);
    assert!(fields.len() > 20, "the parser found only {} fields, so it is broken", fields.len());

    // Counted rather than matched by file, because a field may legitimately be
    // read only by an accessor next to it. Two mentions is the declaration and
    // the default; a third is somebody using it.
    let sources = rust_sources(&root);
    let unread: Vec<_> = fields
        .iter()
        .filter(|field| !DELIBERATELY_INERT.contains(&field.as_str()))
        .filter(|field| {
            sources.iter().map(|(_, body)| body.matches(field.as_str()).count()).sum::<usize>() < 3
        })
        .collect();

    assert!(unread.is_empty(), "configurable but read by nothing, so setting it does nothing: {unread:?}");
}

/// A server the user turned off was skipped when the agent built its tools and
/// reported as broken by `doctor`, which asked the same question its own way.
#[test]
fn a_disabled_language_server_is_gone_from_every_answer() {
    let config = rook_core::Config {
        lsp: vec![
            rook_lsp::ServerConfig { language: "on".into(), command: "a".into(), ..Default::default() },
            rook_lsp::ServerConfig {
                language: "off".into(),
                command: "b".into(),
                enabled: false,
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let effective = rook_core::lsp::configured(&config);
    assert_eq!(effective.len(), 1, "only the enabled one is asked for");
    assert_eq!(effective[0].language, "on");
}

/// A loop built its own language-server pool and registered the tools from it,
/// so what `equip` handed over afterwards was never what answered: the tools
/// held the pool they were made with. A workspace with no Rust in it was offered
/// rust-analyzer for exactly that reason.
#[test]
fn a_loop_has_no_language_servers_until_a_front_end_gives_it_some() {
    let dir = tempfile::tempdir().unwrap();
    let store = rook_store::Store::open(dir.path()).unwrap();
    let (skills, _) = rook_skills::SkillIndex::discover(&[]);
    let rook = rook_core::Rook::from_parts(
        store,
        rook_core::Config::default(),
        rook_skills::Environment::bare("linux", "x86_64", "0.1.0"),
        skills,
        dir.path().to_path_buf(),
    );
    let session = rook.start_session("unequipped").unwrap();
    let agent = rook_core::agent::AgentLoop::new(&rook, std::sync::Arc::new(Silent), session);

    let offered: Vec<String> = agent.tools.specs().into_iter().map(|t| t.name).collect();
    assert!(
        !offered.iter().any(|n| n == "find_symbol"),
        "a pool built here is rebuilt every turn, and its tools outlive being replaced: {offered:?}"
    );
}

/// The wiring a turn inherits from its front end was written out four times, and
/// `rook run` had two thirds of it: MCP servers but no language servers, so a
/// one-shot turn could not ask the type checker anything the chat could.
#[test]
fn equipping_a_loop_gives_it_both_halves() {
    let dir = tempfile::tempdir().unwrap();
    let store = rook_store::Store::open(dir.path()).unwrap();
    let (skills, _) = rook_skills::SkillIndex::discover(&[]);
    let env = rook_skills::Environment::bare("linux", "x86_64", "0.1.0");
    let rook = rook_core::Rook::from_parts(
        store,
        rook_core::Config::default(),
        env,
        skills,
        dir.path().to_path_buf(),
    );

    let session = rook.start_session("equipped").unwrap();
    let provider = std::sync::Arc::new(Silent);
    let mut agent = rook_core::agent::AgentLoop::new(&rook, provider, session);
    let before = agent.tools.specs().len();

    let servers = rook_core::lsp::Servers::new(
        vec![rook_lsp::ServerConfig {
            language: "rust".into(),
            command: "does-not-need-to-exist".into(),
            extensions: vec!["rs".into()],
            ..Default::default()
        }],
        dir.path(),
    );
    rook_core::agent::equip(&mut agent, servers, &rook_core::McpSession::default());

    let after: Vec<String> = agent.tools.specs().into_iter().map(|t| t.name).collect();
    assert!(after.len() > before, "the language-server tools are the ones being counted: {after:?}");
    assert!(
        after.iter().any(|n| n.contains("diagnostics")),
        "a turn that cannot ask what is wrong with a file is missing the point: {after:?}"
    );
}

struct Silent;

#[async_trait::async_trait]
impl rook_llm::Provider for Silent {
    fn id(&self) -> &str {
        "test/silent"
    }
    fn context_window(&self) -> usize {
        8192
    }
    async fn complete(&self, _: rook_llm::Request) -> rook_llm::Result<rook_llm::Response> {
        Err(rook_llm::LlmError::Other("not called".into()))
    }
}

/// `stopped` is read by `session show`, by the delegation report and by
/// `run --json`, and it was written three ways: two hand-written snake_case
/// strings and the debug spelling of the enum, `EndTurn`, beside them.
#[test]
fn why_a_turn_ended_has_one_vocabulary() {
    let spellings: Vec<&str> = [
        rook_llm::StopReason::EndTurn,
        rook_llm::StopReason::ToolUse,
        rook_llm::StopReason::MaxTokens,
        rook_llm::StopReason::Refusal,
        rook_llm::StopReason::Other,
    ]
    .into_iter()
    .map(|r| r.as_str())
    .collect();

    for spelling in &spellings {
        assert!(
            spelling.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
            "{spelling} is a Rust name reaching a user"
        );
    }
    assert!(spellings.contains(&"end_turn"), "the one the loop writes by hand has to be among them");
    assert!(spellings.contains(&"max_tokens"));
}

/// What every request pays for its tool list, all of it.
///
/// A guard against the drift hermes had to correct: one of their tools reached
/// 924 tokens a call before anyone measured it. It used to live in `rook-tools`,
/// where the six the loop adds are invisible — so it guarded 729 tokens of a
/// list that costs 1,476, and the two largest entries were the ones it could
/// not see.
///
/// The numbers are a ratchet, set just above what the list costs today so the
/// next addition trips them. They have been raised once, from 1,700 and 700,
/// when `verify` and `crate_api` took the list from fourteen tools to sixteen —
/// and only after four descriptions had been trimmed to pay for them.
///
/// `web_fetch` and `web_search` are not in this: they are absent unless `[web]`
/// is on, and this prices the default.
#[test]
fn the_whole_advertised_tool_list_stays_within_a_budget() {
    let dir = tempfile::tempdir().unwrap();
    let (skills, _) = rook_skills::SkillIndex::discover(&[]);
    let cost = |t: &rook_llm::ToolSpec| {
        (t.name.len() + t.description.len() + t.parameters.to_string().len()).div_ceil(4)
    };

    let priced = |lazy: bool| {
        let config = rook_core::Config {
            agent: rook_core::config::AgentConfig { lazy_tools: lazy, ..Default::default() },
            ..Default::default()
        };
        let rook = rook_core::Rook::from_parts(
            rook_store::Store::open(dir.path()).unwrap(),
            config,
            rook_skills::Environment::bare("linux", "x86_64", "0.1.0"),
            skills.clone(),
            dir.path().to_path_buf(),
        );
        let session = rook.start_session("pricing").unwrap();
        let mut agent = rook_core::agent::AgentLoop::new(&rook, std::sync::Arc::new(Silent), session);
        // What an interactive front end advertises, which is the expensive case.
        agent.ask_via(std::sync::Arc::new(rook_tools::ask::NoOne));
        agent.tool_specs().iter().map(cost).sum::<usize>()
    };

    let (full, stubs) = (priced(false), priced(true));
    assert!(
        full < 1_800,
        "the whole list costs ~{full} tokens on every eager request; trim a description or \
         merge an argument before raising this"
    );
    // The number actually paid, since lazy loading is the default.
    assert!(
        stubs < 780,
        "the stubs cost ~{stubs} tokens on every request, which is what is \
         actually paid: lazy loading is the default"
    );
    assert!(
        stubs * 2 < full,
        "stubs ({stubs}) must be much cheaper than full schemas ({full}), or lazy loading buys nothing"
    );
}

/// Everything the agent has ever read, run or been told to remember collects
/// under this directory, and `config.toml` is where an MCP server's API key
/// goes. On a shared machine the default mode hands all of it to every other
/// account.
#[cfg(unix)]
#[test]
fn the_agent_state_directory_is_not_readable_by_other_accounts() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("state");

    rook_core::paths::private_dir(&dir).unwrap();

    let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700, "the state directory must be the owner's alone, not {mode:o}");
}

/// The one tool that leaves the machine, and the point of this agent is that it
/// runs here. Off means the model is never shown it, not that the call is
/// refused: a tool it cannot see is one it cannot decide to try.
#[test]
fn nothing_reaches_the_network_until_the_web_is_turned_on() {
    let offered = |enabled: bool| {
        let dir = tempfile::tempdir().unwrap();
        let config = rook_core::Config {
            web: rook_core::config::WebConfig { enabled, ..Default::default() },
            ..Default::default()
        };
        let rook = rook_core::Rook::from_parts(
            rook_store::Store::open(dir.path()).unwrap(),
            config,
            rook_skills::Environment::bare("linux", "x86_64", "0.1.0"),
            rook_skills::SkillIndex::default(),
            dir.path().to_path_buf(),
        );
        let session = rook.start_session("web").unwrap();
        let agent = rook_core::agent::AgentLoop::new(&rook, std::sync::Arc::new(Nothing), session);
        agent.tool_specs().into_iter().map(|s| s.name).collect::<Vec<_>>()
    };

    assert!(!offered(false).iter().any(|n| n == "web_fetch"), "off is the default and means absent");
    assert!(offered(true).iter().any(|n| n == "web_fetch"), "and on means offered");
}

/// A provider that is never asked anything.
struct Nothing;

#[async_trait::async_trait]
impl rook_llm::Provider for Nothing {
    fn id(&self) -> &str {
        "none/none"
    }
    fn context_window(&self) -> usize {
        8192
    }
    async fn complete(&self, _: rook_llm::Request) -> rook_llm::Result<rook_llm::Response> {
        Err(rook_llm::LlmError::Other("not asked".into()))
    }
}
