use rook_llm::{CatalogLimits, MetadataApi, Provider};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn server(replies: Vec<(u16, Value)>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (status, body) in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..at]);
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|n| n.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= at + 4 + length {
                        break;
                    }
                }
                assert!(bytes.len() < 32768);
                let n = socket.read(&mut buffer).await.unwrap();
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&buffer[..n]);
            }
            requests.push(String::from_utf8(bytes).unwrap());
            let body = body.to_string();
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 {status} Response\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
        requests
    });
    (url, task)
}
fn provider(base: &str, api: MetadataApi, model: &str) -> rook_llm::openai::OpenAiCompatible {
    let mut config =
        rook_llm::openai::Config::new(format!("{base}/gateway/v1"), Some("native-key".into()), 32768);
    config.metadata_api = api;
    rook_llm::openai::OpenAiCompatible::new("office", model, config).unwrap()
}
fn list(ids: &[&str]) -> Value {
    json!({"data":ids.iter().map(|id|json!({"id":id})).collect::<Vec<_>>()})
}
#[tokio::test]
async fn lm_studio_keeps_actual_instance_windows_separate_from_architectural_maximum() {
    let (url,task)=server(vec![
        (200,list(&["base","alias-a","alias-b","unloaded","incomplete"])),
        (200,json!({"models":[
            {"key":"base","max_context_length":131072,"loaded_instances":[
                {"id":"alias-a","config":{"context_length":8192}},
                {"id":"alias-b","config":{"context_length":16384}}],
             "quantization":{"name":"Q4_K_M"},"capabilities":{"vision":true,"trained_for_tool_use":false,"reasoning":{"allowed_options":["off","low","high"]}}},
            {"key":"unloaded","max_context_length":65536,"loaded_instances":[]},
            {"key":"incomplete","max_context_length":131072,"loaded_instances":[{"id":"one","config":{"context_length":8192}},{"id":"two","config":{}}]}
        ]})),
    ]).await;
    let models = provider(&url, MetadataApi::LmStudio, "alias-b").models().await.unwrap();
    assert_eq!(models[0].context_window, Some(8192));
    assert_eq!(models[1].context_window, Some(8192));
    assert_eq!(models[2].context_window, Some(16384));
    assert_eq!(models[0].max_context_window, Some(131072));
    assert_eq!(models[0].quantization.as_deref(), Some("Q4_K_M"));
    assert_eq!(models[0].capabilities.tools, Some(false));
    assert_eq!(models[0].capabilities.image_input, Some(true));
    assert_eq!(
        models[0].capabilities.effort_levels,
        Some(vec![rook_llm::Effort::Low, rook_llm::Effort::High])
    );
    assert_eq!(models[3].loaded, Some(false));
    assert_eq!(models[3].context_window, None);
    assert_eq!(models[3].max_context_window, Some(65536));
    assert_eq!(models[3].capabilities.tools, None);
    assert_eq!(models[4].context_window, None, "one unknown instance invalidates an optimistic minimum");
    let requests = task.await.unwrap();
    assert!(requests[0].starts_with("GET /gateway/v1/models "));
    assert!(requests[1].starts_with("GET /gateway/api/v1/models "));
    assert!(requests.iter().all(|r| r.to_ascii_lowercase().contains("authorization: bearer native-key")));
}

#[tokio::test]
async fn authorization_and_malformed_native_responses_do_not_trigger_a_legacy_probe() {
    for (status, body) in
        [(401, json!({"error":"unauthorized"})), (200, json!({"data":[]})), (200, json!({}))]
    {
        let (url, task) = server(vec![(200, list(&["one"])), (status, body)]).await;
        let models = provider(&url, MetadataApi::LmStudio, "one").models().await.unwrap();
        assert_eq!(models[0].context_window, None);
        assert_eq!(models[0].loaded, None);
        assert_eq!(models[0].capabilities, Default::default());
        assert_eq!(task.await.unwrap().len(), 2);
    }
}

