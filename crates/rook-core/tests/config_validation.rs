use rook_core::{Config, Vault};

#[test]
fn mcp_catalog_download_limits_are_shared_with_connection_validation() {
    for (field, value) in [
        ("catalog_max_bytes", 1023),
        ("catalog_max_tools", 0),
        ("catalog_max_pages", 257),
        ("catalog_timeout_secs", 0),
    ] {
        let config: Config = toml::from_str(&format!(
            "[[mcp]]\nname='remote'\nurl='https://example.invalid/mcp'\n{field}={value}"
        ))
        .unwrap();
        assert!(config.mcp[0].catalog_error().unwrap().contains(field));
        assert!(config.validation_errors().iter().any(|e| e.contains(field)));
    }
    let config: Config=toml::from_str("[[mcp]]\nname='remote'\nurl='https://example.invalid/mcp'\ncatalog_max_bytes=1024\ncatalog_max_tools=2\ncatalog_max_pages=3\ncatalog_timeout_secs=1").unwrap();
    assert!(config.validation_errors().is_empty());
    let read: Config = toml::from_str(&config.as_written().unwrap()).unwrap();
    assert_eq!(read.mcp[0].catalog_max_bytes, 1024);
    assert_eq!(read.mcp[0].catalog_max_tools, 2);
    assert_eq!(read.mcp[0].catalog_max_pages, 3);
    assert_eq!(read.mcp[0].catalog_timeout_secs, 1);
}

#[test]
fn transcript_limits_validate_and_roundtrip_without_silent_clamping() {
    for (field, invalid) in [
        ("page_entries", 0),
        ("page_bytes", 4095),
        ("body_bytes", 65537),
        ("search_bytes", 4095),
        ("search_events", 4097),
        ("quote_bytes", 127),
    ] {
        let config: Config = toml::from_str(&format!("[transcript]\n{field}={invalid}")).unwrap();
        assert!(config.validation_errors().iter().any(|e| e.contains(&format!("transcript.{field}"))));
    }
    let config: Config = toml::from_str("[transcript]\npage_entries=3\npage_bytes=4096\nbody_bytes=128\nsearch_bytes=4096\nsearch_events=2\nquote_bytes=512").unwrap();
    assert!(config.validation_errors().is_empty());
    let written = config.as_written().unwrap();
    let reread: Config = toml::from_str(&written).unwrap();
    assert_eq!(
        serde_json::to_value(config.transcript).unwrap(),
        serde_json::to_value(reread.transcript).unwrap()
    );
}

#[test]
fn offline_validation_and_model_resolution_reject_the_same_structural_errors() {
    let base = "[endpoints.desk]\napi='openai'\nurl='http://localhost:8080/v1'\n\
                [models.local]\nmodel='example'\nendpoint='desk'\n";
    for extra in [
        "api='openai'",
        "metadata_api='ollama'",
        "url='http://elsewhere/v1'",
        "key='secret:missing'",
        "parallel=2",
        "key_in_the_clear=true",
        "proxy='direct'",
    ] {
        let config: Config = toml::from_str(&format!("{base}{extra}\n")).unwrap();
        let errors = config.validation_errors();
        let runtime = rook_core::models::endpoint_for(&config, &Vault::empty(), "local").unwrap_err();
        assert_eq!(errors, [runtime.to_string()], "conflict: {extra}");
        assert!(errors[0].contains("also sets"), "{errors:?}");
    }
    for (text, expected) in [
        ("[models.local]\nmodel='x'\nendpoint='missing'", "no `[endpoints.missing]`"),
        ("[models.local]\nmodel='x'", "api` is not set"),
        ("[models.local]\nmodel='x'\napi='unknown'", "not an api"),
        ("[models.local]\nmodel='x'\napi='openai'", "url` is not set"),
        ("[models.local]\napi='openai'\nurl='https://private:credential@host/v1'", "model` is not set"),
    ] {
        let config: Config = toml::from_str(text).unwrap();
        let errors = config.validation_errors();
        let runtime = rook_core::models::endpoint_for(&config, &Vault::empty(), "local").unwrap_err();
        assert_eq!(errors, [runtime.to_string()]);
        assert!(errors[0].contains(expected), "{errors:?}");
        assert!(!errors[0].contains("credential"), "diagnostics must not echo URL credentials");
    }
}

