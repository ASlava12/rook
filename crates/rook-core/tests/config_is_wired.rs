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
