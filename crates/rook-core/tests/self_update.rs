//! Updating rook itself, against a stand-in for GitHub's release API.
//!
//! What is asserted is the whole act: the digest the release lists is checked
//! against the bytes that arrived, the archive is unpacked into the layout a
//! release has, and the version that was there is still there afterwards under
//! a name somebody can rename back.

use std::sync::Arc;

use rook_core::upgrade;
use sha2::Digest;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Answers like GitHub for one release. `digest` is what the API claims, which
/// need not be what is served — that is the case worth a test.
async fn github(tag: &str, asset: &str, bytes: Arc<Vec<u8>>, digest: String) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    let (at, tag, asset) = (base.clone(), tag.to_string(), asset.to_string());
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let (bytes, digest, at, tag, asset) =
                (bytes.clone(), digest.clone(), at.clone(), tag.clone(), asset.clone());
            tokio::spawn(async move {
                let mut scratch = [0u8; 8192];
                let n = socket.read(&mut scratch).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&scratch[..n]).to_string();
                let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
                let (status, kind, body): (&str, &str, Vec<u8>) = if path.ends_with("/releases/latest") {
                    let mut entry = serde_json::json!({
                        "name": asset,
                        "size": bytes.len(),
                        "browser_download_url": format!("{at}/download/{asset}"),
                    });
                    if !digest.is_empty() {
                        entry["digest"] = serde_json::json!(format!("sha256:{digest}"));
                    }
                    let json = serde_json::json!({
                        "tag_name": tag,
                        "html_url": format!("https://example.invalid/releases/{tag}"),
                        "assets": [entry],
                    });
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

/// The layout `.github/workflows/release.yml` packs, so what this unpacks is
/// what is actually published.
fn release_archive(version: &str, target: &str, binary: &[u8], skill: &str) -> (String, Vec<u8>) {
    let top = format!("rook-{version}-{target}");
    let mut tar = tar::Builder::new(Vec::new());
    let mut add = |path: String, body: &[u8], mode: u32| {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(mode);
        header.set_cksum();
        tar.append_data(&mut header, path, body).unwrap();
    };
    add(format!("{top}/bin/rook{}", std::env::consts::EXE_SUFFIX), binary, 0o755);
    add(format!("{top}/bin/rookd{}", std::env::consts::EXE_SUFFIX), b"daemon", 0o755);
    add(format!("{top}/share/rook/skills/{skill}/SKILL.md"), b"---\nname: x\n---\n", 0o644);
    add(format!("{top}/README.md"), b"rook", 0o644);
    let bytes = tar.into_inner().unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut gz, &bytes).unwrap();
    (format!("{top}.tar.gz"), gz.finish().unwrap())
}

fn sha256_of(bytes: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(bytes))
}

/// An install as it looks on a machine: `bin/` with both binaries and the
/// shipped skills where the release puts them.
fn installed(at: &std::path::Path, skill: &str) -> std::path::PathBuf {
    let bin = at.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join(format!("rook{}", std::env::consts::EXE_SUFFIX)), b"the old rook").unwrap();
    std::fs::write(bin.join(format!("rookd{}", std::env::consts::EXE_SUFFIX)), b"the old rookd").unwrap();
    let skills = at.join("share/rook/skills").join(skill);
    std::fs::create_dir_all(&skills).unwrap();
    std::fs::write(skills.join("SKILL.md"), b"the old skill").unwrap();
    bin
}

/// Each of these sets `ROOK_HOME` and `ROOK_RELEASE_API`, which one process
/// has one of.
async fn alone() -> tokio::sync::MutexGuard<'static, ()> {
    static GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    GATE.lock().await
}

#[tokio::test]
async fn a_newer_release_is_fetched_checked_and_put_in_place_keeping_what_it_replaced() {
    let _alone = alone().await;
    let home = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("ROOK_HOME", home.path()) };

    let target = upgrade::target();
    let (name, archive) = release_archive("99.0.0", &target, b"the new rook", "shipped");
    let archive = Arc::new(archive);
    let api = github("v99.0.0", &name, archive.clone(), sha256_of(&archive)).await;
    unsafe { std::env::set_var("ROOK_RELEASE_API", &api) };

    let found = upgrade::check(&Default::default()).await.unwrap();
    assert!(found.newer, "99.0.0 is ahead of {}: {found:?}", found.running);
    assert_eq!(found.latest, "99.0.0");
    assert!(found.asset.is_some(), "the release carries something for {target}: {found:?}");

    let install = tempfile::tempdir().unwrap();
    let bin = installed(install.path(), "withdrawn");
    let layout = upgrade::Layout::beside(&bin);
    let done = upgrade::apply(&found, &layout, &Default::default()).await.unwrap();

    let rook = bin.join(format!("rook{}", std::env::consts::EXE_SUFFIX));
    assert_eq!(std::fs::read(&rook).unwrap(), b"the new rook", "the new binary is in place");
    assert_eq!(
        std::fs::read(bin.join(format!("rook{}.previous", std::env::consts::EXE_SUFFIX))).unwrap(),
        b"the old rook",
        "and going back is a rename"
    );
    assert_eq!(
        std::fs::read(bin.join(format!("rookd{}", std::env::consts::EXE_SUFFIX))).unwrap(),
        b"daemon",
        "the daemon beside it was updated too"
    );
    let skills = install.path().join("share/rook/skills");
    assert!(skills.join("shipped/SKILL.md").is_file(), "the shipped skills are what the release carries");
    assert!(
        !skills.join("withdrawn").exists(),
        "and one the release stopped carrying is not still there: {:?}",
        std::fs::read_dir(&skills).unwrap().flatten().map(|e| e.file_name()).collect::<Vec<_>>()
    );
    assert!(done.verified.contains(&sha256_of(&archive)), "it says what it matched: {}", done.verified);
    assert!(done.restart.contains("already running"), "and that this is not yet it: {}", done.restart);
    assert!(!home.path().join("update").join("v99.0.0").exists(), "the unpacked copy nobody reads is gone");
    unsafe { std::env::remove_var("ROOK_RELEASE_API") };
}

