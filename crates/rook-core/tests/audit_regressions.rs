use rook_core::fileset::{CaptureLimits, FileSet};
use rook_core::memory::{Fact, Scope};
use rook_core::{Config, Rook};
use rook_skills::{Environment, SkillIndex};
use rook_store::Store;
fn rook(dir: &std::path::Path) -> Rook {
    Rook::from_parts(
        Store::open(dir.join("store")).unwrap(),
        Config::default(),
        Environment::bare("linux", "x86_64", "0.4.0"),
        SkillIndex::default(),
        dir.to_path_buf(),
    )
}
#[test]
fn project_scopes_respect_path_components() {
    let scope = Scope::Project("/work/project".into());
    assert!(scope.applies_in("/work/project/src"));
    assert!(!scope.applies_in("/work/project-secret"));
    assert!(!Scope::Project("/work/project-secret".into()).within(&scope));
    assert!(!scope.applies_in("/work/project/../other"));
}
#[test]
fn concurrent_memory_updates_keep_every_fact() {
    let d = tempfile::tempdir().unwrap();
    let rook = rook(d.path());
    let gate = std::sync::Barrier::new(16);
    std::thread::scope(|threads| {
        for n in 0..16 {
            let rook = &rook;
            let gate = &gate;
            threads.spawn(move || {
                gate.wait();
                rook.remember(Fact::new(format!("unique fact {n}"), Scope::Global), None).unwrap();
            });
        }
    });
    assert_eq!(rook.memory().unwrap().facts.len(), 16);
    assert_eq!(rook.memory_history().unwrap().len(), 16);
}
#[test]
fn nested_claims_and_expired_guards_do_not_release_other_leases() {
    let d = tempfile::tempdir().unwrap();
    let rook = rook(d.path());
    let a = vec![d.path().join("a")];
    let b = vec![d.path().join("b")];
    let old = rook.writing(1, &a).unwrap();
    let nested = rook.writing(1, &a).unwrap();
    let other = rook.writing(2, &b).unwrap();
    assert!(rook.writing(1, &[a[0].clone(), b[0].clone()]).is_err());
    drop(nested);
    assert!(rook.writing(2, &a).is_err());
    rook.age_claims_for_test(3601);
    let new = rook.writing(2, &a).unwrap();
    drop(old);
    assert!(rook.writing(1, &a).is_err());
    drop(new);
    drop(other);
    assert!(rook.writing(1, &a).is_ok());
}
#[test]
fn rewind_does_not_include_its_new_fork_or_preceding_checkpoints() {
    let d = tempfile::tempdir().unwrap();
    let rook = rook(d.path());
    let session = rook.start_session("audit").unwrap();
    let x = d.path().join("x");
    let y = d.path().join("y");
    std::fs::write(&x, "original x").unwrap();
    std::fs::write(&y, "original y").unwrap();
    rook.checkpoint_paths(session, "x", std::slice::from_ref(&x), &CaptureLimits::default()).unwrap();
    std::fs::write(&x, "new x").unwrap();
    rook.checkpoint_paths(session, "y", std::slice::from_ref(&y), &CaptureLimits::default()).unwrap();
    std::fs::write(&y, "new y").unwrap();
    rook.rewind(session, 1, true).unwrap();
    assert_eq!(std::fs::read_to_string(x).unwrap(), "new x");
    assert_eq!(std::fs::read_to_string(y).unwrap(), "original y");
}
#[test]
fn rewind_orders_child_and_parent_mutations_together() {
    let d = tempfile::tempdir().unwrap();
    let rook = rook(d.path());
    let parent = rook.start_session("audit").unwrap();
    let child = rook.fork_for_subtask(parent, "child").unwrap();
    let x = d.path().join("x");
    std::fs::write(&x, "original").unwrap();
    rook.checkpoint_paths(child, "child", std::slice::from_ref(&x), &CaptureLimits::default()).unwrap();
    std::fs::write(&x, "v1").unwrap();
    rook.checkpoint_paths(parent, "parent", std::slice::from_ref(&x), &CaptureLimits::default()).unwrap();
    std::fs::write(&x, "v2").unwrap();
    rook.rewind(parent, 0, true).unwrap();
    assert_eq!(std::fs::read_to_string(x).unwrap(), "original");
}
#[cfg(unix)]
#[test]
fn restoring_a_checkpoint_does_not_follow_replaced_symlinks() {
    let d = tempfile::tempdir().unwrap();
    let rook = rook(d.path());
    let outside = tempfile::tempdir().unwrap();
    let x = d.path().join("x");
    let target = outside.path().join("secret");
    std::fs::write(&x, "original").unwrap();
    std::fs::write(&target, "private").unwrap();
    let (set, _) = rook_core::fileset::capture_paths(
        &rook.store,
        "checkpoint",
        "audit",
        d.path(),
        std::slice::from_ref(&x),
        &CaptureLimits::default(),
    )
    .unwrap();
    std::fs::remove_file(&x).unwrap();
    std::os::unix::fs::symlink(&target, &x).unwrap();
    assert!(set.restore(&rook.store, d.path()).is_err());
    assert_eq!(std::fs::read_to_string(target).unwrap(), "private");
}
#[test]
fn restore_validates_every_manifest_entry_before_writing() {
    let d = tempfile::tempdir().unwrap();
    let rook = rook(d.path());
    let x = d.path().join("a");
    std::fs::write(&x, "original").unwrap();
    let (mut set, _) = rook_core::fileset::capture_paths(
        &rook.store,
        "checkpoint",
        "audit",
        d.path(),
        std::slice::from_ref(&x),
        &CaptureLimits::default(),
    )
    .unwrap();
    let id = set.files["a"].clone();
    set.files.insert("z/../../escaped".into(), id);
    std::fs::write(&x, "keep").unwrap();
    assert!(FileSet::restore(&set, &rook.store, d.path()).is_err());
    assert_eq!(std::fs::read_to_string(x).unwrap(), "keep");
}
#[test]
fn invalid_hook_matchers_do_not_become_unconditional_hooks() {
    use rook_core::hooks::{Event, HookConfig, Hooks};
    let config: HookConfig = serde_json::from_value(
        serde_json::json!({"event":"pre_tool", "command":"echo unsafe", "match":"/[/", "timeout_secs":1}),
    )
    .unwrap();
    let (hooks, errors) = Hooks::compile(&[config]);
    assert!(!errors.is_empty());
    assert!(hooks.is_empty());
    let _ = Event::PreTool;
}
#[cfg(unix)]
#[tokio::test]
async fn a_hook_that_never_reads_stdin_still_times_out() {
    use rook_core::hooks::{Event, HookConfig, Hooks};
    let config: HookConfig = serde_json::from_value(
        serde_json::json!({"event":"pre_tool", "command":"sleep 60", "timeout_secs":1}),
    )
    .unwrap();
    let (hooks, errors) = Hooks::compile(&[config]);
    assert!(errors.is_empty());
    let payload = serde_json::json!({"large": "x".repeat(1024 * 1024)});
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        hooks.run(Event::PreTool, "write", &payload),
    )
    .await
    .unwrap();
    assert!(matches!(outcome.decision, Some(rook_tools::policy::Decision::Deny(_))));
}

