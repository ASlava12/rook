//! A whole turn through the binary, against a server standing in for a model.
//!
//! Every other CLI test stops where a model would be needed, so the one command
//! the README calls "for scripts" was never run end to end — and it was the only
//! one that ignored `--json`.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};

/// Answers the streamed turn and then the tools-free completion check.
fn serve_one(reply: &'static str) -> String {
    serve_sequence(vec![
        ("text/event-stream", format!("data: {reply}\n\ndata: [DONE]\n\n")),
        ("application/json", completion_verdict("finish")),
    ])
}

fn completion_verdict(action: &str) -> String {
    serde_json::json!({
        "id": "check", "model": "test-model",
        "choices": [{"index": 0, "message": {"role": "assistant",
            "content": serde_json::json!({"action": action}).to_string()}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 2, "completion_tokens": 1}
    })
    .to_string()
}

fn serve_sequence(replies: Vec<(&'static str, String)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for (mime, body) in replies {
            let Ok((mut socket, _)) = listener.accept() else { return };
            read_request(&mut socket);
            let _ = socket.write_all(
                format!("HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()
            );
            let _ = socket.flush();
        }
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
    let (stdout, stderr, _) = run_with_status(args, endpoint, "");
    (stdout, stderr)
}

fn run_with_status(args: &[&str], endpoint: &str, extra_config: &str) -> (String, String, i32) {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    // `openai-compatible` is the dialect with no endpoint of its own, which is
    // what makes it the one a test can point anywhere.
    std::fs::write(
        home.path().join("config.toml"),
        format!("[agent]\nmodel = \"openai-compatible/test-model\"\n{extra_config}"),
    )
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
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn a_turn_run_for_a_person_streams_the_reply_and_summarises_beside_it() {
    let _serial = one_at_a_time();
    let endpoint = serve_one(answered("the sky is blue"));
    let (stdout, stderr) = run(&["run", "what colour?"], &endpoint);

    assert!(stdout.contains("the sky is blue"), "the answer is the output: {stdout} / {stderr}");
    assert!(stderr.contains("session"), "and the accounting goes beside it: {stderr}");
    assert!(!stdout.contains("session"), "so a pipe gets the answer alone: {stdout}");
}

#[test]
fn a_turn_run_for_a_script_is_one_object_and_nothing_else() {
    let _serial = one_at_a_time();
    let endpoint = serve_one(answered("the sky is blue"));
    let (stdout, _) = run(&["--json", "run", "what colour?"], &endpoint);

    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{e}: {stdout}"));
    assert_eq!(parsed["outcome"]["reply"], "the sky is blue");
    assert_eq!(parsed["outcome"]["steps"], 1);
    assert_eq!(parsed["outcome"]["stopped"], "end_turn", "a script has to know it finished");
    assert_eq!(parsed["outcome"]["input_tokens"], 12, "the completion check is charged too");
    assert!(parsed["session"].as_str().is_some_and(|s| !s.is_empty()), "{parsed}");
}

/// Answers every request with the same tool call, so the loop never reaches an
/// end of its own and runs into the step limit.
fn serve_forever(reply: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        while let Ok((mut socket, _)) = listener.accept() {
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
        }
    });
    format!("http://{addr}/v1")
}

fn asked_for_a_tool() -> &'static str {
    Box::leak(
        serde_json::json!({
            "id": "1",
            "choices": [{"index": 0, "delta": {"role": "assistant", "tool_calls": [{
                "index": 0, "id": "call_1", "type": "function",
                "function": {"name": "list_dir", "arguments": "{\"path\":\".\"}"}
            }]}, "finish_reason": "tool_calls"}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5}
        })
        .to_string()
        .into_boxed_str(),
    )
}

/// Running out of steps leaves the work half done, and it came back as success:
/// a script piping the output onward could not tell it from a finished turn.
#[test]
fn a_turn_that_ran_out_of_steps_says_so_to_the_caller() {
    let _serial = one_at_a_time();
    let endpoint = serve_forever(asked_for_a_tool());
    let (stdout, stderr, code) =
        run_with_status(&["--json", "run", "list everything"], &endpoint, "max_steps = 2\n");

    assert_eq!(code, 2, "an unfinished turn is not a success: {stderr}");
    assert!(stderr.contains("step limit"), "and it says which limit and what to change: {stderr}");

    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(parsed["outcome"]["stopped"], "max_steps", "stdout stays the machine channel");
    assert_eq!(
        parsed["outcome"]["steps"], 2,
        "against the limit this test set, not the default it would reach anyway: {parsed}"
    );
}

#[test]
fn a_turn_that_finished_exits_cleanly() {
    let _serial = one_at_a_time();
    let endpoint = serve_one(answered("done"));
    let (_, stderr, code) = run_with_status(&["run", "what colour?"], &endpoint, "");

    assert_eq!(code, 0, "{stderr}");
    assert!(!stderr.contains("rather than finishing"), "{stderr}");
}

