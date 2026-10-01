//! The CLI as a user meets it: the real binary, a real store, real output.
//!
//! Everything here was verified by hand at some point and would have been
//! verified by hand again. A command that stops printing what it printed, or
//! starts failing on an empty store, is not something the unit tests can see.

use std::path::PathBuf;
use std::process::{Command, Output};

struct Rook {
    home: tempfile::TempDir,
    workspace: tempfile::TempDir,
}

#[test]
fn identified_goal_controls_retry_through_cli_after_daemon_restart() {
    let rook = Rook::new();
    rook.write_config("[agent]\nmodel='missing-model'\ninstall_servers=false\n");
    let daemon = Daemon::start(&rook);
    let run = rook.json(&["task", "start", "Wait for a control", "--yes"]);
    let id = run["id"].as_str().unwrap();
    let generation = run["generation"].as_str().unwrap();
    let first = rook.run(&["--json", "task", "pause", id]);
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    let stderr = String::from_utf8_lossy(&first.stderr);
    assert!(stderr.contains("control ") && stderr.contains(generation), "{stderr}");
    let first: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first["already_applied"], false);
    assert_eq!(first["run"]["status"], "paused");
    let control_id = first["id"].as_str().unwrap().to_owned();
    drop(daemon);
    std::fs::remove_file(rook.home.path().join("rookd.addr")).unwrap();
    let daemon = Daemon::start(&rook);
    let repeated = rook.json(&["task", "pause", id, "--control-id", &control_id, "--generation", generation]);
    assert_eq!(repeated["already_applied"], true);
    assert_eq!(repeated["run"]["status"], "paused");
    let conflict = rook.run(&["task", "cancel", id, "--control-id", &control_id, "--generation", generation]);
    assert!(!conflict.status.success());
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("another action"));
    use std::io::{Read, Write};
    let address = daemon.address.strip_prefix("http://").unwrap();
    let mut socket = std::net::TcpStream::connect(address).unwrap();
    socket
        .write_all(
            format!(
                "POST /api/work/{id}/control HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: 8\r\nConnection: close\r\n\r\n\"resume\""
            )
            .as_bytes(),
        )
        .unwrap();
    let mut legacy = String::new();
    socket.read_to_string(&mut legacy).unwrap();
    assert!(legacy.starts_with("HTTP/1.1 200"), "{legacy}");
    assert!(legacy.contains("\"status\":\"queued\""), "{legacy}");
    assert!(!legacy.contains("\"run\":"), "legacy response remains a bare run: {legacy}");
    let old_retry =
        rook.json(&["task", "pause", id, "--control-id", &control_id, "--generation", generation]);
    assert_eq!(old_retry["already_applied"], true);
    assert_ne!(old_retry["run"]["status"], "paused");
}

#[test]
fn daemon_repl_retains_a_failed_prompt_until_explicit_retry_or_discard() {
    let rook = Rook::new();
    rook.write_config("[agent]\nmodel='missing-model'\ninstall_servers=false\n");
    let session = rook_store::new_session_id();
    let id = rook_store::format_session_id(session);
    {
        let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
        store
            .create_session(&rook_store::SessionMeta::new(
                session,
                "REPL retry",
                rook.workspace.path().display().to_string(),
                rook_store::now_unix(),
            ))
            .unwrap();
    }
    let _daemon = Daemon::start(&rook);
    let output = rook.chat_in_session(&id, "FIRST_UNCERTAIN\nSECOND_MUST_WAIT\n/retry\n/discard\n/quit\n");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.matches("Prompt saved").count(), 2, "{stderr}");
    assert!(stderr.contains("Resolve the saved prompt"), "{stderr}");
    assert!(stderr.contains("no default endpoint"), "{stderr}");
    let history = rook.json(&["session", "history", &id]);
    let history = history.to_string();
    assert_eq!(history.matches("no default endpoint").count(), 2, "{history}");
    assert!(!history.contains("SECOND_MUST_WAIT"), "{history}");
}

#[test]
fn daemon_repl_retry_finds_a_restarted_daemon_at_its_new_address() {
    use std::io::Write;
    let rook = Rook::new();
    rook.write_config("[agent]\nmodel='missing-model'\ninstall_servers=false\n");
    let session = rook_store::new_session_id();
    let id = rook_store::format_session_id(session);
    {
        let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
        store
            .create_session(&rook_store::SessionMeta::new(
                session,
                "REPL reconnect",
                rook.workspace.path().display().to_string(),
                rook_store::now_unix(),
            ))
            .unwrap();
    }
    let daemon = Daemon::start(&rook);
    let stderr_path = rook.home.path().join("repl-restart.err");
    let mut child = Command::new(env!("CARGO_BIN_EXE_rook"))
        .env("ROOK_HOME", rook.home.path())
        .env("ROOK_LOG", "error")
        .args(["--workspace", rook.workspace.path().to_str().unwrap(), "chat", "--session", &id])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::fs::File::create(&stderr_path).unwrap())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    input.write_all(b"FIRST_BEFORE_RESTART\n").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !std::fs::read_to_string(&stderr_path).unwrap_or_default().contains("Prompt saved") {
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            panic!("REPL never reported the first failed prompt");
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    drop(daemon);
    std::fs::remove_file(rook.home.path().join("rookd.addr")).unwrap();
    let restarted = Daemon::start(&rook);
    input.write_all(b"/retry\n/quit\n").unwrap();
    drop(input);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            panic!("REPL did not finish its saved retry after daemon restart");
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let output = child.wait_with_output().unwrap();
    let stderr = std::fs::read_to_string(stderr_path).unwrap();
    assert!(output.status.success(), "{stderr}");
    assert_eq!(stderr.matches("Prompt saved").count(), 2, "{stderr}");
    assert!(!stderr.contains("Cannot reconnect"), "{stderr}");
    assert!(stderr.contains(&format!("reconnected to rookd at {}", restarted.address)), "{stderr}");
    assert_eq!(stderr.matches("no default endpoint").count(), 2, "{stderr}");
    assert!(rook.json(&["session", "history", &id]).to_string().contains("no default endpoint"));
}

#[test]
fn daemon_repl_keeps_a_new_session_after_its_first_prompt_fails() {
    use std::io::Write;
    let rook = Rook::new();
    rook.write_config("[agent]\nmodel='missing-model'\ninstall_servers=false\n");
    let _daemon = Daemon::start(&rook);
    let mut child = Command::new(env!("CARGO_BIN_EXE_rook"))
        .env("ROOK_HOME", rook.home.path())
        .env("ROOK_LOG", "error")
        .args(["--workspace", rook.workspace.path().to_str().unwrap(), "chat"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"FIRST\n/discard\nSECOND\n/quit\n").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let sessions = rook.json(&["session", "ls", "--all"]);
    let sessions = sessions.as_array().unwrap();
    assert_eq!(sessions.len(), 1, "both failed prompts must use the first session: {sessions:?}");
    let id = sessions[0]["id"].as_str().unwrap();
    let history = rook.json(&["session", "history", id]).to_string();
    assert_eq!(history.matches("no default endpoint").count(), 2, "{history}");
}

#[test]
fn model_branch_suggestion_is_reviewable_locally_and_through_daemon() {
    rook_llm::init_tls();
    use std::io::{Read, Write};
    let rook = Rook::new();
    let source = rook_store::new_session_id();
    let target = rook_store::new_session_id();
    let from = rook_store::format_session_id(source);
    let to = rook_store::format_session_id(target);
    {
        let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
        for id in [source, target] {
            store
                .create_session(&rook_store::SessionMeta::new(
                    id,
                    "branch",
                    rook.workspace.path().display().to_string(),
                    1,
                ))
                .unwrap();
        }
        store
            .append_event(
                source,
                rook_store::NewEvent::new(
                    rook_store::EventKind::UserMessage,
                    rook_store::Kind::Message,
                    b"Explore option A",
                ),
            )
            .unwrap();
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for _ in 0..2 {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(std::time::Duration::from_secs(30))).unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0; 4096];
            let request = loop {
                let n = socket.read(&mut chunk).unwrap();
                assert!(n > 0 && bytes.len() + n < 64 * 1024, "bounded model request");
                bytes.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&bytes);
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let length: usize = head
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|n| n.trim().parse().ok())
                        })
                        .unwrap_or(0);
                    if body.len() >= length {
                        break serde_json::from_str::<serde_json::Value>(body).unwrap();
                    }
                }
            };
            requests.push(request);
            let answer = serde_json::json!({
                "id":"summary-test", "model":"test",
                "choices":[{"index":0,"delta":{"role":"assistant","content":"Option A was explored; outcome remains unverified."},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":10,"completion_tokens":8}
            }).to_string();
            let body = format!("data: {answer}\n\ndata: [DONE]\n\n");
            socket.write_all(format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()
            ).as_bytes()).unwrap();
        }
        requests
    });
    rook.write_config(&format!(
        "[agent]\nmodel='local'\n[models.local]\nmodel='test'\napi='openai'\nurl='{endpoint}'\n"
    ));
    let preliminary = rook.json(&["session", "summary-draft", &from, &to]);
    assert_eq!(preliminary["source_through"], 0);
    let local = rook.json(&["session", "summary-draft", &from, &to, "--suggest"]);
    assert_eq!(local["source_session"], from);
    assert_eq!(local["source_through"], 0);
    assert!(local["text"].as_str().unwrap().contains("Option A was explored"));
    let daemon = Daemon::start(&rook);
    let remote = rook.json(&["session", "summary-draft", &from, &to, "--suggest"]);
    assert_eq!(remote, local);
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2);
    for request in requests {
        assert!(request.to_string().contains("Explore option A"));
        assert!(!request.to_string().contains("state of current workspace"));
    }
    let history = rook.json(&["session", "history", &to]);
    assert!(!history.to_string().contains("Option A was explored"), "suggestion must not save itself");
    drop(daemon);
}

#[test]
fn branch_summaries_keep_source_attribution_locally_and_through_daemon() {
    let rook = Rook::new();
    let source = rook_store::new_session_id();
    let target = rook_store::new_session_id();
    let source_name = rook_store::format_session_id(source);
    let target_name = rook_store::format_session_id(target);
    {
        let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
        for id in [source, target] {
            store
                .create_session(&rook_store::SessionMeta::new(
                    id,
                    "branch",
                    rook.workspace.path().display().to_string(),
                    1,
                ))
                .unwrap();
        }
        store
            .append_event(
                source,
                rook_store::NewEvent::new(
                    rook_store::EventKind::UserMessage,
                    rook_store::Kind::Message,
                    b"old branch",
                ),
            )
            .unwrap();
    }
    let draft = rook.json(&["session", "summary-draft", &source_name, &target_name]);
    assert_eq!(draft["source_session"], source_name);
    assert_eq!(draft["source_through"], 0);
    assert!(draft["text"].as_str().unwrap().contains("event #0 (user): old branch"));
    let first = rook.json(&["session", "summary", &source_name, &target_name, "Earlier tests passed there"]);
    assert_eq!(first["event"], 0);
    let daemon = Daemon::start(&rook);
    let remote_draft = rook.json(&["session", "summary-draft", &source_name, &target_name]);
    assert_eq!(remote_draft, draft);
    let second = rook.json(&["session", "summary", &source_name, &target_name, "A second finding"]);
    assert_eq!(second["event"], 1);
    let history = rook.json(&["session", "history", &target_name]);
    assert!(history.to_string().contains(&source_name));
    assert!(history.to_string().contains("historical branch observations"));
    drop(daemon);
}

#[test]
fn branch_summary_draft_scopes_fork_events_locally_and_through_daemon() {
    let rook = Rook::new();
    let source = rook_store::new_session_id();
    let source_name = rook_store::format_session_id(source);
    {
        let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
        store
            .create_session(&rook_store::SessionMeta::new(
                source,
                "source",
                rook.workspace.path().display().to_string(),
                1,
            ))
            .unwrap();
        for text in ["shared history", "source-only finding"] {
            store
                .append_event(
                    source,
                    rook_store::NewEvent::new(
                        rook_store::EventKind::UserMessage,
                        rook_store::Kind::Message,
                        text.as_bytes(),
                    ),
                )
                .unwrap();
        }
    }
    let fork = rook.ok(&["session", "fork", &source_name, "--at", "1"]);
    let target_name = fork.split_whitespace().last().unwrap();
    let check = || {
        let draft = rook.json(&["session", "summary-draft", &source_name, target_name]);
        assert_eq!(draft["scope_known"], true);
        assert_eq!(draft["common_ancestor"], source_name);
        assert_eq!(draft["source_from"], 1);
        assert_eq!(draft["source_through"], 1);
        assert_eq!(draft["scanned_events"], 1);
        assert!(draft["text"].as_str().unwrap().contains("source-only finding"));
        assert!(!draft["text"].as_str().unwrap().contains("shared history"));
        draft
    };
    let local = check();
    let daemon = Daemon::start(&rook);
    assert_eq!(check(), local);
    drop(daemon);
}