#[test]
fn offline_validation_accepts_external_secrets_and_runtime_endpoint_whitespace() {
    let config: Config = toml::from_str(
        "[endpoints.desk]\napi='openai'\nurl='http://localhost:8080/v1'\nkey='secret:missing'\n\
         [models.local]\nmodel='example'\nendpoint=' desk '\n",
    )
    .unwrap();
    assert!(config.validation_errors().is_empty());
    assert!(
        rook_core::models::endpoint_for(&config, &Vault::empty(), "local").is_err(),
        "offline checking must not require credentials that only runtime can resolve"
    );
}

#[test]
fn every_accepted_compaction_threshold_is_used_unchanged() {
    let mut config = Config::default();
    for value in [0.1, 0.5, 0.75, 0.9] {
        config.agent.compact_at = value;
        assert!(config.validation_errors().is_empty());
        assert_eq!(rook_core::context::ContextBudget::new(8192, value).compact_at, value);
    }
    for value in [f32::NAN, f32::INFINITY, -1.0, 0.0, 0.09, 0.91, 1.0] {
        config.agent.compact_at = value;
        assert!(config.validation_errors().iter().any(|e| e.contains("compact_at")), "{value}");
    }
}

#[test]
fn offline_validation_reports_independent_errors_together_and_allows_disabled_servers() {
    let config: Config = toml::from_str(
        "[agent]\ncompact_at=1.0\n[models.broken]\nmodel='example'\n\
         [[mcp]]\nname='duplicate'\n[[mcp]]\nname='duplicate'\nenabled=false\n",
    )
    .unwrap();
    let errors = config.validation_errors();
    assert_eq!(errors.len(), 4, "{errors:?}");
    let config: Config = toml::from_str("[[mcp]]\nname='later'\nenabled=false\n").unwrap();
    assert!(config.validation_errors().is_empty());
}

#[test]
fn model_catalog_limits_reject_values_the_runtime_would_clamp() {
    for (field, invalid) in [
        ("max_bytes", 100),
        ("max_models", 0),
        ("max_pages", 0),
        ("timeout_secs", 61),
        ("cache_max_bytes", 1024),
        ("cache_max_entries", 0),
        ("learned_window_entries", 0),
        ("cache_ttl_secs", 86401),
    ] {
        let config: Config = toml::from_str(&format!("[model_catalog]\n{field}={invalid}\n")).unwrap();
        assert!(config.validation_errors().iter().any(|e| e.contains(&format!("model_catalog.{field}"))));
    }
    let config: Config =
        toml::from_str("[model_catalog]\nmax_bytes=2048\nmax_models=2\ntimeout_secs=1\n").unwrap();
    assert!(config.validation_errors().is_empty());
    assert_eq!(config.model_catalog.limits.bounded().max_bytes, 2048);
    assert_eq!(config.model_catalog.limits.bounded().max_models, 2);
    assert_eq!(config.model_catalog.limits.bounded().timeout_secs, 1);
}

#[test]
fn model_catalog_settings_keep_the_flat_configuration_shape() {
    let config: Config = toml::from_str("[model_catalog]\nmax_bytes=2048\nmax_models=2\ntimeout_secs=1\ncache_enabled=false\ncache_ttl_secs=0\ncache_max_entries=4\ncache_max_bytes=8192\n").unwrap();
    assert!(config.validation_errors().is_empty());
    let value = serde_json::to_value(&config).unwrap();
    assert_eq!(value["model_catalog"]["max_bytes"], 2048);
    assert_eq!(value["model_catalog"]["cache_max_entries"], 4);
    assert_eq!(value["model_catalog"]["cache_enabled"], false);
    assert!(value["model_catalog"].get("limits").is_none());
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("config.toml");
    std::fs::write(&path, config.as_written().unwrap()).unwrap();
    assert!(Config::ignored_in(&path).is_empty());
}

#[test]
fn native_metadata_requires_a_matching_generation_api_and_known_mode() {
    let config: Config = toml::from_str(
        "[models.local]\nmodel='x'\napi='anthropic'\nurl='http://localhost:1234'\nmetadata_api='ollama'\n",
    )
    .unwrap();
    let errors = config.validation_errors();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("metadata_api"));
    assert_eq!(
        errors[0],
        rook_core::models::endpoint_for(&config, &Vault::empty(), "local").unwrap_err().to_string()
    );
    assert!(toml::from_str::<Config>("[endpoints.local]\nmetadata_api='typo'\n").is_err());
}