#[tokio::test]
async fn a_download_that_does_not_match_the_listed_digest_replaces_nothing() {
    let _alone = alone().await;
    let home = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("ROOK_HOME", home.path()) };

    let target = upgrade::target();
    let (name, archive) = release_archive("99.0.0", &target, b"not what was promised", "shipped");
    let api = github("v99.0.0", &name, Arc::new(archive), sha256_of(b"something else entirely")).await;
    unsafe { std::env::set_var("ROOK_RELEASE_API", &api) };

    let found = upgrade::check(&Default::default()).await.unwrap();
    let install = tempfile::tempdir().unwrap();
    let bin = installed(install.path(), "withdrawn");
    let why = upgrade::apply(&found, &upgrade::Layout::beside(&bin), &Default::default()).await.unwrap_err();

    assert!(why.contains("does not match the digest"), "{why}");
    assert_eq!(
        std::fs::read(bin.join(format!("rook{}", std::env::consts::EXE_SUFFIX))).unwrap(),
        b"the old rook",
        "and the binary that was running is untouched"
    );
    assert!(
        !bin.join(format!("rook{}.previous", std::env::consts::EXE_SUFFIX)).exists(),
        "nothing was even moved aside"
    );
    unsafe { std::env::remove_var("ROOK_RELEASE_API") };
}

#[tokio::test]
async fn a_release_no_newer_than_this_build_is_reported_rather_than_installed() {
    let _alone = alone().await;
    let home = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("ROOK_HOME", home.path()) };

    let target = upgrade::target();
    let (name, archive) = release_archive("0.0.1", &target, b"ancient", "shipped");
    let archive = Arc::new(archive);
    let api = github("v0.0.1", &name, archive.clone(), sha256_of(&archive)).await;
    unsafe { std::env::set_var("ROOK_RELEASE_API", &api) };

    let found = upgrade::check(&Default::default()).await.unwrap();
    assert!(!found.newer, "0.0.1 is behind {}: {found:?}", found.running);
    assert_eq!(found.running, rook_core::AGENT_VERSION, "it says what is running, not what it guessed");
    unsafe { std::env::remove_var("ROOK_RELEASE_API") };
}

#[tokio::test]
async fn a_release_with_nothing_for_this_platform_says_so_and_fetches_nothing() {
    let _alone = alone().await;
    let home = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("ROOK_HOME", home.path()) };

    // A real release for a platform that is not this one.
    let (name, archive) = release_archive("99.0.0", "sparc64-unknown-plan9", b"elsewhere", "shipped");
    let archive = Arc::new(archive);
    let api = github("v99.0.0", &name, archive.clone(), sha256_of(&archive)).await;
    unsafe { std::env::set_var("ROOK_RELEASE_API", &api) };

    let found = upgrade::check(&Default::default()).await.unwrap();
    assert!(found.asset.is_none(), "{found:?}");
    // The point of separating the two questions. FreeBSD is a supported target
    // with no published binary, and being unable to fetch one is no reason to
    // be unable to say that a newer version exists — which is what this did
    // until the FreeBSD runner said so.
    assert_eq!(found.latest, "99.0.0", "it still says what is published: {found:?}");
    assert!(found.newer, "and that it is newer than this build: {found:?}");

    let install = tempfile::tempdir().unwrap();
    let bin = installed(install.path(), "withdrawn");
    let why = upgrade::apply(&found, &upgrade::Layout::beside(&bin), &Default::default()).await.unwrap_err();
    assert!(why.contains(&found.target), "it names the platform it has nothing for: {why}");
    assert!(why.contains("from source"), "and what to do instead: {why}");
    unsafe { std::env::remove_var("ROOK_RELEASE_API") };
}
