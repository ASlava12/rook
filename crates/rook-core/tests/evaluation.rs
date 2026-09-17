//! What "better" means for a project, and whether the answer can be moved.
//!
//! The claims here are about the one thing an agent that writes both the code
//! and the tests can always do: make them agree. The scorecard cannot prevent
//! that — a coding agent has to be able to edit the repository — so what is
//! asserted is that it is named rather than lost.

use rook_core::evaluation::{self, Scorecard};

/// A check's command is written by whoever declares the scorecard, in whatever
/// shell their machine has — so a test of one has to be written twice.
///
/// Five of these failed on the Windows runner and passed here, because `sh`
/// spelling reached `cmd /C`: `echo it went wrong; exit 1` runs nothing there.
/// The same mistake the repository already writes down about paths, in the
/// other place a platform disagrees.
fn shell(sh: &str, cmd: &str) -> String {
    match cfg!(windows) {
        true => cmd.to_string(),
        false => sh.to_string(),
    }
}

fn workspace_with(scorecard: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".rook")).unwrap();
    std::fs::write(evaluation::scorecard_path(dir.path()), scorecard).unwrap();
    dir
}

/// Reading a scorecard and running it, which is the ordinary case and the one
/// everything else is a deviation from.
#[test]
fn a_declared_check_is_run_and_its_verdict_is_the_commands() {
    let dir = workspace_with(&format!(
        r#"
[[check]]
name = "passes"
run = "exit 0"

[[check]]
name = "fails"
run = "{}"
"#,
        shell("echo it went wrong; exit 1", "echo it went wrong&& exit 1")
    ));
    let card = evaluation::read(dir.path()).unwrap().expect("a scorecard is declared");
    let before = evaluation::witness(dir.path(), &card);
    let report = evaluation::run(dir.path(), &card, &before);

    assert_eq!(report.checks.len(), 2);
    assert!(report.checks[0].passed, "exit 0 passes");
    assert!(!report.checks[1].passed, "exit 1 does not");
    assert!(
        report.checks[1].said.contains("it went wrong"),
        "and what it printed is kept: {:?}",
        report.checks[1].said
    );
    assert_eq!(report.passed(), 1);
    assert!(!report.clean(), "one failing check is not a clean run");
}

/// The whole point. An agent told a check fails can rewrite what the check
/// looks at, and the cheapest move is exactly that. It is not forbidden — it
/// cannot be, since the agent has to edit the repository — but a pass that
/// arrived this way says so beside itself.
#[test]
fn a_check_that_passes_because_what_it_guards_was_rewritten_says_so() {
    let dir = workspace_with(&format!(
        r#"
[[check]]
name = "tests"
run = "{}"
guards = ["tests/*.cmd", "tests/*.sh"]
"#,
        // Four backslashes: two survive Rust and become one in the TOML, which
        // needs it escaped — `\\c` is not an escape TOML knows, and the
        // scorecard would not parse at all.
        shell("sh tests/check.sh", "cmd /C tests\\\\check.cmd")
    ));
    std::fs::create_dir_all(dir.path().join("tests")).unwrap();
    let named = match cfg!(windows) {
        true => "tests/check.cmd",
        false => "tests/check.sh",
    };
    let check = dir.path().join(named);
    std::fs::write(&check, "exit 1\n").unwrap();

    let card = evaluation::read(dir.path()).unwrap().unwrap();
    // The state of things when the work began.
    let before = evaluation::witness(dir.path(), &card);

    // The precondition, and it is the point: it really did fail first. Without
    // it this test would pass on a scorecard that never ran anything.
    let first = evaluation::run(dir.path(), &card, &before);
    assert!(!first.checks[0].passed, "it fails as written");
    assert!(first.checks[0].touched.is_empty(), "and nothing had been touched yet");

    // The turn's answer to being told no.
    std::fs::write(&check, "exit 0\n").unwrap();

    let after = evaluation::run(dir.path(), &card, &before);
    assert!(after.checks[0].passed, "it passes now");
    assert_eq!(after.checks[0].touched, vec![named.to_string()], "and names what changed");
    assert!(!after.clean(), "so the run is not clean, however green the check is");
    assert!(
        after.summary().contains("not news yet"),
        "and the summary says why rather than reporting a pass: {}",
        after.summary()
    );
}

