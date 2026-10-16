#![cfg(unix)]
use rook_core::{Config, Rook, Vault, agent::policy_for, mcp_apps::Request};
use serde_json::json;
fn setup(root: &std::path::Path) -> Rook {
    let workspace = root.join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let mut config = Config::default();
    config.agent.install_servers = false;
    config.mcp_connections.apps_enabled = true;
    config.mcp.push(rook_mcp::ServerConfig {
        name: "fixture".into(),
        command: "python3".into(),
        args: vec![
            format!("{}/tests/fixtures/mcp_apps.py", env!("CARGO_MANIFEST_DIR")),
            root.join("calls").display().to_string(),
        ],
        ..Default::default()
    });
    Rook::from_parts(
        rook_store::Store::open(root.join("store")).unwrap(),
        config,
        rook_skills::Environment::bare("linux", "x86_64", "1"),
        Default::default(),
        workspace,
    )
}
#[tokio::test]
async fn app_calls_require_exact_unexpired_one_shot_consent_and_never_cross_sessions_or_connections() {
    let root = tempfile::tempdir().unwrap();
    let rook = setup(root.path());
    let owner = rook.start_session("apps").unwrap();
    let foreign = rook.start_session("other").unwrap();
    let manager = rook.connect_mcp().await;
    let policy = policy_for(&rook.config);
    let items = rook.mcp_app_request(owner, &manager, &policy, &Vault::empty(), Request::List).await.unwrap();
    assert_eq!(items["items"][0]["uri"], "ui://fixture/view");
    let opened = rook
        .mcp_app_request(
            owner,
            &manager,
            &policy,
            &Vault::empty(),
            Request::Open { server: "fixture".into(), uri: "ui://fixture/view".into(), generation: 1 },
        )
        .await
        .unwrap();
    let mut id = opened["app"].as_str().unwrap().to_owned();
    assert!(opened["html"].as_str().unwrap().contains("Rook app fixture"));
    assert!(!root.path().join("calls").exists());
    let prepare = |id: &str| Request::Prepare {
        app: id.into(),
        tool: "count".into(),
        arguments: json!({"label":"fixture"}),
    };
    assert!(rook.mcp_app_request(foreign, &manager, &policy, &Vault::empty(), prepare(&id)).await.is_err());
    assert!(
        rook.mcp_app_request(
            owner,
            &manager,
            &policy,
            &Vault::empty(),
            Request::Prepare { app: id.clone(), tool: "other-server__count".into(), arguments: json!({}) }
        )
        .await
        .is_err()
    );
    let pending =
        rook.mcp_app_request(owner, &manager, &policy, &Vault::empty(), prepare(&id)).await.unwrap();
    let nonce = pending["nonce"].as_str().unwrap();
    assert!(!root.path().join("calls").exists(), "preparation is not execution");
    assert!(
        rook.mcp_app_request(
            owner,
            &manager,
            &policy,
            &Vault::empty(),
            Request::Confirm { app: id.clone(), nonce: "forged".into(), approved: true }
        )
        .await
        .is_err()
    );
    let declined = rook
        .mcp_app_request(
            owner,
            &manager,
            &policy,
            &Vault::empty(),
            Request::Confirm { app: id.clone(), nonce: nonce.into(), approved: false },
        )
        .await
        .unwrap();
    assert_eq!(declined["isError"], true);
    assert!(!root.path().join("calls").exists());
    assert!(rook.mcp_app_request(owner, &manager, &policy, &Vault::empty(), prepare(&id)).await.is_err());
    rook.mcp_app_request(owner, &manager, &policy, &Vault::empty(), Request::Close { app: id })
        .await
        .unwrap();
    let reopened = rook
        .mcp_app_request(
            owner,
            &manager,
            &policy,
            &Vault::empty(),
            Request::Open { server: "fixture".into(), uri: "ui://fixture/view".into(), generation: 1 },
        )
        .await
        .unwrap();
    id = reopened["app"].as_str().unwrap().into();
    let pending =
        rook.mcp_app_request(owner, &manager, &policy, &Vault::empty(), prepare(&id)).await.unwrap();
    let nonce = pending["nonce"].as_str().unwrap().to_owned();
    let result = rook
        .mcp_app_request(
            owner,
            &manager,
            &policy,
            &Vault::empty(),
            Request::Confirm { app: id.clone(), nonce: nonce.clone(), approved: true },
        )
        .await
        .unwrap();
    assert_eq!(result["content"][0]["text"], "1");
    assert!(
        rook.mcp_app_request(
            owner,
            &manager,
            &policy,
            &Vault::empty(),
            Request::Confirm { app: id.clone(), nonce, approved: true }
        )
        .await
        .is_err()
    );
    assert_eq!(std::fs::read_to_string(root.path().join("calls")).unwrap().lines().count(), 1);
    manager.reconnect("fixture", rook.mcp_servers(), &rook.config.proxy.for_mcp()).await.unwrap();
    assert!(rook.mcp_app_request(owner, &manager, &policy, &Vault::empty(), prepare(&id)).await.is_err());
    manager.shutdown().await;
}
#[tokio::test]
async fn disabled_apps_and_denied_tools_do_not_get_implicit_ui_permission() {
    let root = tempfile::tempdir().unwrap();
    let mut rook = setup(root.path());
    let owner = rook.start_session("apps").unwrap();
    let manager = rook.connect_mcp().await;
    rook.config.mcp_connections.apps_enabled = false;
    assert!(
        rook.mcp_app_request(owner, &manager, &policy_for(&rook.config), &Vault::empty(), Request::List)
            .await
            .is_err()
    );
    rook.config.mcp_connections.apps_enabled = true;
    rook.config.sandbox.deny.push("fixture".into());
    assert!(
        rook.mcp_app_request(
            owner,
            &manager,
            &policy_for(&rook.config),
            &Vault::empty(),
            Request::Open { server: "fixture".into(), uri: "ui://fixture/view".into(), generation: 1 }
        )
        .await
        .is_err()
    );
    assert!(!root.path().join("calls").exists());
    manager.shutdown().await;
}

#[tokio::test]
async fn explicit_app_consent_cannot_bypass_a_pre_tool_hook() {
    let root = tempfile::tempdir().unwrap();
    let mut rook = setup(root.path());
    rook.config.hooks.push(rook_core::hooks::HookConfig {
        event: rook_core::hooks::Event::PreTool,
        command: "printf '%s' '{\"decision\":\"deny\"}'".into(),
        ..Default::default()
    });
    let owner = rook.start_session("hook gate").unwrap();
    let manager = rook.connect_mcp().await;
    let policy = policy_for(&rook.config);
    let opened = rook
        .mcp_app_request(
            owner,
            &manager,
            &policy,
            &Vault::empty(),
            Request::Open { server: "fixture".into(), uri: "ui://fixture/view".into(), generation: 1 },
        )
        .await
        .unwrap();
    let id = opened["app"].as_str().unwrap();
    let pending = rook
        .mcp_app_request(
            owner,
            &manager,
            &policy,
            &Vault::empty(),
            Request::Prepare { app: id.into(), tool: "count".into(), arguments: json!({"label":"fixture"}) },
        )
        .await
        .unwrap();
    assert!(
        rook.mcp_app_request(
            owner,
            &manager,
            &policy,
            &Vault::empty(),
            Request::Confirm {
                app: id.into(),
                nonce: pending["nonce"].as_str().unwrap().into(),
                approved: true
            }
        )
        .await
        .is_err()
    );
    assert!(!root.path().join("calls").exists());
    manager.shutdown().await;
}
