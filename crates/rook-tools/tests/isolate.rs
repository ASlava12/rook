//! What a contained command can and cannot do, checked by running one. Each
//! test first runs the same command uncontained and asserts it succeeds, so a
//! refusal is the sandbox's and not the machine's.

use std::path::Path;

use rook_tools::exec::spawn_shell;
use rook_tools::isolate::{Backend, Isolation, available};

async fn run(command: &str, cwd: &Path, isolation: Option<&Isolation>) -> (i32, String) {
    let child = spawn_shell(command, cwd, &[], isolation).expect("spawns");
    let out = child.wait_with_output().await.expect("finishes");
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), said)
}

/// Whether anything here contains a command, said once per run when nothing
/// does, so a skipped test is not a silent one.
fn backend() -> Option<Backend> {
    match available() {
        Ok(backend) => Some(backend),
        Err(why) => {
            eprintln!("skipped: {why}");
            None
        }
    }
}

#[tokio::test]
async fn a_contained_command_writes_the_workspace_and_scratch_and_nothing_else() {
    if backend().is_none() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let (workspace, scratch, outside) =
        (root.path().join("ws"), root.path().join("scratch"), root.path().join("outside"));
    for dir in [&workspace, &scratch, &outside] {
        std::fs::create_dir(dir).unwrap();
    }
    let isolation = Isolation {
        workspace: workspace.clone(),
        scratch: vec![scratch.clone()],
        network: true,
        unreadable: vec![],
    };
    let touch = |dir: &Path, name: &str| match cfg!(windows) {
        true => format!("type nul > \"{}\"", dir.join(name).display()),
        false => format!("touch '{}'", dir.join(name).display()),
    };

    let (code, said) = run(&touch(&outside, "plain"), &workspace, None).await;
    assert_eq!(code, 0, "the precondition: uncontained, the write outside succeeds: {said}");
    assert!(outside.join("plain").exists());

    let (code, said) = run(&touch(&outside, "blocked"), &workspace, Some(&isolation)).await;
    assert_ne!(code, 0, "contained, the write outside is refused: {said}");
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
    assert_eq!(code, 0, "reading is everywhere, and the null device takes writes: {said}");
}

#[tokio::test]
async fn tcp_is_refused_when_the_policy_says_no_network() {
    let Some(backend) = backend() else { return };
    if matches!(backend, Backend::Landlock { tcp: false } | Backend::LowIntegrity) {
        eprintln!(
            "skipped: {} cannot restrain the network",
            backend.describe(&Isolation::for_workspace("."))
        );
        return;
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let workspace = tempfile::tempdir().unwrap();
    let connect = format!("bash -c 'exec 3<>/dev/tcp/127.0.0.1/{port}'");

    let (code, said) = run(&connect, workspace.path(), None).await;
    assert_eq!(code, 0, "the precondition: uncontained, the connection is made: {said}");

    let closed =
        Isolation { workspace: workspace.path().into(), scratch: vec![], network: false, unreadable: vec![] };
    let (code, said) = run(&connect, workspace.path(), Some(&closed)).await;
    assert_ne!(code, 0, "with the network switched off it is refused: {said}");

    let open = Isolation { network: true, ..closed };
    let (code, said) = run(&connect, workspace.path(), Some(&open)).await;
    assert_eq!(code, 0, "and with it on, made again: {said}");
}

/// A platform with nothing to contain a command says so, in words a person
/// can act on, rather than pretending.
#[test]
fn what_contains_a_command_here_is_said_either_way() {
    match available() {
        Ok(backend) => {
            let words = backend.describe(&Isolation::for_workspace("."));
            assert!(words.contains("writes to the workspace"), "{words}");
        }
        Err(why) => assert!(why.starts_with("no sandbox"), "{why}"),
    }
}

/// A refused write looks like any permission error. The tool's result says
/// the command was contained and what would widen it, so a model does not
/// keep trying the same write — and a person reading the transcript knows.
#[tokio::test]
async fn a_failed_contained_command_says_it_was_contained() {
    use rook_tools::{Tool, ToolContext};
    if backend().is_none() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let (workspace, outside) = (root.path().join("ws"), root.path().join("outside"));
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&outside).unwrap();
    let mut ctx = ToolContext::new(workspace.clone());
    ctx.isolate = rook_tools::isolate::Mode::Auto;
    ctx.isolation =
        Isolation { workspace: workspace.clone(), scratch: vec![], network: true, unreadable: vec![] };
    ctx.allow_outside_workspace = true;

    let args = serde_json::json!({ "command": format!("touch '{}'", outside.join("x").display()) });
    let out = rook_tools::exec::RunCommand.call(&ctx, &args).await.unwrap();
    assert!(out.is_error, "{}", out.content);
    assert!(out.content.contains("ran contained"), "{}", out.content);
    assert!(out.content.contains("[sandbox] writable"), "{}", out.content);
    assert!(
        out.meta
            .get("isolation")
            .is_some_and(|v| v.as_str().is_some_and(|s| s.contains("writes to the workspace"))),
        "{:?}",
        out.meta
    );

    let fine = serde_json::json!({ "command": format!("touch '{}'", workspace.join("y").display()) });
    let out = rook_tools::exec::RunCommand.call(&ctx, &fine).await.unwrap();
    assert!(!out.is_error, "{}", out.content);
    assert!(!out.content.contains("ran contained"), "a success says nothing about it: {}", out.content);

    // A failure that is not a refusal is not blamed on the sandbox.
    let missing = serde_json::json!({ "command": if cfg!(windows) { "type C:\\nowhere\\at\\all" } else { "cat /nowhere/at/all" } });
    let out = rook_tools::exec::RunCommand.call(&ctx, &missing).await.unwrap();
    assert!(out.is_error, "{}", out.content);
    assert!(!out.content.contains("ran contained"), "a missing file is not the sandbox: {}", out.content);
}

