//! What a model with judgement does with this agent.
//!
//! Every test in the suite scripts the model, so what is checked is the loop's
//! behaviour given an answer — never whether the agent works when the answer is
//! somebody's judgement rather than a fixture. This runs a handful of turns
//! against a real endpoint and asserts on what came back.
//!
//! Not in `cargo xtask ci`: it needs a model, it costs tokens or a local GPU,
//! and it is allowed to be slow. Run it against whatever is to hand —
//! `cargo xtask smoke --model ollama/qwen3:8b`.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

/// One thing an agent has to get right, and how to tell whether it did.
struct Scenario {
    name: &'static str,
    /// Files the workspace starts with.
    seed: &'static [(&'static str, &'static str)],
    prompt: &'static str,
    /// Why this is not something a model can answer from memory.
    check: fn(&Turn, &Path) -> Result<()>,
}

struct Turn {
    reply: String,
    tools: Vec<String>,
    stopped: String,
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "reads before answering",
        seed: &[("config.rs", "pub const PORT: u16 = 8443;\npub const HOST: &str = \"::1\";\n")],
        prompt: "What port does config.rs use? Answer with the number.",
        check: |turn, _| {
            expect(turn.reply.contains("8443"), "the answer is in the file, not in the model", turn)?;
            expect(turn.tools.iter().any(|t| t == "read_file"), "and it had to be read", turn)
        },
    },
    Scenario {
        name: "edits what it was asked to",
        seed: &[("config.rs", "pub const PORT: u16 = 8443;\npub const HOST: &str = \"::1\";\n")],
        prompt: "Change the port in config.rs to 9000. Change nothing else.",
        check: |turn, workspace| {
            let after = std::fs::read_to_string(workspace.join("config.rs"))?;
            expect(after.contains("9000"), "the edit has to land", turn)?;
            expect(after.contains("\"::1\""), "and nothing else may move", turn)
        },
    },
    Scenario {
        name: "uses what a command printed",
        seed: &[("token.txt", "quiet-heron-4417\n")],
        prompt: "Run `cat token.txt` and tell me exactly what it printed.",
        check: |turn, _| {
            expect(turn.reply.contains("quiet-heron-4417"), "the token is only on disk", turn)?;
            expect(turn.tools.iter().any(|t| t == "run_command"), "and only a command reaches it", turn)
        },
    },
    Scenario {
        name: "delegates and uses what came back",
        seed: &[("notes/port.txt", "the service listens on 7331\n")],
        prompt: "Use the delegate tool to have a sub-agent read notes/port.txt and report the port \
                 it names; then answer with that number only.",
        check: |turn, _| {
            expect(turn.tools.iter().any(|t| t == "delegate"), "the tool has to be the one used", turn)?;
            expect(turn.reply.contains("7331"), "the number came back through a sub-agent", turn)
        },
    },
    Scenario {
        name: "does not settle a claim from memory",
        seed: &[("lib.rs", "pub fn add(a: i32, b: i32) -> i32 { a - b }\n")],
        prompt: "Use the verify tool to check this claim: `add` in lib.rs returns the sum of its \
                 arguments.",
        check: |turn, _| {
            expect(turn.tools.iter().any(|t| t == "verify"), "the tool has to be the one used", turn)?;
            let settled = turn.reply.to_lowercase();
            // Words that mean this claim in particular. A bare "not" would be
            // satisfied by "it does not fail", which is the opposite answer.
            expect(
                ["fails", "subtract", "minus", "a - b"].iter().any(|w| settled.contains(w)),
                "the claim is false and the file says so",
                turn,
            )
        },
    },
];

/// No server is ever asked anything here, and a fetched one served from the
/// next session on — which a scenario never has. Off, or every scenario
/// downloaded rust-analyzer first.
fn config_for(model: &str) -> String {
    format!("[agent]\nmodel = \"{model}\"\ninstall_servers = false\n")
}

fn expect(held: bool, why: &str, turn: &Turn) -> Result<()> {
    match held {
        true => Ok(()),
        false => bail!(
            "{why}\n    stopped: {}\n    tools: {:?}\n    said: {}",
            turn.stopped,
            turn.tools,
            turn.reply
        ),
    }
}