#[test]
fn repeated_progress_only_replies_exit_as_incomplete() {
    let _serial = one_at_a_time();
    let mut replies = Vec::new();
    for _ in 0..3 {
        let progress = answered("Let me continue the audit and write the report.");
        replies.push(("text/event-stream", format!("data: {progress}\n\ndata: [DONE]\n\n")));
        replies.push(("application/json", completion_verdict("continue")));
    }
    let endpoint = serve_sequence(replies);
    let (stdout, stderr, code) =
        run_with_status(&["--json", "run", "Audit this directory and write a report"], &endpoint, "");
    assert_eq!(code, 2, "unfinished work must not succeed: {stderr}");
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(parsed["outcome"]["stopped"], "incomplete");
    assert_eq!(parsed["outcome"]["steps"], 3);
    assert!(stderr.contains("/continue"));
}

#[test]
fn output_is_saved_by_the_program_and_write_failure_is_a_nonzero_exit() {
    let _serial = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[agent]\nmodel = \"openai-compatible/test-model\"\n")
        .unwrap();
    for (destination, success) in [("report.md", true), ("occupied", false)] {
        std::fs::create_dir_all(workspace.path().join("occupied")).unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_rook"))
            .env("ROOK_HOME", home.path())
            .env("ROOK_LOG", "error")
            .env("ROOK_LLM_BASE_URL", serve_one(answered("the exact final answer")))
            .arg("--workspace")
            .arg(workspace.path())
            .args(["--json", "run", "--output", destination, "give a report"])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(out.status.success(), success, "{}", String::from_utf8_lossy(&out.stderr));
        assert!(!out.stdout.contains(&7), "terminal notification must not enter JSON output");
        assert!(!out.stderr.contains(&7), "redirected stderr is not a terminal");
        if success {
            assert_eq!(
                std::fs::read_to_string(workspace.path().join(destination)).unwrap(),
                "the exact final answer"
            );
        }
    }
}

fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Runs inside the operator's second check, in a separate process from `rook`.
#[test]
fn evaluation_wait_child() {
    let Ok(root) = std::env::var("ROOK_EVALUATION_WAIT") else { return };
    let root = std::path::PathBuf::from(root);
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(root.join("pending")).unwrap();
    file.write_all(b"x").unwrap();
    file.sync_all().unwrap();
    let until = std::time::Instant::now() + std::time::Duration::from_secs(90);
    while !root.join("release").exists() {
        assert!(std::time::Instant::now() < until, "parent did not release evaluator helper");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[test]
fn work_resume_preserves_the_first_iteration_and_never_repeats_interrupted_checks() {
    let _serial = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "[agent]\nmodel = 'openai-compatible/test-model'\ninstall_servers = false\n",
    )
    .unwrap();
    std::fs::create_dir(workspace.path().join(".rook")).unwrap();
    let executable = std::env::current_exe().unwrap().to_string_lossy().into_owned();
    let quoted = if cfg!(windows) {
        format!("\"{executable}\"")
    } else {
        format!("'{}'", executable.replace('\'', "'\\''"))
    };
    let card = rook_core::evaluation::Scorecard {
        checks: vec![
            rook_core::evaluation::Check {
                name: "known".into(),
                run: "echo x >> known".into(),
                ..Default::default()
            },
            rook_core::evaluation::Check {
                name: "pending".into(),
                run: format!("{quoted} --exact evaluation_wait_child --nocapture"),
                ..Default::default()
            },
        ],
    };
    std::fs::write(
        workspace.path().join(".rook/evaluation.toml"),
        card.checks
            .iter()
            .map(|check| {
                format!(
                    "[[check]]\nname = {}\nrun = {}\n",
                    serde_json::to_string(&check.name).unwrap(),
                    serde_json::to_string(&check.run).unwrap()
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let endpoint = serve_one(answered("finished the work"));
    let command = || {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_rook"));
        cmd.env("ROOK_HOME", home.path())
            .env("ROOK_LOG", "error")
            .env("ROOK_LLM_BASE_URL", &endpoint)
            .env("ROOK_EVALUATION_WAIT", workspace.path())
            .arg("--workspace")
            .arg(workspace.path())
            .arg("--json")
            .stdin(Stdio::null());
        cmd
    };
    struct Active {
        child: std::process::Child,
        release: std::path::PathBuf,
    }
    impl Drop for Active {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            let _ = std::fs::write(&self.release, b"release");
        }
    }
    let mut active = Active {
        child: command()
            .args(["work", "--most", "3", "finish this"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
        release: workspace.path().join("release"),
    };
    let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while std::fs::read(workspace.path().join("pending")).unwrap_or_default() != b"x" {
        assert!(active.child.try_wait().unwrap().is_none(), "work exited before evaluation");
        assert!(std::time::Instant::now() < until, "work did not reach evaluation");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    active.child.kill().unwrap();
    active.child.wait().unwrap();
    std::fs::write(&active.release, b"release").unwrap();
    let saved = {
        let store = rook_store::Store::open(home.path().join("store")).unwrap();
        let meta = store.list_sessions().unwrap();
        assert_eq!(meta.len(), 1);
        rook_store::format_session_id(meta[0].id)
    };
    let known = std::fs::read(workspace.path().join("known")).unwrap();
    // Resume uses the saved contract, even when the live scorecard became unreadable.
    std::fs::write(workspace.path().join(".rook/evaluation.toml"), "invalid = [").unwrap();
    let out = command().args(["work", "--resume", "--most", "10", "finish this"]).output().unwrap();
    assert!(!out.status.success());
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(body["stopped"].as_str().unwrap().contains("unknown operation"), "{body}");
    assert!(body["iterations"].as_array().unwrap().is_empty(), "the first iteration is still in flight");
    let out = command().args(["session", "recovery", &saved]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let receipts: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let operation = receipts[0]["unknown"][0]["id"].as_str().unwrap();
    let out = command()
        .args([
            "session",
            "recovery",
            &saved,
            "--acknowledge",
            operation,
            "--note",
            "inspected the marker and released the check helper",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let out = command().args(["work", "--resume"]).output().unwrap();
    assert!(!out.status.success());
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(body["stopped"].as_str().unwrap().contains("will not repeat"), "{body}");
    assert_eq!(std::fs::read(workspace.path().join("known")).unwrap(), known);
    assert_eq!(std::fs::read(workspace.path().join("pending")).unwrap(), b"x");
    let store = rook_store::Store::open(home.path().join("store")).unwrap();
    assert_eq!(store.list_sessions().unwrap().len(), 1, "resume reused the same session");
    let run = body["run"].as_str().unwrap();
    let state: serde_json::Value =
        serde_json::from_slice(&store.kv_get(&format!("work/state/{run}")).unwrap().unwrap()).unwrap();
    assert_eq!(state["plan"]["most"], 3, "--resume must not reset the saved budget");
    assert_eq!(state["active"]["session"], saved);
}

#[test]
fn a_recipe_selects_a_configured_model_and_saves_a_parameterized_structured_report() {
    let _serial = one_at_a_time();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let endpoint = serve_one(answered(r#"{"ok":true}"#));
    std::fs::write(home.path().join("config.toml"), format!(
        "[agent]\nmodel='openai-compatible/default-model'\ninstall_servers=false\n[models.audit]\nmodel='chosen-model'\napi='openai'\nurl='{endpoint}'\n"
    )).unwrap();
    std::fs::create_dir_all(workspace.path().join(".rook/recipes")).unwrap();
    std::fs::write(workspace.path().join(".rook/recipes/audit.toml"), "version=1\nprompt='Audit {{scope}}'\nmodel='audit'\noutput='{{scope}}.json'\noutput_schema={type='object',required=['ok'],properties={ok={const=true}}}\n[parameters.scope]\ndescription='Scope to inspect'\n[limits]\nsteps=2\ntokens=4000\nseconds=60\n").unwrap();
    let command = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rook"));
        command
            .env("ROOK_HOME", home.path())
            .env("ROOK_LLM_BASE_URL", "http://127.0.0.1:9/v1")
            .arg("--workspace")
            .arg(workspace.path())
            .arg("--json")
            .stdin(Stdio::null());
        command
    };
    let result =
        command().args(["run", "--recipe", "audit", "--param", "scope=src", "--yes"]).output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    let body: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(body["outcome"]["reply"], r#"{"ok":true}"#);
    assert_eq!(std::fs::read_to_string(workspace.path().join("src.json")).unwrap(), r#"{"ok":true}"#);
    {
        let store = rook_store::Store::open(home.path().join("store")).unwrap();
        assert_eq!(store.list_sessions().unwrap()[0].model, "audit");
    }
    let missing = command().args(["run", "--recipe", "audit", "--yes"]).output().unwrap();
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("missing recipe parameter"));
    assert_eq!(std::fs::read_to_string(workspace.path().join("src.json")).unwrap(), r#"{"ok":true}"#);
}

#[test]
fn a_cli_turn_reads_only_explicit_attachments_and_rejects_oversized_files() {
    let _serial = one_at_a_time();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("context.txt");
    std::fs::write(&file, "local context").unwrap();
    let endpoint = serve_one(answered("read attachment"));
    let (stdout, stderr, status) =
        run_with_status(&["run", "--json", "--context", file.to_str().unwrap()], &endpoint, "");
    assert_eq!(status, 0, "{stdout} {stderr}");
    assert!(stdout.contains("read attachment"));
    let large = std::fs::File::create(&file).unwrap();
    large.set_len(256 * 1024 + 1).unwrap();
    let (_, stderr, status) =
        run_with_status(&["run", "--context", file.to_str().unwrap()], "http://127.0.0.1:9/v1", "");
    assert_ne!(status, 0);
    assert!(stderr.contains("exceeds"), "{stderr}");
}
