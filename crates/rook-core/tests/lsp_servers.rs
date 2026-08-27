//! Choosing and starting a language server.
//!
//! Both of these were watched happening: a live turn in a Python project asked
//! about a symbol, and the first configured server was a `rust-analyzer` shim
//! that cannot start. rustup installs one whether or not the component is.

use std::time::Instant;

use rook_core::lsp::Servers;
use rook_lsp::ServerConfig;

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
