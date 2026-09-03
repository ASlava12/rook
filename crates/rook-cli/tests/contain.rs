//! What a contained command can and cannot do, with `rook` itself as the
//! launcher — which on Windows is the only way there is, and everywhere else
//! is the same test with the same answer as `rook-tools` runs on its own.

use std::path::Path;

use rook_tools::isolate::{Isolation, available};

fn with_launcher() {
    // Safety: set before any command is spawned, and read by nothing else.
    unsafe { std::env::set_var(rook_contain::LAUNCHER, env!("CARGO_BIN_EXE_rook")) };
}

async fn run(command: &str, cwd: &Path, isolation: Option<&Isolation>) -> (i32, String) {
    let child = rook_tools::exec::spawn_shell(command, cwd, &[], isolation).expect("spawns");
    let out = child.wait_with_output().await.expect("finishes");
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), said)
}

fn touch(dir: &Path, name: &str) -> String {
    match cfg!(windows) {
        true => format!("type nul > \"{}\"", dir.join(name).display()),
        false => format!("touch '{}'", dir.join(name).display()),
    }
}

#[tokio::test]
async fn a_command_contained_by_rook_writes_the_workspace_and_scratch_and_nothing_else() {
    with_launcher();
    let backend = match available() {
        Ok(backend) => backend,
        Err(why) => {
            eprintln!("skipped: {why}");
            return;
        }
    };
    let root = tempfile::tempdir().unwrap();
    let (workspace, scratch, outside) =
        (root.path().join("ws"), root.path().join("scratch"), root.path().join("outside"));
    for dir in [&workspace, &scratch, &outside] {
        std::fs::create_dir(dir).unwrap();
    }
    let isolation = Isolation { workspace: workspace.clone(), scratch: vec![scratch.clone()], network: true };

    let (code, said) = run(&touch(&outside, "plain"), &workspace, None).await;
    assert_eq!(code, 0, "the precondition: uncontained, the write outside succeeds: {said}");

    let (code, said) = run(&touch(&outside, "blocked"), &workspace, Some(&isolation)).await;
    assert_ne!(code, 0, "contained by {backend:?}, the write outside is refused: {said}");
    assert!(!outside.join("blocked").exists(), "and nothing was written");

    let both = format!("{} && {}", touch(&workspace, "inside"), touch(&scratch, "temp"));
    let (code, said) = run(&both, &workspace, Some(&isolation)).await;
    assert_eq!(code, 0, "the workspace and scratch are writable: {said}");
    assert!(workspace.join("inside").exists() && scratch.join("temp").exists());

    let reads = match cfg!(windows) {
        true => "type C:\\Windows\\System32\\drivers\\etc\\hosts > nul && dir C:\\ > nul",
        false => "cat /etc/hosts > /dev/null && ls / > /dev/null",
    };
    let (code, said) = run(reads, &workspace, Some(&isolation)).await;
    assert_eq!(code, 0, "reading is everywhere: {said}");
}

/// The result of a refused write says the command was contained, in the
/// words the tool uses on every platform.
#[tokio::test]
async fn a_refused_write_is_reported_as_contained() {
    use rook_tools::{Tool, ToolContext};
    with_launcher();
    if let Err(why) = available() {
        eprintln!("skipped: {why}");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let (workspace, outside) = (root.path().join("ws"), root.path().join("outside"));
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&outside).unwrap();
    let mut ctx = ToolContext::new(workspace.clone());
    ctx.isolate = rook_tools::isolate::Mode::Auto;
    ctx.isolation = Isolation { workspace: workspace.clone(), scratch: vec![], network: true };
    ctx.allow_outside_workspace = true;

    let args = serde_json::json!({ "command": touch(&outside, "x") });
    let out = rook_tools::exec::RunCommand.call(&ctx, &args).await.unwrap();
    assert!(out.is_error, "{}", out.content);
    assert!(out.content.contains("ran contained"), "{}", out.content);
}
