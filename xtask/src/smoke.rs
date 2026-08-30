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
    std::fs::write(home.path().join("config.toml"), format!("[agent]\nmodel = \"{model}\"\n"))?;
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
        std::fs::write(home.path().join("config.toml"), format!("[agent]\nmodel = \"{model}\"\n"))?;
        for (name, body) in scenario.seed {
            std::fs::write(workspace.path().join(name), body)?;
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

fn run(rook: &Path, home: &Path, workspace: &Path, prompt: &str) -> Result<Turn> {
    let out = Command::new(rook)
        .env("ROOK_HOME", home)
        .env("ROOK_LOG", "error")
        .args(["--workspace", &workspace.display().to_string(), "--yes", "--json", "run", prompt])
        .output()
        .context("running a turn")?;
    let text = String::from_utf8_lossy(&out.stdout);
    let turn: serde_json::Value = serde_json::from_str(text.trim())
        .with_context(|| format!("reading the turn: {}{}", text, String::from_utf8_lossy(&out.stderr)))?;
    Ok(Turn {
        reply: turn["reply"].as_str().unwrap_or_default().to_string(),
        tools: turn["tools_called"]
            .as_array()
            .map(|a| a.iter().filter_map(|t| t.as_str().map(str::to_string)).collect())
            .unwrap_or_default(),
        stopped: turn["stopped"].as_str().unwrap_or_default().to_string(),
    })
}
