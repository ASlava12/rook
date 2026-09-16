//! Which endpoint a model setting names.
//!
//! The claims here are about a file: what `[models.<name>]` has to say to be
//! usable, where its key is allowed to come from, and that a configuration
//! written before the table existed still means exactly what it meant.

use rook_core::{Config, ModelSource, Vault};

const KEY: &str = "sk-not-a-real-key-at-all";

fn serving(api: &str, url: &str, key: &str) -> ModelSource {
    ModelSource {
        model: "qwen3-coder:30b".into(),
        api: api.into(),
        url: url.into(),
        key: key.into(),
        ..Default::default()
    }
}

fn naming(name: &str, source: ModelSource) -> Config {
    let mut config = Config::default();
    config.models.insert(name.into(), source);
    config
}

/// The whole point of the table: an endpoint the environment has no variable
/// for. There is one `LMSTUDIO_HOST`, so before this there was one machine.
#[test]
fn a_named_source_is_where_the_request_goes() {
    let config = naming("next-door", serving("openai", "http://192.168.1.46:1234/v1", ""));

    let endpoint = rook_core::models::endpoint_for(&config, &Vault::empty(), "next-door")
        .expect("a source the file describes")
        .expect("a name the file has");

    assert_eq!(endpoint.url, "http://192.168.1.46:1234/v1");
    assert_eq!(endpoint.model, "qwen3-coder:30b");
    assert_eq!(endpoint.api, rook_llm::Api::OpenAi);
    assert!(endpoint.key.is_none(), "nothing named a key, so none is sent");
}

/// Every configuration written before the table is one of these, so this is
/// the claim that nothing already working stopped working.
#[test]
fn a_spec_that_is_not_a_configured_name_is_still_read_from_the_environment() {
    let config = naming("next-door", serving("openai", "http://192.168.1.46:1234/v1", ""));
    let vault = Vault::empty();

    let described = rook_core::models::endpoint_for(&config, &vault, "ollama/qwen3:8b").unwrap();
    assert!(described.is_none(), "a provider/model spec is not one of ours to describe");

    let built = rook_core::models::provider_for(&config, &vault, "ollama/qwen3:8b")
        .expect("the older spelling still builds");
    assert_eq!(built.id(), "ollama/qwen3:8b");
}

/// A source's window rather than the agent's: a file naming three endpoints has
/// three different answers to give, and `[agent] context_window` has room for
/// one. Asserted through the provider, so this is also the claim that what the
/// table describes is what the request is actually budgeted against.
#[test]
fn a_sources_own_window_is_what_the_budget_is_made_against() {
    let mut source = serving("openai", "http://127.0.0.1:1234/v1", "");
    source.context_window = Some(262_144);
    let mut config = naming("big", source);
    config.agent.context_window = Some(8_192);

    let built = rook_core::models::provider_for(&config, &Vault::empty(), "big").unwrap();

    assert_eq!(built.context_window(), 262_144, "the source's own, not the agent-wide one");
}

#[test]
fn a_key_kept_as_a_secret_never_appears_in_the_config_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut vault = Vault::load_from(dir.path().join("secrets.toml")).unwrap();
    vault.keep("gateway", KEY).unwrap();
    let config = naming("gw", serving("openai", "https://gw.example/v1", "secret:gateway"));

    let endpoint = rook_core::models::endpoint_for(&config, &vault, "gw").unwrap().unwrap();

    assert_eq!(endpoint.key.as_deref(), Some(KEY));
}

/// Sent without a key, the endpoint answers 401 — which reads as a key that is
/// wrong rather than one that was never found, and sends somebody to look at
/// the wrong thing.
#[test]
fn a_secret_that_is_not_there_says_so_rather_than_sending_no_key() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::load_from(dir.path().join("secrets.toml")).unwrap();
    let config = naming("gw", serving("openai", "https://gw.example/v1", "secret:gateway"));

    let why = rook_core::models::endpoint_for(&config, &vault, "gw").unwrap_err().to_string();

    assert!(why.contains("gateway"), "it has to name the secret: {why}");
    assert!(why.contains("rook secrets add gateway"), "and what to do about it: {why}");
}

#[test]
fn a_variable_that_is_not_set_names_the_variable() {
    let config = naming("gw", serving("openai", "https://gw.example/v1", "env:ROOK_TEST_KEY_NOBODY_SETS"));

    let why = rook_core::models::endpoint_for(&config, &Vault::empty(), "gw").unwrap_err().to_string();

    assert!(why.contains("ROOK_TEST_KEY_NOBODY_SETS"), "it has to name the variable: {why}");
}

