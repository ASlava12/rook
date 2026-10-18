#[path = "../../rook-mcp/tests/support/catalog_http.rs"]
mod catalog_http;
use catalog_http::Fixture;
use rook_mcp::{Server, ServerConfig};
use rook_tools::{ToolBox, ToolContext, mcp::CatalogLimits, policy::Risk};
use serde_json::{Value, json};
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

async fn connected(fixture: &Fixture) -> Arc<Server> {
    Arc::new(
        Server::connect(
            &ServerConfig {
                name: "catalog".into(),
                url: Some(fixture.url.clone()),
                refresh_tools: true,
                ..Default::default()
            },
            &rook_llm::Proxy::Direct,
        )
        .await
        .unwrap(),
    )
}
fn context() -> ToolContext {
    let mut ctx = ToolContext::new(std::env::temp_dir());
    ctx.max_output_bytes = 4096;
    ctx
}

#[tokio::test]
async fn refreshed_bindings_keep_old_boxes_and_invocations_and_reject_changed_page_continuations() {
    let fixture = Fixture::new(false).await;
    let server = connected(&fixture).await;
    let mut tools = ToolBox::default();
    tools.register_mcp_catalog(
        [(server.clone(), server.list_tools().await.unwrap())],
        CatalogLimits::default(),
    );
    tools.refresh_mcp_catalog(CatalogLimits::default(), Duration::from_secs(10)).await;
    let old = tools.clone();
    let removed = tools.without(&["catalog__old"]);
    assert!(removed.get("catalog__old").is_none());
    assert!(
        removed
            .call(&context(), "mcp_call", &json!({"name":"catalog__old","arguments":{}}))
            .await
            .unwrap()
            .is_error,
        "removing a tool must remove its deferred route immediately"
    );
    let invocation = tools.get("catalog__old").unwrap().clone();
    let mut restricted = tools.clone();
    restricted.restrict_mcp(&Default::default(), CatalogLimits::default());
    let mut dropped = tools.without(&["mcp_call", "catalog__new"]);
    let page = tools.call(&context(), "mcp_tools", &json!({"limit":1})).await.unwrap();
    let page: Value = serde_json::from_str(&page.content).unwrap();
    assert!(page["next_offset"].as_u64().unwrap() > 0);
    server.call_tool("advance", &json!({})).await.unwrap();
    assert!(tools.mcp_catalog_changed());
    assert!(tools.refresh_mcp_catalog(CatalogLimits::default(), Duration::from_secs(10)).await);
    assert!(tools.get("catalog__old").is_none());
    assert!(tools.get("catalog__new").is_some());
    assert!(!tools.mcp_catalog_changed());
    assert_eq!(invocation.spec().description, "old");
    assert!(matches!(tools.get("catalog__new").unwrap().risk(&json!({})), Risk::External { .. }));
    assert!(old.get("catalog__old").is_some() && old.get("catalog__new").is_none());
    let continuation = json!({"limit":1,"offset":page["next_offset"],"source":page["source"]});
    assert!(tools.call(&context(), "mcp_tools", &continuation).await.unwrap().is_error);
    assert!(!old.call(&context(), "mcp_tools", &continuation).await.unwrap().is_error);
    assert!(
        old.call(
            &context(),
            "mcp_tools",
            &json!({"query":"different","offset":page["next_offset"],"source":page["source"]})
        )
        .await
        .unwrap()
        .is_error
    );
    dropped.refresh_mcp_catalog(CatalogLimits::default(), Duration::from_secs(10)).await;
    assert!(dropped.get("mcp_call").is_none() && dropped.get("catalog__new").is_none());
    restricted.refresh_mcp_catalog(CatalogLimits::default(), Duration::from_secs(10)).await;
    assert!(restricted.get("mcp_call").is_none() && restricted.get("catalog__new").is_none());
    assert!(
        !tools
            .call(&context(), "mcp_call", &json!({"name":"catalog__new","arguments":{}}))
            .await
            .unwrap()
            .is_error
    );

    let replacement = Fixture::new(false).await;
    let next = connected(&replacement).await;
    tools.register_mcp_catalog([(next.clone(), next.list_tools().await.unwrap())], CatalogLimits::default());
    tools.refresh_mcp_catalog(CatalogLimits::default(), Duration::from_secs(10)).await;
    invocation.call(&context(), &json!({})).await.unwrap();
    tools.call(&context(), "catalog__old", &json!({})).await.unwrap();
    assert_eq!(*fixture.state.calls.lock().unwrap(), ["advance", "new", "old"]);
    assert_eq!(*replacement.state.calls.lock().unwrap(), ["old"]);
    server.shutdown().await;
    next.shutdown().await;
}

#[tokio::test]
async fn a_catalog_can_remove_all_tools_and_add_them_again_without_restoring_excluded_routes() {
    let fixture = Fixture::new(false).await;
    let server = connected(&fixture).await;
    let mut tools = ToolBox::default();
    tools.register_mcp_catalog(
        [(server.clone(), server.list_tools().await.unwrap())],
        CatalogLimits::default(),
    );
    tools.refresh_mcp_catalog(CatalogLimits::default(), Duration::from_secs(10)).await;
    fixture.state.empty.store(true, Ordering::SeqCst);
    server.call_tool("advance", &json!({})).await.unwrap();
    tools.refresh_mcp_catalog(CatalogLimits::default(), Duration::from_secs(10)).await;
    assert!(tools.get("catalog__old").is_none() && tools.get("mcp_tools").is_none());
    assert_eq!(tools.mcp_catalog_summary().discovered, 0);
    fixture.state.empty.store(false, Ordering::SeqCst);
    server.call_tool("advance", &json!({})).await.unwrap();
    tools.refresh_mcp_catalog(CatalogLimits::default(), Duration::from_secs(10)).await;
    assert!(tools.get("catalog__new").is_some() && tools.get("mcp_tools").is_some());
    server.shutdown().await;
}
