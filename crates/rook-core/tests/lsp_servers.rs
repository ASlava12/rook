//! Choosing and starting a language server.
//!
//! Both of these were watched happening: a live turn in a Python project asked
//! about a symbol, and the first configured server was a `rust-analyzer` shim
//! that cannot start. rustup installs one whether or not the component is.

use std::time::Instant;

use rook_core::lsp::Servers;
use rook_lsp::ServerConfig;

static PROCESSES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn mock(
    language: &str,
    extension: &str,
    log: &std::path::Path,
    gate: Option<&std::path::Path>,
) -> ServerConfig {
    let debug = std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().to_path_buf();
    let binary = debug.join(if cfg!(windows) { "lsp-mock.exe" } else { "lsp-mock" });
    assert!(binary.is_file(), "build the owned fixture first: cargo build -p rook-lsp --bin lsp-mock");
    let mut args = vec![format!("--initialize-log={}", log.display())];
    if let Some(gate) = gate {
        args.push(format!("--initialize-gate={}", gate.display()));
    }
    ServerConfig {
        language: language.into(),
        command: binary.to_string_lossy().into_owned(),
        extensions: vec![extension.into()],
        args,
        startup_timeout_secs: 180,
        ..Default::default()
    }
}

async fn reached(log: &std::path::Path) {
    tokio::time::timeout(std::time::Duration::from_secs(90), async {
        while !log.is_file() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the actual mock process must reach its initialize request");
}

fn release_gate(path: &std::path::Path, control: &str) {
    let pending = path.with_extension("pending");
    std::fs::write(&pending, control).unwrap();
    std::fs::rename(pending, path).unwrap();
}

fn server(language: &str, command: &str) -> ServerConfig {
    ServerConfig {
        language: language.into(),
        command: command.into(),
        extensions: vec!["rs".into()],
        startup_timeout_secs: 5,
        ..Default::default()
    }
}

#[tokio::test]
async fn a_held_start_does_not_block_existing_or_independent_servers_and_still_starts_once() {
    use std::sync::Arc;
    use std::time::Duration;
    let _serial = PROCESSES.lock().await;
    let workspace = tempfile::tempdir().unwrap();
    let rust_log = workspace.path().join("rust-started");
    let release = workspace.path().join("release-rust");
    let go_log = workspace.path().join("go-started");
    let python_log = workspace.path().join("python-started");
    let servers = Servers::new(
        vec![
            mock("rust", "rs", &rust_log, Some(&release)),
            mock("python", "py", &python_log, None),
            mock("go", "go", &go_log, None),
        ],
        workspace.path(),
    );
    let python_path = workspace.path().join("main.py");
    let rust_path = workspace.path().join("lib.rs");
    let go_path = workspace.path().join("main.go");
    std::fs::write(&python_path, "x = 1\n").unwrap();
    std::fs::write(&rust_path, "fn main() {}\n").unwrap();
    std::fs::write(&go_path, "package main\n").unwrap();
    let (python, _) = servers.for_path(&python_path).await.unwrap();
    let first = tokio::spawn({
        let servers = servers.clone();
        let path = rust_path.clone();
        async move { servers.for_path(&path).await }
    });
    reached(&rust_log).await;
    let second = tokio::spawn({
        let servers = servers.clone();
        let path = rust_path.clone();
        async move { servers.for_path(&path).await }
    });
    let independent = tokio::time::timeout(Duration::from_secs(90), async {
        let (again, _) = servers.for_path(&python_path).await?;
        let any = servers.any().await?;
        let diagnostics = servers.errors_in(&python_path, Duration::from_secs(5)).await;
        let metrics = tokio::runtime::Handle::current().metrics();
        let before = metrics.num_alive_tasks();
        let mut still_warming = true;
        for _ in 0..64 {
            still_warming &= servers.errors_in(&rust_path, Duration::ZERO).await.is_none();
        }
        tokio::task::yield_now().await;
        let warm_tasks = metrics.num_alive_tasks().saturating_sub(before);
        let (_, language) = servers.for_path(&go_path).await?;
        Ok::<_, rook_lsp::LspError>((
            Arc::ptr_eq(&python, &again) && Arc::ptr_eq(&python, &any),
            diagnostics,
            language,
            still_warming,
            warm_tasks,
        ))
    })
    .await;
    let rust_was_held = !first.is_finished();
    // Release and shut down before asserting the timed result: the failing
    // baseline must not leave its actual mock processes behind.
    release_gate(&release, "release");
    let (first, _) = tokio::time::timeout(Duration::from_secs(90), first).await.unwrap().unwrap().unwrap();
    let (second, _) = tokio::time::timeout(Duration::from_secs(90), second).await.unwrap().unwrap().unwrap();
    servers.shutdown().await;
    assert!(rust_was_held, "independent work must finish before the held startup is released");
    let (reused, diagnostics, language, still_warming, warm_tasks) = independent
        .expect("an unrelated held initialization must not block a running server or another language")
        .unwrap();
    assert!(reused, "the already-running Python process is reused");
    assert!(diagnostics.is_some(), "write diagnostics reach the already-running server");
    assert_eq!(language, "go", "another language starts while Rust remains held");
    assert!(still_warming, "all 64 cold diagnostic requests return while Rust remains held");
    assert!(warm_tasks <= 1, "64 cold writes cannot queue a background task each: {warm_tasks}");
    assert!(Arc::ptr_eq(&first, &second), "concurrent requests share one Rust process");
    assert_eq!(std::fs::read_to_string(&rust_log).unwrap().lines().count(), 1);
    assert_eq!(std::fs::read_to_string(&go_log).unwrap().lines().count(), 1);
    assert_eq!(std::fs::read_to_string(&python_log).unwrap().lines().count(), 1);
}

#[tokio::test]
async fn concurrent_failed_starts_share_one_physical_attempt_and_keep_other_languages_available() {
    use std::sync::Arc;
    let _serial = PROCESSES.lock().await;
    let workspace = tempfile::tempdir().unwrap();
    let log = workspace.path().join("failed-start");
    let release = workspace.path().join("fail-rust");
    let python_log = workspace.path().join("python-start");
    let servers = Servers::new(
        vec![mock("rust", "rs", &log, Some(&release)), mock("python", "py", &python_log, None)],
        workspace.path(),
    );
    let path = workspace.path().join("lib.rs");
    let first = tokio::spawn({
        let servers = servers.clone();
        let path = path.clone();
        async move { servers.for_path(&path).await }
    });
    reached(&log).await;
    assert!(!first.is_finished(), "the first actual process is held before its deliberate failure");
    let joining = Arc::new(tokio::sync::Notify::new());
    let second = tokio::spawn({
        let servers = servers.clone();
        let path = path.clone();
        let joining = joining.clone();
        async move {
            joining.notify_one();
            servers.for_path(&path).await
        }
    });
    joining.notified().await;
    release_gate(&release, "fail");
    let first = first.await.unwrap();
    let second = second.await.unwrap();
    let third = servers.for_path(&path).await;
    let python = servers.for_path(&workspace.path().join("main.py")).await;
    servers.shutdown().await;
    assert!(first.is_err(), "the actual initialize failure must reach its caller");
    assert!(matches!(second, Err(rook_lsp::LspError::Closed { .. })), "waiters reuse the failure");
    assert!(matches!(third, Err(rook_lsp::LspError::Closed { .. })), "later requests reuse the failure");
    assert!(python.is_ok(), "one failed language does not hide another working server");
    assert_eq!(std::fs::read_to_string(log).unwrap().lines().count(), 1);
    assert_eq!(std::fs::read_to_string(python_log).unwrap().lines().count(), 1);
}

#[tokio::test]
async fn shutdown_waits_for_an_admitted_start_and_refuses_a_later_language() {
    use std::sync::Arc;
    let _serial = PROCESSES.lock().await;
    let workspace = tempfile::tempdir().unwrap();
    let rust_log = workspace.path().join("rust-start");
    let go_log = workspace.path().join("go-start");
    let release = workspace.path().join("release-rust");
    let servers = Servers::new(
        vec![mock("rust", "rs", &rust_log, Some(&release)), mock("go", "go", &go_log, None)],
        workspace.path(),
    );
    let first = tokio::spawn({
        let servers = servers.clone();
        let path = workspace.path().join("lib.rs");
        async move { servers.for_path(&path).await }
    });
    reached(&rust_log).await;
    let entering = Arc::new(tokio::sync::Notify::new());
    let shutdown = tokio::spawn({
        let servers = servers.clone();
        let entering = entering.clone();
        async move {
            entering.notify_one();
            servers.shutdown().await;
        }
    });
    entering.notified().await;
    assert!(!shutdown.is_finished(), "shutdown must retain ownership of the pending handshake");
    let later = tokio::time::timeout(
        std::time::Duration::from_secs(90),
        servers.for_path(&workspace.path().join("main.go")),
    )
    .await;
    release_gate(&release, "release");
    assert!(first.await.unwrap().is_ok());
    shutdown.await.unwrap();
    let later = later.expect("closing admission refuses new starts without waiting for the held handshake");
    assert!(matches!(later, Err(rook_lsp::LspError::Closed { .. })));
    assert!(!go_log.exists(), "a server cannot physically start after pool shutdown");
    assert!(servers.for_path(&workspace.path().join("main.go")).await.is_err());
    servers.shutdown().await;
}

#[tokio::test]
async fn cancelling_a_pending_shutdown_reopens_admission_and_a_later_shutdown_still_closes_clients() {
    use std::sync::Arc;
    let _serial = PROCESSES.lock().await;
    let workspace = tempfile::tempdir().unwrap();
    let rust_log = workspace.path().join("rust-start");
    let go_log = workspace.path().join("go-start");
    let release = workspace.path().join("release-rust");
    let servers = Servers::new(
        vec![mock("rust", "rs", &rust_log, Some(&release)), mock("go", "go", &go_log, None)],
        workspace.path(),
    );
    let first = tokio::spawn({
        let servers = servers.clone();
        let path = workspace.path().join("lib.rs");
        async move { servers.for_path(&path).await }
    });
    reached(&rust_log).await;
    let entering = Arc::new(tokio::sync::Notify::new());
    let shutdown = tokio::spawn({
        let servers = servers.clone();
        let entering = entering.clone();
        async move {
            entering.notify_one();
            servers.shutdown().await;
        }
    });
    entering.notified().await;
    assert!(!shutdown.is_finished());
    shutdown.abort();
    assert!(shutdown.await.unwrap_err().is_cancelled());
    let go = tokio::time::timeout(
        std::time::Duration::from_secs(90),
        servers.for_path(&workspace.path().join("main.go")),
    )
    .await;
    let still_held = !first.is_finished();
    release_gate(&release, "release");
    assert!(first.await.unwrap().is_ok());
    servers.shutdown().await;
    assert!(still_held, "Go is admitted independently before Rust is released");
    assert!(go.expect("a cancelled closing writer must not retain admission").is_ok());
    assert_eq!(std::fs::read_to_string(go_log).unwrap().lines().count(), 1);
    assert!(servers.for_path(&workspace.path().join("main.go")).await.is_err());
}

#[tokio::test]
async fn a_stopped_cached_client_does_not_hide_a_startable_server() {
    let _serial = PROCESSES.lock().await;
    let workspace = tempfile::tempdir().unwrap();
    let rust_log = workspace.path().join("rust-start");
    let python_log = workspace.path().join("python-start");
    let servers = Servers::new(
        vec![mock("rust", "rs", &rust_log, None), mock("python", "py", &python_log, None)],
        workspace.path(),
    );
    let path = workspace.path().join("main.py");
    let (python, _) = servers.for_path(&path).await.unwrap();
    python.shutdown().await;
    let stale = servers.for_path(&path).await;
    let chosen = servers.any().await;
    servers.shutdown().await;
    assert!(matches!(stale, Err(rook_lsp::LspError::Closed { .. })));
    let chosen = chosen.expect("a stopped cached Python server must not hide startable Rust");
    assert!(!std::sync::Arc::ptr_eq(&python, &chosen));
    assert_eq!(std::fs::read_to_string(rust_log).unwrap().lines().count(), 1);
    assert_eq!(std::fs::read_to_string(python_log).unwrap().lines().count(), 1);
}

#[tokio::test]
async fn a_server_that_will_not_start_is_only_waited_for_once() {
    let dir = tempfile::tempdir().unwrap();
    let servers = Servers::new(vec![server("rust", "no-such-language-server-anywhere")], dir.path());

    let first = Instant::now();
    assert!(servers.any().await.is_err());
    let cost = first.elapsed();

    let again = Instant::now();
    let Err(second) = servers.any().await else { panic!("it cannot have started") };
    assert!(
        again.elapsed() <= cost,
        "the second ask paid for the discovery again: {cost:?} then {:?}",
        again.elapsed()
    );
    assert!(
        second.to_string().contains("no-such-language-server-anywhere")
            || second.to_string().contains("rust"),
        "and still says which one and why: {second}"
    );
}

#[tokio::test]
async fn a_broken_first_server_does_not_hide_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let servers = Servers::new(
        vec![server("rust", "no-such-language-server-anywhere"), server("also-broken", "nor-this-one")],
        dir.path(),
    );

    // Neither starts here, but both are tried: what comes back names the second
    // command, which is only possible because it did not stop at the first.
    let Err(complaint) = servers.any().await else { panic!("neither can start") };
    assert!(complaint.to_string().contains("nor-this-one"), "{complaint}");
}

/// A `rust-analyzer` on `PATH` was offered to a Python project, and a live
/// model spent a step asking it about a symbol. Four tool schemas in the prompt,
/// bought nothing, and pointed at a language the workspace does not contain.
#[test]
fn only_the_servers_this_workspace_has_files_for_are_offered() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("main.py"), "x = 1\n").unwrap();
    std::fs::create_dir_all(dir.path().join("deep/er")).unwrap();
    std::fs::write(dir.path().join("deep/er/mod.go"), "package main\n").unwrap();

    let config = rook_core::Config {
        lsp: vec![server("rust", "rust-analyzer"), python(), go()],
        ..Default::default()
    };

    let offered: Vec<String> =
        rook_core::lsp::for_workspace(&config, dir.path()).into_iter().map(|c| c.language).collect();

    assert!(offered.contains(&"python".to_string()), "{offered:?}");
    assert!(offered.contains(&"go".to_string()), "found below the top level too: {offered:?}");
    assert!(!offered.contains(&"rust".to_string()), "no .rs here: {offered:?}");
}