/// `config.toml` is not `secrets.toml`: it is not 0600, and it is the file a
/// turn reads when somebody asks why the agent is configured the way it is. A
/// key written there is one `cat` away from the transcript.
#[test]
fn a_key_written_in_the_file_is_taken_back_out_of_what_a_tool_answers() {
    let vault = Vault::empty();
    let config = naming("gw", serving("openai", "https://gw.example/v1", KEY));
    let printed = format!("[models.gw]\nurl = \"https://gw.example/v1\"\nkey = \"{KEY}\"\n");

    // Nothing is hidden until the key has been resolved, or this would pass
    // with the resolution doing nothing at all.
    assert!(vault.redact(&printed).contains(KEY), "the vault cannot know it yet");

    rook_core::models::endpoint_for(&config, &vault, "gw").unwrap().unwrap();

    assert!(
        !vault.redact(&printed).contains(KEY),
        "a `cat config.toml` would have put the key in the transcript: {}",
        vault.redact(&printed)
    );
}

#[test]
fn an_api_this_does_not_speak_is_refused_with_the_ones_it_does() {
    let config = naming("gw", serving("ollama", "http://127.0.0.1:11434/v1", ""));

    let why = rook_core::models::endpoint_for(&config, &Vault::empty(), "gw").unwrap_err().to_string();

    assert!(why.contains("ollama"), "it has to quote what was written: {why}");
    for api in rook_llm::Api::ALL {
        assert!(why.contains(api.as_str()), "and list {}: {why}", api.as_str());
    }
}

/// Half a source is worse than none: it builds a request and sends it
/// somewhere, and what comes back is about the request rather than the file.
#[test]
fn a_source_missing_what_it_needs_says_which_field() {
    let cases = [
        ("url", serving("openai", "", "")),
        ("model", ModelSource { model: String::new(), ..serving("openai", "http://127.0.0.1:1234/v1", "") }),
    ];

    for (field, source) in cases {
        let config = naming("gw", source);
        let why = rook_core::models::endpoint_for(&config, &Vault::empty(), "gw").unwrap_err().to_string();
        assert!(why.contains(&format!("[models.gw] {field}")), "it has to name the field: {why}");
    }
}

/// `rook models` marks the row that is configured, and says so when none of
/// them is. Read through `split_spec` a source name is a model name, so it
/// marked nothing and then reported the configured model missing from a list it
/// was sitting in.
#[test]
fn the_model_a_source_names_is_the_one_a_listing_looks_for() {
    let config = naming("next-door", serving("openai", "http://127.0.0.1:9998/v1", ""));

    assert_eq!(rook_core::models::model_named(&config, "next-door"), "qwen3-coder:30b");
    assert_eq!(
        rook_core::models::model_named(&config, "lmstudio/qwen3-coder:30b"),
        "qwen3-coder:30b",
        "and the older spelling still answers the same thing"
    );
}

/// Home, work, and the machine at home switched off are three sets of reachable
/// endpoints and one file. The order it falls through is the file's to state.
#[test]
fn the_one_asked_for_comes_first_and_the_rest_follow_their_priority() {
    let mut config = Config::default();
    for (name, order) in [("gw", Some(3)), ("next-door", Some(1)), ("upstairs", Some(2))] {
        let mut source = serving("openai", &format!("http://127.0.0.1:1234/{name}"), "");
        source.priority = order;
        config.models.insert(name.into(), source);
    }

    let tried = rook_core::models::endpoints_for(&config, &Vault::empty(), "upstairs").unwrap();
    let order: Vec<&str> = tried.iter().map(|e| e.name.as_str()).collect();

    assert_eq!(order, ["upstairs", "next-door", "gw"], "the named one first, then by priority");
}

/// An endpoint that costs money per token must not become what the agent
/// reaches for because the machine at home is off. Being in the rotation is a
/// decision, so it is spelled.
#[test]
fn a_source_with_no_priority_is_only_ever_used_when_it_is_named() {
    let mut config = Config::default();
    let mut local = serving("openai", "http://127.0.0.1:9998/v1", "");
    local.priority = Some(1);
    config.models.insert("next-door".into(), local);
    config.models.insert("paid".into(), serving("openai", "https://gw.example/v1", ""));

    let tried = rook_core::models::endpoints_for(&config, &Vault::empty(), "next-door").unwrap();
    let order: Vec<&str> = tried.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(order, ["next-door"], "nothing was told to stand behind it");

    let named = rook_core::models::endpoints_for(&config, &Vault::empty(), "paid").unwrap();
    assert_eq!(named[0].name, "paid", "and naming it outright still works");
}

