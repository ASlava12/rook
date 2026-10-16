use async_trait::async_trait;
use rook_core::{
    Config, Rook, Vault,
    agent::AgentLoop,
    review::{CommentRequest, Selection, Side, Target},
};
use rook_llm::{Message, Provider, Request, Response, StopReason, ToolCall};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

static TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn git(root: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .current_dir(root)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "user.name=Review Fixture",
            "-c",
            "user.email=review@example.invalid",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap().trim().into()
}
fn open(root: &Path, config: Config) -> Rook {
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    if !workspace.join(".git").exists() {
        git(&workspace, &["init", "-q"]);
        std::fs::write(workspace.join("sum.txt"), "sum(a, b)\nreturn a + b\n").unwrap();
        git(&workspace, &["add", "sum.txt"]);
        git(&workspace, &["commit", "-qm", "baseline"]);
    }
    Rook::from_parts(
        rook_store::Store::open(root.join("store")).unwrap(),
        config,
        rook_skills::Environment::bare("linux", "x86_64", "0.1.0"),
        rook_skills::SkillIndex::default(),
        workspace,
    )
}
fn comment(source: &str, text: &str) -> CommentRequest {
    CommentRequest {
        source: source.into(),
        path: "sum.txt".into(),
        side: Side::After,
        line: 2,
        text: text.into(),
    }
}

#[tokio::test]
async fn working_review_freezes_edits_additions_deletions_and_keeps_comments_on_the_original_lines() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), Config::default());
    std::fs::write(rook.workspace.join("removed.txt"), "old\n").unwrap();
    git(&rook.workspace, &["add", "removed.txt"]);
    git(&rook.workspace, &["commit", "-qm", "second baseline"]);
    std::fs::write(rook.workspace.join("sum.txt"), "sum(a, b)\nreturn a - b\n").unwrap();
    std::fs::write(rook.workspace.join("new.txt"), "new\n").unwrap();
    std::fs::remove_file(rook.workspace.join("removed.txt")).unwrap();
    let session = rook.start_session("review").unwrap();
    let vault = Vault::empty();
    let review = rook.review_capture(session, Selection::default(), &vault).await.unwrap();
    assert_eq!(review.snapshot.files.len(), 3);
    assert!(review.snapshot.files.iter().find(|file| file.path == "new.txt").unwrap().before.is_none());
    assert!(review.snapshot.files.iter().find(|file| file.path == "removed.txt").unwrap().after.is_none());
    std::fs::write(rook.workspace.join("sum.txt"), "different\ncode\nnow\n").unwrap();
    let page = rook.review_source(session, &review.id, "sum.txt", Side::After, 0, 4096).unwrap();
    assert_eq!(page.text, "sum(a, b)\nreturn a - b\n");
    let saved = rook
        .review_comment(session, &review.id, comment(&review.source, "Restore addition"), &vault)
        .unwrap();
    assert_eq!(saved.comments[0].source, review.source);
    assert_eq!(saved.comments[0].line, 2);
    assert!(
        rook.review_comment(session, &review.id, comment("wrong-source", "wrong target"), &vault).is_err()
    );
    let mut invalid = comment(&review.source, "wrong line");
    invalid.line = 99;
    assert!(rook.review_comment(session, &review.id, invalid, &vault).is_err());
    assert!(rook.review_feedback(session, &review.id, &review.source).unwrap().contains("Restore addition"));
    let id = review.id.clone();
    let source = review.source.clone();
    drop(rook);
    let reopened = open(root.path(), Config::default());
    let saved = reopened.review_get(session, &id).unwrap();
    assert_eq!(saved.source, source);
    assert_eq!(saved.comments.len(), 1);
    reopened.store.delete_session(session).unwrap();
    assert!(reopened.store.kv_get(&format!("review/{session:032x}")).unwrap().is_none());
}

