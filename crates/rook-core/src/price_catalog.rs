//! Public references are an operator form, never a provider or a turn dependency.
use std::collections::HashSet;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};

use crate::config::edit::Editor;
use crate::{Config, Vault};

pub const SOURCE: &str = "https://models.dev/api.json";
const FILE: &str = "price-reference-v1.json";
const ENVELOPE_BYTES: usize = 256;
const MAX_TEXT: usize = 512;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub max_bytes: usize,
    pub max_providers: usize,
    pub max_models: usize,
    pub timeout_secs: u64,
    pub max_age_secs: u64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            max_bytes: 8 * 1024 * 1024,
            max_providers: 512,
            max_models: 32768,
            timeout_secs: 30,
            max_age_secs: 604800,
        }
    }
}
impl Settings {
    fn bounded(self) -> Self {
        Self {
            max_bytes: self.max_bytes.clamp(1024, 16 * 1024 * 1024),
            max_providers: self.max_providers.clamp(1, 1024),
            max_models: self.max_models.clamp(1, 65536),
            timeout_secs: self.timeout_secs.clamp(1, 60),
            max_age_secs: self.max_age_secs.min(2592000),
        }
    }
    pub(crate) fn errors(self) -> Vec<String> {
        let bounded = self.bounded();
        [
            ("max_bytes", self.max_bytes == bounded.max_bytes, "1024..=16777216"),
            ("max_providers", self.max_providers == bounded.max_providers, "1..=1024"),
            ("max_models", self.max_models == bounded.max_models, "1..=65536"),
            ("timeout_secs", self.timeout_secs == bounded.timeout_secs, "1..=60"),
            ("max_age_secs", self.max_age_secs == bounded.max_age_secs, "0..=2592000"),
        ]
        .into_iter()
        .filter(|(_, valid, _)| !valid)
        .map(|(key, _, range)| format!("price_catalog.{key}: expected {range}"))
        .collect()
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Rates {
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
}
impl Rates {
    fn configured(source: &crate::config::ModelSource) -> Self {
        Self {
            input: source.input_usd_per_million,
            output: source.output_usd_per_million,
            cache_read: source.cache_read_usd_per_million,
            cache_write: source.cache_write_usd_per_million,
        }
    }
    fn fields(self) -> [(&'static str, Option<f64>); 4] {
        [
            ("input_usd_per_million", self.input),
            ("output_usd_per_million", self.output),
            ("cache_read_usd_per_million", self.cache_read),
            ("cache_write_usd_per_million", self.cache_write),
        ]
    }
}

#[derive(Debug, Serialize)]
pub struct Review {
    pub source: String,
    pub model: String,
    pub provider: Option<String>,
    pub configured: Rates,
    pub reference: Option<Rates>,
    pub reference_context: Option<u64>,
    pub reference_reasoning: Option<bool>,
    pub reference_tools: Option<bool>,
    pub last_application: String,
    pub apply_fields: Vec<String>,
    pub review_token: Option<String>,
    pub reason: String,
}
#[derive(Debug, Serialize)]
pub struct Listing {
    pub source_url: &'static str,
    pub observed_at: Option<u64>,
    pub age_secs: Option<u64>,
    pub stale: bool,
    pub notices: Vec<String>,
    pub models: Vec<Review>,
}

#[derive(Serialize, Deserialize)]
struct Envelope<'a> {
    version: u32,
    observed_at: u64,
    #[serde(borrow)]
    data: &'a RawValue,
}
struct Catalog {
    observed_at: u64,
    digest: String,
    models: Vec<Model>,
}
#[derive(Debug)]
struct Model {
    provider: String,
    id: String,
    rates: Option<Rates>,
    blocked: Option<&'static str>,
    context: Option<u64>,
    reasoning: Option<bool>,
    tools: Option<bool>,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

// This visitor admits counts and decoded strings before retaining/copying them.
// Unknown large fields remain borrowed RawValue slices of the admitted body.
struct Text(String);
impl<'de> Deserialize<'de> for Text {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Bounded;
        impl Visitor<'_> for Bounded {
            type Value = Text;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a nonempty string of at most 512 bytes")
            }
            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<Text, E> {
                if s.is_empty() || s.len() > MAX_TEXT || s.chars().any(char::is_control) {
                    return Err(E::custom("reference string exceeds bounds or has control characters"));
                }
                Ok(Text(s.into()))
            }
        }
        d.deserialize_str(Bounded)
    }
}
fn map<'a>(
    raw: &'a str,
    maximum: usize,
    mut visit: impl FnMut(&str, &'a RawValue) -> Result<(), String>,
) -> Result<(), String> {
    struct Entries<'b, F> {
        maximum: usize,
        visit: &'b mut F,
    }
    impl<'de, F: FnMut(&str, &'de RawValue) -> Result<(), String>> Visitor<'de> for Entries<'_, F> {
        type Value = ();
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a bounded reference object")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<(), A::Error> {
            let mut keys = HashSet::new();
            while let Some(key) = access.next_key::<Text>()? {
                if keys.len() >= self.maximum {
                    return Err(serde::de::Error::custom("reference entry limit exceeded"));
                }
                if !keys.insert(key.0.clone()) {
                    return Err(serde::de::Error::custom("duplicate reference identity"));
                }
                let value: &RawValue = access.next_value()?;
                (self.visit)(&key.0, value).map_err(serde::de::Error::custom)?;
            }
            Ok(())
        }
    }
    let mut d = serde_json::Deserializer::from_str(raw);
    serde::Deserializer::deserialize_map(&mut d, Entries { maximum, visit: &mut visit })
        .map_err(|e| e.to_string())?;
    d.end().map_err(|e| e.to_string())
}