/// The same question asked of the scorecard itself, which is the shortest way
/// to a green run: delete the check that fails.
#[test]
fn a_scorecard_edited_during_the_run_makes_every_number_in_it_a_different_measurement() {
    let dir = workspace_with(
        r#"
[[check]]
name = "one"
run = "exit 0"
"#,
    );
    let card = evaluation::read(dir.path()).unwrap().unwrap();
    let before = evaluation::witness(dir.path(), &card);

    assert!(evaluation::run(dir.path(), &card, &before).clean(), "it is clean as declared");

    std::fs::write(
        evaluation::scorecard_path(dir.path()),
        "[[check]]\nname = \"one\"\nrun = \"exit 0\"\n# and a comment\n",
    )
    .unwrap();

    let after = evaluation::run(dir.path(), &card, &before);
    assert!(after.checks[0].passed, "the check still passes");
    assert!(after.scorecard_changed, "but the scorecard is not the one the work started against");
    assert!(!after.clean(), "so the run is not clean");
    assert!(after.summary().contains("not the same measurement"), "{}", after.summary());
}

/// A number to watch rather than gate on — coverage, a count, a size. Reported
/// beside the pass and named, so a report says what the number is.
#[test]
fn a_check_can_carry_a_number_as_well_as_a_verdict() {
    let dir = workspace_with(&format!(
        r#"
[[check]]
name = "coverage"
run = "{}"
measures = "coverage"
"#,
        shell("echo some noise; echo 81.4", "echo some noise&& echo 81.4")
    ));
    let card = evaluation::read(dir.path()).unwrap().unwrap();
    let report = evaluation::run(dir.path(), &card, &evaluation::witness(dir.path(), &card));

    assert_eq!(report.checks[0].measured, Some(81.4), "the last line, not the first");
    assert!(report.summary().contains("coverage 81.4"), "{}", report.summary());
}

/// A check that does not come back is not a check that failed, and a report
/// that called it one would send somebody looking for a bug in a suite that is
/// merely slow.
#[test]
fn a_check_that_never_finishes_is_stopped_and_is_not_reported_as_a_failure() {
    // Through a shell that outlives its child on purpose: `sh -c "sleep 60"`
    // may exec into `sleep` and be killed with it, which would let a broken
    // deadline pass. A subshell cannot, so what is tested is the deadline
    // reaching the whole tree rather than just the shell.
    let dir = workspace_with(&format!(
        r#"
[[check]]
name = "hangs"
run = "{}"
timeout_secs = 1
"#,
        // `ping` rather than `timeout`: this check is given no stdin, and
        // `timeout` refuses to run without one — it exited at once, which let
        // a broken deadline pass for being fast.
        shell("(sleep 60); echo done", "ping -n 61 127.0.0.1 >NUL&& echo done")
    ));
    let card = evaluation::read(dir.path()).unwrap().unwrap();
    let began = std::time::Instant::now();
    let report = evaluation::run(dir.path(), &card, &evaluation::witness(dir.path(), &card));

    assert!(began.elapsed() < std::time::Duration::from_secs(30), "it was stopped, not waited out");
    assert!(!report.checks[0].passed, "an unfinished check is not a pass");
    assert_eq!(report.checks[0].status, None, "and it has no exit status, which is how it is told apart");
    assert!(report.checks[0].said.contains("without finishing"), "{:?}", report.checks[0].said);
}

