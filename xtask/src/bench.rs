//! Measuring whether a checklist tool earns its keep.
//!
//! [ADR-0010](../../docs/adr/0010-no-todo-tool.md) declined a planning tool on
//! somebody else's benchmark — k=3 on one harness, four models, and a finding
//! that the instructions rather than the tool were doing the work. The ADR says
//! plainly that a future model may need re-measuring rather than assuming. This
//! is the harness that does the re-measuring here.
//!
//! Three arms, one variable each:
//!
//! - `plan-line`: the default. A sentence of plan asked for, a checklist
//!   forbidden.
//! - `nothing`: neither. The control, without which a difference between the
//!   other two says nothing about whether planning helps at all.
//! - `todo-tool`: a `plan` tool and instructions to keep the list current, in
//!   place of the line.
//!
//! Every task is multi-step and scored by looking at the workspace afterwards,
//! never by reading what the model said about it: a model that reports success
//! is the thing being measured, not the measurement.

use std::path::Path;
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result, bail};

struct Task {
    name: &'static str,
    seed: &'static [(&'static str, &'static str)],
    prompt: &'static str,
    /// True when the workspace says the task was done. Given the workspace
    /// only: what the model claimed is not evidence.
    done: fn(&Path) -> bool,
}

fn reads(root: &Path, name: &str) -> String {
    std::fs::read_to_string(root.join(name)).unwrap_or_default()
}

const TASKS: &[Task] = &[
    Task {
        name: "three todos",
        seed: &[
            (
                "src/parse.py",
                "def parse_port(line):\n    # TODO: raise ValueError when there is no colon\n    \
                 return int(line.split(\":\")[1])\n\n\
                 def parse_host(line):\n    # TODO: strip surrounding whitespace\n    \
                 return line.split(\":\")[0]\n",
            ),
            (
                "src/store.py",
                "def save(rows):\n    # TODO: raise ValueError when rows is empty\n    \
                 with open(\"out.txt\", \"w\") as f:\n        for r in rows:\n            \
                 f.write(str(r) + \"\\n\")\n",
            ),
        ],
        prompt: "Fix every TODO under src/. Make the change each comment asks for and remove the \
                 comment. Do not change anything else.",
        done: |root| {
            let parse = reads(root, "src/parse.py");
            let store = reads(root, "src/store.py");
            !parse.contains("TODO")
                && !store.contains("TODO")
                && parse.contains("ValueError")
                && parse.contains("strip()")
                && store.contains("ValueError")
        },
    },
    Task {
        name: "rename across files",
        seed: &[
            ("lib.py", "def fetch_rows(source):\n    return list(source)\n"),
            (
                "app.py",
                "from lib import fetch_rows\n\n\
                 def main():\n    rows = fetch_rows([1, 2])\n    print(len(fetch_rows(rows)))\n",
            ),
            (
                "test_app.py",
                "from lib import fetch_rows\n\ndef test_it():\n    assert fetch_rows([]) == []\n",
            ),
        ],
        prompt: "Rename the function `fetch_rows` to `load_rows` everywhere it appears, including \
                 every import and call site. Change nothing else.",
        done: |root| {
            let files = ["lib.py", "app.py", "test_app.py"];
            let all: String = files.iter().map(|f| reads(root, f)).collect();
            !all.contains("fetch_rows")
                && reads(root, "lib.py").contains("def load_rows")
                && reads(root, "app.py").matches("load_rows").count() >= 3
                && reads(root, "test_app.py").contains("load_rows")
        },
    },
    Task {
        name: "make the check pass",
        seed: &[
            ("total.py", "def total(rows):\n    return sum(rows) + 1\n"),
            (
                "check.py",
                "from total import total\n\n\
                 assert total([1, 2, 3]) == 6, total([1, 2, 3])\n\
                 assert total([]) == 0, total([])\n\
                 print(\"ok\")\n",
            ),
        ],
        prompt: "`python3 check.py` fails. Find out why, fix the code it checks, and run it again \
                 until it prints ok.",
        done: |root| {
            Command::new("python3")
                .arg("check.py")
                .current_dir(root)
                .output()
                .is_ok_and(|out| out.status.success())
        },
    },
    // The three above are the floor: a capable model passes them, so what they
    // measure is cost. These three have somewhere to go wrong, which is where
    // the reference found the tool bought nothing and cost a closure loop.
    Task {
        name: "rename, not the neighbour",
        seed: &[
            (
                "lib.py",
                "def fetch_rows(source):\n    return list(source)\n\n\
                 def fetch_rows_cached(source):\n    return fetch_rows(source)\n",
            ),
            (
                "app.py",
                "from lib import fetch_rows, fetch_rows_cached\n\n\
                 def main():\n    return fetch_rows([1]) + fetch_rows_cached([2])\n",
            ),
        ],
        prompt: "Rename the function `fetch_rows` to `load_rows`, and only that function. \
                 `fetch_rows_cached` is a different function and keeps its name. Update every \
                 import and call site.",
        done: |root| {
            let lib = reads(root, "lib.py");
            let app = reads(root, "app.py");
            lib.contains("def load_rows(")
                && lib.contains("def fetch_rows_cached(")
                && app.contains("load_rows")
                && app.contains("fetch_rows_cached")
                // The trap: a blind replace renames the neighbour too, and a
                // blind guard against that leaves the original name behind.
                && !lib.contains("def fetch_rows(")
                && !app.contains("import fetch_rows,")
                && !lib.contains("load_rows_cached")
                && !app.contains("load_rows_cached")
        },
    },
    Task {
        name: "the cause is elsewhere",
        seed: &[
            ("util.py", "def clamp(n):\n    return max(1, n)\n"),
            ("total.py", "from util import clamp\n\ndef total(rows):\n    return clamp(sum(rows))\n"),
            (
                "check.py",
                "from total import total\n\n\
                 assert total([1, 2, 3]) == 6, total([1, 2, 3])\n\
                 assert total([]) == 0, total([])\n\
                 print(\"ok\")\n",
            ),
        ],
        prompt: "`python3 check.py` fails. Find the cause and fix it. check.py states what is \
                 required and must not be edited.",
        done: |root| {
            // Passing is not enough: the cheap way through is to weaken the
            // check, and a fix that edits the thing asking the question has
            // answered a different one.
            let untouched = reads(root, "check.py").contains("assert total([]) == 0");
            untouched
                && Command::new("python3")
                    .arg("check.py")
                    .current_dir(root)
                    .output()
                    .is_ok_and(|out| out.status.success())
        },
    },
    Task {
        name: "one is already right",
        seed: &[
            (
                "shape.py",
                "def width(box):\n    # TODO: return 0 for a negative width\n    \
                 return max(0, box[\"w\"])\n\n\
                 def height(box):\n    # TODO: return 0 for a negative height\n    \
                 return box[\"h\"]\n",
            ),
            (
                "check.py",
                "from shape import width, height\n\n\
                 assert width({\"w\": -3}) == 0, width({\"w\": -3})\n\
                 assert width({\"w\": 4}) == 4, width({\"w\": 4})\n\
                 assert height({\"h\": -3}) == 0, height({\"h\": -3})\n\
                 assert height({\"h\": 4}) == 4, height({\"h\": 4})\n\
                 print(\"ok\")\n",
            ),
        ],
        prompt: "Two TODOs in shape.py. Make each true and remove the comment. One of them may \
                 already be satisfied — check before you change it. `python3 check.py` must print \
                 ok when you are done.",
        done: |root| {
            let shape = reads(root, "shape.py");
            !shape.contains("TODO")
                && reads(root, "check.py").contains("assert height({\"h\": 4}) == 4")
                && Command::new("python3")
                    .arg("check.py")
                    .current_dir(root)
                    .output()
                    .is_ok_and(|out| out.status.success())
        },
    },
];

