//! Three layers, and the one that arrives with somebody else's repository.

use std::sync::{Mutex, MutexGuard};

use rook_core::Config;

/// `ROOK_HOME` and `ROOK_CONFIG_DIR` are process-wide.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
fn alone() -> MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner())
}

fn user_config(text: &str) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), text).unwrap();
    unsafe { std::env::set_var("ROOK_HOME", home.path()) };
    home
}

fn project(text: &str) -> tempfile::TempDir {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(workspace.path().join(".rook")).unwrap();
    std::fs::write(workspace.path().join(".rook/config.toml"), text).unwrap();
    workspace
}

/// Key by key, not file by file: a project that sets one thing keeps
/// everything else the person chose, and the person keeps what they did not
/// set from the defaults.
#[test]
fn a_project_setting_one_thing_keeps_everything_else() {
    let _alone = alone();
    let _home = user_config("[agent]\nmax_steps = 300\neffort = \"low\"\n");
    let workspace = project("[agent]\nmax_steps = 700\n");

    let config = Config::load_for(workspace.path()).unwrap();
    assert_eq!(config.agent.max_steps, 700, "the project wins the key it names");
    assert_eq!(config.agent.effort, "low", "and the person keeps the ones it does not");
    assert_eq!(config.agent.compact_at, Config::default().agent.compact_at, "the rest is the default");
}

/// A project's file travels with the repository, so it may say how to work
/// here and not what the agent is allowed to do. The refusal is named rather
/// than silent: a setting that does nothing and says nothing leaves somebody
/// certain they changed something.
#[test]
fn a_project_cannot_widen_what_the_agent_may_do() {
    let _alone = alone();
    let _home = user_config("");
    let workspace = project(
        "[agent]\nmax_steps = 700\nmodel = \"openai-compatible/whatever\"\n\
         [sandbox]\nstance = \"free\"\nisolate = \"off\"\nallow_outside_workspace = true\n",
    );

    let config = Config::load_for(workspace.path()).unwrap();
    assert_eq!(config.agent.max_steps, 700, "what it may set, it sets");
    assert_eq!(config.agent.model, Config::default().agent.model, "and the model is not its to choose");
    assert_eq!(config.sandbox.stance, Config::default().sandbox.stance);
    assert!(!config.sandbox.allow_outside_workspace, "nor the workspace boundary");

    let refused = Config::refused_from_workspace(workspace.path());
    assert_eq!(
        refused,
        ["agent.model", "sandbox.allow_outside_workspace", "sandbox.isolate", "sandbox.stance"],
        "and every one of them is named"
    );
}

/// The case the whole arrangement turns on. `deny` cannot be replaced, only
/// added to — and when no layer names it at all, the project's entries have to
/// join the built-in list rather than become it, or a repository could drop
/// `rm -rf /` by mentioning something else.
#[test]
fn a_project_adds_to_the_deny_list_and_cannot_shorten_it() {
    let _alone = alone();
    let _home = user_config("");
    let workspace = project("[sandbox]\ndeny = [\"make deploy\"]\n");

    let config = Config::load_for(workspace.path()).unwrap();
    assert!(config.sandbox.deny.iter().any(|d| d == "make deploy"), "what the project added is there");
    for built_in in Config::default().sandbox.deny {
        assert!(
            config.sandbox.deny.contains(&built_in),
            "and every built-in denial survived it: {built_in} is missing from {:?}",
            config.sandbox.deny
        );
    }
    assert!(Config::refused_from_workspace(workspace.path()).is_empty(), "adding to deny is not a refusal");
}
