//! Which endpoint a model setting names.
//!
//! Every front end asks this same question — the CLI, the TUI, the daemon, the
//! editor bridge, and the agent loop for its summariser and its errands — and
//! each was calling `rook_llm::from_spec_with` with three arguments of its own.
//! One function now, so a name means the same thing everywhere and `[models]`
//! did not have to be wired into eleven places to be read in one.

use rook_llm::{Api, Endpoint, LlmError, Provider};

use crate::config::Config;
use crate::secrets::Vault;

/// Build the provider a setting names.
///
/// A name in `[models]` wins. Anything else is the `provider/model` spelling
/// read from the environment, which is what every configuration written before
/// this table existed is — so nothing already working changes, and the two
/// spellings can sit side by side in one file.
pub fn provider_for(config: &Config, vault: &Vault, name: &str) -> Result<Box<dyn Provider>, LlmError> {
    built(config, vault, name, rook_llm::Prefer::AsConfigured)
}

/// The same, for work that has no conversation to keep.
///
/// A turn stays on the endpoint it was pointed at: moving part-way through
/// throws away the cached prefix of everything it has sent, and hands the next
/// step to a model that did not write the last one. An errand has neither of
/// those to lose, and queueing it behind a turn on one endpoint while another
/// sits idle costs exactly the time this saves — so it goes to whichever has
/// room, and the configured order decides between equals.
pub fn errand_provider_for(
    config: &Config,
    vault: &Vault,
    name: &str,
) -> Result<Box<dyn Provider>, LlmError> {
    built(config, vault, name, rook_llm::Prefer::WhicheverIsFree)
}

fn built(
    config: &Config,
    vault: &Vault,
    name: &str,
    prefer: rook_llm::Prefer,
) -> Result<Box<dyn Provider>, LlmError> {
    let endpoints = endpoints_for(config, vault, name)?;
    rook_llm::from_endpoints_with(endpoints, config.agent.stream_idle(), prefer)
}

/// Every endpoint worth trying for this setting, the one asked for first.
///
/// Then each source carrying a `priority`, in that order. A source without one
/// is not here: it is used when it is named and at no other time, so a paid
/// gateway does not quietly become what the agent reaches for when the machine
/// at home stops answering.
///
/// Ties break on the name so the list is the same on every build. It reaches
/// logs and, before long, a tool description, and an order that shuffles is an
/// order nobody can read twice.
pub fn endpoints_for(config: &Config, vault: &Vault, name: &str) -> Result<Vec<Endpoint>, LlmError> {
    let asked = match endpoint_for(config, vault, name)? {
        Some(endpoint) => endpoint,
        None => {
            // A bare word is not a spec, and `split_spec` does not say so: it
            // reads `home-lmstudio` as an openai-compatible provider serving a
            // model of that name, which becomes whatever `ROOK_LLM_BASE_URL`
            // points at. That is how a mistyped source name became a request to
            // a paid gateway, with no error at all and a listing of the wrong
            // server's models to show for it.
            if !name.contains('/') && !config.models.is_empty() {
                return Err(LlmError::Other(format!(
                    "{name:?} is not one of the endpoints in `[models]`, and it is not a \
                     `provider/model` spec either. Configured: {}.",
                    named_sources(config)
                )));
            }
            rook_llm::endpoint_from_spec(name, config.agent.context_window)?
        }
    };

    let mut after: Vec<(u32, &str)> = config
        .models
        .iter()
        .filter(|(named, _)| named.as_str() != asked.name)
        .filter_map(|(named, source)| source.priority.map(|order| (order, named.as_str())))
        .collect();
    after.sort_unstable();

    let mut endpoints = vec![asked];
    for (_, named) in after {
        match endpoint_for(config, vault, named) {
            Ok(Some(endpoint)) => endpoints.push(endpoint),
            Ok(None) => {}
            // Half a fallback is still better than none of the rest of them.
            Err(why) => tracing::warn!("`[models.{named}]` cannot be a fallback: {why}"),
        }
    }
    Ok(endpoints)
}

/// The configured names, for an error that can name them.
fn named_sources(config: &Config) -> String {
    match config.models.is_empty() {
        true => "none".to_string(),
        false => config.models.keys().cloned().collect::<Vec<_>>().join(", "),
    }
}

/// The provider `[agent] model` names, for a caller that has no vault of its
/// own.
///
/// Six front ends were spelling the same three arguments, and every one of them
/// would have had to learn `[models]` separately. Loading a vault here is a
/// small file read; a caller about to run a turn has one already and passes it
/// to [`provider_for`], because that is the vault whose redaction the key needs
/// to be in.
pub fn configured(config: &Config) -> Result<Box<dyn Provider>, LlmError> {
    let vault = Vault::load().unwrap_or_else(|_| Vault::empty());
    provider_for(config, &vault, &config.agent.model)
}

