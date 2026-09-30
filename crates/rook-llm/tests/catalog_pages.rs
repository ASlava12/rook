//! Real HTTP pages: cumulative budgets and incomplete listings must never look
//! like a successful complete catalog to its cache consumer.
use rook_llm::{CatalogLimits, Provider};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct Reply {
    status: u16,
    body: String,
    delay: Duration,
}
impl Reply {
    fn json(body: serde_json::Value) -> Self {
        Self { status: 200, body: body.to_string(), delay: Duration::ZERO }
    }
}
async fn server(replies: Vec<Reply>) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for reply in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 4096];
            while bytes.len() < 32768 && !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = socket.read(&mut buffer).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&buffer[..n]);
            }
            requests.push(String::from_utf8(bytes).unwrap());
            seen.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(reply.delay).await;
            let response = format!(
                "HTTP/1.1 {} Response\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                reply.status,
                reply.body.len(),
                reply.body
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
        requests
    });
    (url, count, task)
}
#[derive(Clone, Copy)]
enum Api {
    Anthropic,
    Google,
}
fn provider(api: Api, base: &str) -> Box<dyn Provider> {
    match api {
        Api::Anthropic => Box::new(
            rook_llm::anthropic::Anthropic::new(
                "test",
                "one",
                rook_llm::anthropic::Config::new(base.into(), "private-key".into(), "one"),
            )
            .unwrap(),
        ),
        Api::Google => Box::new(
            rook_llm::google::Google::new(
                "test",
                "one",
                rook_llm::google::Config::new(format!("{base}/v1beta"), "private-key".into(), "one"),
            )
            .unwrap(),
        ),
    }
}
fn page(api: Api, names: &[&str], cursor: Option<&str>) -> serde_json::Value {
    match api {
        Api::Anthropic => {
            serde_json::json!({"data": names.iter().map(|id| serde_json::json!({"id":id,"max_input_tokens":65536})).collect::<Vec<_>>(), "has_more":cursor.is_some(), "last_id":cursor})
        }
        Api::Google => {
            serde_json::json!({"models": names.iter().map(|id| serde_json::json!({"name":format!("models/{id}"),"inputTokenLimit":65536})).collect::<Vec<_>>(), "nextPageToken":cursor})
        }
    }
}

#[tokio::test]
async fn both_dialects_fetch_every_page_with_auth_and_an_encoded_cursor_on_the_same_endpoint() {
    let cursor = "http://127.0.0.1:1/steal?key=bad&other=é +/#";
    for api in [Api::Anthropic, Api::Google] {
        let (url, count, task) = server(vec![
            Reply::json(page(api, &["one"], Some(cursor))),
            Reply::json(page(api, &["two"], None)),
        ])
        .await;
        let models = provider(api, &url)
            .models_with(CatalogLimits { max_models: 9, ..Default::default() })
            .await
            .unwrap();
        assert_eq!(models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["one", "two"]);
        assert_eq!(models[1].context_window, Some(65536));
        assert_eq!(count.load(Ordering::SeqCst), 2);
        let requests = task.await.unwrap();
        let (path, key, size, auth) = match api {
            Api::Anthropic => ("/v1/models", "after_id", "limit", "x-api-key: private-key"),
            Api::Google => ("/v1beta/models", "pageToken", "pageSize", "x-goog-api-key: private-key"),
        };
        for (index, request) in requests.iter().enumerate() {
            let target = request.lines().next().unwrap().split_whitespace().nth(1).unwrap();
            let address = reqwest::Url::parse(&format!("{url}{target}")).unwrap();
            assert_eq!(address.path(), path);
            assert!(request.to_ascii_lowercase().contains(auth));
            let pairs: std::collections::HashMap<_, _> = address.query_pairs().into_owned().collect();
            assert_eq!(pairs[size], "9", "page size stays unchanged across Google pages");
            assert_eq!(pairs.get(key).map(String::as_str), (index == 1).then_some(cursor));
            assert_eq!(
                pairs.len(),
                if index == 0 { 1 } else { 2 },
                "cursor must not inject another query parameter"
            );
        }
    }
}

#[tokio::test]
async fn page_count_and_record_count_are_shared_across_the_whole_listing() {
    for api in [Api::Anthropic, Api::Google] {
        let (url, _, task) = server(vec![Reply::json(page(api, &["one"], Some("next")))]).await;
        let error = provider(api, &url)
            .models_with(CatalogLimits { max_pages: 1, ..Default::default() })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("exceeds 1 pages"), "{error}");
        assert_eq!(task.await.unwrap().len(), 1);
        let (url, _, task) = server(vec![
            Reply::json(page(api, &["one"], Some("next"))),
            Reply::json(page(api, &["two", "three"], None)),
        ])
        .await;
        let error = provider(api, &url)
            .models_with(CatalogLimits { max_models: 2, ..Default::default() })
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("exceeds 1 models"),
            "the second page has only one remaining slot: {error}"
        );
        assert_eq!(task.await.unwrap().len(), 2);
    }
}

