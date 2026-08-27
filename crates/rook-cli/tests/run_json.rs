//! A whole turn through the binary, against a server standing in for a model.
//!
//! Every other CLI test stops where a model would be needed, so the one command
//! the README calls "for scripts" was never run end to end — and it was the only
//! one that ignored `--json`.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};

/// Answers one request with a finished turn, in the OpenAI dialect the
/// `openai-compatible` provider speaks.
fn serve_one(reply: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let Ok((mut socket, _)) = listener.accept() else { return };
        read_request(&mut socket);
        let body = format!("data: {reply}\n\ndata: [DONE]\n\n");
        let _ = socket.write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        );
        let _ = socket.flush();
    });
    format!("http://{addr}/v1")
}

/// A large tool schema splits across frames, so the body is read to the length
/// the headers promised rather than to the first read.
fn read_request(socket: &mut TcpStream) {
    let mut raw = Vec::new();
    let mut scratch = [0u8; 16384];
    while let Ok(n) = socket.read(&mut scratch) {
        if n == 0 {
            return;
        }
        raw.extend_from_slice(&scratch[..n]);
        let text = String::from_utf8_lossy(&raw);
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let length = head
                .lines()
                .find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(str::to_string))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if body.len() >= length {
                return;
            }
        }
    }
}

fn answered(content: &'static str) -> &'static str {
    Box::leak(
        serde_json::json!({
            "id": "1",
            "choices": [{"index": 0, "delta": {"role": "assistant", "content": content},
                         "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5}
        })
        .to_string()
        .into_boxed_str(),
    )
}

fn run(args: &[&str], endpoint: &str) -> (String, String) {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    // `openai-compatible` is the dialect with no endpoint of its own, which is
    // what makes it the one a test can point anywhere.
    std::fs::write(home.path().join("config.toml"), "[agent]\nmodel = \"openai-compatible/test-model\"\n")
        .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_rook"))
        .env("ROOK_HOME", home.path())
        .env("ROOK_LOG", "error")
        .env("ROOK_LLM_BASE_URL", endpoint)
        .arg("--workspace")
        .arg(workspace.path())
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    (String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn a_turn_run_for_a_person_streams_the_reply_and_summarises_beside_it() {
    let endpoint = serve_one(answered("the sky is blue"));
    let (stdout, stderr) = run(&["run", "what colour?"], &endpoint);

    assert!(stdout.contains("the sky is blue"), "the answer is the output: {stdout} / {stderr}");
    assert!(stderr.contains("session"), "and the accounting goes beside it: {stderr}");
    assert!(!stdout.contains("session"), "so a pipe gets the answer alone: {stdout}");
}

#[test]
fn a_turn_run_for_a_script_is_one_object_and_nothing_else() {
    let endpoint = serve_one(answered("the sky is blue"));
    let (stdout, _) = run(&["--json", "run", "what colour?"], &endpoint);

    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{e}: {stdout}"));
    assert_eq!(parsed["outcome"]["reply"], "the sky is blue");
    assert_eq!(parsed["outcome"]["steps"], 1);
    assert_eq!(parsed["outcome"]["stopped"], "end_turn", "a script has to know it finished");
    assert_eq!(parsed["outcome"]["input_tokens"], 10);
    assert!(parsed["session"].as_str().is_some_and(|s| !s.is_empty()), "{parsed}");
}
