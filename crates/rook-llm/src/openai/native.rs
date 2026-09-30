//! Optional local-server facts. Never load a model or trust its architectural
//! maximum as the context currently configured for an instance.
use super::OpenAiCompatible;
use crate::{CatalogLimits, Effort, MetadataApi, ModelInfo, catalog};
use serde::Deserialize;

pub(super) fn matches(api: MetadataApi, id: &str, wanted: &str) -> bool {
    api.matches_model("", id, wanted)
}

fn request(provider: &OpenAiCompatible, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
    let root = provider.config.base_url.trim_end_matches('/').trim_end_matches("/v1");
    let mut request = provider.http.request(method, format!("{root}{path}"));
    if let Some(key) = &provider.config.api_key {
        request = request.bearer_auth(key);
    }
    request
}

pub(super) async fn enrich(
    provider: &OpenAiCompatible,
    models: &mut [ModelInfo],
    limits: CatalogLimits,
    budget: &mut catalog::Budget,
) {
    if models.is_empty() {
        return;
    }
    match provider.config.metadata_api {
        MetadataApi::LmStudio => lm_studio(provider, models, limits, budget).await,
        MetadataApi::Ollama => ollama(provider, models, limits, budget).await,
        MetadataApi::Auto | MetadataApi::None => {}
    }
}

// Google's protobuf envelope permits omitted empty models; native servers
// require an explicit list before absence can mean no models are loaded.
fn models_in<T: serde::de::DeserializeOwned>(text: &str, limit: usize) -> Option<Vec<T>> {
    #[derive(Deserialize)]
    struct Envelope {
        models: Option<serde::de::IgnoredAny>,
    }
    serde_json::from_str::<Envelope>(text).ok()?.models?;
    catalog::entries(text, "models", limit).ok()
}

#[derive(Deserialize)]
struct Instance {
    id: String,
    #[serde(default)]
    config: InstanceConfig,
}
#[derive(Default, Deserialize)]
struct InstanceConfig {
    #[serde(default)]
    context_length: Option<usize>,
}
#[derive(Default, Deserialize)]
struct Quantization {
    #[serde(default)]
    name: Option<String>,
}
#[derive(Default, Deserialize)]
struct Reasoning {
    #[serde(default)]
    allowed_options: Option<Vec<String>>,
}
#[derive(Default, Deserialize)]
struct Capabilities {
    #[serde(default)]
    vision: Option<bool>,
    #[serde(default)]
    trained_for_tool_use: Option<bool>,
    #[serde(default)]
    reasoning: Option<Reasoning>,
}
#[derive(Deserialize)]
struct LocalModel {
    key: String,
    #[serde(default)]
    loaded_instances: Option<Vec<Instance>>,
    #[serde(default)]
    max_context_length: Option<usize>,
    #[serde(default)]
    quantization: Option<Quantization>,
    #[serde(default)]
    capabilities: Option<Capabilities>,
}

async fn lm_studio(
    provider: &OpenAiCompatible,
    models: &mut [ModelInfo],
    limits: CatalogLimits,
    budget: &mut catalog::Budget,
) {
    let text = match budget
        .text(request(provider, reqwest::Method::GET, "/api/v1/models"), &provider.config.base_url)
        .await
    {
        Ok(text) => text,
        Err(crate::LlmError::Status { status: 404 | 405, .. }) => {
            lm_studio_v0(provider, models, limits, budget).await;
            return;
        }
        Err(_) => return,
    };
    let Some(entries) = models_in::<LocalModel>(&text, limits.bounded().max_models) else {
        return;
    };
    for entry in entries {
        let capabilities = entry.capabilities.unwrap_or_default();
        let options = capabilities.reasoning.as_ref().and_then(|r| r.allowed_options.as_ref());
        for model in models.iter_mut() {
            let instance = entry
                .loaded_instances
                .as_ref()
                .and_then(|instances| instances.iter().find(|instance| instance.id == model.id));
            if entry.key != model.id && instance.is_none() {
                continue;
            }
            // An instance alias identifies its own context. A model key may
            // route to any instance: use the minimum only if all are known.
            model.context_window = if let Some(instance) = instance {
                instance.config.context_length.filter(|window| *window > 0)
            } else {
                entry.loaded_instances.as_ref().filter(|instances| !instances.is_empty()).and_then(
                    |instances| {
                        instances
                            .iter()
                            .map(|i| i.config.context_length.filter(|w| *w > 0))
                            .try_fold(usize::MAX, |minimum, window| window.map(|w| minimum.min(w)))
                    },
                )
            };
            model.max_context_window = entry.max_context_length.filter(|window| *window > 0);
            model.loaded = entry.loaded_instances.as_ref().map(|instances| !instances.is_empty());
            model.quantization = entry.quantization.as_ref().and_then(|q| q.name.clone());
            model.capabilities.tools = capabilities.trained_for_tool_use.or(model.capabilities.tools);
            model.capabilities.image_input = capabilities.vision.or(model.capabilities.image_input);
            if let Some(options) = options {
                model.capabilities.reasoning = Some(options.iter().any(|option| option != "off"));
                model.capabilities.effort_levels = Some(
                    Effort::ALL
                        .into_iter()
                        .filter(|level| options.iter().any(|v| v == level.as_str()))
                        .collect(),
                );
            }
        }
    }
}