#[tokio::test]
async fn commit_and_base_reviews_resolve_immutable_git_sources_and_do_not_read_later_working_edits() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), Config::default());
    let session = rook.start_session("review revisions").unwrap();
    let baseline = git(&rook.workspace, &["rev-parse", "HEAD"]);
    let first = rook
        .review_capture(
            session,
            Selection { target: Target::Commit, revision: Some(baseline.clone()), paths: Vec::new() },
            &Vault::empty(),
        )
        .await
        .unwrap();
    assert!(first.snapshot.base.is_none());
    assert!(first.snapshot.files[0].before.is_none());
    std::fs::write(rook.workspace.join("sum.txt"), "sum(a, b)\nreturn a - b\n").unwrap();
    git(&rook.workspace, &["add", "sum.txt"]);
    git(&rook.workspace, &["commit", "-qm", "regression"]);
    let commit = git(&rook.workspace, &["rev-parse", "HEAD"]);
    std::fs::write(rook.workspace.join("sum.txt"), "uncommitted later source\n").unwrap();
    for target in [Target::Commit, Target::Base] {
        let revision = if matches!(target, Target::Commit) { commit.clone() } else { baseline.clone() };
        let snapshot = rook
            .review_capture(
                session,
                Selection { target, revision: Some(revision), paths: Vec::new() },
                &Vault::empty(),
            )
            .await
            .unwrap();
        assert_eq!(snapshot.snapshot.head.as_deref(), Some(commit.as_str()));
        assert_eq!(snapshot.snapshot.files[0].after.as_deref(), Some("sum(a, b)\nreturn a - b\n"));
    }
}

#[tokio::test]
async fn review_limits_are_reached_before_publishing_and_paths_cannot_escape_or_expand_as_git_patterns() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.review.file_bytes = 1024;
    let rook = open(root.path(), config);
    let session = rook.start_session("bounded review").unwrap();
    std::fs::write(rook.workspace.join("sum.txt"), "x".repeat(1025)).unwrap();
    assert!(
        rook.review_capture(session, Selection::default(), &Vault::empty())
            .await
            .unwrap_err()
            .to_string()
            .contains("review.file_bytes")
    );
    let listed =
        rook.review_request(session, rook_core::review::Request::List, &Vault::empty()).await.unwrap();
    assert_eq!(listed["items"], json!([]));
    for path in ["../escape", "/absolute", ":(glob)*"] {
        let selected = Selection { paths: vec![path.into()], ..Default::default() };
        let result = rook.review_capture(session, selected, &Vault::empty()).await;
        if path.starts_with(':') {
            assert!(result.unwrap().snapshot.files.is_empty());
        } else {
            assert!(result.is_err());
        }
    }
}