#[tokio::test]
async fn ollama_prioritizes_the_selected_model_and_uses_running_context_not_gguf_capacity() {
    let (url,task)=server(vec![
        (200,list(&["other","chosen:latest"])),
        (200,json!({"parameters":"temperature 0.7\nnum_ctx 8192","capabilities":["tools","vision","thinking"],"thinking":{"values":[false,"low","high"]},"details":{"quantization_level":"Q5_K_M"},"model_info":{"general.architecture":"llama","llama.context_length":131072}})),
        (200,json!({"models":[{"name":"chosen:latest","model":"chosen:latest","context_length":4096}]})),
        (200,json!({"capabilities":[],"model_info":{"general.architecture":"gemma4","gemma4.context_length":65536}})),
    ]).await;
    let models = provider(&url, MetadataApi::Ollama, "chosen").models().await.unwrap();
    assert_eq!(models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["other", "chosen:latest"]);
    assert_eq!(models[1].context_window, Some(4096));
    assert_eq!(models[1].max_context_window, Some(131072));
    assert_eq!(models[1].loaded, Some(true));
    assert_eq!(models[1].quantization.as_deref(), Some("Q5_K_M"));
    assert_eq!(models[1].capabilities.tools, Some(true));
    assert_eq!(models[1].capabilities.reasoning, Some(true));
    assert_eq!(
        models[1].capabilities.effort_levels,
        Some(vec![rook_llm::Effort::Low, rook_llm::Effort::High])
    );
    assert_eq!(models[0].capabilities.tools, Some(false));
    assert_eq!(models[0].context_window, None);
    assert_eq!(models[0].max_context_window, Some(65536));
    assert_eq!(models[0].loaded, Some(false));
    let requests = task.await.unwrap();
    assert!(requests[1].starts_with("POST /gateway/api/show "));
    assert!(requests[2].starts_with("GET /gateway/api/ps "));
    let body: Value = serde_json::from_str(requests[1].split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(body, json!({"model":"chosen:latest","verbose":false}));
    assert!(requests.iter().all(|r| r.to_ascii_lowercase().contains("authorization: bearer native-key")));
}

#[tokio::test]
async fn native_requests_share_the_page_and_byte_budget_and_missing_details_stay_unknown() {
    let (url, task) =
        server(vec![(200, list(&["other", "chosen:latest"])), (200, json!({"capabilities":["tools"]}))])
            .await;
    let models = provider(&url, MetadataApi::Ollama, "chosen")
        .models_with(CatalogLimits { max_pages: 2, ..Default::default() })
        .await
        .unwrap();
    assert_eq!(models[1].capabilities.tools, Some(true));
    assert_eq!(models[0].capabilities.tools, None);
    assert_eq!(models[1].loaded, None, "a skipped ps is not evidence the model is unloaded");
    assert_eq!(task.await.unwrap().len(), 2);
    let first = json!({"data":[{"id":"one"}],"padding":"x".repeat(600)});
    let second = json!({"models":[{"key":"one","max_context_length":131072,"loaded_instances":[{"id":"one","config":{"context_length":8192}}]}],"padding":"x".repeat(600)});
    assert!(first.to_string().len() < 1024 && second.to_string().len() < 1024);
    assert!(first.to_string().len() + second.to_string().len() > 1024);
    let (url, task) = server(vec![(200, first), (200, second)]).await;
    let models = provider(&url, MetadataApi::LmStudio, "one")
        .models_with(CatalogLimits { max_bytes: 1024, ..Default::default() })
        .await
        .unwrap();
    assert_eq!(models[0].context_window, None);
    assert_eq!(task.await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_missing_native_list_does_not_claim_every_ollama_model_is_unloaded() {
    let (url, task) = server(vec![(200, list(&["one"])), (200, json!({})), (200, json!({}))]).await;
    let models = provider(&url, MetadataApi::Ollama, "one").models().await.unwrap();
    assert_eq!(models[0].loaded, None);
    assert_eq!(task.await.unwrap().len(), 3);
}

#[tokio::test]
async fn an_arbitrary_named_compatible_server_is_not_probed_for_native_apis() {
    for mode in [MetadataApi::Auto, MetadataApi::None] {
        let (url, task) = server(vec![(200, list(&["one"]))]).await;
        let models = provider(&url, mode, "one").models().await.unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(task.await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn shorthand_default_is_an_assumption_and_context_discovery_accepts_ollamas_latest_alias() {
    let (url,task)=server(vec![
        (200,list(&["chosen:latest"])),
        (200,json!({"capabilities":["tools"],"model_info":{"general.architecture":"llama","llama.context_length":131072}})),
        (200,json!({"models":[{"name":"chosen:latest","model":"chosen:latest","context_length":16384}]})),
    ]).await;
    let mut endpoint = rook_llm::endpoint_from_spec("ollama/chosen", None).unwrap();
    assert!(endpoint.context_window.is_none());
    endpoint.url = format!("{url}/v1");
    let provider = rook_llm::from_endpoints_with(
        vec![endpoint],
        std::time::Duration::from_secs(30),
        rook_llm::Prefer::AsConfigured,
    )
    .unwrap();
    assert!(!provider.context_is_explicit());
    assert_eq!(provider.context_window(), 32768);
    assert_eq!(provider.discover_context_window(CatalogLimits::default()).await.unwrap(), Some(16384));
    assert_eq!(task.await.unwrap().len(), 3);
    for spec in ["ollama/chosen", "lmstudio/chosen", "openai/chosen"] {
        let endpoint = rook_llm::endpoint_from_spec(spec, None).unwrap();
        assert!(
            endpoint.context_window.is_none(),
            "built-in defaults must stay distinct from overrides: {spec}"
        );
        let configured = rook_llm::endpoint_from_spec(spec, Some(16384)).unwrap();
        assert_eq!(configured.context_window, Some(16384));
    }
}

#[tokio::test]
async fn boolean_thinking_controls_are_evidence_of_reasoning_but_not_of_effort_levels() {
    let (url, task) =
        server(vec![(200, list(&["one"])), (200, json!({"thinking":{"values":[false,true]}}))]).await;
    let models = provider(&url, MetadataApi::Ollama, "one")
        .models_with(CatalogLimits { max_pages: 2, ..Default::default() })
        .await
        .unwrap();
    assert_eq!(models[0].capabilities.reasoning, Some(true));
    assert_eq!(models[0].capabilities.effort_levels, Some(vec![]));
    assert_eq!(models[0].capabilities.tools, None);
    assert_eq!(models[0].capabilities.image_input, None);
    assert_eq!(task.await.unwrap().len(), 2);
}