#[test]
fn repl_branch_switch_offers_review_and_explicit_skip_locally_and_through_daemon() {
    for routed in [false, true] {
        let rook = Rook::new();
        let source = rook_store::new_session_id();
        let target = rook_store::new_session_id();
        let skipped = rook_store::new_session_id();
        let from = rook_store::format_session_id(source);
        let to = rook_store::format_session_id(target);
        let skip = rook_store::format_session_id(skipped);
        {
            let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
            for id in [source, target, skipped] {
                store
                    .create_session(&rook_store::SessionMeta::new(
                        id,
                        "branch",
                        rook.workspace.path().display().to_string(),
                        1,
                    ))
                    .unwrap();
            }
            store
                .append_event(
                    source,
                    rook_store::NewEvent::new(
                        rook_store::EventKind::UserMessage,
                        rook_store::Kind::Message,
                        b"source-only finding",
                    ),
                )
                .unwrap();
        }
        let daemon = routed.then(|| Daemon::start(&rook));
        let reviewed = rook.chat_in_session(
            &from,
            &format!(
                "/session {from}\n/session {to}\n/summary-draft {to}\n/summary {to} Reviewed historical finding\n/session {to}\n/quit\n"
            ),
        );
        assert!(reviewed.status.success(), "{}", String::from_utf8_lossy(&reviewed.stderr));
        let stdout = String::from_utf8_lossy(&reviewed.stdout);
        assert!(stdout.contains("Carry a reviewed summary"), "{stdout}");
        assert_eq!(stdout.matches("Carry a reviewed summary").count(), 1, "{stdout}");
        assert!(stdout.contains("source-only finding"), "{stdout}");
        assert!(stdout.to_lowercase().contains("saved attributed summary"), "{stdout}");
        assert!(stdout.contains(&format!("continuing {to}")), "{stdout}");
        let history = rook.json(&["session", "history", &to]);
        assert!(history.to_string().contains("Reviewed historical finding"), "{history}");

        let skipped = rook.chat_in_session(&from, &format!("/session {skip}\n/session {skip}\n/quit\n"));
        assert!(skipped.status.success(), "{}", String::from_utf8_lossy(&skipped.stderr));
        let stdout = String::from_utf8_lossy(&skipped.stdout);
        assert!(stdout.contains("Carry a reviewed summary"), "{stdout}");
        assert!(stdout.contains(&format!("continuing {skip}")), "{stdout}");
        assert!(!rook.json(&["session", "history", &skip]).to_string().contains("branch-summary"));
        drop(daemon);
    }
}

#[test]
fn branch_names_and_bookmarks_have_the_same_cli_result_with_and_without_the_daemon() {
    let rook = Rook::new();
    let id = rook_store::new_session_id();
    let name = rook_store::format_session_id(id);
    {
        let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
        store
            .create_session(&rook_store::SessionMeta::new(
                id,
                "old",
                rook.workspace.path().display().to_string(),
                1,
            ))
            .unwrap();
        for text in ["first", "second"] {
            store
                .append_event(
                    id,
                    rook_store::NewEvent::new(
                        rook_store::EventKind::UserMessage,
                        rook_store::Kind::Message,
                        text.as_bytes(),
                    ),
                )
                .unwrap();
        }
    }
    assert_eq!(rook.json(&["session", "rename", &name, "План 👩‍💻"])["title"], "План 👩‍💻");
    assert_eq!(rook.json(&["session", "bookmark", &name, "0", "Начало"])["items"][0]["label"], "Начало");
    let daemon = Daemon::start(&rook);
    assert_eq!(rook.json(&["session", "bookmarks", &name])["items"][0]["seq"], 0);
    assert_eq!(
        rook.json(&["session", "bookmark", &name, "1", "Вывод"])["items"].as_array().unwrap().len(),
        2
    );
    assert_eq!(rook.json(&["session", "rename", &name, "New branch name"])["title"], "New branch name");
    assert_eq!(rook.json(&["session", "unbookmark", &name, "0"])["items"][0]["seq"], 1);
    assert!(!rook.run(&["session", "bookmark", &name, "999", "absent"]).status.success());
    drop(daemon);
    let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
    assert_eq!(store.get_session(id).unwrap().unwrap().title, "New branch name");
}

#[test]
fn event_branches_return_complete_drafts_locally_and_through_the_daemon() {
    let rook = Rook::new();
    let id = rook_store::new_session_id();
    let name = rook_store::format_session_id(id);
    let text = format!("  Привет\n{}\n", "editable ".repeat(800));
    {
        let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
        store
            .create_session(&rook_store::SessionMeta::new(
                id,
                "root",
                rook.workspace.path().display().to_string(),
                1,
            ))
            .unwrap();
        for (kind, text) in [
            (rook_store::EventKind::UserMessage, text.as_str()),
            (rook_store::EventKind::AssistantMessage, "answer"),
        ] {
            store
                .append_event(id, rook_store::NewEvent::new(kind, rook_store::Kind::Message, text.as_bytes()))
                .unwrap();
        }
    }
    let check = || {
        let branch = rook.json(&["session", "branch", &name, "0"]);
        assert_eq!(branch["node"]["parent"], name);
        assert_eq!(branch["node"]["forked_at"], 0);
        assert_eq!(branch["node"]["next_seq"], 0);
        assert_eq!(branch["draft"]["text"], text);
        let after = rook.json(&["session", "branch", &name, "1"]);
        assert_eq!(after["node"]["forked_at"], 2);
        assert!(after["draft"].is_null());
    };
    check();
    let daemon = Daemon::start(&rook);
    check();
    drop(daemon);
    let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
    assert_eq!(store.get_session(id).unwrap().unwrap().next_seq, 2);
}

#[test]
fn branch_pages_match_with_and_without_the_daemon_and_fork_boundaries_are_retained() {
    let rook = Rook::new();
    rook.write_config("[branches]\npage_entries=1\nscan_sessions=2\n");
    let id = rook_store::new_session_id();
    let name = rook_store::format_session_id(id);
    {
        let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
        store
            .create_session(&rook_store::SessionMeta::new(
                id,
                "root branch",
                rook.workspace.path().display().to_string(),
                1,
            ))
            .unwrap();
        for text in ["first", "second"] {
            store
                .append_event(
                    id,
                    rook_store::NewEvent::new(
                        rook_store::EventKind::UserMessage,
                        rook_store::Kind::Message,
                        text.as_bytes(),
                    ),
                )
                .unwrap();
        }
    }
    let first = rook.ok(&["session", "fork", &name, "--at", "1"]);
    let second = rook.ok(&["session", "fork", &name, "--at", "2"]);
    let first = first.split_whitespace().last().unwrap();
    let second = second.split_whitespace().last().unwrap();
    let check = || {
        let page = rook.json(&["session", "tree", &name]);
        assert_eq!(page["selected"]["id"], name);
        assert_eq!(page["children"].as_array().unwrap().len(), 1);
        assert_eq!(page["children"][0]["id"], first);
        assert_eq!(page["children"][0]["forked_at"], 1);
        let next = rook.json(&["session", "tree", &name, "--after", page["next"].as_str().unwrap()]);
        assert_eq!(next["children"][0]["id"], second);
        let child = rook.json(&["session", "tree", second]);
        assert_eq!(child["ancestors"][0]["id"], name);
        assert_eq!(child["selected"]["forked_at"], 2);
        (page, next, child)
    };
    let local = check();
    let daemon = Daemon::start(&rook);
    assert_eq!(local, check());
    assert!(rook.ok(&["session", "tree", &name]).contains("leave workspace files"));
    drop(daemon);
    let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
    assert_eq!(store.get_session(id).unwrap().unwrap().next_seq, 2);
    assert_eq!(std::fs::read_to_string(rook.workspace.path().join("src/main.rs")).unwrap(), "fn main() {}\n");
}

#[test]
fn history_navigation_and_quotes_match_locally_and_with_a_locked_daemon_store() {
    let rook = Rook::new();
    rook.write_config(
        "[transcript]\npage_entries=3\nbody_bytes=128\nquote_bytes=128\nsearch_bytes=4096\nsearch_events=2\n",
    );
    let id = rook_store::new_session_id();
    let name = rook_store::format_session_id(id);
    {
        let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
        store
            .create_session(&rook_store::SessionMeta::new(
                id,
                "history",
                rook.workspace.path().display().to_string(),
                rook_store::now_unix(),
            ))
            .unwrap();
        for n in 0..9 {
            store
                .append_event(
                    id,
                    rook_store::NewEvent::new(
                        rook_store::EventKind::UserMessage,
                        rook_store::Kind::Message,
                        format!("event {n}: {}END", "🙂".repeat(100)).as_bytes(),
                    ),
                )
                .unwrap();
        }
    }
    let check = || {
        let tail = rook.json(&["session", "history", &name]);
        assert_eq!(tail["items"][0]["seq"], 6);
        assert_eq!(tail["items"].as_array().unwrap().len(), 3);
        let older = rook.json(&["session", "history", &name, "--before", "6"]);
        assert_eq!(older["items"][0]["seq"], 3);
        let next = rook.json(&["session", "history", &name, "--from", "6"]);
        assert_eq!(tail, next);
        let found = rook.json(&["session", "find", &name, "event 0"]);
        assert_eq!(found["hits"][0]["seq"], 0);
        assert_eq!(found["next"]["seq"], 2);
        let more = rook.json(&["session", "find", &name, "event 0", "--from", "2", "--through", "9"]);
        assert!(more["hits"].as_array().unwrap().is_empty());
        let entry = rook.json(&["session", "entry", &name, "8", "--offset", "128"]);
        assert_eq!(entry["entry"]["seq"], 8);
        assert!(entry["next_offset"].as_u64().unwrap() > 128);
        let quote = rook.json(&["session", "quote", &name, "0"]);
        let source: serde_json::Value = serde_json::from_str(quote["text"].as_str().unwrap()).unwrap();
        assert_eq!(source["rook_source"]["authority"], "data");
        assert_eq!(source["rook_source"]["session"], name);
        assert_eq!(source["rook_source"]["complete"], false);
        assert!(!rook.run(&["session", "entry", &name, "999"]).status.success());
        (tail, quote)
    };
    let local = check();
    let _daemon = Daemon::start(&rook);
    let remote = check();
    assert_eq!(local, remote);
}