struct Reviewer {
    hold: bool,
    wrapped_line: Option<usize>,
    narrative: bool,
    bare_prefix: bool,
    ambiguous: bool,
    oversized_title: bool,
    ready: tokio::sync::Notify,
    root: Mutex<bool>,
    child: Mutex<usize>,
    requests: Mutex<Vec<Request>>,
}
#[async_trait]
impl Provider for Reviewer {
    fn id(&self) -> &str {
        "fixture/reviewer"
    }
    fn context_window(&self) -> usize {
        32768
    }
    async fn complete(&self, request: Request) -> rook_llm::Result<Response> {
        let message = if request
            .messages
            .first()
            .is_some_and(|message| message.content.starts_with("Classify whether an assistant"))
        {
            Message::assistant(r#"{"action":"finish"}"#)
        } else if request
            .messages
            .first()
            .is_some_and(|message| message.content.starts_with("Repair only the JSON representation"))
        {
            let line = self.wrapped_line.unwrap_or(2);
            Message::assistant(json!({"findings":[{"severity":"high","path":"sum.txt","side":"after","line":line,"end_line":line,"title":"Subtraction replaced addition","body":"For a=3 and b=2 the new expression yields 1 instead of the expected sum 5."}]}).to_string())
        } else if request.tools.len() == 1 && request.tools[0].name == "review_read" {
            if self.hold {
                self.ready.notify_one();
                return std::future::pending().await;
            }
            let mut step = self.child.lock().unwrap();
            let at = *step;
            *step += 1;
            match at {
                0 => call("run_command", json!({"command":"echo damaged > sum.txt"})),
                1 => call("write_file", json!({"path":"sum.txt","content":"damaged"})),
                2 => call("review_read", json!({"path":"sum.txt"})),
                _ => {
                    let line = self.wrapped_line.unwrap_or(2);
                    let title = if self.oversized_title {
                        "x".repeat(257)
                    } else {
                        "Subtraction replaced addition".into()
                    };
                    let reply = json!({"findings":[{"severity":"high","path":"sum.txt","side":"after","line":line,"end_line":line,"title":title,"body":"For a=3 and b=2 the new expression yields 1 instead of the expected sum 5."}]}).to_string();
                    Message::assistant(if self.ambiguous {
                        format!("```json\n{reply}\n```\n```json\n{{\"findings\":[]}}\n```")
                    } else if self.bare_prefix {
                        format!("Reading complete. Reviewed sum.txt; average([]) is an example.\n\n{reply}")
                    } else if self.narrative {
                        format!(
                            "Finding in sum.txt, after line {line}, severity high: Subtraction replaced addition. For a=3 and b=2 the new expression yields 1 instead of the expected sum 5."
                        )
                    } else if self.wrapped_line.is_some() {
                        format!("Confirmed the regression. Here are my findings:\n\n```json\n{reply}\n```")
                    } else {
                        reply
                    })
                }
            }
        } else if request.tools.iter().any(|tool| tool.name == "review_changes") {
            let mut called = self.root.lock().unwrap();
            if !*called {
                *called = true;
                call("review_changes", json!({"target":"working","paths":["sum.txt"]}))
            } else {
                Message::assistant("Review findings recorded.")
            }
        } else {
            Message::assistant("VERDICT: unproven")
        };
        self.requests.lock().unwrap().push(request);
        let stop_reason =
            if message.tool_calls.is_empty() { StopReason::EndTurn } else { StopReason::ToolUse };
        Ok(Response { message, stop_reason, usage: Default::default(), model: self.id().into() })
    }
}
fn call(name: &str, arguments: Value) -> Message {
    Message {
        tool_calls: vec![ToolCall { id: format!("{name}-call"), name: name.into(), arguments }],
        ..Message::assistant("")
    }
}

#[tokio::test]
async fn independent_review_has_only_frozen_reads_and_cannot_write_even_through_shell_or_forged_tools() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.agent.max_steps = 6;
    config.agent.max_turn_tokens = 64000;
    config.agent.plan_first = false;
    config.memory.enabled = false;
    config.web.enabled = false;
    let rook = open(root.path(), config);
    let session = rook.start_session("review regression").unwrap();
    let changed = "sum(a, b)\nreturn a - b\n";
    std::fs::write(rook.workspace.join("sum.txt"), changed).unwrap();
    let provider = Arc::new(Reviewer {
        hold: false,
        wrapped_line: None,
        narrative: false,
        bare_prefix: false,
        ambiguous: false,
        oversized_title: false,
        ready: tokio::sync::Notify::new(),
        root: Mutex::new(false),
        child: Mutex::new(0),
        requests: Mutex::new(Vec::new()),
    });
    let mut agent = AgentLoop::new(&rook, provider.clone(), session);
    let outcome = agent.run("Review changes without repairing code").await.unwrap();
    assert!(outcome.tools_called.contains(&"review_changes".into()));
    let items =
        rook.review_request(session, rook_core::review::Request::List, &Vault::empty()).await.unwrap();
    let review = &items["items"][0];
    assert_eq!(review["state"], "completed", "{review}");
    assert_eq!(review["findings"][0]["line"], 2);
    assert_eq!(std::fs::read_to_string(rook.workspace.join("sum.txt")).unwrap(), changed);
    let child = rook_store::parse_session_id(review["reviewer"].as_str().unwrap()).unwrap();
    assert_eq!(rook.store.get_session(child).unwrap().unwrap().parent, Some(session));
    let events = rook.transcript(child, 0, 100, 4096).unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == "tool-result" && event.body.contains("review context can only read"))
    );
    assert!(
        provider
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.tools.len() == 1 && request.tools[0].name == "review_read")
    );
}