/// An expectation other than zero, for a project that wants to hold something
/// broken until it is deliberately fixed.
#[test]
fn a_check_may_expect_a_status_other_than_success() {
    let dir = workspace_with(
        r#"
[[check]]
name = "still refuses"
run = "exit 3"
expect = 3
"#,
    );
    let card = evaluation::read(dir.path()).unwrap().unwrap();
    let report = evaluation::run(dir.path(), &card, &evaluation::witness(dir.path(), &card));
    assert!(report.checks[0].passed, "exit 3 is what it asked for");
    assert!(report.clean());
}

/// A scorecard nobody wrote is not an error, because most workspaces have
/// none; one that is written wrongly is, and says which check.
#[test]
fn a_missing_scorecard_is_nothing_and_a_broken_one_names_the_check() {
    let bare = tempfile::tempdir().unwrap();
    assert!(evaluation::read(bare.path()).unwrap().is_none(), "no file is no scorecard");

    let dir = workspace_with("[[check]]\nname = \"\"\nrun = \"exit 0\"\n");
    let why = evaluation::read(dir.path()).unwrap_err();
    assert!(why.contains("position 1"), "it says which one: {why}");
    assert!(why.contains("name"), "{why}");

    let dir = workspace_with("[[check]]\nname = \"empty\"\n");
    let why = evaluation::read(dir.path()).unwrap_err();
    assert!(why.contains("empty") && why.contains("run"), "{why}");
}

/// Guards name files, and the patterns people reach for are a directory, a
/// suffix inside one, and a literal path. Anything cleverer belongs in the
/// command the check runs.
#[test]
fn a_guard_covers_a_directory_a_wildcard_and_a_plain_path() {
    let dir = workspace_with(
        r#"
[[check]]
name = "everything"
run = "exit 0"
guards = ["tests/**", "src/*.rs", "Cargo.toml"]
"#,
    );
    for (at, what) in [
        ("tests/deep/one.rs", "under a guarded directory"),
        ("src/lib.rs", "matched by a wildcard"),
        ("Cargo.toml", "named outright"),
    ] {
        let path = dir.path().join(at);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "before\n").unwrap();
        let card = evaluation::read(dir.path()).unwrap().unwrap();
        let before = evaluation::witness(dir.path(), &card);
        std::fs::write(&path, "after\n").unwrap();

        let report = evaluation::run(dir.path(), &card, &before);
        assert!(
            report.checks[0].touched.iter().any(|p| p == at),
            "{at} is {what}: {:?}",
            report.checks[0].touched
        );
    }

    // And something no guard names is not reported, or every run would be
    // tainted by ordinary work and nobody would read the line.
    let path = dir.path().join("README.md");
    std::fs::write(&path, "before\n").unwrap();
    let card = evaluation::read(dir.path()).unwrap().unwrap();
    let before = evaluation::witness(dir.path(), &card);
    std::fs::write(&path, "after\n").unwrap();
    let report = evaluation::run(dir.path(), &card, &before);
    assert!(
        report.checks[0].touched.is_empty(),
        "an unguarded file is ordinary work: {:?}",
        report.checks[0].touched
    );
    assert!(report.clean());
}

/// Content rather than a timestamp: a checkout, a formatter and a rebuild all
/// move mtimes without changing what a check would find, and a report that
/// cried tampering after `git checkout` is a report people stop reading.
#[test]
fn a_file_rewritten_with_the_same_contents_is_not_a_change() {
    let dir = workspace_with(
        r#"
[[check]]
name = "one"
run = "exit 0"
guards = ["fixture.txt"]
"#,
    );
    let path = dir.path().join("fixture.txt");
    std::fs::write(&path, "the same\n").unwrap();
    let card = evaluation::read(dir.path()).unwrap().unwrap();
    let before = evaluation::witness(dir.path(), &card);

    std::fs::write(&path, "the same\n").unwrap();
    let report = evaluation::run(dir.path(), &card, &before);
    assert!(report.checks[0].touched.is_empty(), "same bytes, so nothing changed");
    assert!(report.clean());
}

