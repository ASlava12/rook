//! What a failure tells the user to do. The first command a new user runs is
//! the one most likely to fail, and "transport error: error sending request for
//! url (…)" told them nothing they could act on.

use rook_llm::LlmError;

fn message(endpoint: &str) -> String {
    LlmError::unreachable(endpoint, "connection refused").to_string()
}

#[test]
fn an_endpoint_that_is_not_running_says_to_start_it() {
    let said = message("http://127.0.0.1:11434/v1/models");

    assert!(said.contains("http://127.0.0.1:11434"), "{said}");
    assert!(!said.contains("/v1/models"), "the path a request used is noise: {said}");
    assert!(said.contains("Start the server"), "{said}");
    assert!(said.contains("rook models"), "and name the command that lists them: {said}");
}

#[test]
fn a_local_server_moved_off_the_usual_address_still_reads_as_local() {
    for endpoint in ["http://127.0.0.2:11434", "http://localhost:1234/v1", "http://[::1]:8080"] {
        assert!(message(endpoint).contains("Start the server"), "{endpoint} read as remote");
    }
}

#[test]
fn a_hosted_endpoint_is_told_to_check_the_network_and_the_key() {
    let said = message("https://api.anthropic.com/v1/messages");

    assert!(said.contains("https://api.anthropic.com"), "{said}");
    assert!(said.contains("API key"), "{said}");
    assert!(!said.contains("Start the server"), "nobody can start api.anthropic.com: {said}");
}

#[test]
fn an_empty_api_key_is_reported_as_unset_rather_than_sent() {
    unsafe { std::env::set_var("ANTHROPIC_API_KEY", "  ") };
    let Err(err) = rook_llm::from_spec("anthropic/claude-opus-5", std::time::Duration::from_secs(1)) else {
        panic!("a blank key must not build a provider")
    };
    let err = err.to_string();
    unsafe { std::env::remove_var("ANTHROPIC_API_KEY") };

    assert!(err.contains("ANTHROPIC_API_KEY is not set"), "an exported blank is the usual slip: {err}");
    assert!(err.contains("ollama"), "and it should name a way out: {err}");
}