#[test]
fn a_workspace_no_server_covers_is_offered_none() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notes.md"), "just prose\n").unwrap();

    let config = rook_core::Config { lsp: vec![server("rust", "rust-analyzer")], ..Default::default() };

    assert!(rook_core::lsp::for_workspace(&config, dir.path()).is_empty(), "and so the tools are not");
}

fn python() -> ServerConfig {
    ServerConfig {
        language: "python".into(),
        command: "pyright-langserver".into(),
        extensions: vec!["py".into()],
        ..Default::default()
    }
}

fn go() -> ServerConfig {
    ServerConfig {
        language: "go".into(),
        command: "gopls".into(),
        extensions: vec!["go".into()],
        ..Default::default()
    }
}

/// A server this agent installed is trusted without being asked to prove it.
///
/// The `--version` probe exists for `PATH`, where rustup leaves a
/// `rust-analyzer` shim that is not a server. Asked of our own file it threw a
/// working one away: `pyright-langserver --version` refuses to run without
/// `--stdio` and answers with an error, so Python was reported missing on a
/// machine where it was installed — and the agent offered to install it again,
/// every session, for ever.
#[test]
fn a_server_we_installed_is_not_asked_to_prove_itself() {
    let home = tempfile::tempdir().unwrap();
    // Every process here shares one environment, so this test owns it.
    static ALONE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = ALONE.lock().unwrap_or_else(|e| e.into_inner());
    let previous = std::env::var_os("ROOK_HOME");
    unsafe { std::env::set_var("ROOK_HOME", home.path()) };

    // Asked of the code that decides where it goes rather than spelled out:
    // an npm install puts `pyright-langserver.cmd` on Windows and
    // `pyright-langserver` everywhere else, and a test that spells the unix
    // name passes here and fails on the Windows runner — which is what it did.
    let binary = rook_core::install::current("pyright-langserver");
    std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
    // Answers `--version` the way pyright does: badly.
    std::fs::write(&binary, "#!/bin/sh\necho 'Error: Connection input stream is not set' >&2\nexit 1\n")
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let found = rook_core::lsp::detected();

    let python = found.iter().find(|c| c.language == "python");
    assert!(python.is_some(), "an installed server is offered whatever it says to --version: {found:?}");
    assert_eq!(python.unwrap().command, binary.to_string_lossy());

    match previous {
        Some(was) => unsafe { std::env::set_var("ROOK_HOME", was) },
        None => unsafe { std::env::remove_var("ROOK_HOME") },
    }
}