#[derive(Deserialize)]
struct Provider<'a> {
    id: Text,
    #[serde(borrow)]
    models: &'a RawValue,
}
#[derive(Deserialize)]
struct Offering<'a> {
    id: Text,
    #[serde(borrow)]
    cost: Option<&'a RawValue>,
    limit: Option<Limits>,
    reasoning: Option<bool>,
    tool_call: Option<bool>,
    #[serde(borrow)]
    provider: Option<&'a RawValue>,
    #[serde(borrow)]
    experimental: Option<&'a RawValue>,
}
#[derive(Deserialize)]
struct Limits {
    context: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Cost<'a> {
    input: f64,
    output: f64,
    reasoning: Option<f64>,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
    input_audio: Option<f64>,
    output_audio: Option<f64>,
    #[serde(borrow)]
    tiers: Option<&'a RawValue>,
    #[serde(borrow)]
    context_over_200k: Option<&'a RawValue>,
}
fn decode(body: &str, settings: Settings) -> Result<Vec<Model>, String> {
    if body.len() > settings.max_bytes {
        return Err("reference exceeds byte limit".into());
    }
    let mut models = Vec::new();
    map(body, settings.max_providers, |key, raw| {
        let provider: Provider<'_> = serde_json::from_str(raw.get()).map_err(|e| e.to_string())?;
        if key != provider.id.0 {
            return Err("provider key differs from its exact ID".into());
        }
        map(provider.models.get(), settings.max_models.saturating_sub(models.len()), |key, raw| {
            let model: Offering<'_> = serde_json::from_str(raw.get()).map_err(|e| e.to_string())?;
            if key != model.id.0 {
                return Err("model key differs from its exact ID; aliases are not accepted".into());
            }
            let mut blocked = None;
            let rates = if let Some(cost) = model.cost {
                let cost: Cost<'_> = serde_json::from_str(cost.get()).map_err(|e| e.to_string())?;
                if [
                    Some(cost.input),
                    Some(cost.output),
                    cost.cache_read,
                    cost.cache_write,
                    cost.reasoning,
                    cost.input_audio,
                    cost.output_audio,
                ]
                .into_iter()
                .flatten()
                .any(|r| !r.is_finite() || !(0.0..=1_000_000.0).contains(&r))
                {
                    return Err("reference rate outside finite USD bounds".into());
                }
                if cost.tiers.is_some()
                    || cost.context_over_200k.is_some()
                    || cost.reasoning.is_some_and(|r| r != cost.output)
                    || cost.input_audio.is_some_and(|r| r != cost.input)
                    || cost.output_audio.is_some_and(|r| r != cost.output)
                {
                    blocked = Some(
                        "variable/context/modality rates cannot be represented by Rook's flat token rates",
                    );
                }
                Some(Rates {
                    input: Some(cost.input),
                    output: Some(cost.output),
                    cache_read: cost.cache_read,
                    cache_write: cost.cache_write,
                })
            } else {
                None
            };
            if model.provider.is_some() || model.experimental.is_some() {
                blocked =
                    Some("offering has provider or experimental overrides; flat rates need manual review");
            }
            models.push(Model {
                provider: provider.id.0.clone(),
                id: model.id.0,
                rates,
                blocked,
                context: model.limit.and_then(|l| l.context),
                reasoning: model.reasoning,
                tools: model.tool_call,
            });
            Ok(())
        })
    })?;
    Ok(models)
}

