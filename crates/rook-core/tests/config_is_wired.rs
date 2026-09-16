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

/// The mirror of the test below it: that one asks whether a field this code
/// declares is read anywhere, and this asks whether a key somebody wrote is a
/// field at all. serde ignores what it does not recognise, so `max_turn_sec`
/// beside `max_turn_secs` is a limit raised in a file and not in the agent,
/// with nothing anywhere saying so. Read off codex's *warn about ignored
/// configuration settings*.
#[test]
fn a_setting_nothing_reads_is_named_rather_than_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "skill_sources = []\n\n[agent]\nmodel = \"x/y\"\nmax_turn_sec = 100\nmax_steps = 7\n\n\
         [sandbox]\nstanse = \"assist\"\n\n[nowhere]\nthing = 1\n",
    )
    .unwrap();

    let ignored = rook_core::Config::ignored_in(&path);
    assert!(ignored.contains(&"agent.max_turn_sec".to_string()), "a misspelled field: {ignored:?}");
    assert!(ignored.contains(&"sandbox.stanse".to_string()), "in any table: {ignored:?}");
    assert!(ignored.contains(&"nowhere".to_string()), "and a table that is not one: {ignored:?}");
    // The precondition: the real ones beside them are not named, or this would
    // pass by calling everything ignored.
    assert!(!ignored.contains(&"agent.model".to_string()), "a real field is not: {ignored:?}");
    assert!(!ignored.contains(&"agent.max_steps".to_string()), "{ignored:?}");
    assert!(!ignored.contains(&"skill_sources".to_string()), "{ignored:?}");

    assert_eq!(rook_core::Config::nearest_to("agent.max_turn_sec").as_deref(), Some("max_turn_secs"));
    assert_eq!(rook_core::Config::nearest_to("sandbox.stanse").as_deref(), Some("stance"));
    // And nothing rather than a guess when nothing is close.
    assert_eq!(rook_core::Config::nearest_to("nowhere"), None);
}

/// A field that is `None` until somebody sets it is still a field.
///
/// The names were read off a default serialised to TOML, which has no null, so
/// a `None` was not written and its key was not there to be found. `[agent]
/// context_window` is the only such field and `doctor` called it a setting
/// nothing reads — to a person who had set it deliberately, with a comment
/// beside it saying why. Deleting it, which is what that advice means, drops
/// the window from what they chose to what gets assumed.
#[test]
fn a_field_that_is_unset_by_default_is_still_a_name_this_knows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[agent]\nmodel = \"x/y\"\ncontext_window = 262144\ncontext_windo = 1\n").unwrap();

    let ignored = rook_core::Config::ignored_in(&path);
    assert!(!ignored.contains(&"agent.context_window".to_string()), "six places read it: {ignored:?}");
    // The precondition, or this passes by having stopped naming anything: the
    // typo beside it is still caught, and still gets its suggestion.
    assert!(ignored.contains(&"agent.context_windo".to_string()), "a typo is still named: {ignored:?}");
    assert_eq!(rook_core::Config::nearest_to("agent.context_windo").as_deref(), Some("context_window"));
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
    rook_core::agent::equip(
        &mut agent,
        servers,
        &rook_core::McpSession::default(),
        rook_core::agent::jobs_for(&rook.config),
    );

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
    // Raised three times. From 1,800: `edit_file` grew a `files` array so a
    // refactor across several files is one call that either lands whole or
    // writes nothing, and the shape has to appear twice because `$ref` is not
    // read the same way by all three dialects. From 1,900: `subagents`, which
    // is how a parent reads and redirects children it left running — a
    // capability with its own verbs rather than an argument on an existing one.
    // From 2,050: `move_file`, because the alternative is reading a file and
    // writing it somewhere else, which retypes every line of it through the
    // model and is where a long one loses one. From 2,150: `web_fetch` and
    // `web_search`, which are in every list now rather than in the lists of
    // people who had turned them on — an agent that cannot look anything up
    // answers from what it was trained on. From 2,300: `docs`, which is four
    // arguments and the difference between an answer about a technology and a
    // recollection of one — its passages carry the page they came from, so what
    // it costs buys a citation rather than a claim. Each time the new
    // description was cut to the bone first; what is left is the shape of the
    // arguments, which a tool cannot be called without.
    assert!(
        full < 2_500,
        "the whole list costs ~{full} tokens on every eager request; trim a description or \
         merge an argument before raising this"
    );
    // The number actually paid, since lazy loading is the default.
    // Raised from 850 for `stance`, which is how the agent asks a person for
    // more latitude — advertised only where there is more to ask for, which is
    // the default. Its first sentence and its `required` were cut first; a
    // stub is mostly the shape of its arguments. Raised from 900 for
    // `move_file`, whose stub is eleven tokens: two argument names and a
    // sentence that has to say the contents are kept, since that is the whole
    // reason to call it rather than read and write. Raised from 950 for the two
    // web tools, which every list carries now: looking something up is not a
    // capability to be configured into existence, and sixteen tokens is what
    // being able to is worth. Raised from 1,000 for `docs`: four argument names
    // and a sentence saying it looks locally first, which is the part that has
    // to reach a model deciding whether to answer from memory.
    assert!(
        stubs < 1_100,
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

/// A choice the model cannot see is one it cannot make, and a choice that does
/// not exist is a question asked in every request that has one answer. So the
/// endpoints appear where there are some and nowhere else — and with the
/// argument they constrain rather than in the system prompt, which is the front
/// of every request and where prompt caching matches.
#[test]
fn the_endpoints_a_sub_task_may_be_sent_to_are_named_only_where_there_are_some() {
    let delegate = |models: std::collections::BTreeMap<String, rook_core::ModelSource>| {
        let dir = tempfile::tempdir().unwrap();
        // The full schema, which is what a model fetches before it calls.
        // Lazily, a spec is cut to its first sentence and a stub of its
        // arguments — the first attempt at this put the names in the tool's
        // description and they were dropped exactly there.
        let config = rook_core::Config {
            models,
            agent: rook_core::config::AgentConfig { lazy_tools: false, ..Default::default() },
            ..Default::default()
        };
        let rook = rook_core::Rook::from_parts(
            rook_store::Store::open(dir.path()).unwrap(),
            config,
            rook_skills::Environment::bare("linux", "x86_64", "0.1.0"),
            rook_skills::SkillIndex::default(),
            dir.path().to_path_buf(),
        );
        let session = rook.start_session("endpoints").unwrap();
        let agent = rook_core::agent::AgentLoop::new(&rook, std::sync::Arc::new(Nothing), session);
        agent
            .tool_specs()
            .into_iter()
            .find(|spec| spec.name == "delegate")
            .expect("delegate is always offered")
    };

    let bare = delegate(Default::default());
    assert!(bare.parameters["properties"].get("model").is_none(), "nothing to choose between");

    let mut named = std::collections::BTreeMap::new();
    named.insert(
        "next-door".to_string(),
        rook_core::ModelSource {
            model: "qwen3-coder:30b".into(),
            api: "openai".into(),
            url: "http://127.0.0.1:1234/v1".into(),
            ..Default::default()
        },
    );
    let offered = delegate(named);

    let choice = &offered.parameters["properties"]["model"];
    assert!(choice.is_object(), "the field is there");
    assert_eq!(choice["enum"], serde_json::json!(["next-door"]), "and it names what there is");
}

/// A source is a table whose name somebody chose, so the walk that names unread
/// settings cannot compare it to a template — and skipping it whole meant
/// `ur1 = "http://…"` was a source with no address, silently, because serde
/// fills the rest from defaults and says nothing about the leftover. What is
/// inside one is an ordinary struct, and a typo there is the same mistake this
/// exists to catch.
#[test]
fn a_typo_inside_a_model_source_is_named_like_any_other() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let written = concat!(
        "[models.next-door]\n",
        "mode = \"qwen3-coder:30b\"\n",
        "api = \"openai\"\n",
        "ur1 = \"http://127.0.0.1:1234/v1\"\n",
        "parallel = 2\n",
    );
    std::fs::write(&path, written).unwrap();

    let ignored = rook_core::Config::ignored_in(&path);

    assert!(ignored.contains(&"models.next-door.ur1".to_string()), "{ignored:?}");
    assert!(ignored.contains(&"models.next-door.mode".to_string()), "{ignored:?}");
    // The precondition: the real ones beside them are not named, or this would
    // pass by calling every source wrong.
    assert!(!ignored.contains(&"models.next-door.api".to_string()), "{ignored:?}");
    assert!(!ignored.contains(&"models.next-door.parallel".to_string()), "{ignored:?}");
    // And the name of the source itself is not a setting to be reported.
    assert!(!ignored.iter().any(|name| name == "models" || name == "models.next-door"), "{ignored:?}");

    // Suggested for, which needed the lookup to walk the path a segment at a
    // time: asking for a key literally named `models.next-door` found nothing,
    // so a source could never be suggested for at all.
    assert_eq!(rook_core::Config::nearest_to("models.next-door.mode").as_deref(), Some("model"));
}