#[test]
fn rewind_restores_explicitly_allowed_external_paths() {
    let d = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let mut rook = rook(d.path());
    rook.config.sandbox.allow_outside_workspace = true;
    let path = outside.path().canonicalize().unwrap().join("external.txt");
    std::fs::write(&path, "before").unwrap();
    let session = rook.start_session("external").unwrap();
    rook.checkpoint_paths(session, "before", std::slice::from_ref(&path), &CaptureLimits::default()).unwrap();
    std::fs::write(&path, "after").unwrap();
    rook.rewind(session, 0, true).unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), "before");
}

#[test]
fn an_ordinary_fork_of_a_subtask_is_not_another_delegation() {
    let d = tempfile::tempdir().unwrap();
    let rook = rook(d.path());
    let parent = rook.start_session("parent").unwrap();
    let child = rook.fork_for_subtask(parent, "child").unwrap();
    let branch = rook.fork_session(child, 0).unwrap();
    assert!(!branch.tags.iter().any(|t| t == "subtask"));
    let path = d.path().join("branch-only.txt");
    std::fs::write(&path, "before").unwrap();
    rook.checkpoint_paths(branch.id, "branch", std::slice::from_ref(&path), &CaptureLimits::default())
        .unwrap();
    std::fs::write(&path, "branch work").unwrap();
    rook.rewind(parent, 0, true).unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), "branch work");
}
