#![cfg(test)]

use super::{Asset, fetch_asset};
use sha2::Digest;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Fixture {
    asset: Asset,
    requests: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn fixture(payload: &[u8], replies: Vec<Vec<u8>>) -> Fixture {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let origin = format!("http://{address}");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    let task = tokio::spawn(async move {
        for reply in replies {
            let reply = String::from_utf8(reply).unwrap().replace("OWNED_ORIGIN", &origin).into_bytes();
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0; 4096];
            while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                let count = socket.read(&mut chunk).await.unwrap();
                assert!(count > 0 && request.len() + count <= 16 * 1024);
                request.extend_from_slice(&chunk[..count]);
            }
            seen.lock().unwrap().push(String::from_utf8(request).unwrap().to_ascii_lowercase());
            let _ = socket.write_all(&reply).await;
            let _ = socket.shutdown().await;
        }
    });
    Fixture {
        asset: Asset {
            tag: "v1.0.0".into(),
            name: "owned.bin".into(),
            url: format!("http://{address}/asset"),
            sha256: hex::encode(sha2::Sha256::digest(payload)),
            size: payload.len() as u64,
        },
        requests,
        task,
    }
}

fn reply(status: &str, length: usize, extra: &str, bytes: &[u8]) -> Vec<u8> {
    let mut response =
        format!("HTTP/1.1 {status}\r\nContent-Length: {length}\r\nConnection: close\r\n{extra}\r\n")
            .into_bytes();
    response.extend_from_slice(bytes);
    response
}

fn client() -> reqwest::Client {
    crate::install::release_client("http://127.0.0.1", &rook_llm::Proxy::default()).unwrap()
}

#[tokio::test]
async fn interrupted_asset_resumes_only_the_missing_range() {
    let payload = b"abcdefghij";
    let fixture = fixture(
        payload,
        vec![
            reply("200 OK", 10, "", &payload[..3]),
            reply("206 Partial Content", 7, "Content-Range: bytes 3-9/10\r\n", &payload[3..]),
        ],
    )
    .await;
    let result = fetch_asset(&client(), &fixture.asset).await.unwrap();
    assert_eq!(result, payload);
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(!requests[0].contains("range:"));
    assert!(requests[1].contains("range: bytes=3-\r\n"), "{requests:?}");
    assert!(requests.iter().all(|r| r.contains("accept-encoding: identity\r\n")));
}

#[tokio::test]
async fn ignored_range_restarts_without_splicing_the_old_prefix() {
    let payload = b"abcdefghij";
    let fixture =
        fixture(payload, vec![reply("200 OK", 10, "", &payload[..4]), reply("200 OK", 10, "", payload)])
            .await;
    assert_eq!(fetch_asset(&client(), &fixture.asset).await.unwrap(), payload);
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].contains("range: bytes=4-\r\n"));
}

