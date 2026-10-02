//! Generation uses a fresh, credential-matched snapshot of catalog facts.
//!
//! Freeze it for the provider lifetime: changing tool mode during a turn would
//! invalidate the prompt/schema prefix. A catalog refresh affects the next
//! provider construction, without adding network probes to ordinary turns.
use std::path::PathBuf;

use async_trait::async_trait;
use rook_llm::{
    CatalogLimits, Effort, EffortUse, Endpoint, LlmError, ModelCapabilities, ModelInfo, Provider, Request,
    Response, ResponseStream,
};

use super::{Entry, Settings, credential, decode_cache, now, read, remember};
use crate::{Config, Vault};

pub(crate) struct Snapshot {
    settings: Settings,
    directory: PathBuf,
    current: u64,
    entries: Vec<Entry>,
}
impl Snapshot {
    pub fn new(config: &Config) -> Self {
        let settings = config.model_catalog.bounded();
        let directory = crate::paths::home().join("cache");
        let entries =
            if settings.cache_enabled { read(&directory, settings).unwrap_or_default() } else { Vec::new() };
        Self { settings, directory, current: now(), entries }
    }
    pub fn decorate(
        &self,
        config: &Config,
        vault: &Vault,
        endpoint: &Endpoint,
        inner: Box<dyn Provider>,
    ) -> Box<dyn Provider> {
        let Ok(scope) = crate::models::catalog_scope(config, vault, &endpoint.name) else { return inner };
        let credential = credential(&scope, endpoint);
        // Native local metadata is only used by these generation dialects.
        // A cloud source merely named "ollama" must still match exact ids.
        let metadata_api = match endpoint.api {
            rook_llm::Api::OpenAi | rook_llm::Api::Responses => endpoint.metadata_api,
            _ => rook_llm::MetadataApi::None,
        };
        let observed = self
            .entries
            .iter()
            .find(|entry| {
                entry.scope == scope
                    && entry.credential == credential
                    && entry.complete
                    && entry.observed_at <= self.current
                    && self.current - entry.observed_at < self.settings.cache_ttl_secs
            })
            .and_then(|entry| decode_cache(entry, self.current, self.settings, true).ok())
            .and_then(|listing| {
                listing
                    .models
                    .into_iter()
                    .find(|model| metadata_api.matches_model(&endpoint.name, &model.id, &endpoint.model))
            })
            .map(|mut model| {
                // Normalize once: repeated cache entries must not be copied
                // into every generation request or effort-control lookup.
                if let Some(levels) = &mut model.capabilities.effort_levels {
                    *levels = Effort::ALL.into_iter().filter(|level| levels.contains(level)).collect();
                }
                model
            });
        Box::new(Observed {
            inner,
            observed,
            settings: self.settings,
            directory: self.directory.clone(),
            scope,
            credential,
            model: endpoint.model.clone(),
            metadata_api,
        })
    }
}

struct Observed {
    inner: Box<dyn Provider>,
    observed: Option<ModelInfo>,
    settings: Settings,
    directory: PathBuf,
    scope: String,
    credential: String,
    model: String,
    metadata_api: rook_llm::MetadataApi,
}

impl Observed {
    fn capabilities(&self) -> ModelCapabilities {
        self.observed.as_ref().map(|model| model.capabilities.clone()).unwrap_or_default()
    }

    fn selected_effort(&self, requested: Effort) -> Option<Effort> {
        let facts = self.capabilities();
        if facts.reasoning == Some(false) {
            return None;
        }
        let Some(allowed) = &facts.effort_levels else { return Some(requested) };
        // Intersect metadata with the dialect's known wire mapping. A catalog
        // flag cannot teach a client an unknown provider-specific parameter.
        let candidates: Vec<_> = Effort::ALL
            .into_iter()
            .filter_map(|effort| match self.inner.effort_use(effort) {
                EffortUse::Parameter { value, .. } => {
                    let applied = Effort::parse(&value).unwrap_or(effort);
                    allowed.contains(&applied).then_some((effort, applied))
                }
                _ => None,
            })
            .collect();
        let rank = |level: Effort| Effort::ALL.iter().position(|entry| *entry == level).unwrap_or(0);
        candidates
            .iter()
            .filter(|(_, applied)| rank(*applied) <= rank(requested))
            .max_by_key(|(_, applied)| rank(*applied))
            .or_else(|| candidates.iter().min_by_key(|(_, applied)| rank(*applied)))
            .map(|(effort, _)| *effort)
    }