struct Arm {
    name: &'static str,
    config: &'static str,
}

const ARMS: &[Arm] = &[
    Arm { name: "plan-line", config: "plan_first = true\ntodo_tool = false\n" },
    Arm { name: "nothing", config: "plan_first = false\ntodo_tool = false\n" },
    Arm { name: "todo-tool", config: "plan_first = false\ntodo_tool = true\n" },
];

/// One run's numbers. Cost is what the reference found the tool spent, so it is
/// measured beside the pass rather than instead of it.
#[derive(Default)]
struct Run {
    passed: bool,
    steps: u64,
    input_tokens: u64,
    output_tokens: u64,
    seconds: f64,
}

pub fn bench(model: Option<String>, repeats: usize, only: Option<String>) -> Result<()> {
    let model = model
        .or_else(|| std::env::var("ROOK_BENCH_MODEL").ok())
        .filter(|m| !m.trim().is_empty())
        .context("no model to measure — pass --model, or set ROOK_BENCH_MODEL")?;
    let built = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["build", "-p", "rook-cli"])
        .status()
        .context("building rook")?;
    if !built.success() {
        bail!("rook did not build");
    }
    let rook = Path::new("target/debug/rook").canonicalize().context("finding the built rook")?;
    let arms: Vec<&Arm> = match &only {
        Some(want) => ARMS.iter().filter(|a| want.split(',').any(|w| w.trim() == a.name)).collect(),
        None => ARMS.iter().collect(),
    };
    if arms.is_empty() {
        bail!(
            "no arm called {only:?}; the arms are: {}",
            ARMS.iter().map(|a| a.name).collect::<Vec<_>>().join(", ")
        );
    }

    println!("{model}, {repeats} run(s) per task per arm\n");
    let mut table: Vec<(String, String, Vec<Run>)> = Vec::new();
    for arm in &arms {
        for task in TASKS {
            let mut runs = Vec::new();
            for n in 0..repeats {
                let run = once(&rook, &model, arm, task)?;
                println!(
                    "  {:<11} {:<22} #{n}  {}  {} steps, {} in / {} out, {:.0}s",
                    arm.name,
                    task.name,
                    if run.passed { "pass" } else { "FAIL" },
                    run.steps,
                    run.input_tokens,
                    run.output_tokens,
                    run.seconds
                );
                runs.push(run);
            }
            table.push((arm.name.into(), task.name.into(), runs));
        }
    }
    report(&table);
    Ok(())
}