#[tokio::test]
async fn digest_refusal_remains_terminal_without_a_retry() {
    let payload = b"abcdefghij";
    let mut fixture = fixture(payload, vec![reply("200 OK", 10, "", payload)]).await;
    fixture.asset.sha256 = "0".repeat(64);
    let why = fetch_asset(&client(), &fixture.asset).await.unwrap_err();
    assert!(why.contains("does not match the digest"), "{why}");
    assert_eq!(fixture.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_changed_resumed_payload_fails_the_full_digest_without_another_retry() {
    let payload = b"abcdefghij";
    let fixture = fixture(
        payload,
        vec![
            reply("200 OK", 10, "", &payload[..3]),
            reply("206 Partial Content", 7, "Content-Range: bytes 3-9/10\r\n", b"DEFGHIJ"),
        ],
    )
    .await;
    let why = fetch_asset(&client(), &fixture.asset).await.unwrap_err();
    assert!(why.contains("does not match the digest"), "{why}");
    assert_eq!(fixture.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn ambiguous_or_changed_ranges_are_refused_before_splicing() {
    let payload = b"abcdefghij";
    for value in [
        "bytes 2-9/10",
        "bytes 3-8/10",
        "bytes 3-10/11",
        "bytes 3-9/*",
        "bytes +3-9/10",
        "bytes 3-9/18446744073709551616",
        "bytes 3-9/10\r\nContent-Range: bytes 3-9/10",
    ] {
        let fixture = fixture(
            payload,
            vec![
                reply("200 OK", 10, "", &payload[..3]),
                reply("206 Partial Content", 7, &format!("Content-Range: {value}\r\n"), &payload[3..]),
            ],
        )
        .await;
        let why = fetch_asset(&client(), &fixture.asset).await.unwrap_err();
        assert!(why.contains("Content-Range"), "{value}: {why}");
        assert_eq!(fixture.requests.lock().unwrap().len(), 2, "{value}");
    }
}

#[tokio::test]
async fn repeated_body_failures_stop_after_three_attempts() {
    let payload = b"abcdefghij";
    let fixture = fixture(
        payload,
        vec![
            reply("200 OK", 10, "", &payload[..3]),
            reply("206 Partial Content", 7, "Content-Range: bytes 3-9/10\r\n", &payload[3..5]),
            reply("206 Partial Content", 5, "Content-Range: bytes 5-9/10\r\n", &payload[5..6]),
        ],
    )
    .await;
    let why = fetch_asset(&client(), &fixture.asset).await.unwrap_err();
    assert!(why.contains("after 3 attempts"), "{why}");
    assert!(why.contains("6 bytes received"), "{why}");
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[1].contains("range: bytes=3-\r\n"));
    assert!(requests[2].contains("range: bytes=5-\r\n"));
}

#[tokio::test]
async fn a_chunked_body_with_unknown_metadata_size_can_resume() {
    let payload = b"abcdefghij";
    let first =
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n3\r\nabc\r\n".to_vec();
    let mut fixture = fixture(
        payload,
        vec![first, reply("206 Partial Content", 7, "Content-Range: bytes 3-9/10\r\n", &payload[3..])],
    )
    .await;
    fixture.asset.size = 0;
    assert_eq!(fetch_asset(&client(), &fixture.asset).await.unwrap(), payload);
    assert_eq!(fixture.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn a_clean_eof_before_the_listed_size_can_resume() {
    let payload = b"abcdefghij";
    let first =
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n3\r\nabc\r\n0\r\n\r\n"
            .to_vec();
    let fixture = fixture(
        payload,
        vec![first, reply("206 Partial Content", 7, "Content-Range: bytes 3-9/10\r\n", &payload[3..])],
    )
    .await;
    assert_eq!(fetch_asset(&client(), &fixture.asset).await.unwrap(), payload);
    assert_eq!(fixture.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn status_encoding_and_unsolicited_partial_refusals_are_terminal() {
    let payload = b"abcdefghij";
    for (status, headers, expected) in [
        ("429 Too Many Requests", "", "429"),
        ("200 OK", "Content-Encoding: gzip\r\n", "content encoding"),
        ("206 Partial Content", "Content-Range: bytes 0-9/10\r\n", "Content-Range"),
    ] {
        let fixture = fixture(payload, vec![reply(status, 10, headers, payload)]).await;
        let why = fetch_asset(&client(), &fixture.asset).await.unwrap_err();
        assert!(why.contains(expected), "{status}: {why}");
        assert_eq!(fixture.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn oversized_metadata_is_refused_before_any_request() {
    let mut fixture = fixture(b"data", vec![reply("200 OK", 4, "", b"data")]).await;
    fixture.asset.size = super::MOST_ASSET_BYTES as u64 + 1;
    let why = fetch_asset(&client(), &fixture.asset).await.unwrap_err();
    assert!(why.contains("limit"), "{why}");
    assert!(fixture.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn oversized_or_contradictory_headers_are_refused_before_body_read() {
    for (size, length) in [(0, super::MOST_ASSET_BYTES + 1), (10, 11)] {
        let mut fixture = fixture(b"abcdefghij", vec![reply("200 OK", length, "", b"")]).await;
        fixture.asset.size = size;
        let why = fetch_asset(&client(), &fixture.asset).await.unwrap_err();
        assert!(why.contains("limit") || why.contains("length"), "{why}");
        assert_eq!(fixture.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn range_and_identity_headers_survive_allowed_redirects() {
    let payload = b"abcdefghij";
    let redirect = reply("302 Found", 0, "Location: OWNED_ORIGIN/next\r\n", b"");
    let fixture = fixture(
        payload,
        vec![
            redirect.clone(),
            reply("200 OK", 10, "", &payload[..3]),
            redirect,
            reply("206 Partial Content", 7, "Content-Range: bytes 3-9/10\r\n", &payload[3..]),
        ],
    )
    .await;
    assert_eq!(fetch_asset(&client(), &fixture.asset).await.unwrap(), payload);
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(!requests[0].contains("range:"));
    assert!(!requests[1].contains("range:"));
    assert!(requests[2].contains("range: bytes=3-\r\n"));
    assert!(requests[3].contains("range: bytes=3-\r\n"));
    assert!(requests.iter().all(|request| request.contains("accept-encoding: identity\r\n")));
}

#[tokio::test]
async fn an_unknown_redirect_host_remains_refused() {
    let fixture =
        fixture(b"data", vec![reply("302 Found", 0, "Location: https://example.invalid/asset\r\n", b"")])
            .await;
    let why = fetch_asset(&client(), &fixture.asset).await.unwrap_err();
    assert!(why.contains("outside its release hosts"), "{why}");
    assert_eq!(fixture.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn one_budget_covers_the_whole_fetch() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        std::future::pending::<()>().await;
        drop(socket);
    });
    let asset = Asset {
        tag: "v1".into(),
        name: "held.bin".into(),
        url: format!("http://{address}/asset"),
        size: 4,
        sha256: hex::encode(sha2::Sha256::digest(b"data")),
    };
    let why = super::download::with_patience(&client(), &asset, std::time::Duration::from_millis(50))
        .await
        .unwrap_err();
    task.abort();
    assert!(why.contains("exceeded") && why.contains("nothing was installed"), "{why}");
}