#[test]
fn responses_is_available_in_named_sources_shared_endpoints_and_editor_choices() {
    for api in ["responses", "openai-responses"] {
        let text = format!(
            "[endpoints.cloud]\napi='{api}'\nurl='https://api.openai.com/v1'\nkey='literal-key'\n[models.reasoner]\nendpoint='cloud'\nmodel='gpt-6-astra'\n"
        );
        let config: Config = toml::from_str(&text).unwrap();
        assert!(config.validation_errors().is_empty());
        let endpoint =
            rook_core::models::endpoint_for(&config, &Vault::empty(), "reasoner").unwrap().unwrap();
        assert_eq!(endpoint.api, rook_llm::Api::Responses);
    }
    assert!(rook_llm::Api::ALL.iter().any(|api| api.as_str() == "responses"));
}

#[test]
fn provider_state_limits_are_validated_without_connecting() {
    for (limit, valid) in [
        (0, false),
        (1023, false),
        (1024, true),
        (4 * 1024 * 1024, true),
        (32 * 1024 * 1024, true),
        (32 * 1024 * 1024 + 1, false),
    ] {
        let mut config = Config::default();
        config.agent.max_provider_state_bytes = limit;
        assert_eq!(
            !config.validation_errors().iter().any(|error| error.contains("max_provider_state_bytes")),
            valid
        );
    }
}

#[test]
fn mcp_advertisement_limits_validate_and_keep_zero_as_deferred_only() {
    for (field, invalid) in [
        ("max_bytes", 4095),
        ("max_bytes", 1048577),
        ("max_server_bytes", 262145),
        ("max_server_tools", 129),
        ("max_tools", 1),
        ("max_tools", 65),
    ] {
        let config: Config = toml::from_str(&format!("[mcp_catalog]\n{field}={invalid}\n")).unwrap();
        assert!(config.validation_errors().iter().any(|e| e.contains(&format!("mcp_catalog.{field}"))));
    }
    let config: Config =
        toml::from_str("[mcp_catalog]\nmax_bytes=4096\nmax_server_bytes=0\nmax_server_tools=0\n").unwrap();
    assert!(config.validation_errors().is_empty());
}

#[test]
fn connection_limits_validate_offline_and_roundtrip() {
    for (field, value) in
        [("max_servers", 0), ("max_servers", 257), ("parallel_connects", 0), ("parallel_connects", 33)]
    {
        let config: Config = toml::from_str(&format!("[mcp_connections]\n{field}={value}")).unwrap();
        assert!(config.mcp_connections.error().unwrap().contains(field));
        assert!(config.validation_errors().iter().any(|e| e.contains(field)));
    }
    let config: Config = toml::from_str("[mcp_connections]\nmax_servers=1\nparallel_connects=1\n[[mcp]]\nname='one'\nenabled=false\n[[mcp]]\nname='two'\nenabled=false").unwrap();
    assert!(config.mcp.len() > config.mcp_connections.max_servers);
    assert!(config.validation_errors().iter().any(|e| e.contains("too many")));
    let reread: Config = toml::from_str(&config.as_written().unwrap()).unwrap();
    assert_eq!(reread.mcp_connections.max_servers, 1);
    assert_eq!(reread.mcp_connections.parallel_connects, 1);
}

#[test]
fn oauth_limits_are_the_same_in_offline_config_and_protocol_setup() {
    for (field, value) in [("max_response_bytes", 4095), ("timeout_secs", 0), ("login_timeout_secs", 1801)] {
        let config: Config = toml::from_str(&format!(
            "[[mcp]]\nname='server'\nurl='https://example.com/mcp'\n[mcp.oauth]\n{field}={value}"
        ))
        .unwrap();
        assert!(config.mcp[0].oauth.error().unwrap().contains(field));
        assert!(config.validation_errors().iter().any(|error| error.contains(field)));
    }
    for (field, value) in [
        ("oauth_max_entries", 0),
        ("oauth_max_bytes", 65535),
        ("oauth_max_pending", 0),
        ("oauth_max_pending", 65),
    ] {
        let config: Config = toml::from_str(&format!("[mcp_connections]\n{field}={value}")).unwrap();
        assert!(config.validation_errors().iter().any(|error| error.contains(field)));
    }
    let config:Config=toml::from_str("[[mcp]]\nname='server'\nurl='https://example.com/mcp'\n[mcp.oauth]\nclient_id='public-client'\ncallback_port=9321\nscopes=['files:read']").unwrap();
    assert!(config.validation_errors().is_empty());
    let reread: Config = toml::from_str(&config.as_written().unwrap()).unwrap();
    assert_eq!(reread.mcp[0].oauth.client_id, "public-client");
    assert_eq!(reread.mcp[0].oauth.scopes, ["files:read"]);
    assert_eq!(reread.mcp[0].oauth.callback_port, 9321);
}