/// One task, one arm, one fresh workspace and store.
fn once(rook: &Path, model: &str, arm: &Arm, task: &Task) -> Result<Run> {
    let home = tempfile::tempdir()?;
    let workspace = tempfile::tempdir()?;
    // A window large enough that this measures planning rather than
    // compaction, and a step ceiling that ends a wanderer without ending a
    // worker.
    std::fs::write(
        home.path().join("config.toml"),
        format!("[agent]\nmodel = \"{model}\"\nmax_steps = 30\ninstall_servers = false\n{}", arm.config),
    )?;
    for (name, body) in task.seed {
        let path = workspace.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap_or(workspace.path()))?;
        std::fs::write(path, body)?;
    }

    let started = Instant::now();
    let out = Command::new(rook)
        .env("ROOK_HOME", home.path())
        .env("ROOK_LOG", "error")
        .args(["--workspace", &workspace.path().display().to_string(), "--yes", "--json", "run", task.prompt])
        .output()
        .context("running a turn")?;
    let seconds = started.elapsed().as_secs_f64();
    let printed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    let turn = printed.get("outcome").unwrap_or(&printed);
    // The workspace decides, not the turn: a run that ended badly and left the
    // work done is a pass, and one that reported success and left it undone is
    // not.
    let passed = (task.done)(workspace.path());
    if !passed {
        // A failure that deletes its own evidence can only be rerun, and a
        // rerun of a model is a different run. The one defect this harness has
        // found so far was visible in a transcript nobody had asked it to keep.
        let (home, workspace) = (home.keep(), workspace.keep());
        println!(
            "  {} × {} failed — ROOK_HOME={} {} session show, workspace {}",
            arm.name,
            task.name,
            home.display(),
            rook.display(),
            workspace.display()
        );
    }
    Ok(Run {
        passed,
        steps: turn["steps"].as_u64().unwrap_or(0),
        input_tokens: turn["input_tokens"].as_u64().unwrap_or(0),
        output_tokens: turn["output_tokens"].as_u64().unwrap_or(0),
        seconds,
    })
}

fn report(table: &[(String, String, Vec<Run>)]) {
    println!("\n{:<11} {:<22} {:>7} {:>9} {:>9} {:>7}", "arm", "task", "passed", "steps", "tokens", "secs");
    println!("{}", "─".repeat(70));
    let mut by_arm: Vec<(String, usize, usize, u64, u64, f64)> = Vec::new();
    for (arm, task, runs) in table {
        let passed = runs.iter().filter(|r| r.passed).count();
        let steps = mean(runs.iter().map(|r| r.steps as f64));
        let tokens = mean(runs.iter().map(|r| (r.input_tokens + r.output_tokens) as f64));
        let secs = mean(runs.iter().map(|r| r.seconds));
        println!("{arm:<11} {task:<22} {passed:>4}/{:<2} {steps:>9.1} {tokens:>9.0} {secs:>7.0}", runs.len());
        let row = by_arm.iter_mut().find(|(name, ..)| name == arm);
        let entry = match row {
            Some(entry) => entry,
            None => {
                by_arm.push((arm.clone(), 0, 0, 0, 0, 0.0));
                by_arm.last_mut().expect("just pushed")
            }
        };
        entry.1 += passed;
        entry.2 += runs.len();
        entry.3 += runs.iter().map(|r| r.input_tokens + r.output_tokens).sum::<u64>();
        entry.4 += runs.iter().map(|r| r.steps).sum::<u64>();
        entry.5 += runs.iter().map(|r| r.seconds).sum::<f64>();
    }
    println!("\n{:<11} {:>7} {:>12} {:>9} {:>8}", "arm", "passed", "tokens", "steps", "secs");
    println!("{}", "─".repeat(52));
    for (arm, passed, of, tokens, steps, secs) in &by_arm {
        println!("{arm:<11} {passed:>4}/{of:<2} {tokens:>12} {steps:>9} {secs:>8.0}");
    }
    println!(
        "\nThe workspace decides each pass, never what the model said about it.\n\
         Cost is beside the pass because that is what the tool was declined for."
    );
}

fn mean(xs: impl Iterator<Item = f64>) -> f64 {
    let (sum, n) = xs.fold((0.0, 0usize), |(s, n), x| (s + x, n + 1));
    match n {
        0 => 0.0,
        n => sum / n as f64,
    }
}