#[tokio::test]
async fn concurrent_comments_are_retained_and_a_corrupted_snapshot_is_refused() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), Config::default());
    std::fs::write(rook.workspace.join("sum.txt"), "sum(a, b)\nreturn a - b\n").unwrap();
    let session = rook.start_session("concurrent comments").unwrap();
    let vault = Vault::empty();
    let review = rook.review_capture(session, Selection::default(), &vault).await.unwrap();
    std::thread::scope(|scope| {
        for index in 0..8 {
            let rook = &rook;
            let review = &review;
            let vault = &vault;
            scope.spawn(move || {
                rook.review_comment(
                    session,
                    &review.id,
                    comment(&review.source, &format!("correction {index}")),
                    vault,
                )
                .unwrap()
            });
        }
    });
    let saved = rook.review_get(session, &review.id).unwrap();
    assert_eq!(saved.comments.len(), 8, "transactional append must not lose concurrent comments");
    let key = format!("review/{session:032x}");
    let mut stored: Value = serde_json::from_slice(&rook.store.kv_get(&key).unwrap().unwrap()).unwrap();
    stored["items"][0]["snapshot"]["files"][0]["after"] = json!("different retained source\n");
    rook.store.kv_set(&key, &serde_json::to_vec(&stored).unwrap()).unwrap();
    assert!(rook.review_get(session, &review.id).unwrap_err().to_string().contains("checksum"));
}

#[tokio::test]
async fn redacting_multiline_credentials_preserves_saved_line_anchors() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), Config::default());
    let private = "private-first-line\nprivate-second-line";
    std::fs::write(rook.workspace.join("sum.txt"), format!("{private}\nreturn a - b\n")).unwrap();
    let session = rook.start_session("redacted source").unwrap();
    let vault = Vault::empty();
    vault.also_hide(private);
    let review = rook.review_capture(session, Selection::default(), &vault).await.unwrap();
    let after = review.snapshot.files[0].after.as_ref().unwrap();
    assert_eq!(after.lines().count(), 3);
    assert!(!after.contains("private-first-line"));
    assert!(!after.contains("private-second-line"));
    let mut request = comment(&review.source, "line remains source-bound");
    request.line = 3;
    rook.review_comment(session, &review.id, request, &vault).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn mode_only_changes_are_reviewable_and_symlinks_and_fifos_cannot_be_source_files() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), Config::default());
    let file = rook.workspace.join("sum.txt");
    let session = rook.start_session("source types").unwrap();
    git(&rook.workspace, &["config", "core.filemode", "true"]);
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
    git(&rook.workspace, &["add", "sum.txt"]);
    git(&rook.workspace, &["commit", "-qm", "make executable"]);
    let review = rook
        .review_capture(
            session,
            Selection {
                target: Target::Commit,
                revision: Some("HEAD".into()),
                paths: vec!["sum.txt".into()],
            },
            &Vault::empty(),
        )
        .await
        .unwrap();
    assert_eq!(review.snapshot.files.len(), 1, "mode changes must not be silently dropped");
    assert_eq!(review.snapshot.files[0].before, review.snapshot.files[0].after);
    assert!(review.snapshot.files[0].diff.contains(":100644 100755"));
    std::fs::remove_file(&file).unwrap();
    std::fs::write(rook.workspace.join("target.txt"), "unrelated source\n").unwrap();
    symlink("target.txt", &file).unwrap();
    assert!(rook.review_capture(session, Selection::default(), &Vault::empty()).await.is_err());
    std::fs::remove_file(&file).unwrap();
    assert!(std::process::Command::new("mkfifo").arg(&file).status().unwrap().success());
    let vault = Vault::empty();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        rook.review_capture(session, Selection::default(), &vault),
    )
    .await;
    assert!(result.expect("FIFO admission must not wait for a writer").is_err());
}

