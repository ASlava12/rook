use rook_llm::ToolSpec;
use rook_mcp::{Server, ServerConfig, ToolDescriptor};
use rook_tools::{
    ToolBox, ToolContext,
    mcp::{CatalogLimits, namespaced},
    policy::Risk,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn server(name: &str) -> (Arc<Server>, Arc<Mutex<Vec<Value>>>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let task = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut raw = Vec::new();
            let request: Value = loop {
                let mut buffer = [0; 4096];
                let n = socket.read(&mut buffer).await.unwrap();
                if n == 0 {
                    break Value::Null;
                }
                assert!(raw.len() + n <= 65536);
                raw.extend_from_slice(&buffer[..n]);
                let text = String::from_utf8_lossy(&raw);
                let Some((head, body)) = text.split_once("\r\n\r\n") else { continue };
                let length = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if body.len() < length {
                    continue;
                }
                break serde_json::from_str(body).unwrap_or_default();
            };
            if request["id"].is_null() {
                socket
                    .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await
                    .ok();
                continue;
            }
            let result = match request["method"].as_str().unwrap() {
                "initialize" => {
                    json!({"protocolVersion":"2025-06-18","serverInfo":{"name":"fixture","version":"1"},"capabilities":{}})
                }
                "tools/call" => {
                    recorded.lock().unwrap().push(request["params"].clone());
                    json!({"content":[{"type":"text","text":"called"},{"type":"image","mimeType":"image/png","data":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg=="}]})
                }
                _ => panic!("unexpected request {request}"),
            };
            let body = json!({"jsonrpc":"2.0","id":request["id"],"result":result}).to_string();
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(header.as_bytes()).await.unwrap();
            socket.write_all(body.as_bytes()).await.unwrap();
        }
    });
    let server = Server::connect(
        &ServerConfig { name: name.into(), url: Some(url), ..Default::default() },
        &rook_llm::Proxy::Direct,
    )
    .await
    .unwrap();
    (Arc::new(server), calls, task)
}
fn descriptor(name: &str, description: &str) -> ToolDescriptor {
    ToolDescriptor {
        name: name.into(),
        description: description.into(),
        input_schema: json!({"type":"object","properties":{"items":{"type":"array","items":{"type":"object","properties":{"mode":{"type":"string","enum":["read","write"]}},"required":["mode"],"additionalProperties":false}}},"required":["items"],"additionalProperties":false}),
        annotations: Default::default(),
    }
}

#[tokio::test]
async fn overflow_stays_discoverable_callable_and_bounded_without_changing_schemas_or_policy() {
    let (alpha, calls, task1) = server("alpha").await;
    let (beta, _, task2) = server("beta").await;
    let small = descriptor("first", "Handle a structured request.");
    let huge = descriptor("last", &"Каталог 📦 \"text\" ".repeat(2000));
    let expected = ToolSpec {
        name: "alpha__last".into(),
        description: huge.description.clone(),
        parameters: huge.input_schema.clone(),
    };
    let limits =
        CatalogLimits { max_tools: 64, max_bytes: 4096, max_server_bytes: 1200, max_server_tools: 1 };
    assert!(serde_json::to_vec(&expected).unwrap().len() > limits.max_bytes);
    let input = vec![(beta.clone(), vec![small.clone()]), (alpha.clone(), vec![huge.clone(), small.clone()])];
    let mut tools = ToolBox::default();
    tools.register_mcp_catalog(input.clone(), limits);
    let mut reversed = ToolBox::default();
    reversed.register_mcp_catalog(
        input.into_iter().rev().map(|(s, mut t)| {
            t.reverse();
            (s, t)
        }),
        limits,
    );
    let specs = serde_json::to_value(tools.specs()).unwrap();
    assert_eq!(
        specs,
        serde_json::to_value(reversed.specs()).unwrap(),
        "server/list order cannot change the prefix"
    );
    assert_eq!(specs, serde_json::to_value(tools.stubs()).unwrap(), "lazy mode must preserve nested schemas");
    assert!(serde_json::to_vec(&specs).unwrap().len() <= limits.max_bytes);
    assert!(tools.get("alpha__last").is_none(), "fixture must exceed advertisement budget");
    assert_eq!(tools.get("alpha__first").unwrap().spec().parameters, small.input_schema);
    assert!(tools.get("beta__first").is_some());
    let summary = tools.mcp_catalog_summary();
    assert_eq!((summary.discovered, summary.advertised, summary.deferred), (3, 2, 1));
    assert_eq!(summary.deferred_names, ["alpha__last"]);
    assert_eq!(summary.omitted_deferred, 0);

    let mut ctx = ToolContext::new(std::env::temp_dir());
    ctx.max_output_bytes = 4096;
    let mut found = Vec::new();
    let mut offset = 0;
    loop {
        let answer = tools.call(&ctx, "mcp_tools", &json!({"limit":1,"offset":offset})).await.unwrap();
        assert!(!answer.is_error, "{}", answer.content);
        let page: Value = serde_json::from_str(&answer.content).unwrap();
        found.extend(
            page["tools"].as_array().unwrap().iter().map(|v| v["name"].as_str().unwrap().to_string()),
        );
        let Some(next) = page["next_offset"].as_u64() else { break };
        assert!(next > offset);
        offset = next;
    }
    assert_eq!(found, ["alpha__first", "alpha__last", "beta__first"]);
    let mut schema = String::new();
    let mut offset = 0;
    loop {
        let page = tools
            .call(&ctx, "mcp_tools", &json!({"name":"alpha__last","offset":offset,"limit":701}))
            .await
            .unwrap();
        assert!(page.content.len() <= ctx.max_output_bytes, "{}", page.content.len());
        assert!(!page.is_error, "{}", page.content);
        let page: Value = serde_json::from_str(&page.content).unwrap();
        assert_eq!(page["offset"], offset);
        schema.push_str(page["schema"].as_str().unwrap());
        let Some(next) = page["next_offset"].as_u64() else { break };
        assert!(next > offset);
        offset = next;
    }
    assert_eq!(
        serde_json::from_str::<Value>(&schema).unwrap(),
        serde_json::to_value(&expected).unwrap(),
        "Unicode, escaping and all constraints survive pagination"
    );
    let args = json!({"name":"alpha__last","arguments":{"items":[{"mode":"write"}]}});
    assert_eq!(
        tools.get("mcp_call").unwrap().risk(&args),
        Risk::External { name: "alpha__last".into(), claims_read_only: false }
    );
    assert_eq!(tools.get("mcp_call").unwrap().invocation(&args), ("alpha__last", &args["arguments"]));
    let result = tools.call(&ctx, "mcp_call", &args).await.unwrap();
    assert!(!result.is_error);
    assert_eq!(result.images.len(), 1);
    assert_eq!(calls.lock().unwrap().as_slice(), &[json!({"name":"last","arguments":args["arguments"]})]);
    for args in [json!({"name":"absent","arguments":{}}), json!({"name":"alpha__last","arguments":null})] {
        assert!(tools.call(&ctx, "mcp_call", &args).await.unwrap().is_error);
    }
    assert_eq!(calls.lock().unwrap().len(), 1, "invalid calls cannot reach the server");
    let mut deferred = ToolBox::default();
    deferred.register_mcp_catalog(
        [(alpha.clone(), vec![small, huge])],
        CatalogLimits { max_server_tools: 0, ..limits },
    );
    assert_eq!(deferred.names(), ["mcp_tools", "mcp_call"]);
    assert_eq!((deferred.mcp_catalog_summary().advertised, deferred.mcp_catalog_summary().deferred), (0, 2));
    let many: Vec<_> =
        (0..40).map(|n| descriptor(&format!("tool_{n:02}"), "A structured operation.")).collect();
    let input_bytes: usize = many
        .iter()
        .map(|d| {
            serde_json::to_vec(&ToolSpec {
                name: namespaced("alpha", &d.name),
                description: d.description.clone(),
                parameters: d.input_schema.clone(),
            })
            .unwrap()
            .len()
        })
        .sum();
    for bound in [
        CatalogLimits { max_tools: 64, max_bytes: 4096, max_server_bytes: 262144, max_server_tools: 128 },
        CatalogLimits { max_tools: 64, max_bytes: 65536, max_server_bytes: 1000, max_server_tools: 128 },
    ] {
        assert!(
            input_bytes > bound.max_bytes.min(bound.max_server_bytes),
            "fixture must exceed the byte budget without reaching the count limit"
        );
        let mut bounded = ToolBox::default();
        bounded.register_mcp_catalog([(alpha.clone(), many.clone())], bound);
        let specs = bounded.specs();
        let direct: Vec<_> = specs.iter().filter(|s| s.name.starts_with("alpha__")).collect();
        assert!(!direct.is_empty() && direct.len() < many.len());
        let summary = bounded.mcp_catalog_summary();
        assert_eq!(summary.discovered, many.len());
        assert_eq!(summary.advertised, direct.len());
        assert_eq!(summary.deferred, many.len() - direct.len());
        assert_eq!(summary.deferred_names.len(), summary.deferred.min(16));
        assert_eq!(summary.omitted_deferred, summary.deferred - summary.deferred_names.len());
        assert!(serde_json::to_vec(&specs).unwrap().len() <= bound.max_bytes);
        assert!(
            direct.iter().map(|s| serde_json::to_vec(s).unwrap().len() + 1).sum::<usize>()
                <= bound.max_server_bytes
        );
        let listing = bounded.call(&ctx, "mcp_tools", &json!({})).await.unwrap();
        assert_eq!(serde_json::from_str::<Value>(&listing.content).unwrap()["total"], many.len());
    }
    let mut count_limited = ToolBox::default();
    assert!(many.len() > 3);
    count_limited.register_mcp_catalog(
        [(alpha, many)],
        CatalogLimits { max_tools: 3, max_server_tools: 128, max_server_bytes: 262144, ..Default::default() },
    );
    assert_eq!(count_limited.specs().len(), 3, "two helpers plus one direct tool");
    assert_eq!(count_limited.mcp_catalog_summary().advertised, 1);
    assert_eq!(count_limited.mcp_catalog_summary().deferred, 39);
    task1.abort();
    task2.abort();
}

#[test]
fn external_names_are_bounded_and_keep_distinct_original_pairs_distinct() {
    let cases: [(String, String); 7] = [
        ("a.b".into(), "read".into()),
        ("a_b".into(), "read".into()),
        ("a__b".into(), "c".into()),
        ("a".into(), "b__c".into()),
        ("a".into(), "📦".repeat(200)),
        ("a".into(), "x".repeat(200)),
        ("".into(), "".into()),
    ];
    let names: std::collections::BTreeSet<_> = cases.iter().map(|(s, t)| namespaced(s, t)).collect();
    assert_eq!(names.len(), cases.len());
    assert!(names.iter().all(|n| !n.is_empty()
        && n.len() <= 64
        && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')));
    assert_eq!(namespaced("docs", "search"), "docs__search");
}
