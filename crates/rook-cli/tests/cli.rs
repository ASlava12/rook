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
}

/// `CARGO_BIN_EXE_` is only set for this package's own binaries, so the daemon
/// has to be found rather than named.
///
/// Built here if it is not there. Assuming `cargo test --workspace` had already
/// built it was true of every incremental run and false of a clean one: the
/// crates compile in dependency order and this test binary can run before the
/// daemon is linked — which is what CI does, every time.
fn rookd() -> PathBuf {
    static BUILT: std::sync::Once = std::sync::Once::new();
    let path = PathBuf::from(env!("CARGO_BIN_EXE_rook")).with_file_name(if cfg!(windows) {
        "rookd.exe"
    } else {
        "rookd"
    });

    BUILT.call_once(|| {
        if path.exists() {
            return;
        }
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
        let mut child = Command::new(rookd())
            .env("ROOK_HOME", rook.home.path())
            .env("ROOK_LOG", "error")
            .args(["--workspace", rook.workspace.path().to_str().unwrap()])
            .args(["--port", "0"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
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
                return Self { child, address: address.trim().to_string() };
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
        panic!("rookd never published its address");
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
        "no tool servers connected",
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

    let lines: Vec<&str> =
        out.lines().filter(|l| ["assist", "readonly", "high", "low"].contains(l)).collect();
    assert_eq!(lines, ["assist", "readonly", "high", "low"], "each reads back what was set:\n{out}");
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

    assert!(doctor("").contains("off"), "the default is off and should say so");
    assert!(doctor("[web]\nenabled = true\n").contains("no search engine"));
    assert!(
        doctor("[web]\nenabled = true\nsearch = \"brave\"\n").contains("BRAVE_API_KEY"),
        "named but unusable is the case worth catching before a turn does"
    );
    let searx = doctor("[web]\nenabled = true\nsearch = \"searxng\"\n");
    assert!(searx.contains("web_search"), "{searx}");
    assert!(searx.contains("searxng"), "{searx}");
}