fn read(directory: &Path, settings: Settings) -> Result<Catalog, String> {
    let file = std::fs::File::open(directory.join(FILE))
        .map_err(|e| format!("no usable reference cache ({})", e.kind()))?;
    let maximum = settings.max_bytes + ENVELOPE_BYTES;
    if file.metadata().map_err(|e| e.to_string())?.len() > maximum as u64 {
        return Err("reference cache exceeds byte limit".into());
    }
    let mut bytes = Vec::new();
    file.take(maximum as u64 + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() > maximum {
        return Err("reference cache exceeds byte limit".into());
    }
    let envelope: Envelope<'_> =
        serde_json::from_slice(&bytes).map_err(|_| "reference cache is corrupt; refresh it explicitly")?;
    if envelope.version != 1 || envelope.observed_at > now() {
        return Err("reference cache version or observation clock is invalid; refresh it explicitly".into());
    }
    let models = decode(envelope.data.get(), settings)?;
    Ok(Catalog { observed_at: envelope.observed_at, digest: hex::encode(Sha256::digest(&bytes)), models })
}

/// The only production HTTP destination is public and fixed. No endpoint key,
/// vault/helper, catalog credential or turn state is used to construct it.
pub async fn refresh(directory: &Path, settings: Settings) -> Result<(), String> {
    fetch(directory, settings.bounded(), SOURCE).await
}
async fn fetch(directory: &Path, settings: Settings, url: &str) -> Result<(), String> {
    rook_llm::init_tls();
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(settings.timeout_secs))
        .build()
        .map_err(|e| e.to_string())?;
    // A finite, non-streaming catalog gets an overall bound; streamed model
    // answers have their separate, resetting idle timers.
    tokio::time::timeout(Duration::from_secs(settings.timeout_secs), async {
        let mut response =
            client.get(url).send().await.map_err(|_| "reference fetch failed; previous cache preserved")?;
        if !response.status().is_success() {
            return Err(format!("reference HTTP {}; previous cache preserved", response.status()));
        }
        if response.content_length().is_some_and(|n| n > settings.max_bytes as u64) {
            return Err("reference exceeds byte limit; previous cache preserved".into());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) =
            response.chunk().await.map_err(|_| "reference body failed; previous cache preserved")?
        {
            if chunk.len() > settings.max_bytes.saturating_sub(bytes.len()) {
                return Err("reference exceeds byte limit; previous cache preserved".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| "reference is not UTF-8")?;
        decode(text, settings)?;
        let data: &RawValue = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let bytes = crate::model_catalog::encode(
            &Envelope { version: 1, observed_at: now(), data },
            settings.max_bytes + ENVELOPE_BYTES,
        )
        .map_err(|e| e.to_string())?;
        crate::paths::private_dir(directory).map_err(|e| e.to_string())?;
        rook_contain::files::write_private(directory, Path::new(FILE), &bytes).map_err(|e| e.to_string())
    })
    .await
    .map_err(|_| "reference fetch timed out; previous cache preserved".to_string())?
}

fn token(
    editor: &Editor,
    config: &Config,
    vault: &Vault,
    name: &str,
    catalog: &Catalog,
) -> Result<String, String> {
    let scope = crate::models::catalog_scope(config, vault, name).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    for part in ["price-review-v1", &editor.revision(), &scope, &catalog.digest, name] {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part.as_bytes());
    }
    // env: credentials are read passively, never executed. A rotation between
    // preview and apply must invalidate the preview too.
    if let Some(source) = config.models.get(name) {
        let key = if source.endpoint.trim().is_empty() {
            &source.key
        } else {
            &config.endpoints.get(source.endpoint.trim()).ok_or("endpoint disappeared")?.key
        };
        if let Some(variable) = key.trim().strip_prefix("env:") {
            hash.update(std::env::var(variable).unwrap_or_default().as_bytes());
        }
    }
    Ok(hex::encode(hash.finalize()))
}
fn inspect_editor(
    editor: &Editor,
    directory: &Path,
    vault: &Vault,
    selected: Option<&str>,
) -> Result<Listing, String> {
    let config = editor.config()?;
    if selected.is_some_and(|s| s.len() > 256 || !config.models.contains_key(s)) {
        return Err("select an exact named source from [models]".into());
    }
    let settings = config.price_catalog.bounded();
    let mut notices = vec!["Public USD per million token references, not account billing. Manual values are preserved. Reference capabilities/context never configure runtime. Applies to the global config; project overrides retain priority.".into()];
    let catalog = match read(directory, settings) {
        Ok(c) => Some(c),
        Err(e) => {
            notices.push(e);
            None
        }
    };
    let age = catalog.as_ref().map(|c| now().saturating_sub(c.observed_at));
    let stale = age.is_none_or(|age| age > settings.max_age_secs);
    let mut models = Vec::new();
    for (name, source) in &config.models {
        if selected.is_some_and(|s| s != name) {
            continue;
        }
        let configured = Rates::configured(source);
        let identity = crate::models::reference_identity(&config, name).map_err(|e| e.to_string())?;
        let offering = identity
            .and_then(|p| catalog.as_ref()?.models.iter().find(|m| m.provider == p && m.id == source.model));
        let mut review = Review {
            source: name.clone(),
            model: source.model.clone(),
            provider: identity.map(str::to_owned),
            configured,
            reference: offering.and_then(|m| m.rates),
            reference_context: offering.and_then(|m| m.context),
            reference_reasoning: offering.and_then(|m| m.reasoning),
            reference_tools: offering.and_then(|m| m.tools),
            last_application: source.price_reference.clone(),
            apply_fields: Vec::new(),
            review_token: None,
            reason: String::new(),
        };
        review.reason = if identity.is_none() { "unknown local/custom/proxied billing identity; no model-name price guesses" }
            else if catalog.is_none() { "refresh the public reference explicitly" }
            else if stale { "reference is stale; refresh before applying" }
            else if let Some(offering) = offering {
                if let Some(reason) = offering.blocked { reason }
                else if let Some(rates) = offering.rates {
                    for ((field, reference), (_, configured)) in rates.fields().into_iter().zip(configured.fields()) {
                        if configured.is_none() && reference.is_some() { review.apply_fields.push(field.into()); }
                    }
                    if review.apply_fields.is_empty() { "no missing rates; configured values retain priority" }
                    else { "review and explicitly apply the missing rates; zero rates are references, not a billing guarantee" }
                } else { "offering has no price reference" }
            } else { "no exact provider/model offering; aliases are not accepted" }.into();
        if !review.apply_fields.is_empty()
            && let Some(catalog) = &catalog
        {
            review.review_token = Some(token(editor, &config, vault, name, catalog)?);
        }
        models.push(review);
    }
    Ok(Listing {
        source_url: SOURCE,
        observed_at: catalog.as_ref().map(|c| c.observed_at),
        age_secs: age,
        stale,
        notices,
        models,
    })
}

/// Offline inspection reads bounded config/cache/vault definitions only. A
/// token commits to the whole config, credential definition and cache version.
pub fn inspect(
    config_path: &Path,
    directory: &Path,
    vault: &Vault,
    source: Option<&str>,
) -> Result<Listing, String> {
    inspect_editor(&Editor::open(config_path.to_owned())?, directory, vault, source)
}

/// Recompute the preview, preserve explicit rates and use the editor's external
/// change guard. Never blindly apply an old browser/terminal review.
pub fn apply(
    config_path: &Path,
    directory: &Path,
    vault: &Vault,
    source: &str,
    reviewed: &str,
) -> Result<Listing, String> {
    if reviewed.len() != 64 {
        return Err("invalid review token; inspect again".into());
    }
    let mut editor = Editor::open(config_path.to_owned())?;
    let listing = inspect_editor(&editor, directory, vault, Some(source))?;
    let review = listing.models.first().ok_or("source disappeared; inspect again")?;
    if review.review_token.as_deref() != Some(reviewed) {
        return Err(
            "reference, config or account changed, or no safe missing rates remain; inspect again".into()
        );
    }
    let rates = review.reference.ok_or("reference rates disappeared")?;
    for (field, value) in rates.fields() {
        if review.apply_fields.iter().any(|f| f == field)
            && let Some(value) = value
        {
            editor.set(&["models".into(), source.into(), field.into()], &value.to_string())?;
        }
    }
    let attribution = format!(
        "{} | {}/{} | observed_at={} | applied_fields={}",
        SOURCE,
        review.provider.as_deref().unwrap_or_default(),
        review.model,
        listing.observed_at.unwrap_or_default(),
        review.apply_fields.join(",")
    );
    editor.set(&["models".into(), source.into(), "price_reference".into()], &attribution)?;
    editor.save()?;
    inspect(config_path, directory, vault, Some(source))
}

#[cfg(test)]
mod tests;
