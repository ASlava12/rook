//! Fetching a language server, against a stand-in for GitHub's release API.
//!
//! What is asserted is the checking: the bytes are compared to the digest the
//! release lists as they arrive, and a mismatch installs nothing.

use std::sync::Arc;

use rook_core::install::{Installer, RUST_ANALYZER};
use sha2::Digest;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Answers like GitHub: the release API with one asset, and the asset itself.
/// `digest` is what the API claims, which need not be what is served.
async fn github(asset_name: &'static str, bytes: Arc<Vec<u8>>, digest: String) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    let at = base.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let (bytes, digest, at) = (bytes.clone(), digest.clone(), at.clone());
            tokio::spawn(async move {
                let mut scratch = [0u8; 8192];
                let n = socket.read(&mut scratch).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&scratch[..n]).to_string();
                let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
                let (status, kind, body): (&str, &str, Vec<u8>) = if path.ends_with("/releases/latest") {
                    let mut entry = serde_json::json!({
                        "name": asset_name,
                        "size": bytes.len(),
                        "browser_download_url": format!("{at}/download/{asset_name}"),
                    });
                    // An empty digest is a release that lists none, not one
                    // that lists an empty one.
                    if !digest.is_empty() {
                        entry["digest"] = serde_json::json!(format!("sha256:{digest}"));
                    }
                    let json = serde_json::json!({ "tag_name": "2026-01-01", "assets": [entry] });
                    ("200 OK", "application/json", json.to_string().into_bytes())
                } else if path.starts_with("/download/") {
                    ("200 OK", "application/octet-stream", (*bytes).clone())
                } else {
                    ("404 Not Found", "text/plain", b"no".to_vec())
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(&body).await;
            });
        }
    });
    base
}

fn gzipped(payload: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(payload).unwrap();
    encoder.finish().unwrap()
}

fn sha256_of(bytes: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(bytes))
}

fn here() -> rook_skills::Environment {
    rook_skills::Environment::bare("linux", "x86_64", "0.1.0")
}

#[tokio::test]
async fn a_server_is_fetched_checked_against_the_listed_digest_and_put_in_place() {
    let payload = b"#!/bin/sh\necho I am rust-analyzer\n".to_vec();
    let gz = Arc::new(gzipped(&payload));
    let api = github("rust-analyzer-x86_64-unknown-linux-gnu.gz", gz.clone(), sha256_of(&gz)).await;
    let into = tempfile::tempdir().unwrap();

    let done = Installer::at(api, into.path().to_path_buf())
        .unwrap()
        .install(&RUST_ANALYZER, &here())
        .await
        .unwrap();

    assert_eq!(done.tag, "2026-01-01");
    let versioned = into.path().join("rust-analyzer").join("2026-01-01").join("rust-analyzer");
    assert_eq!(std::fs::read(&versioned).unwrap(), payload, "unpacked, not stored as the gzip");
    assert!(done.verified.contains(&sha256_of(&gz)), "says which digest it matched: {}", done.verified);
    assert!(done.unverified.contains("not reviewed"), "and what it did not check: {}", done.unverified);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(std::fs::metadata(&versioned).unwrap().permissions().mode() & 0o111, 0, "runnable");
    }
}

/// The whole point: the file the server sends is compared with the digest the
/// release lists, and a file that does not match installs nothing.
#[tokio::test]
async fn a_download_that_does_not_match_the_listed_digest_installs_nothing() {
    let gz = Arc::new(gzipped(b"not what was promised"));
    let api = github("rust-analyzer-x86_64-unknown-linux-gnu.gz", gz, "0".repeat(64)).await;
    let into = tempfile::tempdir().unwrap();

    let refused = Installer::at(api, into.path().to_path_buf())
        .unwrap()
        .install(&RUST_ANALYZER, &here())
        .await
        .unwrap_err();

    assert!(refused.contains("does not match the digest"), "{refused}");
    assert!(refused.contains("nothing was installed"), "{refused}");
    assert!(!into.path().join("rust-analyzer").exists(), "not even a directory");
}

#[tokio::test]
async fn a_release_that_lists_no_digest_is_not_fetched_at_all() {
    // Served with an empty digest: the API entry says nothing to check against.
    let gz = Arc::new(gzipped(b"anything"));
    let api = github("rust-analyzer-x86_64-unknown-linux-gnu.gz", gz, String::new()).await;
    let into = tempfile::tempdir().unwrap();

    let refused = Installer::at(api, into.path().to_path_buf())
        .unwrap()
        .install(&RUST_ANALYZER, &here())
        .await
        .unwrap_err();
    assert!(refused.contains("no sha256 digest"), "{refused}");
    assert!(refused.contains("nothing was fetched"), "{refused}");
}
