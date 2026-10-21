//! Separate installer values targeting one cache must share overlapping work.
use rook_core::install::{Installed, Installer, RUST_ANALYZER};
use sha2::Digest;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type Tasks = tokio::task::JoinSet<Result<Installed, String>>;
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
struct Fixture {
    owned: tempfile::TempDir,
    cache: PathBuf,
    api: String,
    payload: &'static [u8],
    release_requests: Arc<AtomicUsize>,
    asset_requests: Arc<AtomicUsize>,
    observed: tokio::sync::watch::Receiver<usize>,
    release: tokio::sync::watch::Sender<bool>,
    bad_digest: Arc<AtomicBool>,
    early_metadata: Arc<AtomicBool>,
    server: tokio::task::JoinHandle<()>,
    _serial: tokio::sync::MutexGuard<'static, ()>,
}
impl Fixture {
    async fn new() -> Self {
        let serial = SERIAL.lock().await;
        let owned = tempfile::tempdir().unwrap();
        let cache = owned.path().join("servers");
        let payload = b"owned fixture; never executed\n";
        let compressed = {
            use std::io::Write;
            let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            gz.write_all(payload).unwrap();
            Arc::new(gz.finish().unwrap())
        };
        let digest = hex::encode(sha2::Sha256::digest(compressed.as_slice()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", listener.local_addr().unwrap());
        let release_requests = Arc::new(AtomicUsize::new(0));
        let asset_requests = Arc::new(AtomicUsize::new(0));
        let (entered, observed) = tokio::sync::watch::channel(0);
        let (release, gate) = tokio::sync::watch::channel(false);
        let bad_digest = Arc::new(AtomicBool::new(false));
        let early_metadata = Arc::new(AtomicBool::new(false));
        let current = RUST_ANALYZER.binary_in(&cache.join("rust-analyzer/current"));
        let (bad, early) = (bad_digest.clone(), early_metadata.clone());
        let endpoint = api.clone();
        let releases = release_requests.clone();
        let assets = asset_requests.clone();
        let server = tokio::spawn(async move {
            for _ in 0..512 {
                let Ok(Ok((mut socket, _))) =
                    tokio::time::timeout(std::time::Duration::from_secs(30), listener.accept()).await
                else {
                    return;
                };
                let (compressed, digest, endpoint, releases, assets, entered, mut gate) = (
                    compressed.clone(),
                    digest.clone(),
                    endpoint.clone(),
                    releases.clone(),
                    assets.clone(),
                    entered.clone(),
                    gate.clone(),
                );
                let (bad, early, current) = (bad.clone(), early.clone(), current.clone());
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    while !bytes.windows(4).any(|part| part == b"\r\n\r\n") {
                        let mut chunk = [0_u8; 4096];
                        let Ok(Ok(n)) =
                            tokio::time::timeout(std::time::Duration::from_secs(5), socket.read(&mut chunk))
                                .await
                        else {
                            return;
                        };
                        if n == 0 {
                            return;
                        }
                        assert!(bytes.len() + n <= 8192);
                        bytes.extend_from_slice(&chunk[..n]);
                    }
                    let request = String::from_utf8(bytes).unwrap();
                    let (mime, body) = if request.lines().next().unwrap().contains("/releases/latest ") {
                        let previous = releases.fetch_add(1, Ordering::SeqCst);
                        if previous > 0 && !current.is_file() {
                            early.store(true, Ordering::SeqCst);
                        }
                        let digest = if bad.load(Ordering::SeqCst) { "0".repeat(64) } else { digest };
                        ("application/json", serde_json::json!({"tag_name":"owned-v1", "assets":[{
                            "name":"rust-analyzer-x86_64-unknown-linux-gnu.gz", "size":compressed.len(),
                            "digest":format!("sha256:{digest}"), "browser_download_url":format!("{endpoint}/asset.gz")
                        }]}).to_string().into_bytes())
                    } else {
                        assert!(request.starts_with("GET /asset.gz "));
                        let count = assets.fetch_add(1, Ordering::SeqCst) + 1;
                        entered.send_replace(count);
                        let Ok(Ok(_)) = tokio::time::timeout(
                            std::time::Duration::from_secs(60),
                            gate.wait_for(|open| *open),
                        )
                        .await
                        else {
                            return;
                        };
                        ("application/octet-stream", compressed.as_ref().clone())
                    };
                    let header = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    if socket.write_all(header.as_bytes()).await.is_ok() {
                        let _ = socket.write_all(&body).await;
                    }
                });
            }
        });
        Self {
            owned,
            cache,
            api,
            payload,
            release_requests,
            asset_requests,
            observed,
            release,
            bad_digest,
            early_metadata,
            server,
            _serial: serial,
        }
    }
    fn launch(&self, tasks: &mut Tasks) -> (tokio::task::AbortHandle, tokio::sync::oneshot::Receiver<()>) {
        self.launch_with(tasks, self.cache.clone(), self.api.clone(), Default::default())
    }
    fn launch_with(
        &self,
        tasks: &mut Tasks,
        cache: PathBuf,
        api: String,
        proxy: rook_llm::Proxy,
    ) -> (tokio::task::AbortHandle, tokio::sync::oneshot::Receiver<()>) {
        let installer = Installer::at(api, cache, &proxy).unwrap();
        let (polled, began) = tokio::sync::oneshot::channel();
        let handle = tasks.spawn(async move {
            let environment = rook_skills::Environment::bare("linux", "x86_64", "test");
            let mut running = Box::pin(installer.install(&RUST_ANALYZER, &environment));
            let mut polled = Some(polled);
            std::future::poll_fn(|context| {
                let result = running.as_mut().poll(context);
                if let Some(signal) = polled.take() {
                    let _ = signal.send(());
                }
                result
            })
            .await
        });
        (handle, began)
    }
    async fn assets(&self, minimum: usize) {
        let mut observed = self.observed.clone();
        tokio::time::timeout(
            std::time::Duration::from_secs(20),
            observed.wait_for(|count| *count >= minimum),
        )
        .await
        .unwrap()
        .unwrap();
    }
    fn check(&self, results: Vec<Result<Installed, String>>, count: usize) {
        assert_eq!(results.len(), count);
        for result in results {
            let installed = result.unwrap();
            assert_eq!(installed.tag, "owned-v1");
            assert_eq!(std::fs::read(installed.path).unwrap(), self.payload);
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
async fn collect(mut tasks: Tasks) -> Vec<Result<Installed, String>> {
    let mut results = Vec::new();
    while let Some(result) =
        tokio::time::timeout(std::time::Duration::from_secs(30), tasks.join_next()).await.unwrap()
    {
        match result {
            Ok(result) => results.push(result),
            Err(error) => assert!(error.is_cancelled(), "installer task panicked: {error}"),
        }
    }
    results
}
async fn polled(signal: tokio::sync::oneshot::Receiver<()>) {
    tokio::time::timeout(std::time::Duration::from_secs(20), signal).await.unwrap().unwrap();
}

#[tokio::test]
async fn independent_installers_join_one_held_download_for_the_same_cache() {
    let f = Fixture::new().await;
    let mut tasks = Tasks::new();
    f.launch(&mut tasks);
    f.assets(1).await;
    assert!(!f.cache.exists());
    polled(f.launch(&mut tasks).1).await;
    f.release.send(true).unwrap();
    let results = collect(tasks).await;
    assert_eq!(f.asset_requests.load(Ordering::SeqCst), 1, "physical downloads duplicated: {results:?}");
    assert_eq!(f.release_requests.load(Ordering::SeqCst), 1);
    f.check(results, 2);
}

#[tokio::test]
async fn cancelling_the_first_caller_does_not_cancel_a_joined_installation() {
    let f = Fixture::new().await;
    let mut tasks = Tasks::new();
    let first = f.launch(&mut tasks).0;
    f.assets(1).await;
    polled(f.launch(&mut tasks).1).await;
    first.abort();
    f.release.send(true).unwrap();
    f.check(collect(tasks).await, 1);
    assert_eq!(f.asset_requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancelling_every_waiter_drops_work_and_a_later_caller_can_retry() {
    let f = Fixture::new().await;
    let mut tasks = Tasks::new();
    f.launch(&mut tasks);
    f.assets(1).await;
    polled(f.launch(&mut tasks).1).await;
    tasks.abort_all();
    assert!(collect(tasks).await.is_empty());
    assert!(!f.cache.exists());
    let mut retry = Tasks::new();
    f.launch(&mut retry);
    f.assets(2).await;
    f.release.send(true).unwrap();
    f.check(collect(retry).await, 1);
    assert_eq!(f.asset_requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn different_caches_download_while_the_first_cache_is_still_held() {
    let f = Fixture::new().await;
    let mut tasks = Tasks::new();
    f.launch(&mut tasks);
    f.assets(1).await;
    f.launch_with(&mut tasks, f.owned.path().join("other-cache"), f.api.clone(), Default::default());
    f.assets(2).await;
    assert!(!f.cache.exists());
    f.release.send(true).unwrap();
    f.check(collect(tasks).await, 2);
}

#[tokio::test]
async fn a_failed_flight_is_shared_but_not_cached_for_a_later_explicit_retry() {
    let f = Fixture::new().await;
    f.bad_digest.store(true, Ordering::SeqCst);
    let mut tasks = Tasks::new();
    f.launch(&mut tasks);
    f.assets(1).await;
    polled(f.launch(&mut tasks).1).await;
    f.release.send(true).unwrap();
    let results = collect(tasks).await;
    assert_eq!(results.len(), 2);
    for result in results {
        assert!(result.unwrap_err().contains("does not match"));
    }
    assert!(!f.cache.exists());
    assert_eq!(f.asset_requests.load(Ordering::SeqCst), 1);
    f.bad_digest.store(false, Ordering::SeqCst);
    let mut retry = Tasks::new();
    f.launch(&mut retry);
    f.check(collect(retry).await, 1);
    assert_eq!(f.asset_requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn different_source_or_proxy_waits_for_the_destination_then_runs_its_own_operation() {
    for proxy_case in [false, true] {
        let f = Fixture::new().await;
        let mut tasks = Tasks::new();
        f.launch(&mut tasks);
        f.assets(1).await;
        let (api, proxy) = if proxy_case {
            (f.api.clone(), rook_llm::Proxy::Direct)
        } else {
            (format!("{}/mirror", f.api), Default::default())
        };
        polled(f.launch_with(&mut tasks, f.cache.clone(), api, proxy).1).await;
        f.release.send(true).unwrap();
        f.check(collect(tasks).await, 2);
        assert_eq!(f.release_requests.load(Ordering::SeqCst), 2);
        assert_eq!(f.asset_requests.load(Ordering::SeqCst), 2);
        assert!(
            !f.early_metadata.load(Ordering::SeqCst),
            "conflicting source reached HTTP before the first destination committed"
        );
    }
}

#[tokio::test]
async fn a_successful_flight_does_not_suppress_a_later_explicit_install() {
    let f = Fixture::new().await;
    f.release.send(true).unwrap();
    for _ in 0..2 {
        let mut tasks = Tasks::new();
        f.launch(&mut tasks);
        f.check(collect(tasks).await, 1);
    }
    assert_eq!(f.asset_requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn active_installations_are_bounded_and_completed_work_releases_capacity() {
    let f = Fixture::new().await;
    let mut tasks = Tasks::new();
    for index in 0..64 {
        f.launch_with(
            &mut tasks,
            f.owned.path().join(format!("cache-{index}")),
            f.api.clone(),
            Default::default(),
        );
        f.assets(index + 1).await;
    }
    let refused_cache = f.owned.path().join("refused");
    // Matching callers may still join an admitted operation at saturation.
    polled(f.launch_with(&mut tasks, f.owned.path().join("cache-0"), f.api.clone(), Default::default()).1)
        .await;
    f.launch_with(&mut tasks, refused_cache.clone(), f.api.clone(), Default::default());
    let refused = tokio::time::timeout(std::time::Duration::from_secs(20), tasks.join_next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(refused.unwrap_err().contains("64 language-server installations"));
    assert!(!refused_cache.exists());
    assert_eq!(f.asset_requests.load(Ordering::SeqCst), 64);
    f.release.send(true).unwrap();
    f.check(collect(tasks).await, 65);
    let mut next = Tasks::new();
    f.launch_with(&mut next, refused_cache, f.api.clone(), Default::default());
    f.check(collect(next).await, 1);
    assert_eq!(f.asset_requests.load(Ordering::SeqCst), 65);
}
