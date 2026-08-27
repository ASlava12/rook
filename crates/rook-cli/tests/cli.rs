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
    address: String,
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
        let address_file = rook.home.path().join("rookd.addr");
        for _ in 0..80 {
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
    assert!(!write.status.success(), "a write cannot go over the API");
    let err = String::from_utf8_lossy(&write.stderr);
    assert!(err.contains(&daemon.address), "and must say where the lock is: {err}");
}

#[test]
fn the_store_is_readable_again_once_the_daemon_stops() {
    let rook = Rook::new();
    {
        let _daemon = Daemon::start(&rook);
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

#[test]
fn every_slash_command_answers_on_an_empty_session() {
    let rook = Rook::new();
    let out = rook
        .chat("/help\n/context\n/session\n/skills\n/memory\n/search nothing\n/diff\n/mcp\n/goal\n/quit\n");

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
    let out = rook.chat("/mode\n/mode readonly\n/mode\n/effort\n/effort low\n/effort\n/quit\n");

    let lines: Vec<&str> = out.lines().filter(|l| ["ask", "readonly", "high", "low"].contains(l)).collect();
    assert_eq!(lines, ["ask", "readonly", "high", "low"], "each reads back what was set:\n{out}");
}

#[test]
fn a_setting_the_repl_does_not_have_is_refused_by_name() {
    let rook = Rook::new();
    let out = rook.chat("/mode yolo\n/effort glacial\n/quit\n");

    assert!(out.contains(r#"no mode "yolo""#), "{out}");
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
