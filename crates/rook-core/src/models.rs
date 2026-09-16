//! Which endpoint a model setting names.
//!
//! Every front end asks this same question — the CLI, the TUI, the daemon, the
//! editor bridge, and the agent loop for its summariser and its errands — and
//! each was calling `rook_llm::from_spec_with` with three arguments of its own.
//! One function now, so a name means the same thing everywhere and `[models]`
//! did not have to be wired into eleven places to be read in one.

use rook_llm::{Api, Endpoint, LlmError, Provider};

use crate::config::{Config, ModelSource};
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

/// Endpoints the file describes that no source asks for.
///
/// Inert rather than wrong, and worth saying for the reason a config field
/// nothing reads is worth failing a build over: it looks like a working
/// address. The two ways to get one are a source deleted and its endpoint
/// left behind, and a `[models.<name>] endpoint` that does not quite spell
/// it — and the second is a typo the agent has already refused a turn over.
pub fn unpointed(config: &Config) -> Vec<String> {
    let asked: std::collections::BTreeSet<&str> =
        config.models.values().map(|source| source.endpoint.trim()).filter(|at| !at.is_empty()).collect();
    config.endpoints.keys().filter(|name| !asked.contains(name.as_str())).cloned().collect()
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
    chosen(config, None)
}

/// The same, for a session that has been switched to another endpoint.
///
/// `None` is the configured one, which is what every turn ran on before
/// anything could be switched — so a front end that does not offer the switch
/// passes it and behaves as it did.
pub fn chosen(config: &Config, named: Option<&str>) -> Result<Box<dyn Provider>, LlmError> {
    let vault = Vault::load().unwrap_or_else(|_| Vault::empty());
    provider_for(config, &vault, named.unwrap_or(&config.agent.model))
}

/// Whether a name can be run on, and why not where it cannot.
///
/// The same question `chosen` will ask when the turn starts, asked early so
/// that switching to a name with a typo in it says so at once rather than at
/// the top of the next turn — and asked through the same function, so there is
/// one answer to what a usable endpoint is.
pub fn usable(config: &Config, named: &str) -> Result<(), String> {
    let vault = Vault::load().unwrap_or_else(|_| Vault::empty());
    endpoints_for(config, &vault, named).map(|_| ()).map_err(|why| why.to_string())
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
    let address = address_of(config, name, source)?;
    let at = &address.at;

    let api = Api::parse(address.api).ok_or_else(|| {
        let known = Api::ALL.map(Api::as_str).join(", ");
        LlmError::Other(match address.api.trim().is_empty() {
            true => format!("`[{at}] api` is not set. It has to be one of: {known}."),
            false => format!(
                "`[{at}] api` is {:?}, which is not an api this speaks. It has to be one of: \
                 {known}.",
                address.api
            ),
        })
    })?;
    if address.url.trim().is_empty() {
        return Err(LlmError::Other(format!(
            "`[{at}] url` is not set, so there is nowhere to send the request. Most \
             openai-compatible servers want `/v1` on the end of it."
        )));
    }
    if source.model.trim().is_empty() {
        return Err(LlmError::Other(format!(
            "`[models.{name}] model` is not set, so there is nothing to ask {} for. \
             `rook models --source {name}` lists what it serves.",
            address.url.trim()
        )));
    }

    Ok(Some(Endpoint {
        name: name.to_string(),
        api,
        url: address.url.trim().to_string(),
        key: key_for(at, address.key, vault)?,
        model: source.model.trim().to_string(),
        // The source's own first: `[agent] context_window` is one number for
        // whatever the agent is pointed at, and a file naming three endpoints
        // has three different answers to give.
        context_window: source.context_window.or(config.agent.context_window),
        // One unless the file says otherwise. The `provider/model` spelling has
        // nowhere to say it and so has no limit, which is how it has always
        // behaved; a table that can say it defaults to the safe answer.
        parallel: Some(address.parallel.unwrap_or(1)),
        key_in_the_clear: address.key_in_the_clear,
        queue: address.queue,
    }))
}

/// Where a source's requests go, from wherever the file wrote it down.
struct Address<'a> {
    /// `models.home-llama` or `endpoints.desk`, whichever table the address is
    /// actually in, so a complaint about it names the line somebody has to
    /// open — and so does the secret it looks for.
    at: String,
    api: &'a str,
    url: &'a str,
    key: &'a str,
    parallel: Option<usize>,
    key_in_the_clear: bool,
    /// The queue, where it is shared. A source carrying its own address has one
    /// of its own and says `None`; several sources naming one endpoint all say
    /// that endpoint, and so wait in one line at the server.
    queue: Option<String>,
}

