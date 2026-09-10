//! A proxy in the environment is a proxy to the internet.
//!
//! Pointing rook at a model on the next desk failed with an empty 502 after
//! eighty seconds, because `http_proxy` was set for the VPN and reqwest honours
//! it for every address including one on this network. What rook printed was
//! `cannot reach http://192.168.1.46:1234: operation timed out`, which sends you
//! to look at the machine that was answering fine.
//!
//! One test rather than two: both arms set the same process-wide variables, and
//! this file is its own binary so nothing else is looking at them.

use std::sync::{Arc, Mutex};

use rook_llm::Provider;
use rook_llm::openai::{Config, OpenAiCompatible};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Answers anything with one model, and remembers the request lines it was
/// asked — which is how a proxy is told from an endpoint after the fact.
async fn listening() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let asked: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let heard = asked.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else { return };
            let heard = heard.clone();
            tokio::spawn(async move {
                let mut scratch = [0u8; 8192];
                let read = socket.read(&mut scratch).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&scratch[..read]).to_string();
                heard.lock().unwrap().push(request.lines().next().unwrap_or_default().to_string());
                let body = r#"{"data":[{"id":"model","object":"model"}]}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    (format!("{addr}"), asked)
}

fn provider(url: String) -> OpenAiCompatible {
    OpenAiCompatible::new("lmstudio/model", "model", Config::new(url, None, 8192)).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_proxy_is_for_the_internet_and_not_for_the_endpoint_on_this_network() {
    let (proxy, went_through_the_proxy) = listening().await;
    let (endpoint, answered_directly) = listening().await;

    // Set before either provider is built: reqwest reads the environment when
    // the client is constructed, not when a request is sent.
    unsafe {
        for name in ["ALL_PROXY", "all_proxy", "HTTPS_PROXY", "https_proxy", "NO_PROXY", "no_proxy"] {
            std::env::remove_var(name);
        }
        std::env::set_var("HTTP_PROXY", format!("http://{proxy}"));
        std::env::set_var("http_proxy", format!("http://{proxy}"));
    }

    // The precondition, and the half that used to fail: a proxy really is
    // configured, and the endpoint is one this machine can reach itself.
    let models = provider(format!("http://{endpoint}/v1")).models().await.unwrap();
    assert_eq!(models.len(), 1, "the endpoint answered: {models:?}");
    assert!(
        went_through_the_proxy.lock().unwrap().is_empty(),
        "an address on this network went through the proxy anyway: {:?}",
        went_through_the_proxy.lock().unwrap()
    );
    // More than one is fine and expected: an `lmstudio` id asks a second time,
    // on LM Studio's own endpoint, for the window the compatible listing omits.
    assert!(!answered_directly.lock().unwrap().is_empty(), "and it was asked directly");

    // TEST-NET-3, which is reserved and routed nowhere: if this arrives at all
    // it arrived through the proxy, and the proxy is still the way out.
    let _ = provider("http://198.51.100.7:9/v1".to_string()).models().await;
    let seen = went_through_the_proxy.lock().unwrap().clone();
    assert!(
        seen.iter().any(|line| line.contains("198.51.100.7")),
        "a public address must still use the proxy, and none reached it: {seen:?}"
    );
}