#[tokio::test]
async fn cancellation_marks_a_started_review_interrupted_and_retains_its_source() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), Config::default());
    std::fs::write(rook.workspace.join("sum.txt"), "sum(a, b)\nreturn a - b\n").unwrap();
    let session = rook.start_session("cancel review").unwrap();
    let provider = Arc::new(Reviewer {
        hold: true,
        wrapped_line: None,
        narrative: false,
        bare_prefix: false,
        ambiguous: false,
        oversized_title: false,
        ready: tokio::sync::Notify::new(),
        root: Mutex::new(false),
        child: Mutex::new(0),
        requests: Mutex::new(Vec::new()),
    });
    let mut agent = AgentLoop::new(&rook, provider.clone(), session);
    {
        let running = agent.run("Review the changes");
        tokio::pin!(running);
        tokio::select! {
            result = &mut running => panic!("review should be held, got {result:?}"),
            _ = provider.ready.notified() => {},
            _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => panic!("reviewer was not reached"),
        }
        let before =
            rook.review_request(session, rook_core::review::Request::List, &Vault::empty()).await.unwrap();
        assert_eq!(
            before["items"][0]["state"], "started",
            "the fixture must cancel an actual admitted reviewer"
        );
    }
    let after =
        rook.review_request(session, rook_core::review::Request::List, &Vault::empty()).await.unwrap();
    assert_eq!(after["items"][0]["state"], "interrupted");
    let id = after["items"][0]["id"].as_str().unwrap();
    assert_eq!(
        rook.review_get(session, id).unwrap().snapshot.files[0].after.as_deref(),
        Some("sum(a, b)\nreturn a - b\n")
    );
}

#[tokio::test]
async fn redaction_expansion_past_file_limits_does_not_publish_an_unreadable_review() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.review.file_bytes = 1024;
    let rook = open(root.path(), config);
    let session = rook.start_session("redaction bound").unwrap();
    let source = "abcdef".repeat(170);
    assert!(source.len() <= 1024);
    let vault = Vault::empty();
    vault.also_hide("abcdef");
    assert!(vault.redact(&source).len() > 1024, "the redacted representation must actually exceed the cap");
    std::fs::write(rook.workspace.join("sum.txt"), source).unwrap();
    assert!(rook.review_capture(session, Selection::default(), &vault).await.is_err());
    let listed = rook.review_request(session, rook_core::review::Request::List, &vault).await.unwrap();
    assert_eq!(listed["items"], json!([]), "invalid snapshots must not be committed before validation");
}

