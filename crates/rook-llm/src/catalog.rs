//! Bounded model metadata. Missing capability information stays unknown.
use std::time::Duration;

use futures_util::StreamExt;
use serde::de::{DeserializeOwned, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};

use crate::{Effort, LlmError, Result};

/// Optional native metadata beside the generation API. Auto recognizes the
/// legacy provider/model shorthand only; a named source can declare its server.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MetadataApi {
    #[default]
    Auto,
    None,
    Ollama,
    LmStudio,
}
impl MetadataApi {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::None => "none",
            Self::Ollama => "ollama",
            Self::LmStudio => "lmstudio",
        }
    }
    /// Selected-model matching shared by live metadata and runtime cache lookup.
    pub fn matches_model(self, provider: &str, id: &str, wanted: &str) -> bool {
        id == wanted
            || (self.resolved(provider) == Self::Ollama
                && (id.strip_suffix(":latest") == Some(wanted) || wanted.strip_suffix(":latest") == Some(id)))
    }
    pub(crate) fn resolved(self, name: &str) -> Self {
        if self != Self::Auto {
            return self;
        }
        match name.split('/').next().unwrap_or_default() {
            "ollama" => Self::Ollama,
            "lmstudio" => Self::LmStudio,
            _ => Self::None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct CatalogLimits {
    pub max_bytes: usize,
    pub max_models: usize,
    pub max_pages: usize,
    pub timeout_secs: u64,
}
impl Default for CatalogLimits {
    fn default() -> Self {
        Self { max_bytes: 1024 * 1024, max_models: 1024, max_pages: 16, timeout_secs: 5 }
    }
}
impl CatalogLimits {
    pub fn bounded(self) -> Self {
        Self {
            max_bytes: self.max_bytes.clamp(1024, 4 * 1024 * 1024),
            max_models: self.max_models.clamp(1, 4096),
            max_pages: self.max_pages.clamp(1, 64),
            timeout_secs: self.timeout_secs.clamp(1, 60),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelCapabilities {
    pub tools: Option<bool>,
    pub image_input: Option<bool>,
    pub reasoning: Option<bool>,
    pub adaptive_thinking: Option<bool>,
    /// None means unknown; an empty list means the endpoint explicitly offers
    /// none of Rook's effort levels. It does not imply thinking is disabled.
    pub effort_levels: Option<Vec<Effort>>,
}
impl ModelCapabilities {
    pub(crate) fn anthropic(value: &serde_json::Value) -> Self {
        let support = |name: &str| value.get(name)?.get("supported")?.as_bool();
        let effort = value.get("effort");
        let effort_levels = effort.and_then(|e| {
            if !e.get("supported")?.as_bool()? {
                return Some(Vec::new());
            }
            let known = Effort::ALL.iter().any(|level| {
                e.get(level.as_str()).and_then(|v| v.get("supported")).and_then(|v| v.as_bool()).is_some()
            });
            known.then(|| {
                Effort::ALL
                    .into_iter()
                    .filter(|level| {
                        e.get(level.as_str()).and_then(|v| v.get("supported")).and_then(|v| v.as_bool())
                            == Some(true)
                    })
                    .collect()
            })
        });
        Self {
            tools: None,
            image_input: support("image_input"),
            reasoning: support("thinking"),
            adaptive_thinking: value.pointer("/thinking/types/adaptive/supported").and_then(|v| v.as_bool()),
            effort_levels,
        }
    }
}

/// One budget for an entire catalog operation, including optional enrichment.
/// A new page never resets its deadline or replenishes its byte allowance.
pub(crate) struct Budget {
    limits: CatalogLimits,
    bytes_left: usize,
    requests: usize,
    deadline: tokio::time::Instant,
}
impl Budget {
    pub(crate) fn new(limits: CatalogLimits) -> Self {
        let limits = limits.bounded();
        Self {
            limits,
            bytes_left: limits.max_bytes,
            requests: 0,
            deadline: tokio::time::Instant::now() + Duration::from_secs(limits.timeout_secs),
        }
    }

    fn timed_out(&self, base: &str) -> LlmError {
        LlmError::unreachable(
            base,
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("model metadata request timed out after {}s", self.limits.timeout_secs),
            ),
        )
    }

    pub(crate) async fn text(&mut self, request: reqwest::RequestBuilder, base: &str) -> Result<String> {
        if self.requests >= self.limits.max_pages {
            return Err(LlmError::Decode(format!("model metadata exceeds {} pages", self.limits.max_pages)));
        }
        if tokio::time::Instant::now() >= self.deadline {
            return Err(self.timed_out(base));
        }
        self.requests += 1;
        let deadline = self.deadline;
        let read = async {
            let response = request.send().await.map_err(|e| LlmError::unreachable(base, e))?;
            let status = response.status();
            let retry_after = crate::retry_after(response.headers());
            if response.content_length().is_some_and(|length| length > self.bytes_left as u64) {
                return Err(LlmError::Decode(format!(
                    "model metadata exceeds {} bytes across pages",
                    self.limits.max_bytes
                )));
            }
            let mut body = Vec::new();
            let mut chunks = response.bytes_stream();
            while let Some(chunk) = chunks.next().await {
                let chunk = chunk.map_err(|e| LlmError::unreachable(base, e))?;
                if chunk.len() > self.bytes_left {
                    return Err(LlmError::Decode(format!(
                        "model metadata exceeds {} bytes across pages",
                        self.limits.max_bytes
                    )));
                }
                self.bytes_left -= chunk.len();
                body.extend_from_slice(&chunk);
            }
            let body = String::from_utf8(body)
                .map_err(|_| LlmError::Decode("model metadata is not UTF-8".into()))?;
            if !status.is_success() {
                return Err(LlmError::Status { status: status.as_u16(), retry_after, body });
            }
            Ok(body)
        };
        tokio::time::timeout_at(deadline, read).await.map_err(|_| self.timed_out(base))?
    }

    /// Keep the first observation of overlapping IDs, but count every received
    /// record toward the limit. Repeating models cannot renew the budget.
    pub(crate) async fn list<T: DeserializeOwned>(
        &mut self,
        mut request: impl FnMut(Option<&str>) -> reqwest::RequestBuilder,
        base: &str,
        paging: Paging,
        id: impl Fn(&T) -> &str,
    ) -> Result<Vec<T>> {
        let mut models = Vec::new();
        let mut seen_models = std::collections::HashSet::new();
        let mut seen_cursors = std::collections::HashSet::new();
        let mut received = 0;
        let mut cursor: Option<String> = None;
        loop {
            if received >= self.limits.max_models && cursor.is_some() {
                return Err(LlmError::Decode(format!(
                    "model metadata exceeds {} models across pages",
                    self.limits.max_models
                )));
            }
            let text = self.text(request(cursor.as_deref()), base).await?;
            let page: Vec<T> = entries(&text, paging.field(), self.limits.max_models - received)?;
            // This small shape skips the array without retaining another copy.
            let info: Continuation = serde_json::from_str(&text)
                .map_err(|e| LlmError::Decode(format!("invalid model pagination: {e}")))?;
            let next = paging.next(info)?;
            received += page.len();
            for model in page {
                if seen_models.insert(id(&model).to_string()) {
                    models.push(model);
                }
            }
            let Some(next) = next else { return Ok(models) };
            if !seen_cursors.insert(next.clone()) {
                return Err(LlmError::Decode("model pagination repeated a cursor".into()));
            }
            cursor = Some(next);
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Paging {
    Single,
    Anthropic,
    Google,
}
impl Paging {
    fn field(self) -> &'static str {
        if matches!(self, Self::Google) { "models" } else { "data" }
    }
    fn next(self, page: Continuation) -> Result<Option<String>> {
        match self {
            Self::Anthropic if page.has_more == Some(true) => {
                page.last_id.filter(|id| !id.is_empty()).map(Some).ok_or_else(|| {
                    LlmError::Decode("model page has_more is true but last_id is missing".into())
                })
            }
            Self::Google => Ok(page.next_page_token.filter(|token| !token.is_empty())),
            Self::Single
                if page.has_more == Some(true) || page.next_page_token.is_some_and(|v| !v.is_empty()) =>
            {
                Err(LlmError::Decode(
                    "this API does not define model pagination; refusing an incomplete listing".into(),
                ))
            }
            _ => Ok(None),
        }
    }
}
#[derive(Deserialize)]
struct Continuation {
    #[serde(default)]
    has_more: Option<bool>,
    #[serde(default)]
    last_id: Option<String>,
    #[serde(default, rename = "nextPageToken")]
    next_page_token: Option<String>,
}

/// Stop the sequence visitor at the configured count, before allocating the
/// rest of a huge listing. Unrelated metadata is skipped without retaining it.
pub fn entries<T: DeserializeOwned>(text: &str, field: &str, limit: usize) -> Result<Vec<T>> {
    struct Array<T> {
        limit: usize,
        marker: std::marker::PhantomData<T>,
    }
    impl<'de, T: Deserialize<'de>> DeserializeSeed<'de> for Array<T> {
        type Value = Vec<T>;
        fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> std::result::Result<Vec<T>, D::Error> {
            d.deserialize_seq(self)
        }
    }
    impl<'de, T: Deserialize<'de>> Visitor<'de> for Array<T> {
        type Value = Vec<T>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a bounded model list")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Vec<T>, A::Error> {
            let mut entries = Vec::new();
            loop {
                if entries.len() == self.limit {
                    return match seq.next_element::<serde::de::IgnoredAny>()? {
                        Some(_) => Err(serde::de::Error::custom(format!(
                            "model metadata exceeds {} models",
                            self.limit
                        ))),
                        None => Ok(entries),
                    };
                }
                match seq.next_element()? {
                    Some(entry) => entries.push(entry),
                    None => return Ok(entries),
                }
            }
        }
    }
    struct Envelope<'a, T> {
        field: &'a str,
        limit: usize,
        marker: std::marker::PhantomData<T>,
    }
    impl<'de, T: Deserialize<'de>> Visitor<'de> for Envelope<'_, T> {
        type Value = Vec<T>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a model listing object")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<Vec<T>, A::Error> {
            let mut entries = None;
            let mut empty_list_marker = false;
            let mut fields_seen = false;
            while let Some(key) = map.next_key::<String>()? {
                fields_seen = true;
                if key == self.field {
                    if entries.is_some() {
                        return Err(serde::de::Error::custom("duplicate model list"));
                    }
                    entries =
                        Some(map.next_value_seed(Array {
                            limit: self.limit,
                            marker: std::marker::PhantomData,
                        })?);
                } else if self.field == "data" && key == "object" {
                    // Some compatible servers spell an empty listing with
                    // only this marker. Preserve that established behavior.
                    empty_list_marker = map.next_value::<String>()? == "list";
                } else if self.field == "models" && key == "nextPageToken" {
                    // Protobuf JSON may omit an empty repeated models field.
                    map.next_value::<Option<String>>()?;
                    empty_list_marker = true;
                } else {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
            entries
                .or_else(|| (empty_list_marker || (self.field == "models" && !fields_seen)).then(Vec::new))
                .ok_or_else(|| serde::de::Error::custom("missing model list"))
        }
    }
    let mut decoder = serde_json::Deserializer::from_str(text);
    let models = serde::Deserializer::deserialize_map(
        &mut decoder,
        Envelope { field, limit, marker: std::marker::PhantomData },
    )
    .map_err(|e| LlmError::Decode(format!("invalid model metadata: {e}")))?;
    decoder.end().map_err(|e| LlmError::Decode(format!("invalid model metadata: {e}")))?;
    Ok(models)
}

/// Length-prefix each part so a delimiter in a URL or model cannot alias
/// another connection. Credentials never become a printable cache key.
pub(crate) fn context_key(parts: &[&str], proxy: &crate::Proxy) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part.as_bytes());
    }
    let proxy = format!("{proxy:?}");
    hash.update((proxy.len() as u64).to_le_bytes());
    hash.update(proxy.as_bytes());
    // Capture environment routing once, alongside construction of the HTTP
    // client. Reading it on every lookup would change an existing client's key
    // even though that client's actual proxy route stayed unchanged.
    for variable in [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "NO_PROXY",
        "no_proxy",
    ] {
        let value = std::env::var(variable).unwrap_or_default();
        hash.update((value.len() as u64).to_le_bytes());
        hash.update(value.as_bytes());
    }
    hash.finalize().into()
}
