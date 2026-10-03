use super::*;
use serde_json::{Value, json};
fn git(root: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn actual_cli_and_daemon_restore_only_reviewed_missing_checkout_files_and_keep_durable_attribution() {
    rook_llm::init_tls();
    for shared in [false, true] {
        let rook = Rook::new();
        rook.write_config("[agent]\ninstall_servers=false\n");
        git(rook.workspace.path(), &["init", "-q"]);
        git(rook.workspace.path(), &["add", "."]);
        git(rook.workspace.path(), &["commit", "-qm", "baseline"]);
        let (parent, child, checkout, admin, index) = {
            let engine = rook_core::Rook::from_parts(
                rook_store::Store::open(rook.home.path().join("store")).unwrap(),
                rook_core::Config::default(),
                rook_skills::Environment::bare("test", "test", "0.10.0"),
                rook_skills::SkillIndex::default(),
                rook.workspace.path().to_path_buf(),
            );
            let parent = engine.start_session("parent").unwrap();
            let child = engine.fork_for_subtask(parent, "child").unwrap();
            let common = PathBuf::from(git(
                rook.workspace.path(),
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            ));
            let checkout = common.join("rook-worktrees").join(rook_store::format_session_id(child));
            let base = git(rook.workspace.path(), &["rev-parse", "HEAD"]);
            engine.store.kv_set(&format!("worktree/{child:032x}"), json!({"path":checkout,"repository":rook.workspace.path().canonicalize().unwrap(),"base":base,"finished":false,"removed":false}).to_string().as_bytes()).unwrap();
            git(rook.workspace.path(), &["worktree", "add", "--detach", checkout.to_str().unwrap(), &base]);
            engine.move_session(child, &checkout).unwrap();
            std::fs::write(checkout.join("staged.bin"), [0, 255, 42]).unwrap();
            git(&checkout, &["add", "staged.bin"]);
            let marker = std::fs::read_to_string(checkout.join(".git")).unwrap();
            let admin = PathBuf::from(marker.trim().strip_prefix("gitdir: ").unwrap());
            let index = std::fs::read(admin.join("index")).unwrap();
            engine.store.flush().unwrap();
            (
                rook_store::format_session_id(parent),
                rook_store::format_session_id(child),
                checkout,
                admin,
                index,
            )
        };
        assert!(
            checkout
                .canonicalize()
                .unwrap()
                .starts_with(rook.workspace.path().canonicalize().unwrap().join(".git/rook-worktrees"))
        );
        std::fs::remove_dir_all(&checkout).unwrap();
        let daemon = shared.then(|| Daemon::start(&rook));
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let client = reqwest::Client::new();
        let path = format!("/api/sessions/{parent}/worktrees/{child}");
        let inspect = || -> Value {
            if let Some(daemon) = &daemon {
                runtime.block_on(async {
                    client
                        .get(format!("{}{path}", daemon.address))
                        .send()
                        .await
                        .unwrap()
                        .error_for_status()
                        .unwrap()
                        .json()
                        .await
                        .unwrap()
                })
            } else {
                rook.json(&["session", "worktree", &parent, &child])
            }
        };
        let apply = |token: &str| -> bool {
            if let Some(daemon) = &daemon {
                runtime.block_on(async {
                    let response = client
                        .post(format!("{}{path}", daemon.address))
                        .json(&json!({"review_token":token}))
                        .send()
                        .await
                        .unwrap();
                    let success = response.status().is_success();
                    if !success {
                        eprintln!("daemon restore refused: {}", response.text().await.unwrap());
                    }
                    success
                })
            } else {
                let out = rook.run(&["session", "worktree", &parent, &child, "--restore", token]);
                if !out.status.success() {
                    eprintln!("local restore refused: {}", String::from_utf8_lossy(&out.stderr));
                }
                out.status.success()
            }
        };
        let diagnosis = inspect();
        assert_eq!(diagnosis["state"], "missing_checkout");
        assert!(!checkout.exists());
        let token = diagnosis["review_token"].as_str().unwrap();
        assert!(!apply(&"0".repeat(64)));
        assert!(!checkout.exists());
        std::fs::create_dir(&checkout).unwrap();
        std::fs::create_dir(checkout.join("src")).unwrap();
        std::fs::write(checkout.join("src/main.rs"), "manual recovery\n").unwrap();
        std::fs::write(checkout.join("untracked"), "keep\n").unwrap();
        assert_eq!(inspect()["review_token"], token, "recreated directory retains the reviewed Git source");
        assert!(apply(token), "shared={shared}");
        assert!(!apply(token), "present checkout cannot be restored twice");
        let restored = inspect();
        assert_eq!(restored["state"], "present");
        assert!(restored["review_token"].is_null());
        assert_eq!(restored["last_restore"]["state"], "completed");
        assert_eq!(restored["last_restore"]["source"], token);
        assert_eq!(restored["last_restore"]["restored"], 1);
        assert_eq!(restored["last_restore"]["preserved"], 1);
        assert_eq!(std::fs::read(checkout.join("staged.bin")).unwrap(), [0, 255, 42]);
        assert_eq!(std::fs::read(checkout.join("src/main.rs")).unwrap(), b"manual recovery\n");
        assert_eq!(std::fs::read(checkout.join("untracked")).unwrap(), b"keep\n");
        assert_eq!(std::fs::read(admin.join("index")).unwrap(), index);
        assert_eq!(std::fs::read(rook.workspace.path().join("src/main.rs")).unwrap(), b"fn main() {}\n");
        if let Some(daemon) = &daemon {
            let response = runtime.block_on(async {
                client
                    .post(format!("{}{path}", daemon.address))
                    .header("content-type", "application/json")
                    .body(" ".repeat(4097))
                    .send()
                    .await
                    .unwrap()
            });
            assert_eq!(response.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);
        }
        let chat = rook.chat_in_session(&parent, &format!("/worktree {child}\n/quit\n"));
        assert!(chat.status.success(), "{}", String::from_utf8_lossy(&chat.stderr));
        assert!(String::from_utf8_lossy(&chat.stdout).contains("index_sha256"));
        drop(daemon);
        let reopened = rook.json(&["session", "worktree", &parent, &child]);
        assert_eq!(reopened["last_restore"]["state"], "completed");
        assert_eq!(reopened["last_restore"]["source"], token);
    }
}