/// A guarded file that appears or disappears is exactly as interesting as one
/// whose contents changed — deleting the test is the other way to make it stop
/// failing.
#[test]
fn a_guarded_file_that_is_deleted_is_a_change_like_any_other() {
    let dir = workspace_with(
        r#"
[[check]]
name = "one"
run = "exit 0"
guards = ["tests/**"]
"#,
    );
    std::fs::create_dir_all(dir.path().join("tests")).unwrap();
    let path = dir.path().join("tests/one.rs");
    std::fs::write(&path, "fn a() {}\n").unwrap();
    let card = evaluation::read(dir.path()).unwrap().unwrap();
    let before = evaluation::witness(dir.path(), &card);

    std::fs::remove_file(&path).unwrap();
    let report = evaluation::run(dir.path(), &card, &before);
    assert_eq!(report.checks[0].touched, vec!["tests/one.rs".to_string()], "a deletion is a change");
}

/// The output of a check is bounded while it arrives. A suite that fails
/// everywhere prints megabytes, and a report that held all of it is a report
/// that costs more than the run.
#[test]
fn what_a_check_prints_is_bounded_rather_than_kept_whole() {
    let dir = workspace_with(&format!(
        r#"
[[check]]
name = "loud"
run = "{}"
"#,
        shell(
            "i=0; while [ $i -lt 20000 ]; do echo 'a line of output that is not short at all'; i=$((i+1)); done; exit 1",
            "for /L %i in (1,1,20000) do @echo a line of output that is not short at all&& exit 1"
        )
    ));
    let card = evaluation::read(dir.path()).unwrap().unwrap();
    let report = evaluation::run(dir.path(), &card, &evaluation::witness(dir.path(), &card));

    // The precondition: it really did print more than the cap, so this is
    // about the bound and not about a command that happened to be quiet.
    assert!(!report.checks[0].passed);
    assert!(report.checks[0].said.len() < 8_000, "kept {} bytes", report.checks[0].said.len());
    assert!(report.checks[0].said.contains("a line of output"), "and it is the output, not a summary");
}

/// Both streams are drained. A check whose stderr filled its pipe while
/// nobody read it would never return, which is how `hooks` once deadlocked.
#[test]
fn a_check_that_writes_to_both_streams_finishes() {
    let dir = workspace_with(&format!(
        r#"
[[check]]
name = "both"
run = "{}"
timeout_secs = 60
"#,
        shell(
            "i=0; while [ $i -lt 5000 ]; do echo out; echo err 1>&2; i=$((i+1)); done; exit 0",
            "for /L %i in (1,1,5000) do @(echo out&& echo err 1>&2)"
        )
    ));
    let card = evaluation::read(dir.path()).unwrap().unwrap();
    let report = evaluation::run(dir.path(), &card, &evaluation::witness(dir.path(), &card));

    assert!(report.checks[0].passed, "it finished: {:?}", report.checks[0].said);
    assert!(report.checks[0].status.is_some(), "and by exiting rather than by being stopped");
}

/// The scorecard is a file in the workspace, and the workspace is what a turn
/// edits — so `Scorecard` has to round-trip through TOML for anything that
/// writes one to be trustworthy.
#[test]
fn a_scorecard_survives_being_written_and_read_again() {
    let card = Scorecard {
        checks: vec![rook_core::evaluation::Check {
            name: "tests".into(),
            run: "cargo test --workspace".into(),
            expect: 0,
            measures: String::new(),
            guards: vec!["crates/*/tests/**".into()],
            timeout_secs: 900,
        }],
    };
    let written = toml::to_string_pretty(&card).unwrap();
    let dir = workspace_with(&written);
    let read = evaluation::read(dir.path()).unwrap().unwrap();

    assert_eq!(read.checks.len(), 1);
    assert_eq!(read.checks[0].name, "tests");
    assert_eq!(read.checks[0].guards, vec!["crates/*/tests/**".to_string()]);
    assert_eq!(read.checks[0].timeout_secs, 900);
}