/// The older spelling is not a second-class citizen: a spec can be the
/// preferred endpoint and still have configured ones behind it.
#[test]
fn a_provider_model_spec_can_have_configured_endpoints_behind_it() {
    let mut config = Config::default();
    let mut local = serving("openai", "http://127.0.0.1:9998/v1", "");
    local.priority = Some(1);
    config.models.insert("next-door".into(), local);

    let tried = rook_core::models::endpoints_for(&config, &Vault::empty(), "ollama/qwen3:8b").unwrap();
    let order: Vec<&str> = tried.iter().map(|e| e.name.as_str()).collect();

    assert_eq!(order, ["ollama/qwen3:8b", "next-door"]);
}

/// A list exists because some of it may be unusable today. One fallback whose
/// key has gone missing must not take the working endpoint down with it.
#[test]
fn a_fallback_that_cannot_be_built_is_left_out_rather_than_fatal() {
    let mut config = Config::default();
    let mut broken = serving("openai", "https://gw.example/v1", "secret:nobody-kept-this");
    broken.priority = Some(1);
    config.models.insert("broken".into(), broken);
    let mut fine = serving("openai", "http://127.0.0.1:9998/v1", "");
    fine.priority = Some(2);
    config.models.insert("fine".into(), fine);
    config.models.insert("asked".into(), serving("openai", "http://127.0.0.1:9999/v1", ""));

    let tried = rook_core::models::endpoints_for(&config, &Vault::empty(), "asked").unwrap();
    let order: Vec<&str> = tried.iter().map(|e| e.name.as_str()).collect();

    assert_eq!(order, ["asked", "fine"], "the broken one is skipped and the rest stand");
}

/// How a mistyped source name became a request to a paid gateway: `split_spec`
/// reads a bare word as an openai-compatible provider serving a model of that
/// name, which is whatever `ROOK_LLM_BASE_URL` points at. Silently.
#[test]
fn a_bare_name_that_is_not_a_configured_source_is_refused_by_name() {
    let config = naming("next-door", serving("openai", "http://127.0.0.1:9998/v1", ""));

    let why = rook_core::models::endpoints_for(&config, &Vault::empty(), "nextdoor").unwrap_err().to_string();

    assert!(why.contains("nextdoor"), "it has to quote what was written: {why}");
    assert!(why.contains("next-door"), "and list what there is: {why}");
}

/// With no table at all, a bare word means what it has always meant. Somebody
/// who has not configured `[models]` is not making this mistake.
#[test]
fn a_bare_name_still_means_what_it_did_where_nothing_is_configured() {
    let config = Config::default();

    let tried = rook_core::models::endpoints_for(&config, &Vault::empty(), "ollama/qwen3:8b").unwrap();

    assert_eq!(tried.len(), 1);
    assert_eq!(tried[0].name, "ollama/qwen3:8b");
}

/// `[agent] model` always says something, because an unconfigured install is
/// still meant to work — so the difference between a choice and the shipped
/// guess is not visible in the loaded configuration at all. It is visible in
/// the file, and that is the only place to look.
#[test]
fn a_model_nobody_chose_is_told_apart_from_one_somebody_did() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let with_sources = concat!(
        "[models.next-door]\n",
        "model = \"qwen3-coder:30b\"\n",
        "api = \"openai\"\n",
        "url = \"http://127.0.0.1:1234/v1\"\n",
    );

    std::fs::write(&path, with_sources).unwrap();
    let config = rook_core::Config::load_from(path.clone()).unwrap();
    assert_eq!(
        rook_core::models::unchosen(&config, &path).as_deref(),
        Some(["next-door".to_string()].as_slice()),
        "the file names endpoints and no model, so there is something to ask about"
    );

    // Named, and the question does not arise however little the name resembles
    // a considered choice.
    std::fs::write(&path, format!("[agent]\nmodel = \"next-door\"\n\n{with_sources}")).unwrap();
    let config = rook_core::Config::load_from(path.clone()).unwrap();
    assert!(rook_core::models::unchosen(&config, &path).is_none(), "somebody chose");

    // And nothing to offer is nothing to ask: an install with no `[models]` has
    // only the guess, and the error that guess produces is already the right
    // one. A second complaint about the same file is noise.
    std::fs::write(&path, "[agent]\nmax_steps = 7\n").unwrap();
    let config = rook_core::Config::load_from(path.clone()).unwrap();
    assert!(rook_core::models::unchosen(&config, &path).is_none(), "nothing to choose between");
}