/// A configuration people keep is half comments — why a setting is off, which
/// machine an address belongs to, what to do when travelling. Writing the
/// loaded struct back deletes every one of them, which is why this edits the
/// document instead.
#[test]
fn setting_one_value_leaves_the_rest_of_the_file_where_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let written = concat!(
        "# Why this file looks the way it does.\n",
        "[agent]\n",
        "# Left off deliberately: this LM Studio keeps one model resident.\n",
        "# errand_model = \"lmstudio/gemma\"\n",
        "model = \"home-llama\"\n",
        "max_steps = 200\n",
        "\n",
        "[models.home-llama]\n",
        "# In secrets.toml at 0600, not here: this file is in a repository.\n",
        "key = \"secret:home-llama\"\n",
    );
    std::fs::write(&path, written).unwrap();

    rook_core::Config::set_in(&path, "agent.max_steps", "7").unwrap();
    let after = std::fs::read_to_string(&path).unwrap();

    assert!(after.contains("max_steps = 7"), "the value changed: {after}");
    for kept in [
        "# Why this file looks the way it does.",
        "# Left off deliberately: this LM Studio keeps one model resident.",
        "# errand_model = \"lmstudio/gemma\"",
        "# In secrets.toml at 0600, not here: this file is in a repository.",
    ] {
        assert!(after.contains(kept), "{kept:?} was lost:\n{after}");
    }
    // And a whole number rather than the text of one, which is what a command
    // line hands over: the shape comes from the defaults, not from the look of
    // the value.
    assert!(!after.contains("max_steps = \"7\""), "written as a string:\n{after}");
    assert_eq!(rook_core::Config::load_from(path.clone()).unwrap().agent.max_steps, 7);

    // A name that is not a setting is refused before the file is touched, and
    // the file is the proof: a check that only reported would leave a typo
    // written down.
    let before = std::fs::read_to_string(&path).unwrap();
    let why = rook_core::Config::set_in(&path, "agent.max_step", "9").unwrap_err();
    assert!(why.contains("max_steps"), "it suggests the real one: {why}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before, "and changed nothing");

    // So is a value the setting cannot hold, and by the code that will read it
    // rather than by a second opinion about types.
    let why = rook_core::Config::set_in(&path, "agent.max_steps", "lots").unwrap_err();
    assert!(why.contains("max_steps"), "{why}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before, "and changed nothing");
}