async fn lm_studio_v0(
    provider: &OpenAiCompatible,
    models: &mut [ModelInfo],
    limits: CatalogLimits,
    budget: &mut catalog::Budget,
) {
    #[derive(Deserialize)]
    struct Entry {
        id: String,
        #[serde(default)]
        loaded_context_length: Option<usize>,
        #[serde(default)]
        max_context_length: Option<usize>,
        #[serde(default)]
        state: Option<String>,
        #[serde(default)]
        quantization: Option<String>,
    }
    let Ok(text) = budget
        .text(request(provider, reqwest::Method::GET, "/api/v0/models"), &provider.config.base_url)
        .await
    else {
        return;
    };
    let Ok(entries) = catalog::entries::<Entry>(&text, "data", limits.bounded().max_models) else { return };
    for entry in entries {
        if let Some(model) = models.iter_mut().find(|model| model.id == entry.id) {
            model.context_window = if entry.state.as_deref() == Some("not-loaded") {
                None
            } else {
                entry.loaded_context_length.filter(|window| *window > 0)
            };
            model.max_context_window = entry.max_context_length.filter(|window| *window > 0);
            model.loaded = match entry.state.as_deref() {
                Some("loaded") => Some(true),
                Some("not-loaded") => Some(false),
                _ => None,
            };
            model.quantization = entry.quantization;
        }
    }
}

#[derive(Default, Deserialize)]
struct Details {
    #[serde(default)]
    quantization_level: Option<String>,
}
#[derive(Deserialize)]
struct Show {
    #[serde(default)]
    capabilities: Option<Vec<String>>,
    #[serde(default)]
    thinking: Option<Thinking>,
    #[serde(default)]
    parameters: Option<String>,
    #[serde(default)]
    details: Option<Details>,
    #[serde(default)]
    model_info: serde_json::Value,
}
#[derive(Deserialize)]
struct Thinking {
    #[serde(default)]
    values: Option<Vec<serde_json::Value>>,
}

async fn show(provider: &OpenAiCompatible, model: &mut ModelInfo, budget: &mut catalog::Budget) -> bool {
    let req = request(provider, reqwest::Method::POST, "/api/show")
        .json(&serde_json::json!({"model":model.id,"verbose":false}));
    let Ok(text) = budget.text(req, &provider.config.base_url).await else { return false };
    let Ok(info) = serde_json::from_str::<Show>(&text) else { return false };
    if let Some(capabilities) = info.capabilities {
        let has = |name: &str| capabilities.iter().any(|value| value == name);
        model.capabilities.tools = Some(has("tools"));
        model.capabilities.image_input = Some(has("vision"));
        model.capabilities.reasoning = Some(has("thinking"));
    }
    if let Some(values) = info.thinking.and_then(|thinking| thinking.values) {
        model.capabilities.reasoning = model.capabilities.reasoning.or_else(|| {
            Some(values.iter().any(|value| {
                value.as_bool() == Some(true) || value.as_str().is_some_and(|level| level != "off")
            }))
        });
        model.capabilities.effort_levels = Some(
            Effort::ALL
                .into_iter()
                .filter(|level| values.iter().any(|value| value.as_str() == Some(level.as_str())))
                .collect(),
        );
    }
    model.quantization =
        info.details.and_then(|details| details.quantization_level).or(model.quantization.take());
    model.max_context_window = info
        .model_info
        .get("general.architecture")
        .and_then(|arch| arch.as_str())
        .and_then(|arch| info.model_info.get(format!("{arch}.context_length")))
        .and_then(|v| v.as_u64())
        .and_then(|n| usize::try_from(n).ok())
        .filter(|window| *window > 0);
    // A Modelfile's num_ctx is an actual configured default; the GGUF maximum
    // above is only a capacity. A running instance, read below, takes priority.
    let configured = info
        .parameters
        .as_deref()
        .into_iter()
        .flat_map(str::lines)
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            if parts.next()? != "num_ctx" {
                return None;
            }
            parts.next()?.parse::<usize>().ok().filter(|window| *window > 0)
        })
        .min();
    model.context_window = configured.or(model.context_window);
    true
}

async fn ollama(
    provider: &OpenAiCompatible,
    models: &mut [ModelInfo],
    limits: CatalogLimits,
    budget: &mut catalog::Budget,
) {
    let selected = models.iter().position(|model| matches(MetadataApi::Ollama, &model.id, &provider.model));
    if let Some(index) = selected {
        show(provider, &mut models[index], budget).await;
    }
    #[derive(Deserialize)]
    struct Running {
        #[serde(default)]
        name: String,
        #[serde(default)]
        model: String,
        #[serde(default)]
        context_length: Option<usize>,
    }
    let running = match budget
        .text(request(provider, reqwest::Method::GET, "/api/ps"), &provider.config.base_url)
        .await
    {
        Ok(text) => models_in::<Running>(&text, limits.bounded().max_models),
        Err(_) => None,
    };
    for (index, model) in models.iter_mut().enumerate() {
        if selected != Some(index) && !show(provider, model, budget).await {
            break;
        }
    }
    if let Some(running) = running {
        for model in models {
            let entry = running.iter().find(|entry| {
                matches(MetadataApi::Ollama, &entry.name, &model.id)
                    || matches(MetadataApi::Ollama, &entry.model, &model.id)
            });
            model.loaded = Some(entry.is_some());
            if let Some(entry) = entry {
                model.context_window = entry.context_length.filter(|window| *window > 0);
            }
        }
    }
}