/// Reading is allowed everywhere a build might need, which is nearly
/// everywhere — but the agent's own state directory is every project's
/// transcripts, every checkpoint's contents and everything it was told to
/// remember. A command run for one project has no business reading another's,
/// and with the network on, reading is the whole of what an exfiltration needs.
#[tokio::test]
async fn a_contained_command_cannot_read_the_agents_own_store() {
    let Some(backend) = backend() else { return };
    if !backend.hides_paths() {
        eprintln!("skipped: {backend:?} restricts writing, not reading");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let (workspace, state) = (root.path().join("ws"), root.path().join("state"));
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&state).unwrap();
    std::fs::write(state.join("transcript"), "another project's history\n").unwrap();
    let read = format!("cat '{}'", state.join("transcript").display());

    let (code, said) = run(&read, &workspace, None).await;
    assert_eq!(code, 0, "the precondition: uncontained, it reads: {said}");
    assert!(said.contains("another project"), "{said}");

    let isolation = Isolation {
        workspace: workspace.clone(),
        scratch: vec![],
        network: true,
        unreadable: vec![state.clone()],
    };
    let (code, said) = run(&read, &workspace, Some(&isolation)).await;
    assert_ne!(code, 0, "contained, the read is refused: {said}");
    assert!(!said.contains("another project"), "and nothing of it came back: {said}");

    // Everything else still reads, or a build could not run at all.
    let (code, said) = run("cat /etc/hosts > /dev/null", &workspace, Some(&isolation)).await;
    assert_eq!(code, 0, "the rest of the machine is still readable: {said}");
}

/// What was kept out of reach is said, and where the platform cannot keep it
/// out of reach that is said instead — a boundary believed to hold and not
/// holding is worse than none.
#[test]
fn whether_the_store_is_out_of_reach_is_said_either_way() {
    let Ok(backend) = available() else { return };
    let plain = Isolation::for_workspace(".");
    assert!(!backend.describe(&plain).contains("store"), "nothing to say when nothing is hidden");

    let hiding = Isolation { unreadable: vec!["/nowhere".into()], ..Isolation::for_workspace(".") };
    let said = backend.describe(&hiding);
    match backend.hides_paths() {
        true => assert!(said.contains("cannot read the agent's own store"), "{said}"),
        false => assert!(said.contains("cannot stop it reading"), "{said}"),
    }
}