#[test]
fn mcp_probes_and_explicit_calls_work_while_the_conversation_store_is_locked() {
    let rook = Rook::new();
    let _locked = rook_store::Store::open(rook.home.path().join("store")).unwrap();
    let empty = rook.run(&["mcp", "ls", "--json"]);
    assert!(empty.status.success(), "{}", String::from_utf8_lossy(&empty.stderr));
    let body: serde_json::Value = serde_json::from_slice(&empty.stdout).unwrap();
    assert_eq!(body, serde_json::json!({"connected":[], "failed":[]}));
    assert!(!rook.run(&["mcp", "tools", "missing"]).status.success());
    let binary = serde_json::to_string(env!("CARGO_BIN_EXE_rook")).unwrap();
    let args = serde_json::json!(["--workspace", rook.workspace.path(), "mcp", "serve", "--yes"]);
    rook.write_config(&format!(
        "[[mcp]]\nname='local'\ncommand={binary}\nargs={args}\n\n[[mcp]]\nname='disabled'\ncommand='must-not-be-started'\nenabled=false\n"
    ));
    let listed = rook.run(&["mcp", "ls", "--json"]);
    assert!(listed.status.success(), "{}", String::from_utf8_lossy(&listed.stderr));
    let body: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(body["connected"].as_array().unwrap().len(), 1);
    assert_eq!(body["connected"][0]["name"], "local");
    assert_eq!(body["failed"], serde_json::json!([]));
    let tools = rook.run(&["mcp", "tools", "local", "--json"]);
    assert!(tools.status.success(), "{}", String::from_utf8_lossy(&tools.stderr));
    let body: serde_json::Value = serde_json::from_slice(&tools.stdout).unwrap();
    assert!(body.as_array().unwrap().iter().any(|tool| tool["name"] == "read_file"));
    let read = rook.run(&["mcp", "call", "local", "read_file", r#"{"path":"src/main.rs"}"#]);
    assert!(read.status.success(), "{}", String::from_utf8_lossy(&read.stderr));
    assert!(String::from_utf8_lossy(&read.stdout).contains("fn main() {}"));

    // Removing the store dependency must preserve plugin discovery and its
    // trust boundary: user-installed servers count, repository declarations do not.
    for (directory, name) in [
        (rook.home.path().join("plugins/trusted"), "trusted"),
        (rook.workspace.path().join(".rook/plugins/repository"), "repository"),
    ] {
        std::fs::create_dir_all(&directory).unwrap();
        let manifest = serde_json::json!({
            "name":name,
            "mcpServers":{"probe":{"command":env!("CARGO_BIN_EXE_rook"),"args":args}}
        });
        std::fs::write(directory.join("plugin.json"), manifest.to_string()).unwrap();
    }
    let listed = rook.run(&["mcp", "ls", "--json"]);
    assert!(listed.status.success(), "{}", String::from_utf8_lossy(&listed.stderr));
    let body: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    let connected = body["connected"].as_array().unwrap();
    assert_eq!(connected.len(), 2, "{body}");
    assert!(connected.iter().any(|server| server["name"] == "trusted__probe"));
    assert_eq!(body["failed"], serde_json::json!([]));
    assert!(String::from_utf8_lossy(&listed.stderr).contains("not started"));
}

#[test]
fn offline_config_check_never_contacts_endpoints_or_requires_secrets_or_the_store() {
    let rook = Rook::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    rook.write_config(&format!(
        "[models.local]\napi='openai'\nurl='http://{}/v1'\nmodel='example'\nkey='secret:missing'\n",
        listener.local_addr().unwrap()
    ));
    let _locked = rook_store::Store::open(rook.home.path().join("store")).unwrap();
    let out = rook.run(&["config", "check", "--offline", "--json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(body["valid"], true);
    assert_eq!(body["connections_checked"], false);
    assert_eq!(body["models"], serde_json::json!([]));
    assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
}

#[test]
fn config_check_reports_structural_errors_and_typos_with_a_failing_exit_status() {
    let rook = Rook::new();
    rook.write_config("[agent]\ncompact_at=1.0\nmax_step=4\n[models.local]\nmodel='example'\n");
    // Even the online command must diagnose a broken file before asking a
    // provider. JSON remains machine-readable on failure.
    let out = rook.run(&["config", "check", "--json"]);
    assert!(!out.status.success());
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(body["valid"], false);
    assert_eq!(body["connections_checked"], false);
    assert_eq!(body["ignored"], serde_json::json!(["agent.max_step"]));
    assert_eq!(body["errors"].as_array().unwrap().len(), 2);
    let out = rook.run(&["config", "check", "--offline"]);
    assert!(!out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("compact_at") && text.contains("api` is not set"), "{text}");
    rook.write_config("[agent]\nmax_step=4\n");
    assert!(!rook.run(&["config", "check", "--offline"]).status.success());
}

impl Rook {
    fn new() -> Self {
        let rook = Self { home: tempfile::tempdir().unwrap(), workspace: tempfile::tempdir().unwrap() };
        std::fs::create_dir_all(rook.workspace.path().join("src")).unwrap();
        std::fs::write(rook.workspace.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        rook
    }

    /// The chat REPL, driven from a pipe. Every slash command is reachable
    /// without a model; only sending a prompt needs one.
    fn chat(&self, lines: &str) -> String {
        use std::io::Write;
        let mut child = Command::new(env!("CARGO_BIN_EXE_rook"))
            .env("ROOK_HOME", self.home.path())
            .env("ROOK_LOG", "error")
            .args(["--workspace", self.workspace.path().to_str().unwrap(), "chat"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(lines.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn chat_in_session(&self, session: &str, lines: &str) -> Output {
        use std::io::Write;
        let mut child = Command::new(env!("CARGO_BIN_EXE_rook"))
            .env("ROOK_HOME", self.home.path())
            .env("ROOK_LOG", "error")
            .args(["--workspace", self.workspace.path().to_str().unwrap(), "chat", "--session", session])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(lines.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_rook"))
            .env("ROOK_HOME", self.home.path())
            .env("ROOK_LOG", "error")
            .arg("--workspace")
            .arg(self.workspace.path())
            .args(args)
            .output()
            .unwrap()
    }

    /// Same, with the built-in skills pointed somewhere real — a plain
    /// `cargo build` leaves none beside the binary.
    fn with_builtin_skills(&self, args: &[&str]) -> String {
        let skills = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../skills");
        let out = Command::new(env!("CARGO_BIN_EXE_rook"))
            .env("ROOK_HOME", self.home.path())
            .env("ROOK_LOG", "error")
            .env("ROOK_BUILTIN_SKILLS", skills)
            .arg("--workspace")
            .arg(self.workspace.path())
            .args(args)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// With something on stdin, which is how a one-shot turn is usually reached.
    fn piped(&self, args: &[&str], input: &str) -> String {
        use std::io::Write;
        let mut child = Command::new(env!("CARGO_BIN_EXE_rook"))
            .env("ROOK_HOME", self.home.path())
            .env("ROOK_LOG", "error")
            .arg("--workspace")
            .arg(self.workspace.path())
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
    }

    /// Run from a directory rather than with `--workspace`, which is the only
    /// way to exercise what happens when the user names none.
    fn from(&self, dir: &std::path::Path, args: &[&str]) -> String {
        let out = Command::new(env!("CARGO_BIN_EXE_rook"))
            .env("ROOK_HOME", self.home.path())
            .env("ROOK_LOG", "error")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
    }

    fn write_config(&self, toml: &str) {
        std::fs::create_dir_all(self.home.path()).unwrap();
        std::fs::write(self.home.path().join("config.toml"), toml).unwrap();
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "`rook {}` failed: {}{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let mut args = args.to_vec();
        args.push("--json");
        let out = self.ok(&args);
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("not JSON: {e}\n{out}"))
    }

    fn skill(&self, name: &str, body: &str) -> &Self {
        let dir = self.home.path().join("skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), body).unwrap();
        self
    }
}

#[test]
fn every_read_command_works_on_a_store_with_nothing_in_it() {
    let rook = Rook::new();
    // A first run must not need a prior one: the empty case is the first thing
    // any user sees, and it is the one nobody tries by hand twice.
    for args in [
        &["store", "stat"][..],
        &["store", "ls"],
        &["store", "refs"],
        &["session", "ls"],
        &["skills", "ls"],
        &["checkpoint", "ls"],
        &["memory", "ls"],
        &["doctor"],
    ] {
        rook.ok(args);
    }
}

#[test]
fn json_output_is_json_on_every_command_that_offers_it() {
    let rook = Rook::new();
    for args in [&["store", "stat"][..], &["store", "ls"], &["session", "ls"], &["skills", "ls"], &["doctor"]]
    {
        rook.json(args);
    }
}

#[test]
fn a_checkpoint_round_trips_through_the_store() {
    let rook = Rook::new();
    let created = rook.ok(&["checkpoint", "create", "before"]);
    assert!(created.contains("before"), "{created}");

    let listed = rook.json(&["checkpoint", "ls"]);
    assert_eq!(listed.as_array().unwrap().len(), 1, "{listed}");

    let stats = rook.json(&["store", "stat"]);
    assert!(stats["objects"].as_u64().unwrap() > 0, "{stats}");
}

#[test]
fn a_skill_is_discovered_scoped_and_explained() {
    let rook = Rook::new();
    rook.skill(
        "bsd-sed",
        "---\nname: bsd-sed\ndescription: In-place edits on BSD userland.\nversion: 1.0.0\n\
         requires:\n  os: [plan9]\n---\nUse `sed -i ''`.\n",
    );

    let listed = rook.ok(&["skills", "ls"]);
    assert!(!listed.contains("bsd-sed"), "a skill for another OS must not be offered: {listed}");

    let all = rook.ok(&["skills", "ls", "--all"]);
    assert!(all.contains("bsd-sed"), "{all}");

    let why = rook.ok(&["skills", "why", "bsd-sed"]);
    assert!(why.contains("plan9"), "it must say what did not match: {why}");
}

#[test]
fn a_skill_written_by_a_person_can_be_versioned_and_rolled_back() {
    let rook = Rook::new();
    let head = "---\nname: notes\ndescription: How to take notes.\nversion: 1.0.0\n---\n";
    rook.skill("notes", &format!("{head}First version.\n"));
    rook.ok(&["skills", "capture", "notes", "-m", "first"]);

    rook.skill("notes", &format!("{head}Second version.\n"));
    rook.ok(&["skills", "capture", "notes", "-m", "second"]);

    let history = rook.json(&["skills", "history", "notes"]);
    let versions = history.as_array().unwrap();
    assert_eq!(versions.len(), 2, "{history}");

    let first = versions.iter().find(|v| v["note"] == "first").unwrap()["object"].as_str().unwrap();
    let rolled = rook.ok(&["skills", "rollback", "notes", first]);
    assert!(rook.ok(&["skills", "show", "notes"]).contains("First version"));

    // The undo it offers has to be one: a rollback that says it is undoable and
    // names nothing is the claim without the capture behind it.
    let undo = rolled
        .lines()
        .find_map(|l| l.strip_prefix("undo with `rook skills rollback notes "))
        .and_then(|l| l.strip_suffix("`"))
        .unwrap_or_else(|| panic!("no undo point named in: {rolled}"))
        .to_string();
    rook.ok(&["skills", "rollback", "notes", &undo]);
    assert!(
        rook.ok(&["skills", "show", "notes"]).contains("Second version"),
        "the undo point must hold what was on disk before the rollback"
    );
}

#[test]
fn an_unreadable_config_is_reported_rather_than_silently_defaulted() {
    let rook = Rook::new();
    std::fs::write(rook.home.path().join("config.toml"), "this is not = = toml\n").unwrap();

    let out = rook.run(&["store", "stat"]);

    assert!(!out.status.success(), "a broken config must not look like a working one");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("config.toml"), "and must name the file: {err}");
}

#[test]
fn a_partial_config_section_keeps_the_other_defaults() {
    let rook = Rook::new();
    std::fs::write(rook.home.path().join("config.toml"), "[storage.retention]\nmax_total_bytes = 1024\n")
        .unwrap();

    let out = rook.ok(&["store", "prune", "--dry-run"]);

    assert!(out.contains("size budget"), "the configured cap must be in effect: {out}");
}

#[test]
fn maintenance_says_what_it_would_do_before_it_does_it() {
    let rook = Rook::new();
    rook.ok(&["checkpoint", "create", "seed"]);

    let before = rook.json(&["store", "stat"])["objects"].as_u64().unwrap();

    let dry = rook.ok(&["store", "maintain", "--dry-run"]);
    assert!(dry.contains("[dry run]"), "{dry}");

    let after = rook.json(&["store", "stat"])["objects"].as_u64().unwrap();
    assert_eq!(before, after, "a dry run must not change the store");
    assert!(before > 0, "and there was something it could have changed");
}

#[test]
fn an_unknown_object_is_an_error_not_an_empty_answer() {
    let rook = Rook::new();
    let out = rook.run(&["store", "cat", "deadbeef"]);

    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("deadbeef"), "{err}");
}

#[test]
fn the_workspace_is_where_the_flag_says_it_is() {
    let rook = Rook::new();
    let elsewhere: PathBuf = tempfile::tempdir().unwrap().keep();
    std::fs::write(elsewhere.join("only-here.txt"), "x").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_rook"))
        .env("ROOK_HOME", rook.home.path())
        .args(["--workspace", elsewhere.to_str().unwrap(), "checkpoint", "create", "there"])
        .output()
        .unwrap();

    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("1 file"), "it captured the wrong tree");
    assert!(rook.workspace.path().join("src/main.rs").exists(), "and left the other one alone");
}

/// The daemon holds the store's single write lock, so this is the one path
/// where the CLI reads over HTTP instead of from disk. It was verified by hand
/// when it was written and would be verified by hand every time it changed.
struct Daemon {
    child: std::process::Child,
    address: String,
    /// One daemon at a time, for the reason `tui_pty` serialises its windows:
    /// each of these starts a whole `rookd` from cold — opening a store,
    /// discovering skills and plugins, binding a port — and nine at once on the
    /// Windows runner starved one past thirty seconds of having published
    /// nothing. The deadline had already been raised from four seconds to
    /// thirty, which is the tell that the number was never the problem.
    ///
    /// Held by the daemon rather than taken by each test, so the tenth test
    /// cannot forget it, and released after `Drop` has killed the child —
    /// fields drop after the `Drop` impl has run, and this one is last.
    _one: std::sync::MutexGuard<'static, ()>,
}

fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    GATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// `CARGO_BIN_EXE_` is only set for this package's own binaries, so the daemon
/// has to be found rather than named.
///
/// Build once before daemon tests, including incremental runs: an existing
/// binary may speak an older store format than the CLI being tested.
fn rookd() -> PathBuf {
    static BUILT: std::sync::Once = std::sync::Once::new();
    let path = PathBuf::from(env!("CARGO_BIN_EXE_rook")).with_file_name(if cfg!(windows) {
        "rookd.exe"
    } else {
        "rookd"
    });

    BUILT.call_once(|| {
        let built = Command::new(env!("CARGO"))
            .args(["build", "-p", "rookd"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .status();
        assert!(built.is_ok_and(|s| s.success()), "could not build rookd for the daemon tests");
    });
    assert!(path.exists(), "{} is still not there after building it", path.display());
    path
}

impl Daemon {
    /// Port 0: the OS picks a free one and rookd writes where it landed, so two
    /// tests can never collide over a number someone chose.
    fn start(rook: &Rook) -> Self {
        let one = one_at_a_time();
        // Kept rather than discarded: a daemon that never published its address
        // and one that exited on the way to binding are the same silence, and
        // the runner where that happens is not the one this is read on.
        let complaints = rook.home.path().join("rookd.err");
        let mut child = Command::new(rookd())
            .env("ROOK_HOME", rook.home.path())
            .env("ROOK_LOG", "error")
            .args(["--workspace", rook.workspace.path().to_str().unwrap()])
            .args(["--port", "0"])
            .stdout(std::process::Stdio::null())
            .stderr(std::fs::File::create(&complaints).unwrap())
            .spawn()
            .unwrap();
        // Generous, because it only tells a failed start from a slow one:
        // `rookd` opens a store, discovers skills and plugins and binds a port
        // before it writes anything, and four seconds of that was a claim about
        // speed rather than a deadline. It returns the moment the file appears.
        let address_file = rook.home.path().join("rookd.addr");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            if let Ok(address) = std::fs::read_to_string(&address_file) {
                std::thread::sleep(std::time::Duration::from_millis(150));
                return Self { child, address: address.trim().to_string(), _one: one };
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let alive = match child.try_wait() {
            Ok(None) => "still running, so it is slow rather than broken".to_string(),
            Ok(Some(status)) => format!("exited with {status}"),
            Err(e) => format!("unknown: {e}"),
        };
        let said = std::fs::read_to_string(&complaints).unwrap_or_default();
        let _ = child.kill();
        let _ = child.wait();
        panic!("rookd never published its address in 30s: {alive}\nits stderr:\n{said}");
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn a_read_routes_through_the_daemon_and_answers_the_same() {
    let rook = Rook::new();
    rook.skill(
        "routed",
        "---\nname: routed\ndescription: Reachable either way.\nversion: 1.0.0\n---\nbody\n",
    );
    rook.ok(&["checkpoint", "create", "seed"]);
    let direct = rook.json(&["skills", "ls"]);
    let direct_stats = rook.json(&["store", "stat"]);

    let daemon = Daemon::start(&rook);

    assert_eq!(rook.json(&["skills", "ls"]), direct, "routed output must be identical");
    assert_eq!(rook.json(&["store", "stat"]), direct_stats);

    let write = rook.run(&["store", "gc"]);
    assert!(write.status.success(), "{}", String::from_utf8_lossy(&write.stderr));
    assert!(
        String::from_utf8_lossy(&write.stdout).contains("scanned"),
        "a write goes over the API and answers: {}",
        String::from_utf8_lossy(&write.stdout)
    );
    drop(daemon);
}

/// Reading stdin to the end is the point — `slow_build | rook run "why?"` has
/// to wait for the build — but an idle pipe never ends, and every supervisor
/// hands a process one. Three and a half hours of a run that printed nothing
/// at all is what this is about.
#[test]
fn a_run_waiting_on_an_idle_pipe_says_so_rather_than_going_quiet() {
    use std::io::BufRead;
    let rook = Rook::new();
    let mut child = Command::new(env!("CARGO_BIN_EXE_rook"))
        .env("ROOK_HOME", rook.home.path())
        .args(["--workspace", rook.workspace.path().to_str().unwrap()])
        .args(["run", "anything"])
        // Open and silent, which is what a backgrounded shell, `nohup` and a
        // CI step all hand it.
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    // Line by line rather than to the end: the end is when the child exits,
    // and what is being asserted is that it speaks while it is still waiting.
    let stderr = child.stderr.take().unwrap();
    let (say, heard) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stderr).lines().map_while(Result::ok) {
            if say.send(line).is_err() {
                return;
            }
        }
    });

    // Generous: this is about telling a wait from a hang, not about how long
    // the wait is. The grace inside is two seconds.
    let said = match heard.recv_timeout(std::time::Duration::from_secs(60)) {
        Ok(said) => said,
        Err(_) => {
            let _ = child.kill();
            panic!("it said nothing at all, which is the failure");
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    assert!(said.contains("waiting for input on stdin"), "it says what it is waiting for: {said}");
    assert!(said.contains("/dev/null"), "and what to do about it: {said}");
}

/// Stopping the daemon meant finding its process id first — `pgrep`, then
/// `kill`, for a program a window had started on its own.
#[test]
fn the_daemon_says_what_it_is_and_stops_when_asked() {
    let rook = Rook::new();
    let daemon = Daemon::start(&rook);

    let said = String::from_utf8_lossy(&rook.run(&["daemon", "status"]).stdout).to_string();
    assert!(said.contains(&daemon.address), "it says where it is answering: {said}");
    assert!(said.contains("turns"), "and what it is doing: {said}");

    let stopped = rook.run(&["daemon", "stop"]);
    assert!(stopped.status.success(), "{}", String::from_utf8_lossy(&stopped.stderr));

    // Its address file is how every other window finds it, and it goes on the
    // way out — so this is also the wait for the process to be gone.
    let address_file = rook.home.path().join("rookd.addr");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while address_file.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(!address_file.exists(), "stopping takes the address file with it");

    let after = String::from_utf8_lossy(&rook.run(&["daemon", "status"]).stdout).to_string();
    assert!(after.contains("nothing is answering"), "and then there is nothing to talk to: {after}");
    drop(daemon);
}

/// A restart that moves the port strands every window already attached: they
/// read the address once, when they attached, and go on asking the old one.
#[test]
fn a_restart_comes_back_on_the_address_the_windows_are_holding() {
    let rook = Rook::new();
    let daemon = Daemon::start(&rook);
    let address_file = rook.home.path().join("rookd.addr");

    let restarted = rook.run(&["daemon", "restart"]);
    let said = String::from_utf8_lossy(&restarted.stdout).to_string();
    assert!(restarted.status.success(), "{}", String::from_utf8_lossy(&restarted.stderr));
    assert!(said.contains(&daemon.address), "back where it was: {said}");
    assert_eq!(
        std::fs::read_to_string(&address_file).unwrap().trim(),
        daemon.address,
        "and the file every window reads says so too"
    );

    // Left running by this command rather than by the fixture, so it is this
    // test's to stop.
    let stopped = rook.run(&["daemon", "stop"]);
    assert!(stopped.status.success(), "{}", String::from_utf8_lossy(&stopped.stderr));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while address_file.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    drop(daemon);
}

/// The writes the daemon serves. Refusing these meant stopping the daemon to
/// set a goal or to forget a fact, which is the same store answering either
/// way — and `store maintain` is what somebody reaches for exactly when a
/// long-running daemon has filled the disk.
#[test]
fn the_writes_the_daemon_serves_go_over_it_rather_than_refusing() {
    let rook = Rook::new();
    // A failed run leaves a session behind, which is all a goal needs.
    let _ = rook.run(&["run", "something to remember"]);
    let session = rook.json(&["session", "ls", "--all"])[0]["id"].as_str().unwrap().to_string();

    let daemon = Daemon::start(&rook);

    let set = rook.run(&["session", "goal", &session, "ship", "the", "thing"]);
    assert!(set.status.success(), "{}", String::from_utf8_lossy(&set.stderr));
    // Succeeding is not the claim — going over the daemon is. A command that
    // opened the store itself would pass every assertion below it and prove
    // nothing, and the line it prints when it routes is what tells them apart.
    let said = String::from_utf8_lossy(&set.stderr);
    assert!(said.contains(&daemon.address), "it has to have routed: {said}");
    let read = rook.ok(&["session", "goal", &session]);
    assert!(read.contains("ship the thing"), "the goal has to come back: {read}");

    let maintained = rook.run(&["store", "maintain", "--dry-run"]);
    assert!(maintained.status.success(), "{}", String::from_utf8_lossy(&maintained.stderr));
    assert!(
        String::from_utf8_lossy(&maintained.stdout).contains("sessions deleted"),
        "{}",
        String::from_utf8_lossy(&maintained.stdout)
    );

    let missing = rook.run(&["memory", "rm", "01NOSUCHFACT"]);
    assert!(!missing.status.success(), "a fact that is not there is still not there");
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("no fact"),
        "and says so rather than naming the lock: {}",
        String::from_utf8_lossy(&missing.stderr)
    );
    drop(daemon);
}

/// Memory is what a person edits while an agent is working, and all of it
/// refused: adding a fact, searching for one, and every way of asking what
/// changed. None of that needed an endpoint it could not have.
#[test]
fn memory_is_readable_and_writable_with_the_daemon_up() {
    let rook = Rook::new();
    rook.ok(&["memory", "add", "the port is 8443"]);

    let daemon = Daemon::start(&rook);
    let added = rook.run(&["memory", "add", "deploys go out on Thursday"]);
    assert!(added.status.success(), "{}", String::from_utf8_lossy(&added.stderr));
    let said = String::from_utf8_lossy(&added.stderr);
    assert!(said.contains(&daemon.address), "it has to have routed rather than opened the store: {said}");

    let found = rook.ok(&["memory", "search", "deploys"]);
    assert!(found.contains("Thursday"), "the fact just added has to be findable: {found}");

    let history = rook.ok(&["memory", "history"]);
    assert!(history.lines().count() >= 3, "two versions and a header: {history}");

    let since = rook.ok(&["memory", "since", "1"]);
    assert!(since.contains("Thursday"), "what changed today includes it: {since}");

    // Two objects to diff, which is why the fact before the daemon started
    // exists: a history of one has nothing to compare.
    let versions: Vec<String> = rook
        .json(&["memory", "history"])
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["object"].as_str().unwrap().to_string())
        .collect();
    let diff = rook.ok(&["memory", "diff", &versions[1], &versions[0]]);
    assert!(diff.contains("Thursday"), "the diff names the fact that arrived: {diff}");
    drop(daemon);
}

/// One command from each family that used to need the daemon stopped. The
/// point is not that each works — it is that "stop the daemon" is no longer an
/// answer the tool gives, so the list has to be walked rather than sampled.
#[test]
fn no_command_needs_the_daemon_stopped_any_more() {
    let rook = Rook::new();
    rook.skill("kept", "---\nname: kept\nversion: 1.0.0\ndescription: a skill to version\n---\n\nBody.");
    let _ = rook.run(&["run", "something to fork"]);
    let session = rook.json(&["session", "ls", "--all"])[0]["id"].as_str().unwrap().to_string();

    let daemon = Daemon::start(&rook);
    let routed = |args: &[&str]| {
        let out = rook.run(args);
        let said = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(out.status.success(), "`{}` failed: {said}", args.join(" "));
        assert!(said.contains(&daemon.address), "`{}` did not route: {said}", args.join(" "));
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    assert!(routed(&["skills", "capture", "kept", "-m", "first"]).contains("captured kept"));
    assert!(routed(&["skills", "history", "kept"]).contains("first"));
    assert!(routed(&["skills", "why", "kept"]).contains("chosen: kept"));
    assert!(routed(&["checkpoint", "create", "before"]).contains("checkpoint before"));
    assert!(routed(&["checkpoint", "ls"]).contains("before"));
    assert!(routed(&["store", "verify"]).contains("verified"));
    assert!(routed(&["store", "prune", "--dry-run"]).contains("sessions deleted"));
    assert!(routed(&["session", "fork", &session, "--at", "1"]).contains("forked"));
    assert!(routed(&["session", "rm", &session]).contains("removed session"));
    drop(daemon);
}

/// The diagnostic has to answer when things are wrong, and a daemon holding
/// the store is one of the times somebody runs it. It needs no store to say
/// anything it says.
#[test]
fn doctor_answers_with_the_daemon_up() {
    let rook = Rook::new();
    let alone = rook.ok(&["doctor"]);

    let daemon = Daemon::start(&rook);
    let out = rook.run(&["doctor"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(said.contains("toolchains detected"), "and says the same things: {said}");
    assert_eq!(
        said.lines().find(|l| l.starts_with("store")),
        alone.lines().find(|l| l.starts_with("store")),
        "including where the store is, which it names without opening"
    );
    // And the daemon itself, which is what somebody running this wants to
    // know about: with none up it says so, and with one up it says where.
    assert!(alone.contains("none running"), "with nothing up: {alone}");
    assert!(said.contains(&daemon.address), "and where it is answering: {said}");
    drop(daemon);
}

/// A transcript and a search are what somebody wants while the daemon is up, and
/// both needed the store stopped to get. Routed, they must answer what the store
/// answers — and the search must carry its filters, or a narrowed question comes
/// back widened with nothing saying so.
#[test]
fn every_read_answers_the_same_through_the_daemon_as_it_does_direct() {
    let rook = Rook::new();
    rook.skill("greet", "---\nname: greet\nversion: 1.0.0\ndescription: say hello\n---\n\nHello.");
    // The turn fails for want of a model and leaves the session behind, which is
    // all this needs: something with a transcript to read.
    // Two of them, because one session makes a filtered search and an unfiltered
    // one the same answer, and a test that cannot tell them apart proves nothing
    // about the filter.
    let _ = rook.run(&["run", "alpha worth finding"]);
    let _ = rook.run(&["run", "beta worth finding"]);
    let sessions = rook.json(&["session", "ls", "--all"]);
    assert_eq!(sessions.as_array().unwrap().len(), 2, "both failed runs leave a session");
    let alpha = sessions
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["title"].as_str().is_some_and(|t| t.starts_with("alpha")))
        .expect("the alpha session");
    let id = alpha["id"].as_str().unwrap().to_string();

    // Everything seeded before anything is measured: a search reports how many
    // objects it scanned, and a fact written between the two readings is one
    // more object.
    rook.ok(&["memory", "add", "the daemon was up", "--tag", "note"]);
    let direct_search = rook.json(&["search", "worth finding"]);
    let direct_show = rook.json(&["session", "show", &id]);
    let direct_diff = rook.json(&["session", "diff", &id]);
    let direct_memory = rook.json(&["memory", "ls"]);
    let direct_context = rook.json(&["session", "context", &id]);
    let direct_objects = rook.json(&["store", "ls"]);
    let direct_refs = rook.json(&["store", "refs"]);
    let direct_skill = rook.json(&["skills", "show", "greet"]);
    let object = direct_objects[0]["short"].as_str().expect("a listing of objects names one").to_string();
    let direct_cat = rook.ok(&["store", "cat", &object]);
    assert!(!direct_show.as_array().unwrap().is_empty(), "there is a transcript to compare");
    assert!(!direct_memory.as_array().unwrap().is_empty(), "and a fact to compare");

    let daemon = Daemon::start(&rook);

    assert_eq!(rook.json(&["search", "worth finding"]), direct_search, "routed search must match");
    assert_eq!(rook.json(&["session", "show", &id]), direct_show, "and so must the transcript");
    assert_eq!(rook.json(&["session", "diff", &id]), direct_diff, "and the diff");
    assert_eq!(rook.json(&["memory", "ls"]), direct_memory, "and what it remembers");
    assert_eq!(rook.json(&["session", "context", &id]), direct_context, "and what it costs");
    assert_eq!(rook.json(&["store", "ls"]), direct_objects, "and the objects behind all of it");
    assert_eq!(rook.json(&["store", "refs"]), direct_refs, "and what names them");
    assert_eq!(rook.json(&["skills", "show", "greet"]), direct_skill, "and a skill's body");
    assert_eq!(
        rook.ok(&["store", "cat", &object]),
        direct_cat,
        "and one object's bytes, which is what the other listings point at"
    );

    let narrowed = rook.run(&["--json", "search", "worth finding", "--session", &id]);
    assert!(narrowed.status.success(), "{}", String::from_utf8_lossy(&narrowed.stderr));
    let note = String::from_utf8_lossy(&narrowed.stderr);
    assert!(note.contains(&daemon.address), "it has to have gone over the API: {note}");
    let hits = serde_json::from_slice::<serde_json::Value>(&narrowed.stdout).unwrap();
    let text = hits.to_string();
    assert!(text.contains("alpha"), "the session it was narrowed to is in the answer: {text}");
    assert!(
        !text.contains("beta"),
        "and the other is not — a filter dropped on the way to the daemon widens the answer \
         with nothing saying so: {text}"
    );
}

/// A command answering is no longer the question, because every command
/// answers either way. What has to end with the daemon is the routing: after
/// it stops, the store is opened here again and nothing is said about a
/// daemon.
#[test]
fn the_store_is_opened_here_again_once_the_daemon_stops() {
    let rook = Rook::new();
    {
        let daemon = Daemon::start(&rook);
        let routed = rook.run(&["store", "stat"]);
        assert!(
            String::from_utf8_lossy(&routed.stderr).contains(&daemon.address),
            "while it runs, a read goes over it: {}",
            String::from_utf8_lossy(&routed.stderr)
        );
    }
    for _ in 0..40 {
        let out = rook.run(&["store", "stat"]);
        let said = String::from_utf8_lossy(&out.stderr).into_owned();
        if out.status.success() && !said.contains("using the running rookd") {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("the lock outlived the daemon");
}

#[test]
fn the_first_command_a_new_user_runs_says_what_to_do_when_it_fails() {
    let rook = Rook::new();
    // No model is reachable on a fresh machine, which is the ordinary case and
    // the worst first impression the tool can make.
    let out = rook.run(&["models"]);

    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("cannot reach"), "{err}");
    assert!(err.contains("Start the server"), "a raw transport error is not actionable: {err}");
    assert!(err.contains("rook models"), "{err}");
}

#[test]
fn doctor_carries_the_advice_rather_than_only_the_failure() {
    let rook = Rook::new();
    let out = rook.ok(&["doctor"]);

    let model = out.split("model:").nth(1).unwrap();
    assert!(model.contains("cannot reach"), "{model}");
    assert!(model.contains("Start the server"), "doctor exists to say what to do: {model}");

    // What contains a command here is said either way, in the words a
    // command's own result would use.
    let commands = out.split("commands:").nth(1).unwrap().lines().nth(1).unwrap_or_default().to_string();
    assert!(
        commands.contains("contained — ") || commands.contains("not contained — no sandbox"),
        "{commands}"
    );
}

/// A session is bound to a project: its transcript names that project's files,
/// its checkpoints restore into it, and its memory is scoped to it. Resuming one
/// from somewhere else read the old conversation and edited the new directory.
#[test]
fn a_session_resumed_from_elsewhere_goes_on_where_it_belongs() {
    let rook = Rook::new();
    let _ = rook.run(&["run", "started here"]);
    let id = rook.json(&["session", "ls", "--all"])[0]["id"].as_str().unwrap().to_string();
    let elsewhere = tempfile::tempdir().unwrap();

    let out = rook.from(elsewhere.path(), &["run", "--session", &id, "and now?"]);

    assert!(out.contains("where this session belongs"), "{out}");
    assert!(out.contains(&rook.workspace.path().display().to_string()), "and names it: {out}");

    // `-C` is the user deciding, and is left alone.
    let named = rook.from(
        elsewhere.path(),
        &["--workspace", elsewhere.path().to_str().unwrap(), "run", "--session", &id, "and now?"],
    );
    assert!(!named.contains("where this session belongs"), "{named}");
}

#[test]
fn every_slash_command_answers_on_an_empty_session() {
    let rook = Rook::new();
    let out = rook.chat(
        "/help\n/context\n/session\n/skills\n/memory\n/search nothing\n/diff\n/mcp\n/goal\n/jobs\n/quit\n",
    );

    // Each line is what that command says when there is nothing to report,
    // which is the state every new session starts in.
    for expected in [
        "/context [window]",
        "usable tokens",
        "0 events",
        "nothing remembered yet",
        "nothing matched",
        "nothing changed on disk yet",
        "no MCP servers installed",
        "no goal set",
        "nothing running in the background",
    ] {
        assert!(out.contains(expected), "no sign of {expected:?} in:\n{out}");
    }
}

#[test]
fn the_repl_carries_a_goal_and_starts_a_fresh_session_on_request() {
    let rook = Rook::new();
    let out = rook.chat("/goal ship the release\n/goal\n/new later\n/session\n/goal\n/quit\n");

    assert!(out.contains("goal set"), "{out}");
    assert!(out.contains("ship the release"), "{out}");
    assert!(
        out.matches("no goal set").count() == 1,
        "a new session starts without the old one's goal:\n{out}"
    );
}

#[test]
fn an_unknown_command_says_so_rather_than_being_sent_to_the_model() {
    let rook = Rook::new();
    let out = rook.chat("/nonsense\n/quit\n");

    assert!(out.to_lowercase().contains("nonsense"), "{out}");
    assert!(!out.contains("cannot reach"), "it must not have gone to the provider:\n{out}");
}

#[test]
fn the_repl_can_change_the_approvals_and_the_effort() {
    let rook = Rook::new();
    // Asked for by the name it had and by the name it has: an editor, a script
    // or a habit holding `/mode` must keep working.
    let out = rook.chat("/mode\n/stance readonly\n/stance\n/effort\n/effort low\n/effort\n/quit\n");

    let lines: Vec<&str> = out
        .lines()
        .map(|l| l.trim_start_matches(['›', ' ']))
        .filter(|l| ["assist", "readonly"].contains(l) || l.starts_with("requested "))
        .collect();
    assert_eq!(
        lines,
        [
            "assist",
            "readonly",
            "requested high; mapping: not sent: no effort mapping for this model",
            "requested low; mapping: not sent: no effort mapping for this model",
            "requested low; mapping: not sent: no effort mapping for this model",
        ],
        "each reads back the preference and its actual mapping:\n{out}"
    );
}

#[test]
fn a_setting_the_repl_does_not_have_is_refused_by_name() {
    let rook = Rook::new();
    let out = rook.chat("/stance yolo\n/effort glacial\n/quit\n");

    assert!(out.contains(r#"no stance "yolo""#), "{out}");
    assert!(out.contains(r#"no effort "glacial""#), "{out}");
}

/// The built-in skills are packaged beside the binary by `cargo xtask dist`, so
/// anyone who builds and copies the binary alone has none — and a count of zero
/// says nothing about why, in the one command whose job is to explain.
#[test]
fn doctor_says_where_the_skills_would_have_come_from() {
    let rook = Rook::new();
    let said = rook.ok(&["doctor"]);

    assert!(said.contains("skills: 0 usable"), "{said}");
    assert!(said.contains("none are installed next to"), "{said}");
    assert!(said.contains("cargo xtask dist"), "and what puts them there: {said}");
    assert!(said.contains("ROOK_BUILTIN_SKILLS"), "and the way round it: {said}");
}

#[test]
fn doctor_stops_explaining_once_the_skills_are_there() {
    let rook = Rook::new();
    let said = rook.with_builtin_skills(&["doctor"]);

    assert!(!said.contains("skills: 0 usable, 0 blocked"), "the shipped skills were found: {said}");
    assert!(!said.contains("none are installed next to"), "{said}");
}

/// rustup installs a `rust-analyzer` shim whether or not the component is, so
/// "the command exists" reported a server that fails on its first request. Any
/// command that is not a language server stands in for it here.
#[test]
fn doctor_reports_a_language_server_that_does_not_actually_run() {
    let rook = Rook::new();
    rook.write_config(&format!(
        "[[lsp]]\nlanguage = \"rust\"\ncommand = {:?}\nextensions = [\"rs\"]\nstartup_timeout_secs = 5\n",
        env!("CARGO_BIN_EXE_rook")
    ));

    let said = rook.ok(&["doctor"]);
    assert!(said.contains("✗ rust"), "a binary that is present but is not a server: {said}");
    assert!(!said.contains("✓ rust"), "presence must not be reported as capability: {said}");
}

#[test]
fn doctor_says_when_no_language_server_is_configured_at_all() {
    let rook = Rook::new();
    rook.write_config("[[lsp]]\nlanguage = \"none\"\ncommand = \"\"\nextensions = []\nenabled = false\n");

    let said = rook.ok(&["doctor"]);
    assert!(said.contains("none found on PATH"), "{said}");
}

/// `cargo test 2>&1 | rook run "why?"` is how a one-shot turn is usually
/// reached, and the pipe used to be dropped without a word.
#[test]
fn run_takes_what_is_piped_into_it() {
    let rook = Rook::new();
    let said = rook.piped(&["run"], "error: cannot borrow `x` as mutable\n");

    assert!(!said.contains("nothing to do"), "the pipe is the prompt when there is no other: {said}");
}

#[test]
fn run_with_neither_a_prompt_nor_a_pipe_says_both_are_possible() {
    let rook = Rook::new();
    let said = rook.piped(&["run"], "");

    assert!(said.contains("pass a prompt, or pipe one in"), "{said}");
}

#[test]
fn a_pipe_too_large_for_the_window_is_refused_and_says_what_to_do_instead() {
    let rook = Rook::new();
    rook.write_config("[agent]\nmodel = \"ollama/small\"\ncontext_window = 16\n");

    let said = rook.piped(&["run", "explain"], &"x".repeat(4_096));
    assert!(said.contains("16-token window"), "the bound is the model's, not a constant: {said}");
    assert!(said.contains("file"), "and a file is read in pages, which is the way out: {said}");
}

/// The CLI understood `last` where a turn was continued and nowhere else, so
/// `session show last` answered that it was not a session id.
#[test]
fn every_command_that_takes_a_session_takes_last() {
    let rook = Rook::new();
    // The REPL starts a session as it opens, which is the cheapest way to have
    // one without a model to talk to.
    rook.chat("/quit\n");

    for command in
        [vec!["session", "show", "last"], vec!["session", "context", "last"], vec!["session", "diff", "last"]]
    {
        let out = rook.run(&command);
        assert!(
            out.status.success(),
            "`rook {}` refused `last`: {}",
            command.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let refused = rook.run(&["session", "show", "not-an-id"]);
    let said = String::from_utf8_lossy(&refused.stderr);
    assert!(said.contains("neither a session id nor `last`"), "{said}");
}

/// Sessions belong to the workspace they ran in — the same reasoning `last`
/// follows — and a list of every session on the machine is not what someone
/// standing in a project asked for.
#[test]
fn listing_sessions_shows_this_workspace_and_says_what_it_hid() {
    let rook = Rook::new();
    rook.chat("/quit\n");

    let elsewhere = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_rook"))
        .env("ROOK_HOME", rook.home.path())
        .env("ROOK_LOG", "error")
        .args(["--workspace", elsewhere.path().to_str().unwrap(), "chat"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    let mut child = out;
    child.stdin.take().unwrap().write_all(b"/quit\n").unwrap();
    child.wait().unwrap();

    let here = rook.ok(&["session", "ls"]);
    assert!(here.contains("1 more in other workspaces"), "and it says how to see them: {here}");
    assert!(!here.contains(elsewhere.path().to_str().unwrap()), "{here}");

    let all = rook.ok(&["session", "ls", "--all"]);
    assert!(all.contains(elsewhere.path().to_str().unwrap()), "--all is the way back to everything: {all}");
    assert!(!all.contains("more in other workspaces"), "nothing is hidden, so nothing is said: {all}");
}

/// A rule that will not compile was a line in the log file, and the log file is
/// not where anyone looks when the agent is behaving oddly.
#[test]
fn doctor_names_a_sandbox_rule_that_does_not_compile() {
    let rook = Rook::new();
    rook.write_config("[sandbox]\ndeny = ['/rm -rf ([/', 'git push --force']\n");

    let said = rook.ok(&["doctor"]);
    assert!(said.contains("approvals:"), "{said}");
    assert!(said.contains("rm -rf (["), "the rule that is wrong: {said}");
    assert!(said.contains("stops the agent"), "and what it costs: {said}");
}

/// A hook whose `match` will not parse fires on every subject instead of the
/// one it names — deliberately, since never firing is worse — but that was only
/// ever said in the log file, where it reads as the hook simply misbehaving.
#[test]
fn doctor_lists_the_hooks_and_the_matcher_that_does_not_parse() {
    let rook = Rook::new();
    rook.write_config("[[hooks]]\nevent = \"pre_tool\"\nmatch = \"/([/\"\ncommand = \"my-policy-check\"\n");

    let said = rook.ok(&["doctor"]);
    assert!(said.contains("hooks: 1"), "{said}");
    assert!(said.contains("pre_tool"), "the spelling from config.toml, not the Rust name: {said}");
    assert!(!said.contains("PreTool"), "{said}");
    assert!(said.contains("runs on every subject"), "and what the broken pattern costs: {said}");
}

/// Past the recall budget, pinning one more fact costs another one its place —
/// and the place it loses is in the context, where nobody can see it happen.
#[test]
fn memory_ls_says_when_pinning_has_outgrown_the_recall_budget() {
    let rook = Rook::new();
    rook.write_config("[memory]\ncontext_budget_tokens = 20\n");
    for i in 0..6 {
        rook.ok(&["memory", "add", "--pin", &format!("a pinned fact number {i} about this and that")]);
    }

    let said = rook.ok(&["memory", "ls"]);
    assert!(said.contains("recall budget of 20"), "{said}");
    assert!(said.contains("will not reach the model"), "{said}");
}

/// The window was a constant, so the report described a model nobody was using:
/// a session at 55% of a 6k window read as 1% of 128k, which is the difference
/// between "about to compact" and "nothing to think about".
#[test]
fn session_context_measures_against_the_model_that_is_configured() {
    let rook = Rook::new();
    rook.write_config("[agent]\nmodel = \"ollama/small\"\ncontext_window = 6000\n");
    rook.chat("/quit\n");

    let said = rook.ok(&["session", "context", "last"]);
    assert!(said.contains("window            6000"), "{said}");

    let overridden = rook.ok(&["session", "context", "last", "--window", "128000"]);
    assert!(overridden.contains("128000"), "and it can still be asked about another: {overridden}");
}

/// Installed and working is one question; used in this workspace is another,
/// and a ✓ against a language with no files here answered the first as if it
/// were the second.
#[test]
fn doctor_marks_a_server_this_workspace_has_no_files_for() {
    let rook = Rook::new();
    rook.write_config(
        "[[lsp]]\nlanguage = \"go\"\ncommand = \"gopls\"\nextensions = [\"go\"]\nstartup_timeout_secs = 2\n",
    );

    let said = rook.ok(&["doctor"]);
    assert!(said.contains("no go files here"), "the workspace is Rust and a text file: {said}");
}

#[test]
fn asking_a_language_server_where_none_applies_says_why() {
    let rook = Rook::new();
    rook.write_config(
        "[[lsp]]\nlanguage = \"go\"\ncommand = \"gopls\"\nextensions = [\"go\"]\nstartup_timeout_secs = 2\n",
    );

    let out = rook.run(&["lsp", "servers"]);
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("no language server applies here"), "{said}");
    assert!(said.contains("handles a file in"), "and what would have made one apply: {said}");
}

/// Everything `[web]` can be set to, and what doctor says about it. A setting
/// that is on but unusable is the one worth catching before a turn finds out.
#[test]
fn doctor_says_what_the_web_configuration_will_actually_do() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let doctor = |config: &str| {
        std::fs::write(home.path().join("config.toml"), config).unwrap();
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_rook"))
            .env("ROOK_HOME", home.path())
            .env_remove("BRAVE_API_KEY")
            .args(["--workspace", workspace.path().to_str().unwrap(), "doctor"])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        text.split("web:").nth(1).unwrap_or_default().split("\n\n").next().unwrap_or_default().to_string()
    };

    // The default is on and searches through the engine that needs nothing set
    // up, and what a person wants from `doctor` is which one that is.
    let out_of_the_box = doctor("");
    assert!(out_of_the_box.contains("web_search"), "the default can search: {out_of_the_box}");
    assert!(out_of_the_box.contains("duckduckgo"), "and says through whom: {out_of_the_box}");

    assert!(doctor("[web]\nenabled = false\n").contains("off"), "and off still says so");
    assert!(doctor("[web]\nsearch = \"\"\n").contains("no search engine"));
    assert!(
        doctor("[web]\nsearch = \"brave\"\n").contains("BRAVE_API_KEY"),
        "named but unusable is the case worth catching before a turn does"
    );
    let searx = doctor("[web]\nsearch = \"searxng\"\n");
    assert!(searx.contains("web_search"), "{searx}");
    assert!(searx.contains("searxng"), "{searx}");
}

/// What a model says about a technology is what it was trained on, and it says
/// so nowhere. A kept set is the alternative, and it is only worth keeping if
/// an answer out of it carries both addresses: the local copy it was made from
/// and the page anybody else can check it against.
#[test]
fn docs_answers_from_the_local_copy_and_names_where_it_came_from() {
    let rook = Rook::new();

    let empty = rook.ok(&["docs", "ls"]);
    assert!(empty.contains("nothing kept yet"), "{empty}");
    assert!(empty.contains("rook docs add"), "and how to gather one: {empty}");

    // Seeded rather than fetched: a test that reaches the internet tests the
    // internet. The store is closed again before the binary opens it.
    {
        let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
        let (skills, _) = rook_skills::SkillIndex::discover(&[]);
        let engine = rook_core::Rook::from_parts(
            store,
            rook_core::Config::default(),
            rook_skills::Environment::bare("linux", "x86_64", "0.1.0"),
            skills,
            rook.workspace.path().to_path_buf(),
        );
        engine
            .keep_docs(&rook_core::docs::DocSet::new(
                "redis",
                rook_core::docs::LATEST,
                vec![rook_core::docs::Page {
                    url: "https://redis.io/docs/persistence".into(),
                    title: "Persistence".into(),
                    text: "Redis persists with an append only file, rewritten in the background \
                           once it grows past a configured size."
                        .into(),
                    ..Default::default()
                }],
            ))
            .unwrap();
    }

    let listed = rook.ok(&["docs", "ls"]);
    assert!(listed.contains("redis latest"), "{listed}");
    assert!(listed.contains("1 page(s)"), "{listed}");

    let shown = rook.ok(&["docs", "show", "redis"]);
    assert!(shown.contains("docs/redis/latest"), "the local copy: {shown}");
    assert!(shown.contains("https://redis.io/docs/persistence"), "and the source: {shown}");

    let asked = rook.ok(&["docs", "show", "redis", "--question", "how does persistence work"]);
    assert!(asked.contains("append only file"), "the passage that answers: {asked}");
    assert!(asked.contains("[from https://redis.io/docs/persistence]"), "with its page: {asked}");

    let dropped = rook.ok(&["docs", "rm", "redis"]);
    assert!(dropped.contains("dropped 1 set"), "{dropped}");
    assert!(rook.ok(&["docs", "ls"]).contains("nothing kept yet"), "and it is gone");
}

/// `/docs redis persistence` is a topic of two words, and reading the second
/// as a version files the gathering under `docs/redis/persistence` — where
/// nothing will look for it again.
#[test]
fn a_two_word_topic_is_not_read_as_a_topic_and_a_version() {
    let rook = Rook::new();
    {
        let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
        let (skills, _) = rook_skills::SkillIndex::discover(&[]);
        let engine = rook_core::Rook::from_parts(
            store,
            rook_core::Config::default(),
            rook_skills::Environment::bare("linux", "x86_64", "0.1.0"),
            skills,
            rook.workspace.path().to_path_buf(),
        );
        for (topic, version) in [("redis persistence", "latest"), ("redis", "6.2")] {
            engine
                .keep_docs(&rook_core::docs::DocSet::new(
                    topic,
                    version,
                    vec![rook_core::docs::Page {
                        url: "https://redis.io/".into(),
                        title: "Redis".into(),
                        text: format!("the {topic} {version} set"),
                        ..Default::default()
                    }],
                ))
                .unwrap();
        }
    }

    // Through `/docs`, where the guess is made: the command takes one line and
    // has to decide which of its words is a version.
    let said = rook.chat("/docs redis persistence\n/docs redis 6.2\n/quit\n");
    assert!(said.contains("docs/redis-persistence/latest"), "two words are the topic: {said}");
    assert!(said.contains("redis 6.2 · kept as docs/redis/6.2"), "a version still names one: {said}");
    // And neither gathered anything: both were already here, which is the
    // whole point of the copy.
    assert!(!said.contains("gathered"), "{said}");
}

#[test]
fn diagnostics_export_works_locally_and_through_the_daemon_without_overwriting_files() {
    let rook = Rook::new();
    let id = rook_store::new_session_id();
    let name = rook_store::format_session_id(id);
    {
        let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
        store
            .create_session(&rook_store::SessionMeta::new(
                id,
                "private-title",
                rook.workspace.path().display().to_string(),
                rook_store::now_unix(),
            ))
            .unwrap();
        store
            .append_event(
                id,
                rook_store::NewEvent::new(
                    rook_store::EventKind::UserMessage,
                    rook_store::Kind::Message,
                    b"private-prompt",
                ),
            )
            .unwrap();
    }
    let local = rook.json(&["session", "diagnostics", &name]);
    assert_eq!(local["session"]["id"], name);
    assert!(!local.to_string().contains("private-prompt"));
    assert!(!local.to_string().contains("private-title"));
    let _daemon = Daemon::start(&rook);
    let out = rook.run(&["session", "diagnostics", &name]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("using the running rookd"));
    let remote: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(remote["session"], local["session"]);
    let destination = rook.home.path().join("support.json");
    let args = ["session", "diagnostics", &name, "--output", destination.to_str().unwrap()];
    assert!(rook.run(&args).status.success());
    let original = std::fs::read(&destination).unwrap();
    assert!(!rook.run(&args).status.success());
    assert_eq!(std::fs::read(&destination).unwrap(), original);
}

#[test]
fn html_export_scopes_and_escapes_history_locally_and_through_the_daemon() {
    let rook = Rook::new();
    let id = rook_store::new_session_id();
    let named = rook_store::format_session_id(id);
    {
        let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
        store
            .create_session(&rook_store::SessionMeta::new(
                id,
                "HTML export",
                rook.workspace.path().display().to_string(),
                rook_store::now_unix(),
            ))
            .unwrap();
        for (kind, label, body) in [
            (rook_store::EventKind::UserMessage, "", "outside-before".to_string()),
            (
                rook_store::EventKind::ToolCall,
                "<script>",
                "<script>alert('bad')</script> & \"quoted\"".to_string(),
            ),
            (rook_store::EventKind::ToolResult, "run_command", format!("{}END", "&".repeat(9000))),
            (rook_store::EventKind::AssistantMessage, "", "outside-after".to_string()),
        ] {
            store
                .append_event(
                    id,
                    rook_store::NewEvent::new(kind, rook_store::Kind::Message, body.as_bytes()).label(label),
                )
                .unwrap();
        }
        for _ in 0..513 {
            store
                .append_event(
                    id,
                    rook_store::NewEvent::new(
                        rook_store::EventKind::AssistantMessage,
                        rook_store::Kind::Message,
                        b"bounded export",
                    ),
                )
                .unwrap();
        }
    }
    let local = rook.home.path().join("local.html");
    let remote = rook.home.path().join("remote.html");
    let export = |path: &std::path::Path| {
        rook.run(&[
            "session",
            "export-html",
            named.as_str(),
            "--from",
            "1",
            "--through",
            "2",
            "--output",
            path.to_str().unwrap(),
        ])
    };
    let result = export(&local);
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    let html = std::fs::read_to_string(&local).unwrap();
    assert!(html.contains("selected events #1–#2"), "{html}");
    assert!(html.contains("&lt;script&gt;alert(&#39;bad&#39;)&lt;/script&gt; &amp; &quot;quoted&quot;"));
    assert!(html.contains("<details><summary>Show tool content</summary>"));
    assert!(html.contains("Body shortened after 8192 bytes"));
    assert!(!html.contains("outside-before") && !html.contains("outside-after"));
    assert!(!export(&local).status.success(), "existing export must not be replaced");
    assert_eq!(std::fs::read_to_string(&local).unwrap(), html);
    let repl_local = rook.home.path().join("REPL local.html");
    let lines = format!("/export-html 1..2 {}\n/quit\n", repl_local.display());
    let result = rook.chat_in_session(&named, &lines);
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert_eq!(std::fs::read_to_string(&repl_local).unwrap(), html);
    let _daemon = Daemon::start(&rook);
    let result = export(&remote);
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert_eq!(std::fs::read_to_string(&remote).unwrap(), html);
    let repl_remote = rook.home.path().join("REPL daemon.html");
    let lines = format!("/export-html 1..2 {}\n/quit\n", repl_remote.display());
    let result = rook.chat_in_session(&named, &lines);
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert_eq!(std::fs::read_to_string(&repl_remote).unwrap(), html);
    let invalid = rook.home.path().join("invalid.html");
    let result = rook.run(&[
        "session",
        "export-html",
        &named,
        "--from",
        "2",
        "--through",
        "1",
        "--output",
        invalid.to_str().unwrap(),
    ]);
    assert!(!result.status.success());
    assert!(!invalid.exists());
    let too_large = rook.home.path().join("too-large.html");
    let result = rook.run(&[
        "session",
        "export-html",
        &named,
        "--from",
        "4",
        "--through",
        "516",
        "--output",
        too_large.to_str().unwrap(),
    ]);
    assert!(!result.status.success());
    assert!(!too_large.exists());
}

#[test]
fn recovery_inspection_and_acknowledgement_reach_the_daemon_and_reject_stale_ids() {
    let rook = Rook::new();
    let id = rook_store::new_session_id();
    let named = rook_store::format_session_id(id);
    {
        let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
        store
            .create_session(&rook_store::SessionMeta::new(
                id,
                "interrupted",
                rook.workspace.path().display().to_string(),
                rook_store::now_unix(),
            ))
            .unwrap();
        let receipt = serde_json::json!({
            "version":1,"session":named,"turn":"old-turn","owner":"dead-process","pid":0,
            "started_at":0,"updated_at":0,"status":"running","task":"write one file","start_seq":0,
            "completed_operations":0,"last_result_seq":null,"background":[],"unknown":[],
            "pending":{"session":named,"id":"operation-one","tool":"write_file","arguments":"{}",
                "started_at":0,"call_seq":0,"may_have_effects":true,"job":null,"registry":null}
        });
        store.kv_set(&format!("execution/{id:032x}"), &serde_json::to_vec(&receipt).unwrap()).unwrap();
        store.flush().unwrap();
    }
    let _daemon = Daemon::start(&rook);
    let receipts = rook.json(&["session", "recovery", &named]);
    assert_eq!(receipts[0]["unknown"][0]["id"], "operation-one");
    let result = rook.run(&[
        "session",
        "recovery",
        &named,
        "--acknowledge",
        "operation-one",
        "--note",
        "inspected the destination",
    ]);
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    let receipts = rook.json(&["session", "recovery", &named]);
    assert!(receipts[0]["unknown"].as_array().unwrap().is_empty());
    let stale =
        rook.run(&["session", "recovery", &named, "--acknowledge", "operation-one", "--note", "old page"]);
    assert!(!stale.status.success());
    assert!(String::from_utf8_lossy(&stale.stderr).contains("refresh"));
}

/// A release check that cannot reach the release API says so and exits
/// non-zero. The point is the wiring as much as the message: a command nobody
/// can reach is a feature nobody has, and "the check passed" is the wrong
/// thing to print when nothing was checked.
#[test]
fn a_release_check_that_cannot_reach_github_says_so_rather_than_saying_up_to_date() {
    let rook = Rook::new();
    // A port nothing is listening on, so the failure is the connection and not
    // a parse of somebody's error page.
    let out = Command::new(env!("CARGO_BIN_EXE_rook"))
        .env("ROOK_HOME", rook.home.path())
        .env("ROOK_LOG", "error")
        .env("ROOK_RELEASE_API", "http://127.0.0.1:1")
        .env("NO_PROXY", "*")
        .env("no_proxy", "*")
        .args(["update", "--check"])
        .output()
        .unwrap();

    assert!(!out.status.success(), "it must not exit 0: {}", String::from_utf8_lossy(&out.stdout));
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("could not reach"), "and it names what it could not reach: {said}");
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("up to date"),
        "and never claims a version it did not read"
    );
}

/// Kill rookd while it waits for a model, then resume the same durable task.
/// The correction is accepted with no worker available to acknowledge it.
#[test]
fn durable_work_and_pending_corrections_survive_a_daemon_restart() {
    durable_recovery(false, false);
}

#[test]
fn a_session_goal_and_pending_correction_survive_a_daemon_restart() {
    durable_recovery(true, false);
}

#[test]
fn a_scheduled_session_resumes_after_daemon_restart_without_a_duplicate_launch() {
    durable_recovery(false, true);
}

fn durable_recovery(in_conversation: bool, scheduled: bool) {
    rook_llm::init_tls();
    use std::io::{Read, Write};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let rook = Rook::new();
    std::fs::write(rook.workspace.path().join("evidence.txt"), "done").unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let release = Arc::new(AtomicBool::new(false));
    let arrived = Arc::new(AtomicBool::new(false));
    let stopped = Arc::new(AtomicBool::new(false));
    struct Stop(Arc<AtomicBool>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let _stop = Stop(stopped.clone());
    let gate = release.clone();
    let ready = arrived.clone();
    std::thread::spawn(move || {
        while !stopped.load(Ordering::SeqCst) {
            let Ok((mut socket, _)) = listener.accept() else {
                std::thread::sleep(std::time::Duration::from_millis(10));
                continue;
            };
            let gate = gate.clone();
            let ready = ready.clone();
            let stopped = stopped.clone();
            std::thread::spawn(move || {
                socket.set_read_timeout(Some(std::time::Duration::from_secs(60))).unwrap();
                let mut bytes = Vec::new();
                let mut chunk = [0; 16384];
                let request = loop {
                    let Ok(n) = socket.read(&mut chunk) else { return };
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&bytes);
                    if let Some((head, body)) = text.split_once("\r\n\r\n") {
                        let length: usize = head
                            .lines()
                            .find_map(|line| {
                                line.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse().ok())
                            })
                            .unwrap_or(0);
                        if body.len() >= length {
                            break serde_json::from_str::<serde_json::Value>(body).unwrap();
                        }
                    }
                };
                ready.store(true, Ordering::SeqCst);
                while !gate.load(Ordering::SeqCst) {
                    if stopped.load(Ordering::SeqCst) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                let messages = request["messages"].as_array().unwrap();
                let classify = messages
                    .iter()
                    .any(|m| m["content"].as_str().is_some_and(|s| s.starts_with("Classify whether")));
                let checking = messages.iter().any(|m| {
                    m["content"].as_str().is_some_and(|s| s.contains("the agent has just finished a turn"))
                });
                let read = messages.iter().any(|m| m["role"] == "tool");
                let content = if classify {
                    r#"{"action":"finish"}"#
                } else if checking {
                    "Evidence read.\nVERDICT: holds"
                } else {
                    "evidence.txt contains done."
                };
                let mut message = serde_json::json!({"role":"assistant", "content":content});
                let finish = if !classify && !read {
                    message["content"] = serde_json::json!("");
                    message["tool_calls"] = serde_json::json!([{"index":0,"id":"read-evidence","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"evidence.txt\"}"}}]);
                    "tool_calls"
                } else {
                    "stop"
                };
                let choice = if request["stream"] == true {
                    serde_json::json!({"index":0,"delta":message,"finish_reason":finish})
                } else {
                    serde_json::json!({"index":0,"message":message,"finish_reason":finish})
                };
                let answer = serde_json::json!({"id":"test","model":"test","choices":[choice],"usage":{"prompt_tokens":10,"completion_tokens":5}}).to_string();
                let (mime, body) = if request["stream"] == true {
                    ("text/event-stream", format!("data: {answer}\n\ndata: [DONE]\n\n"))
                } else {
                    ("application/json", answer)
                };
                let _ = socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes());
            });
        }
    });
    rook.write_config(&format!("[agent]\nmodel = 'local'\ninstall_servers = false\none_script = false\n[models.local]\nmodel = 'test'\napi = 'openai'\nurl = '{endpoint}'\n[work]\nretry_initial_secs = 1\n"));
    let conversation = in_conversation.then(|| {
        let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
        let engine = rook_core::Rook::from_parts(
            store,
            rook_core::Config::default(),
            rook_skills::Environment::bare("linux", "x86_64", "0.1.0"),
            rook_skills::SkillIndex::discover(&[]).0,
            rook.workspace.path().into(),
        );
        let session = engine.start_session("existing conversation").unwrap();
        engine.log(session, rook_store::EventKind::UserMessage, "", "Preserve this conversation").unwrap();
        rook_store::format_session_id(session)
    });
    let daemon = Daemon::start(&rook);
    let schedule = scheduled.then(|| {
        rook.json(&[
            "task",
            "schedule",
            "Inspect evidence.txt and report the contents",
            "--when",
            "every 1d",
            "--timezone",
            "Europe/Moscow",
            "--stance",
            "autonomous",
        ])
    });
    let run =
        if let Some(task) = &schedule {
            let task = rook.json(&["task", "run", task["id"].as_str().unwrap()]);
            let session = task["pending"].as_str().unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                let output = rook.run(&["--json", "task", "show", session]);
                if output.status.success() {
                    break serde_json::from_slice(&output.stdout).unwrap();
                }
                assert!(std::time::Instant::now() < deadline, "scheduled run never started");
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        } else if let Some(session) = &conversation {
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                reqwest::Client::new().post(format!("{}/api/work", daemon.address)).json(&serde_json::json!({
                "goal":"Inspect evidence.txt and report the contents", "autonomous":false,
                "conversation":{"session":session, "model":null, "effort":"high", "stance":"autonomous"},
                "max_iterations":0, "max_tokens":0, "max_seconds":0,
            })).send().await.unwrap().error_for_status().unwrap().json::<serde_json::Value>().await.unwrap()
            })
        } else {
            rook.json(&["task", "start", "Inspect evidence.txt and report the contents", "--yes"])
        };
    let id = run["id"].as_str().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !arrived.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(arrived.load(Ordering::SeqCst), "must kill while a real model request is in flight");
    let queued = rook.json(&[
        "task",
        "steer",
        id,
        "Include the filename",
        "--message-id",
        "correction-one",
        "--wait-secs",
        "0",
    ]);
    assert!(queued["applied_at"].is_null(), "a busy model has not heard it yet: {queued}");
    let old = rook.json(&["task", "show", id]);
    drop(daemon);
    std::fs::remove_file(rook.home.path().join("rookd.addr")).unwrap();
    let _daemon = Daemon::start(&rook);
    let saved = rook.json(&["task", "show", id]);
    assert_eq!(saved["instructions"][0]["id"], "correction-one");
    assert_eq!(saved["session"], old["session"], "recovery uses its existing journal");
    if let Some(session) = &conversation {
        assert_eq!(saved["id"], *session);
        assert_eq!(saved["session"], *session, "the restarted daemon continues the original conversation");
    }
    release.store(true, Ordering::SeqCst);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    if let Some(task) = &schedule {
        let schedule_id = task["id"].as_str().unwrap();
        loop {
            let current = rook.json(&["task", "show", schedule_id]);
            assert_eq!(current["history"].as_array().unwrap().len(), 1, "restart must not launch twice");
            assert_eq!(current["history"][0]["session"], id);
            if current["history"][0]["status"] == "Completed" {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "scheduled session did not finish: {current}");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        rook.ok(&["task", "disable", schedule_id]);
        rook.ok(&["task", "delete", schedule_id]);
        assert_eq!(rook.json(&["task", "list"]), serde_json::json!([]));
        return;
    }
    let finished = loop {
        let current = rook.json(&["task", "show", id]);
        if current["status"] == "completed" {
            break current;
        }
        assert!(std::time::Instant::now() < deadline, "did not finish: {current}");
        assert_ne!(current["status"], "blocked", "{current}");
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    assert!(finished["instructions"][0]["applied_at"].is_u64(), "{finished}");
    assert!(finished["tokens"].as_u64().unwrap() > 0);
    assert!(finished["verification"].as_str().unwrap().contains("holds"));
    let duplicate = rook.json(&[
        "task",
        "steer",
        id,
        "Include the filename",
        "--message-id",
        "correction-one",
        "--wait-secs",
        "0",
    ]);
    assert_eq!(
        duplicate, finished["instructions"][0],
        "retrying a receipt must not create a second instruction"
    );
    rook.ok(&["task", "forget", id]);
    assert_eq!(rook.json(&["task", "list"]), serde_json::json!([]));
}

#[test]
fn config_edit_requires_a_terminal_and_never_creates_a_file_from_a_pipe() {
    let rook = Rook::new();
    let out = rook.run(&["config", "edit"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("interactive terminal"));
    assert!(!rook.home.path().join("config.toml").exists());
    let out = rook.run(&["config", "edit", "--json"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("omit --json"));
}

#[test]
fn effort_mapping_follows_the_model_selected_in_this_session() {
    let rook = Rook::new();
    rook.write_config(
        r#"
[agent]
model = "plain"
install_servers = false
[models.plain]
api = "openai"
url = "http://127.0.0.1:1/v1"
model = "local-model"
[models.reasoner]
api = "openai"
url = "http://127.0.0.1:1/v1"
model = "gpt-5"
"#,
    );
    let out = rook.chat("/effort max\n/model reasoner\n/effort\n/quit\n");
    let before = out.find("not sent: no effort mapping for this model").unwrap_or_else(|| panic!("{out}"));
    let after = out.find("sent reasoning_effort=high").unwrap_or_else(|| panic!("{out}"));
    assert!(before < after, "the session's choice changes the mapping: {out}");
    assert!(out.contains("requested max"), "the preference is preserved: {out}");
}

#[test]
fn models_enforces_configured_catalog_limits_and_exposes_capabilities_without_the_store() {
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};
    let _one = one_at_a_time();
    let rook = Rook::new();
    let _store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut seen = 0;
        while seen < 2 && Instant::now() < deadline {
            let Ok((mut socket, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            };
            socket.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            socket.set_write_timeout(Some(Duration::from_secs(10))).unwrap();
            let mut bytes = Vec::new();
            while !bytes.contains(&b'\n') {
                let mut chunk = [0; 4096];
                let n = socket.read(&mut chunk).unwrap();
                assert!(n > 0 && bytes.len() + n <= 8192, "bounded mock request line");
                bytes.extend_from_slice(&chunk[..n]);
            }
            assert!(String::from_utf8_lossy(&bytes).starts_with("GET /v1/models "));
            let body =
                r#"{"data":[{"id":"one","supported_parameters":["tools","reasoning_effort"]},{"id":"two"}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).unwrap();
            seen += 1;
        }
        seen
    });
    for limit in [1, 2] {
        rook.write_config(&format!("[agent]\nmodel='local'\n[models.local]\napi='openai'\nurl='http://{address}/v1'\nmodel='one'\n[model_catalog]\nmax_models={limit}\nmax_bytes=2048\ntimeout_secs=10\n"));
        let out = rook.run(&["models", "--source", "local", "--json"]);
        if limit == 1 {
            assert!(!out.status.success());
            assert!(String::from_utf8_lossy(&out.stderr).contains("exceeds 1 models"));
        } else {
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            let models: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
            assert_eq!(models.as_array().unwrap().len(), 2);
            assert_eq!(models[0]["capabilities"]["tools"], true);
            assert_eq!(models[0]["capabilities"]["reasoning"], true);
            assert!(models[1]["capabilities"]["tools"].is_null());
        }
    }
    assert_eq!(server.join().unwrap(), 2);
}

#[test]
fn models_offline_reports_configuration_without_running_credential_helpers() {
    let _one = one_at_a_time();
    let rook = Rook::new();
    rook.write_config("[agent]\nmodel='local'\n[models.local]\napi='openai'\nurl='http://127.0.0.1:1/v1'\nmodel='private-model'\nkey='secret:missing'\ncontext_window=8192\n");
    let _store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
    let value = rook.json(&["models", "--offline", "--metadata"]);
    assert_eq!(value["origin"], "configuration");
    assert_eq!(value["credentials_resolved"], false);
    assert_eq!(value["models"][0]["id"], "private-model");
    assert_eq!(value["models"][0]["context_window"], 8192);
    assert!(value["models"][0]["capabilities"]["tools"].is_null());
    assert!(value["observed_at"].is_null());
    let legacy_shape = rook.json(&["models", "--offline"]);
    assert!(legacy_shape.is_array());
    for flags in [
        vec!["models", "--offline", "--refresh"],
        vec!["models", "--offline", "--recheck"],
        vec!["models", "--metadata"],
    ] {
        assert!(!rook.run(&flags).status.success(), "invalid flags: {flags:?}");
    }
    assert!(!rook.home.path().join("cache/models-v1.json").exists());
}

#[test]
fn models_offline_allows_a_cloud_spec_without_a_key_but_online_still_requires_one() {
    let _one = one_at_a_time();
    let rook = Rook::new();
    rook.write_config("[agent]\nmodel='anthropic/claude-catalog-test'\n");
    for offline in [true, false] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rook"));
        command
            .env("ROOK_HOME", rook.home.path())
            .env("ROOK_LOG", "error")
            .env_remove("ANTHROPIC_API_KEY")
            .env("ANTHROPIC_BASE_URL", "http://127.0.0.1:1")
            .arg("--workspace")
            .arg(rook.workspace.path())
            .args(["models", "--json", "--metadata"]);
        if offline {
            command.arg("--offline");
        }
        let out = command.output().unwrap();
        if offline {
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            let result: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
            assert_eq!(result["origin"], "configuration");
            assert_eq!(result["models"][0]["id"], "claude-catalog-test");
            assert_eq!(result["credentials_resolved"], false);
        } else {
            assert!(!out.status.success());
            assert!(String::from_utf8_lossy(&out.stderr).contains("ANTHROPIC_API_KEY is not set"));
        }
    }
}

#[test]
fn managed_mcp_commands_reload_one_connection_in_the_running_daemon() {
    let rook = Rook::new();
    rook.write_config("[[mcp]]\nname='local'\nenabled=false\n");
    let _daemon = Daemon::start(&rook);
    let initial = rook.json(&["mcp", "status"]);
    assert_eq!(initial["servers"][0]["state"], "disabled");
    let binary = serde_json::to_string(env!("CARGO_BIN_EXE_rook")).unwrap();
    let args = serde_json::json!(["--workspace", rook.workspace.path(), "mcp", "serve", "--yes"]);
    rook.write_config(&format!("[[mcp]]\nname='local'\ncommand={binary}\nargs={args}\n"));
    let connected = rook.json(&["mcp", "reconnect", "local"]);
    assert_eq!(connected["servers"][0]["state"], "connected");
    assert!(connected["servers"][0]["tools"].as_u64().unwrap() > 0);
    let generation = connected["servers"][0]["generation"].clone();
    rook.write_config("[[mcp]]\nname='local'\ncommand='/missing/mcp-command'\n");
    let failed = rook.run(&["mcp", "reconnect", "local"]);
    assert!(!failed.status.success());
    let kept = rook.json(&["mcp", "status"]);
    assert_eq!(kept["servers"][0]["state"], "connected");
    assert_eq!(kept["servers"][0]["generation"], generation);
    assert!(kept["servers"][0]["error"].is_string());
    rook.write_config("[[mcp]]\nname='local'\nenabled=false\n");
    let disabled = rook.json(&["mcp", "reconnect", "local"]);
    assert_eq!(disabled["servers"][0]["state"], "disabled");
    assert_eq!(disabled["servers"][0]["tools"], 0);
}

#[path = "scenarios/followups.rs"]
mod followups;