#[tokio::test]
async fn wrapped_and_narrative_reviewer_findings_complete_only_with_valid_retained_line_anchors() {
    let _serial = TESTS.lock().await;
    for (line, narrative, ambiguous, oversized_title, bare_prefix) in [
        (2, false, false, false, false),
        (2, false, false, false, true),
        (2, true, false, false, false),
        (99, false, false, false, false),
        (2, false, true, false, false),
        (2, false, false, true, false),
    ] {
        let root = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.agent.max_steps = 6;
        config.agent.plan_first = false;
        config.memory.enabled = false;
        config.web.enabled = false;
        let rook = open(root.path(), config);
        let changed = "sum(a, b)\nreturn a - b\n";
        std::fs::write(rook.workspace.join("sum.txt"), changed).unwrap();
        let session = rook.start_session("fenced review").unwrap();
        let provider = Arc::new(Reviewer {
            hold: false,
            wrapped_line: Some(line),
            narrative,
            bare_prefix,
            ambiguous,
            oversized_title,
            ready: tokio::sync::Notify::new(),
            root: Mutex::new(false),
            child: Mutex::new(0),
            requests: Mutex::new(Vec::new()),
        });
        AgentLoop::new(&rook, provider.clone(), session).run("Review changes").await.unwrap();
        {
            let requests = provider.requests.lock().unwrap();
            let repairs = requests
                .iter()
                .filter(|request| {
                    request.messages.first().is_some_and(|message| {
                        message.content.starts_with("Repair only the JSON representation")
                    })
                })
                .collect::<Vec<_>>();
            assert_eq!(repairs.len(), usize::from(narrative));
            assert!(repairs.iter().all(|request| request.tools.is_empty()));
        }
        let items =
            rook.review_request(session, rook_core::review::Request::List, &Vault::empty()).await.unwrap();
        let saved = &items["items"][0];
        assert_eq!(
            saved["state"],
            if line == 2 && !ambiguous && !oversized_title { "completed" } else { "failed" },
            "{saved}"
        );
        if line == 2 && !ambiguous && !oversized_title {
            assert_eq!(saved["findings"][0]["line"], 2);
            assert_eq!(saved["findings"][0]["path"], "sum.txt");
        } else {
            assert_eq!(saved["findings"], json!([]));
        }
        assert_eq!(std::fs::read_to_string(rook.workspace.join("sum.txt")).unwrap(), changed);
    }
}

#[tokio::test]
async fn diff_pages_keep_the_frozen_source_identity_after_live_files_change() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), Config::default());
    let session = rook.start_session("saved diff").unwrap();
    let vault = Vault::empty();
    std::fs::write(rook.workspace.join("sum.txt"), "changed saved line\n").unwrap();
    let review = rook.review_capture(session, Selection::default(), &vault).await.unwrap();
    let request =
        || rook_core::review::Request::Diff { review: review.id.clone(), path: "sum.txt".into(), offset: 0 };
    let before = rook.review_request(session, request(), &vault).await.unwrap();
    std::fs::write(rook.workspace.join("sum.txt"), "unrelated newer workspace\n").unwrap();
    assert_eq!(before, rook.review_request(session, request(), &vault).await.unwrap());
    assert_eq!(before["source"], review.source);
    assert!(before["diff"].as_str().unwrap().contains("changed saved line"));
    let foreign = rook.start_session("foreign").unwrap();
    assert!(rook.review_request(foreign, request(), &vault).await.is_err());
}

#[tokio::test]
async fn binary_changes_keep_original_hashes_without_inventing_text() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), Config::default());
    let before = [0u8, 255, 1, 2];
    std::fs::write(rook.workspace.join("data.bin"), before).unwrap();
    git(&rook.workspace, &["add", "data.bin"]);
    git(&rook.workspace, &["commit", "-qm", "binary baseline"]);
    let after = [0u8, 254, 9];
    std::fs::write(rook.workspace.join("data.bin"), after).unwrap();
    let owner = rook.start_session("binary changes").unwrap();
    let captured = rook
        .review_capture(
            owner,
            Selection { paths: vec!["data.bin".into()], ..Default::default() },
            &Vault::empty(),
        )
        .await
        .unwrap();
    let file = &captured.snapshot.files[0];
    assert!(file.binary());
    assert!(file.before.is_none() && file.after.is_none());
    use sha2::Digest;
    assert_eq!(file.before_hash.as_deref(), Some(hex::encode(sha2::Sha256::digest(before)).as_str()));
    assert_eq!(file.after_hash.as_deref(), Some(hex::encode(sha2::Sha256::digest(after)).as_str()));
    assert!(
        file.diff.contains("Binary content omitted") && file.diff.contains("not been independently reviewed")
    );
    assert!(rook.review_source(owner, &captured.id, "data.bin", Side::After, 0, 1024).is_err());
    let result = rook.review_get(owner, &captured.id).unwrap();
    assert!(result.snapshot.files[0].binary());
}
