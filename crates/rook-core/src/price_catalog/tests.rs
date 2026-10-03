#![cfg(test)]
use super::*;
use serde_json::json;

fn body() -> String {
    json!({"openai":{"id":"openai","models":{"physical":{"id":"physical","cost":{"input":2.0,"output":6.0,"cache_read":0.2,"cache_write":3.0},"limit":{"context":999999},"tool_call":true,"reasoning":true}}}}).to_string()
}
fn seed(directory: &Path, text: &str, observed_at: u64) {
    std::fs::create_dir_all(directory).unwrap();
    let data: &RawValue = serde_json::from_str(text).unwrap();
    std::fs::write(
        directory.join(FILE),
        serde_json::to_vec(&Envelope { version: 1, observed_at, data }).unwrap(),
    )
    .unwrap();
}
fn fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.toml");
    let cache = temp.path().join("cache");
    std::fs::write(&path, "# keep my comment\n[models.\"source.with.dot\"]\nmodel='physical'\nendpoint='cloud'\ncontext_window=1234\ninput_usd_per_million=9.0\n[endpoints.cloud]\napi='responses'\nurl='https://api.openai.com/v1'\nkey='secret:account'\nproxy='direct'\n").unwrap();
    seed(&cache, &body(), now());
    (temp, path, cache)
}
fn review(path: &Path, cache: &Path, vault: &Vault) -> Listing {
    inspect(path, cache, vault, Some("source.with.dot")).unwrap()
}
fn review_token(path: &Path, cache: &Path, vault: &Vault) -> String {
    review(path, cache, vault).models.remove(0).review_token.unwrap()
}

#[test]
fn reference_review_uses_the_existing_trimmed_endpoint_resolver_and_cooperative_write_guard() {
    let (_temp, path, cache) = fixture();
    let mut editor = Editor::open(path.clone()).unwrap();
    editor.set(&["models".into(), "source.with.dot".into(), "endpoint".into()], " cloud ").unwrap();
    editor.save().unwrap();
    let token = review_token(&path, &cache, &Vault::empty());
    let before = std::fs::read(&path).unwrap();
    let lock = crate::config::edit::lock(&path).unwrap();
    assert!(
        apply(&path, &cache, &Vault::empty(), "source.with.dot", &token)
            .unwrap_err()
            .contains("being updated")
    );
    assert!(Config::set_in(&path, "agent.max_steps", "9").unwrap_err().contains("being updated"));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    drop(lock);
    assert!(apply(&path, &cache, &Vault::empty(), "source.with.dot", &token).is_ok());
}

#[test]
fn application_preserves_manual_values_comments_context_and_freezes_historical_rates() {
    let (_temp, path, cache) = fixture();
    let vault = Vault::empty();
    let listing = review(&path, &cache, &vault);
    assert_eq!(listing.models[0].reference_context, Some(999999));
    assert_eq!(listing.models[0].apply_fields.len(), 3);
    assert!(!listing.models[0].apply_fields.contains(&"input_usd_per_million".to_owned()));
    apply(&path, &cache, &vault, "source.with.dot", listing.models[0].review_token.as_ref().unwrap())
        .unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("# keep my comment"));
    let config: Config = toml::from_str(&text).unwrap();
    let model = &config.models["source.with.dot"];
    assert_eq!(model.input_usd_per_million, Some(9.0));
    assert_eq!(model.output_usd_per_million, Some(6.0));
    assert_eq!(model.context_window, Some(1234));
    assert_eq!(model.cache_read_usd_per_million, Some(0.2));
    assert!(model.price_reference.contains("openai/physical"));
    assert!(review(&path, &cache, &vault).models[0].review_token.is_none());

    let snapshot = crate::model_route::Prices::new(&config);
    let facts = rook_llm::AttemptFacts {
        completion_confirmed: true,
        usage_reported: true,
        usage: Some(rook_llm::Usage {
            input_tokens: 120,
            output_tokens: 7,
            cache_read_tokens: 80,
            cache_write_tokens: 10,
        }),
        ..Default::default()
    };
    let dispatch = rook_llm::Dispatch::bounded("source.with.dot", "physical", true).unwrap();
    let cost = snapshot.estimate(Some(&dispatch), &facts).unwrap();
    let expected = (30.0 * 9.0 + 7.0 * 6.0 + 80.0 * 0.2 + 10.0 * 3.0) / 1_000_000.0;
    assert!((cost.estimated_usd - expected).abs() < 1e-12);
    let bytes = serde_json::to_vec(&cost).unwrap();
    let mut editor = Editor::open(path.clone()).unwrap();
    editor.set(&["models".into(), "source.with.dot".into(), "output_usd_per_million".into()], "99").unwrap();
    editor.save().unwrap();
    assert_eq!(snapshot.estimate(Some(&dispatch), &facts).unwrap().estimated_usd, cost.estimated_usd);
    let saved: crate::model_route::Cost = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(saved.output_usd_per_million, 6.0);
    assert_ne!(
        crate::model_route::Prices::new(&Editor::open(path).unwrap().config().unwrap())
            .estimate(Some(&dispatch), &facts)
            .unwrap()
            .estimated_usd,
        saved.estimated_usd
    );
}