    fn request(&self, mut request: Request) -> rook_llm::Result<Request> {
        let facts = self.capabilities();
        if facts.image_input == Some(false)
            && request.messages.iter().any(|message| !message.images.is_empty())
        {
            return Err(LlmError::Other(format!(
                "{}: cached model metadata reports no image input support; select an image-capable model",
                self.id()
            )));
        }
        if facts.tools == Some(false) && !request.tools.is_empty() {
            return Err(LlmError::Other(format!(
                "{}: cached model metadata reports no native tool support; use prompt-encoded tools",
                self.id()
            )));
        }
        if facts.tools == Some(false) {
            request.messages =
                rook_llm::Request::new(rook_llm::prompted::history(&request.messages)).messages;
        }
        request.effort = request.effort.and_then(|level| self.selected_effort(level));
        request.model_capabilities = facts;
        Ok(request)
    }
}

#[async_trait]
impl Provider for Observed {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn dispatch_identity(&self) -> Option<rook_llm::Dispatch> {
        self.inner.dispatch_identity()
    }
    fn context_window(&self) -> usize {
        self.inner.context_window()
    }
    fn context_key(&self) -> Option<[u8; 32]> {
        self.inner.context_key()
    }
    fn context_is_explicit(&self) -> bool {
        self.inner.context_is_explicit()
    }
    fn supports_tools(&self) -> bool {
        self.inner.supports_tools()
            && self.observed.as_ref().and_then(|model| model.capabilities.tools) != Some(false)
    }

    fn image_input_support(&self) -> Option<bool> {
        self.observed.as_ref().and_then(|model| model.capabilities.image_input)
    }

    fn can_replay_reasoning(&self, messages: &[rook_llm::Message]) -> bool {
        self.inner.can_replay_reasoning(messages)
    }

    fn can_resume_reasoning(&self, messages: &[rook_llm::Message]) -> bool {
        self.inner.can_resume_reasoning(messages)
    }
    fn takes_effort(&self) -> bool {
        self.inner.takes_effort()
            && Effort::ALL.into_iter().any(|level| self.selected_effort(level).is_some())
    }
    fn effort_use(&self, effort: Effort) -> EffortUse {
        match self.selected_effort(effort) {
            Some(level) => self.inner.effort_use(level),
            None => EffortUse::Omitted {
                reason: "catalog does not offer an effort level supported by this dialect",
            },
        }
    }
    fn supports_streaming(&self) -> bool {
        self.inner.supports_streaming()
    }
    async fn complete(&self, request: Request) -> rook_llm::Result<Response> {
        self.inner.complete(self.request(request)?).await
    }

    async fn complete_with_metadata(&self, request: Request) -> rook_llm::Result<rook_llm::Completion> {
        self.inner.complete_with_metadata(self.request(request)?).await
    }
    async fn stream(&self, request: Request) -> rook_llm::Result<ResponseStream> {
        self.inner.stream(self.request(request)?).await
    }
    async fn models(&self) -> rook_llm::Result<Vec<ModelInfo>> {
        self.models_with(self.settings.limits).await
    }
    async fn models_with(&self, limits: CatalogLimits) -> rook_llm::Result<Vec<ModelInfo>> {
        let models = self.inner.models_with(limits).await?;
        if self.settings.cache_enabled {
            // This route alone produced the list: a fallback's catalog never
            // receives the preferred route's cache identity or credentials.
            if let Err(error) =
                remember(&self.directory, self.settings, &self.scope, &self.credential, now(), &models)
            {
                tracing::debug!(kind=?error.kind(),"model observations could not be cached");
            }
        }
        Ok(models)
    }
    async fn discover_context_window(&self, limits: CatalogLimits) -> rook_llm::Result<Option<usize>> {
        if self.context_is_explicit() {
            return Ok(Some(self.context_window()));
        }
        if let Some(window) =
            self.observed.as_ref().and_then(|model| model.context_window).filter(|window| *window > 0)
        {
            return Ok(Some(window));
        }
        Ok(self
            .models_with(limits)
            .await?
            .into_iter()
            .find(|model| self.metadata_api.matches_model(self.id(), &model.id, &self.model))
            .and_then(|model| model.context_window)
            .filter(|window| *window > 0))
    }
    async fn reachable(&self) -> rook_llm::Result<()> {
        self.inner.reachable().await
    }
}
