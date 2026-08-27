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

impl Rook {
    fn new() -> Self {
        let rook = Self { home: tempfile::tempdir().unwrap(), workspace: tempfile::tempdir().unwrap() };
        std::fs::create_dir_all(rook.workspace.path().join("src")).unwrap();
        std::fs::write(rook.workspace.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        rook
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
    rook.ok(&["skills", "rollback", "notes", first]);
    assert!(rook.ok(&["skills", "show", "notes"]).contains("First version"));
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
    port: u16,
}

/// `CARGO_BIN_EXE_` is only set for this package's own binaries, and both land
/// in the same directory. `cargo test --workspace`, which the CI gate runs,
/// builds it; a lone `-p rook-cli` may not have.
fn rookd() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_BIN_EXE_rook")).with_file_name(if cfg!(windows) {
        "rookd.exe"
    } else {
        "rookd"
    });
    assert!(path.exists(), "{} is not built — run `cargo test --workspace`", path.display());
    path
}

impl Daemon {
    fn start(rook: &Rook, port: u16) -> Self {
        let child = Command::new(rookd())
            .env("ROOK_HOME", rook.home.path())
            .env("ROOK_LOG", "error")
            .args(["--workspace", rook.workspace.path().to_str().unwrap()])
            .args(["--port", &port.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let daemon = Self { child, port };
        for _ in 0..80 {
            if rook.home.path().join("rookd.addr").exists() {
                std::thread::sleep(std::time::Duration::from_millis(150));
                return daemon;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("rookd never published its address on port {port}");
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

    let daemon = Daemon::start(&rook, 18211);

    assert_eq!(rook.json(&["skills", "ls"]), direct, "routed output must be identical");
    assert_eq!(rook.json(&["store", "stat"]), direct_stats);

    let write = rook.run(&["store", "gc"]);
    assert!(!write.status.success(), "a write cannot go over the API");
    let err = String::from_utf8_lossy(&write.stderr);
    assert!(err.contains(&daemon.port.to_string()), "and must say where the lock is: {err}");
}

#[test]
fn the_store_is_readable_again_once_the_daemon_stops() {
    let rook = Rook::new();
    {
        let _daemon = Daemon::start(&rook, 18212);
        assert!(!rook.run(&["store", "gc"]).status.success(), "the daemon holds the lock");
    }
    for _ in 0..40 {
        if rook.run(&["store", "gc"]).status.success() {
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
}