#[test]
fn changed_config_cache_credential_endpoint_and_manual_values_refuse_old_reviews() {
    for change in ["config", "cache", "credential", "endpoint", "manual"] {
        let (temp, path, cache) = fixture();
        let mut vault = Vault::load_from(temp.path().join("secrets.toml")).unwrap();
        vault.keep("account", "old").unwrap();
        let token = review_token(&path, &cache, &vault);
        match change {
            "config" => {
                let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
                std::io::Write::write_all(&mut file, b"\n# changed outside form\n").unwrap();
            }
            "cache" => seed(&cache, &body().replace("6.0", "7.0"), now()),
            "credential" => vault.keep("account", "new").unwrap(),
            "endpoint" => Config::set_in(&path, "endpoints.cloud.url", "https://example.com/v1").unwrap(),
            "manual" => {
                let mut editor = Editor::open(path.clone()).unwrap();
                editor
                    .set(&["models".into(), "source.with.dot".into(), "output_usd_per_million".into()], "17")
                    .unwrap();
                editor.save().unwrap();
            }
            _ => unreachable!(),
        }
        let before = std::fs::read(&path).unwrap();
        assert!(apply(&path, &cache, &vault, "source.with.dot", &token).is_err(), "change={change}");
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[test]
fn offline_helpers_stale_future_and_corrupt_caches_never_guess_prices() {
    let (temp, path, cache) = fixture();
    let mut vault = Vault::load_from(temp.path().join("secrets.toml")).unwrap();
    vault
        .refer("account", &crate::Source::Command("this-command-must-never-execute-for-prices".into()))
        .unwrap();
    assert!(review(&path, &cache, &vault).models[0].review_token.is_some());
    for text in ["corrupt".to_owned(), " ".repeat(Settings::default().max_bytes + ENVELOPE_BYTES + 1)] {
        std::fs::write(cache.join(FILE), text).unwrap();
        let listing = review(&path, &cache, &vault);
        assert!(listing.models[0].reference.is_none());
        assert!(listing.models[0].review_token.is_none());
        assert!(!listing.notices.is_empty());
    }
    seed(&cache, &body(), now() - 604801);
    let listing = review(&path, &cache, &vault);
    assert!(
        listing.stale && listing.models[0].reference.is_some() && listing.models[0].review_token.is_none()
    );
    seed(&cache, &body(), now() + 3600);
    assert!(review(&path, &cache, &vault).models[0].reference.is_none());
    seed(&cache, &body(), now());
    Config::set_in(&path, "endpoints.cloud.url", "http://localhost:1234/v1").unwrap();
    assert!(review(&path, &cache, &vault).models[0].provider.is_none());
    Config::set_in(&path, "endpoints.cloud.url", "https://api.openai.com/v1").unwrap();
    Config::set_in(&path, "endpoints.cloud.proxy", "http://127.0.0.1:99").unwrap();
    assert!(review(&path, &cache, &vault).models[0].provider.is_none());
}

#[test]
fn decoder_reaches_byte_provider_model_string_duplicate_and_alias_limits() {
    let text = body();
    assert!(text.len() > 100);
    assert!(decode(&text, Settings { max_bytes: 100, ..Default::default() }).unwrap_err().contains("byte"));
    let provider_overflow = r#"{"p":{"id":"p","models":{}},"q":{"id":"q","models":{}}}"#;
    assert!(
        decode(provider_overflow, Settings { max_providers: 1, ..Default::default() })
            .unwrap_err()
            .contains("entry limit")
    );
    let models_overflow = r#"{"p":{"id":"p","models":{"a":{"id":"a"},"b":{"id":"b"}}}}"#;
    assert!(
        decode(models_overflow, Settings { max_models: 1, ..Default::default() })
            .unwrap_err()
            .contains("entry limit")
    );
    let too_long = "ж".repeat(257);
    assert!(too_long.len() > MAX_TEXT);
    let long = json!({"p":{"id":"p","models":{too_long.clone():{"id":too_long}}}}).to_string();
    assert!(decode(&long, Settings::default()).is_err());
    assert!(
        decode(r#"{"p":{"id":"p","models":{"a":{"id":"a"},"a":{"id":"a"}}}}"#, Settings::default())
            .unwrap_err()
            .contains("duplicate")
    );
    assert!(
        decode(&text.replace("\"id\":\"physical\"", "\"id\":\"Physical\""), Settings::default())
            .unwrap_err()
            .contains("aliases")
    );
    assert!(decode(&text.replace("\"id\":\"openai\"", "\"id\":\"other\""), Settings::default()).is_err());
    assert!(decode(&text.replace("\"input\":2.0", "\"input\":-1"), Settings::default()).is_err());
}

#[test]
fn variable_rates_and_provider_overrides_are_visible_but_cannot_be_applied() {
    let (_temp, path, cache) = fixture();
    for extra in ["tier", "reasoning", "audio", "provider"] {
        let mut data: serde_json::Value = serde_json::from_str(&body()).unwrap();
        let model = &mut data["openai"]["models"]["physical"];
        match extra {
            "tier" => model["cost"]["tiers"] = json!([{ "input":20 }]),
            "reasoning" => model["cost"]["reasoning"] = json!(77),
            "audio" => model["cost"]["input_audio"] = json!(77),
            _ => model["provider"] = json!({"api":"https://elsewhere"}),
        }
        seed(&cache, &data.to_string(), now());
        let listing = review(&path, &cache, &Vault::empty());
        assert!(listing.models[0].reference.is_some());
        assert!(listing.models[0].review_token.is_none(), "{extra}");
    }
}

#[tokio::test]
async fn actual_refresh_admits_headers_chunks_counts_and_preserves_previous_cache_on_failure() {
    use std::io::{Read, Write};
    for mode in ["good", "header", "chunk", "count", "redirect", "status"] {
        let temp = tempfile::tempdir().unwrap();
        seed(temp.path(), &body(), now());
        let before = std::fs::read(temp.path().join(FILE)).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let response = match mode {
            "header" => "HTTP/1.1 200 OK\r\nContent-Length: 99999\r\nConnection: close\r\n\r\n".to_string(),
            "chunk" => {
                let chunk = " ".repeat(4097);
                format!(
                    "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{chunk}\r\n0\r\n\r\n",
                    chunk.len()
                )
            }
            "redirect" => {
                "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/secrets\r\nContent-Length: 0\r\n\r\n"
                    .into()
            }
            "status" => "HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\n\r\n".into(),
            _ => {
                let text = if mode == "count" {
                    r#"{"p":{"id":"p","models":{"a":{"id":"a"},"b":{"id":"b"}}}}"#.to_string()
                } else {
                    body()
                };
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                )
            }
        };
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_nonblocking(false).unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
            let mut request = [0; 4096];
            let n = socket.read(&mut request).unwrap();
            assert!(!String::from_utf8_lossy(&request[..n]).to_ascii_lowercase().contains("authorization:"));
            let _ = socket.write_all(response.as_bytes());
        });
        let result =
            fetch(temp.path(), Settings { max_bytes: 4096, max_models: 1, ..Default::default() }, &url).await;
        server.join().unwrap();
        if mode == "good" {
            assert!(result.is_ok(), "{result:?}");
            assert_eq!(read(temp.path(), Settings::default()).unwrap().models.len(), 1);
        } else {
            assert!(result.is_err(), "{mode}");
            assert_eq!(std::fs::read(temp.path().join(FILE)).unwrap(), before);
        }
    }
}