/// Either the endpoint a source names or the one it spells out itself.
fn address_of<'a>(config: &'a Config, name: &str, source: &'a ModelSource) -> Result<Address<'a>, LlmError> {
    let named = source.endpoint.trim();
    if named.is_empty() {
        return Ok(Address {
            at: format!("models.{name}"),
            api: &source.api,
            url: &source.url,
            key: &source.key,
            parallel: source.parallel,
            key_in_the_clear: source.key_in_the_clear,
            queue: None,
        });
    }

    let Some(endpoint) = config.endpoints.get(named) else {
        let known = match config.endpoints.keys().cloned().collect::<Vec<_>>() {
            names if names.is_empty() => "there are no `[endpoints]` in this file".to_string(),
            names => format!("the ones there are: {}", names.join(", ")),
        };
        return Err(LlmError::Other(format!(
            "`[models.{name}] endpoint` is {named:?}, and no `[endpoints.{named}]` says where that \
             is — {known}."
        )));
    };

    // Both halves written is refused rather than resolved: one of the two is
    // being ignored, and a rule about which would be a rule nobody remembers
    // when the request goes to the wrong machine.
    let also: Vec<&str> = [
        (!source.api.trim().is_empty()).then_some("api"),
        (!source.url.trim().is_empty()).then_some("url"),
        (!source.key.trim().is_empty()).then_some("key"),
        source.parallel.is_some().then_some("parallel"),
        source.key_in_the_clear.then_some("key_in_the_clear"),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !also.is_empty() {
        let these = also.join(", ");
        return Err(LlmError::Other(format!(
            "`[models.{name}]` names `endpoint = {named:?}` and also sets {these}. The address is \
             in one place or the other, and this way one of them is silently unused — move \
             {these} into `[endpoints.{named}]`, or drop `endpoint` and let this source carry its \
             own address."
        )));
    }

    Ok(Address {
        at: format!("endpoints.{named}"),
        api: &endpoint.api,
        url: &endpoint.url,
        key: &endpoint.key,
        parallel: endpoint.parallel,
        key_in_the_clear: endpoint.key_in_the_clear,
        queue: Some(named.to_string()),
    })
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
fn key_for(at: &str, written: &str, vault: &Vault) -> Result<Option<String>, LlmError> {
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
                "`[{at}] key` names the secret {secret:?}, which is not one \
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
                "`[{at}] key` names the variable {variable:?}, which is not set in \
                 this process. Set it, or keep the value with `rook secrets add` and write \
                 `secret:<name>` here instead."
            ))),
        };
    }
    vault.also_hide(written);
    Ok(Some(written.to_string()))
}

/// What one endpoint said when it was asked just now.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Answered {
    pub name: String,
    /// How long it took to answer, or to fail to. Milliseconds rather than a
    /// `Duration`, because this crosses the daemon's api and a duration on the
    /// wire is two fields nobody reads.
    pub took_ms: u64,
    /// The models it listed. Empty from an endpoint that answered but lists
    /// none, which some do, so it is not the same question as whether it is
    /// there.
    pub serving: Vec<String>,
    /// What went wrong, where something did.
    pub refused: Option<String>,
}

impl Answered {
    pub fn answering(&self) -> bool {
        self.refused.is_none()
    }
}

/// Put every endpoint back in the rotation, then ask each one.
///
/// The waiting in [`rook_llm`] answers the case where a laptop moved networks
/// and nobody noticed. This answers the other one: somebody has topped up an
/// account or started a server and wants to know whether it worked — now,
/// rather than after a timer they cannot see. Clearing first is the whole
/// point: an endpoint excluded a moment ago would otherwise be reported as out
/// on the strength of the exclusion rather than on what it says, which is
/// exactly the question being asked.
///
/// All at once, because the ones worth asking about are the ones that do not
/// answer, and each of those costs the connect timeout. Four of them in a row
/// is a minute of a person watching nothing.
pub async fn recheck(config: &Config, vault: &Vault) -> Vec<Answered> {
    rook_llm::answering_again(None);
    let asking = config.models.keys().map(|name| async move {
        let started = std::time::Instant::now();
        let built = endpoint_for(config, vault, name).and_then(|found| match found {
            Some(endpoint) => rook_llm::from_endpoints_with(
                vec![endpoint],
                config.agent.stream_idle(),
                rook_llm::Prefer::AsConfigured,
            ),
            None => Err(LlmError::Other(format!("{name} describes no endpoint"))),
        });
        let (serving, refused) = match built {
            // Its models rather than `reachable`, because the list is the
            // useful half of the answer: an endpoint that is up and serving
            // something other than what the file names is a different problem
            // from one that is down, and they look the same otherwise.
            Ok(provider) => match provider.models().await {
                Ok(models) => (models.into_iter().map(|m| m.id).collect(), None),
                Err(why) => (Vec::new(), Some(why.to_string())),
            },
            Err(why) => (Vec::new(), Some(why.to_string())),
        };
        Answered { name: name.clone(), took_ms: started.elapsed().as_millis() as u64, serving, refused }
    });
    futures_util::future::join_all(asking).await
}

/// The endpoints to offer where nobody has chosen one, and nothing otherwise.
///
/// `[agent] model` always says something, because an unconfigured install is
/// still meant to work — but what it says then is a guess at a local Ollama,
/// not a decision. On a machine without one the first turn fails with `cannot
/// reach 127.0.0.1:11434` while the endpoints somebody did write down sit in
/// the same file, unasked about.
///
/// So the question is whether the file names a model, not whether the setting
/// has a value: the difference between a choice and a default is not visible
/// in the loaded configuration, and it is the whole of what this is for.
/// `None` where there is nothing to offer — an install with no `[models]` has
/// only the guess, and the error it produces is already the right one.
pub fn unchosen(config: &Config, path: &std::path::Path) -> Option<Vec<String>> {
    if config.models.is_empty() || names_a_model(path) {
        return None;
    }
    Some(config.models.keys().cloned().collect())
}

/// Whether the file says which model to run on.
///
/// Read from the file rather than from the loaded configuration, for the reason
/// above. A file that will not parse is somebody else's error to report and
/// answers `true` here, so this stays quiet rather than adding a second
/// complaint about the same file.
fn names_a_model(path: &std::path::Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else { return false };
    let Ok(written) = toml::from_str::<toml::Value>(&text) else { return true };
    written.get("agent").and_then(|agent| agent.get("model")).is_some()
}