pub fn smoke(model: Option<String>) -> Result<()> {
    let model =
        model.or_else(|| std::env::var("ROOK_SMOKE_MODEL").ok()).filter(|m| !m.trim().is_empty()).context(
            "no model to smoke against — pass --model, or set ROOK_SMOKE_MODEL. Anything \
             `rook models` can reach will do: `--model ollama/qwen3:8b` for a local one",
        )?;

    // The shipped path, not the library: what is being asked is whether the
    // thing a user runs works.
    let built = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["build", "-p", "rook-cli"])
        .status()
        .context("building rook")?;
    if !built.success() {
        bail!("rook did not build");
    }
    let rook = Path::new("target/debug/rook").canonicalize().context("finding the built rook")?;

    // Once, before four turns each fail with the same sentence: whether anything
    // is listening is one question, and `rook models` is the command the failure
    // itself recommends.
    let home = tempfile::tempdir()?;
    std::fs::write(home.path().join("config.toml"), config_for(&model))?;
    let reachable = Command::new(&rook)
        .env("ROOK_HOME", home.path())
        .env("ROOK_LOG", "error")
        .arg("models")
        .output()
        .context("asking what the endpoint offers")?;
    if !reachable.status.success() {
        bail!("{model} is not reachable:\n{}", String::from_utf8_lossy(&reachable.stderr).trim());
    }

    println!("{:<34} {}\n{}", "scenario", model, "─".repeat(60));
    let mut failed = 0;
    for scenario in SCENARIOS {
        let home = tempfile::tempdir()?;
        let workspace = tempfile::tempdir()?;
        std::fs::write(home.path().join("config.toml"), config_for(&model))?;
        for (name, body) in scenario.seed {
            let path = workspace.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap_or(workspace.path()))?;
            std::fs::write(path, body)?;
        }

        let outcome = run(&rook, home.path(), workspace.path(), scenario.prompt);
        let verdict = outcome.and_then(|turn| {
            // Before what it said: a model that flailed to the step limit and
            // happened to mention the right word has not done the task.
            expect(turn.stopped != "max_steps", "it ran out of steps rather than finishing", &turn)?;
            (scenario.check)(&turn, workspace.path())
        });
        match verdict {
            Ok(()) => println!("{:<34} ok", scenario.name),
            Err(e) => {
                failed += 1;
                println!("{:<34} FAILED — {e}", scenario.name);
                // What the turn actually did: the verdict names the symptom and
                // the transcript names the cause — which tool, with what
                // arguments, answered what. Without it a red run against a
                // small model reads as "the model is small" whether or not the
                // tool it reached for was broken.
                println!("{}", transcript(&rook, home.path()));
            }
        }
    }

    println!("{}", "─".repeat(60));
    match failed {
        0 => {
            println!("smoke: ok");
            Ok(())
        }
        n => bail!("{n} of {} scenarios failed against {model}", SCENARIOS.len()),
    }
}

/// The tool calls and results of the one session in `home`, bounded.
fn transcript(rook: &Path, home: &Path) -> String {
    let listed = Command::new(rook)
        .env("ROOK_HOME", home)
        .env("ROOK_LOG", "error")
        .args(["--json", "session", "ls", "--all"])
        .output();
    let Ok(listed) = listed else { return "    (no transcript: `session ls` failed)".into() };
    let sessions: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap_or_default();
    // The turn's own session, not a checker's: `verify` forks one, and it
    // is listed too.
    let Some(id) = sessions
        .as_array()
        .into_iter()
        .flatten()
        .find(|s| s["parent"].is_null())
        .and_then(|s| s["id"].as_str())
    else {
        return "    (no transcript: no session was recorded)".into();
    };
    let shown = Command::new(rook)
        .env("ROOK_HOME", home)
        .env("ROOK_LOG", "error")
        .args(["session", "show", id, "--limit", "40", "--max-body", "1200"])
        .output();
    match shown {
        Ok(out) => String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .take(60)
            .map(|l| format!("    {l}"))
            .collect::<Vec<_>>()
            .join("\n"),
        Err(e) => format!("    (no transcript: {e})"),
    }
}

fn run(rook: &Path, home: &Path, workspace: &Path, prompt: &str) -> Result<Turn> {
    let out = Command::new(rook)
        .env("ROOK_HOME", home)
        .env("ROOK_LOG", "error")
        .args(["--workspace", &workspace.display().to_string(), "--yes", "--json", "run", prompt])
        .output()
        .context("running a turn")?;
    let text = String::from_utf8_lossy(&out.stdout);
    let printed: serde_json::Value = serde_json::from_str(text.trim())
        .with_context(|| format!("reading the turn: {}{}", text, String::from_utf8_lossy(&out.stderr)))?;
    turn_from(&printed).with_context(|| {
        format!(
            "the turn printed no outcome (exit {}):\n{}{}",
            out.status,
            text.trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        )
    })
}

/// What `rook run --json` prints is `{"session", "outcome", "changes"}`, and
/// the outcome is what a scenario judges. Read from the nested object: the
/// first shape of this read the top level, every field came back empty, and
/// four scenarios failed in CI saying nothing about why.
fn turn_from(printed: &serde_json::Value) -> Result<Turn> {
    let outcome =
        printed.get("outcome").filter(|o| o.is_object()).context("no `outcome` in what was printed")?;
    Ok(Turn {
        reply: outcome["reply"].as_str().unwrap_or_default().to_string(),
        tools: outcome["tools_called"]
            .as_array()
            .map(|a| a.iter().filter_map(|t| t.as_str().map(str::to_string)).collect())
            .unwrap_or_default(),
        stopped: outcome["stopped"].as_str().unwrap_or_default().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::turn_from;

    /// The contract with `rook run --json`, pinned: the outcome is nested,
    /// and a print with none is an error rather than an empty turn.
    #[test]
    fn a_turn_is_read_from_the_nested_outcome_the_cli_prints() {
        let printed = serde_json::json!({
            "session": "01ABC",
            "outcome": { "reply": "done", "stopped": "end_turn", "tools_called": ["read_file"] },
            "changes": null
        });
        let turn = turn_from(&printed).unwrap();
        assert_eq!((turn.reply.as_str(), turn.stopped.as_str()), ("done", "end_turn"));
        assert_eq!(turn.tools, ["read_file"]);

        let flat = serde_json::json!({ "reply": "done", "stopped": "end_turn", "tools_called": [] });
        assert!(
            turn_from(&flat).is_err(),
            "the top-level shape is not the CLI's, and reading it is how every field came back empty"
        );
    }
}