/// The model a setting actually asks for, whatever spelling it used.
///
/// `rook_llm::split_spec` answered this while every setting was a
/// `provider/model` spec, and a configured name is not one: it reads
/// `home-lmstudio` as an openai-compatible provider serving a model of that
/// name. So `rook models` marked no row and then said the configured model was
/// not offered — about a model sitting in the list it had just printed.
pub fn model_named(config: &Config, name: &str) -> String {
    match config.models.get(name.trim()) {
        Some(source) => source.model.trim().to_string(),
        None => rook_llm::split_spec(name).1.to_string(),
    }
}

/// The endpoint a configured name describes, or `None` where the name is not
/// one of ours and belongs to the older spelling.
///
/// Separate from [`provider_for`] because `rook config check` wants the
/// endpoint without opening a connection: what it reports is whether the file
/// describes something reachable, and half of that is answerable without
/// asking anybody.
pub fn endpoint_for(config: &Config, vault: &Vault, name: &str) -> Result<Option<Endpoint>, LlmError> {
    let name = name.trim();
    let Some(source) = config.models.get(name) else { return Ok(None) };

    let api = Api::parse(&source.api).ok_or_else(|| {
        let known = Api::ALL.map(Api::as_str).join(", ");
        LlmError::Other(match source.api.trim().is_empty() {
            true => format!("`[models.{name}] api` is not set. It has to be one of: {known}."),
            false => format!(
                "`[models.{name}] api` is {:?}, which is not an api this speaks. It has to be \
                 one of: {known}.",
                source.api
            ),
        })
    })?;
    if source.url.trim().is_empty() {
        return Err(LlmError::Other(format!(
            "`[models.{name}] url` is not set, so there is nowhere to send the request. Most \
             openai-compatible servers want `/v1` on the end of it."
        )));
    }
    if source.model.trim().is_empty() {
        return Err(LlmError::Other(format!(
            "`[models.{name}] model` is not set, so there is nothing to ask {} for. \
             `rook models --source {name}` lists what it serves.",
            source.url.trim()
        )));
    }

    Ok(Some(Endpoint {
        name: name.to_string(),
        api,
        url: source.url.trim().to_string(),
        key: key_for(name, &source.key, vault)?,
        model: source.model.trim().to_string(),
        // The source's own first: `[agent] context_window` is one number for
        // whatever the agent is pointed at, and a file naming three endpoints
        // has three different answers to give.
        context_window: source.context_window.or(config.agent.context_window),
        // One unless the file says otherwise. The `provider/model` spelling has
        // nowhere to say it and so has no limit, which is how it has always
        // behaved; a table that can say it defaults to the safe answer.
        parallel: Some(source.parallel.unwrap_or(1)),
        key_in_the_clear: source.key_in_the_clear,
    }))
}

/// Where a source's key comes from.
///
/// The value itself is the last case and not the first, so a key can stay in a
/// keychain or a password manager and only be named here.
///
/// Whatever it turns out to be is handed to the vault for redaction, including
/// the two spellings that never pass through it: a key in `config.toml` or in a
/// variable is a key a turn can print by reading that file or its own
/// environment, and every other credential is already taken back out of what a
/// tool answers.
fn key_for(source: &str, written: &str, vault: &Vault) -> Result<Option<String>, LlmError> {
    let written = written.trim();
    if written.is_empty() {
        return Ok(None);
    }
    if let Some(secret) = written.strip_prefix("secret:") {
        let secret = secret.trim();
        // Named and missing is an error rather than a request sent without a
        // key: the endpoint would answer 401, which reads as a key that is
        // wrong rather than one that was never found.
        return match vault.value(secret) {
            Some(value) => Ok(Some(value)),
            None => Err(LlmError::Other(format!(
                "`[models.{source}] key` names the secret {secret:?}, which is not one \
                 `rook secrets ls` has. Add it with `rook secrets add {secret}`, or point the \
                 key somewhere else."
            ))),
        };
    }
    if let Some(variable) = written.strip_prefix("env:") {
        let variable = variable.trim();
        return match std::env::var(variable).ok().filter(|value| !value.trim().is_empty()) {
            Some(value) => {
                vault.also_hide(&value);
                Ok(Some(value))
            }
            None => Err(LlmError::Other(format!(
                "`[models.{source}] key` names the variable {variable:?}, which is not set in \
                 this process. Set it, or keep the value with `rook secrets add` and write \
                 `secret:<name>` here instead."
            ))),
        };
    }
    vault.also_hide(written);
    Ok(Some(written.to_string()))
}
