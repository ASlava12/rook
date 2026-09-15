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
    let agent = &config.agent;
    match endpoint_for(config, vault, name)? {
        Some(endpoint) => rook_llm::from_endpoint_with(endpoint, agent.stream_idle()),
        None => rook_llm::from_spec_with(name, agent.stream_idle(), agent.context_window),
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