#[tokio::test]
async fn bytes_are_counted_across_pages_even_when_each_page_fits_alone() {
    for api in [Api::Anthropic, Api::Google] {
        let mut first = page(api, &["one"], Some("next"));
        first["padding"] = serde_json::json!("x".repeat(550));
        let mut second = page(api, &["two"], None);
        second["padding"] = serde_json::json!("x".repeat(550));
        assert!(first.to_string().len() < 1024 && second.to_string().len() < 1024);
        let (url, _, task) = server(vec![Reply::json(first), Reply::json(second)]).await;
        let error = provider(api, &url)
            .models_with(CatalogLimits { max_bytes: 1024, ..Default::default() })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("exceeds 1024 bytes across pages"), "{error}");
        task.await.unwrap();
    }
}

#[tokio::test]
async fn later_pages_do_not_get_a_fresh_timeout() {
    let api = Api::Google;
    let mut first = Reply::json(page(api, &["one"], Some("next")));
    first.delay = Duration::from_millis(2250);
    let mut second = Reply::json(page(api, &["two"], None));
    second.delay = Duration::from_millis(2250);
    let (url, count, task) = server(vec![first, second]).await;
    let error = provider(api, &url)
        .models_with(CatalogLimits { timeout_secs: 4, ..Default::default() })
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("timed out after 4s"),
        "two individually fast pages must share one deadline: {error}"
    );
    assert_eq!(count.load(Ordering::SeqCst), 2);
    task.abort();
}

#[tokio::test]
async fn repeating_a_cursor_or_omitting_a_required_cursor_is_an_error_not_a_partial_list() {
    for api in [Api::Anthropic, Api::Google] {
        let (url, _, task) = server(vec![
            Reply::json(page(api, &["one"], Some("loop"))),
            Reply::json(page(api, &["two"], Some("loop"))),
        ])
        .await;
        let error = provider(api, &url).models().await.unwrap_err();
        assert!(error.to_string().contains("repeated a cursor"), "{error}");
        assert_eq!(task.await.unwrap().len(), 2);
    }
    for body in [
        serde_json::json!({"data":[{"id":"one"}],"has_more":true}),
        serde_json::json!({"data":[],"has_more":true,"last_id":""}),
    ] {
        let (url, _, task) = server(vec![Reply::json(body)]).await;
        assert!(
            provider(Api::Anthropic, &url)
                .models()
                .await
                .unwrap_err()
                .to_string()
                .contains("last_id is missing")
        );
        task.await.unwrap();
    }
}

#[tokio::test]
async fn overlaps_are_stable_but_duplicates_still_spend_the_record_budget() {
    let api = Api::Google;
    for limit in [3, 4] {
        let (url, _, task) = server(vec![
            Reply::json(page(api, &["one", "two"], Some("next"))),
            Reply::json(page(api, &["two", "three"], None)),
        ])
        .await;
        let result =
            provider(api, &url).models_with(CatalogLimits { max_models: limit, ..Default::default() }).await;
        if limit == 3 {
            assert!(result.is_err());
        } else {
            assert_eq!(
                result.unwrap().iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
                ["one", "two", "three"]
            );
        }
        task.await.unwrap();
    }
}

#[tokio::test]
async fn an_empty_google_page_can_advance_but_not_forever() {
    let api = Api::Google;
    let (url, _, task) = server(vec![
        Reply::json(serde_json::json!({"nextPageToken":"next"})),
        Reply::json(page(api, &["one"], None)),
    ])
    .await;
    assert_eq!(provider(api, &url).models().await.unwrap()[0].id, "one");
    task.await.unwrap();
    let (url, _, task) =
        server(vec![Reply::json(page(api, &[], Some("one"))), Reply::json(page(api, &[], Some("two")))])
            .await;
    assert!(
        provider(api, &url)
            .models_with(CatalogLimits { max_pages: 2, ..Default::default() })
            .await
            .unwrap_err()
            .to_string()
            .contains("exceeds 2 pages")
    );
    task.await.unwrap();
}

#[tokio::test]
async fn a_compatible_api_does_not_silently_accept_an_unknown_pagination_protocol() {
    let (url, _, task) =
        server(vec![Reply::json(serde_json::json!({"data":[{"id":"one"}],"has_more":true,"last_id":"one"}))])
            .await;
    let provider = rook_llm::openai::OpenAiCompatible::new(
        "test",
        "one",
        rook_llm::openai::Config::new(url, None, 8192),
    )
    .unwrap();
    assert!(provider.models().await.unwrap_err().to_string().contains("refusing an incomplete listing"));
    task.await.unwrap();
}

#[tokio::test]
async fn google_omitted_empty_models_is_valid_but_an_error_object_is_not_a_catalog() {
    for (body, valid) in [
        (serde_json::json!({}), true),
        (serde_json::json!({"nextPageToken":""}), true),
        (serde_json::json!({"error":"bad request"}), false),
    ] {
        let (url, _, task) = server(vec![Reply::json(body)]).await;
        let result = provider(Api::Google, &url).models().await;
        if valid {
            assert!(result.unwrap().is_empty());
        } else {
            assert!(result.unwrap_err().to_string().contains("missing model list"));
        }
        task.await.unwrap();
    }
}
