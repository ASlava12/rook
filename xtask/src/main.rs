//! Repo automation: `cargo xtask <task>`.
//!
//! Kept as a Rust binary rather than a Makefile or a shell script so it behaves
//! the same on Windows as it does on the BSDs — which is the whole point of a
//! four-platform target list.

use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

/// Platforms Rook ships for.
///
/// Tier 1 is built and tested in CI. Tier 2 is built but not tested, because no
/// hosted runner offers it — which is exactly how FreeBSD support rots in other
/// projects, so it is at least kept compiling.
const TARGETS: &[(&str, &str, Tier)] = &[
    ("x86_64-unknown-linux-gnu", "linux", Tier::One),
    ("aarch64-unknown-linux-gnu", "linux", Tier::One),
    ("x86_64-unknown-linux-musl", "linux (static)", Tier::One),
    ("x86_64-apple-darwin", "macos", Tier::One),
    ("aarch64-apple-darwin", "macos", Tier::One),
    ("x86_64-pc-windows-msvc", "windows", Tier::One),
    ("aarch64-pc-windows-msvc", "windows", Tier::Two),
    // Built and tested natively in a VM; see .github/workflows/ci.yml.
    ("x86_64-unknown-freebsd", "freebsd", Tier::One),
    ("aarch64-unknown-freebsd", "freebsd", Tier::Two),
];

#[derive(Clone, Copy, PartialEq)]
enum Tier {
    One,
    Two,
}

#[derive(Parser)]
#[command(name = "xtask", about = "Rook repo automation")]
struct Cli {
    #[command(subcommand)]
    task: Task,
}

#[derive(Subcommand)]
enum Task {
    /// fmt + clippy + test, the same gate CI runs.
    Ci,
    /// Print the supported target matrix.
    Targets,
    /// Check that every target still compiles. Needs the toolchains installed.
    CrossCheck {
        /// Include tier-2 targets.
        #[arg(long)]
        all: bool,
    },
    /// Build release binaries and report their size.
    Dist {
        #[arg(long)]
        target: Option<String>,
    },
    /// Measure what the store's compaction actually achieves.
    Compaction,
    /// Work with the upstream agent sources in `references/`.
    Refs {
        #[command(subcommand)]
        action: RefsCmd,
    },
}

#[derive(Subcommand)]
enum RefsCmd {
    /// Clone the reference sources (shallow). They are not fetched by `git clone`.
    Init { name: Option<String> },
    /// How far each pinned pointer has drifted from upstream, and what landed.
    Status { name: Option<String> },
    /// Move one pointer to the current upstream tip, printing what came in.
    Advance { name: String },
}

struct Reference {
    name: String,
    path: String,
    branch: String,
}

/// Read the submodule table straight from `.gitmodules` so this stays correct
/// when a reference is added without touching xtask.
fn references() -> Result<Vec<Reference>> {
    let out = git(&["config", "-f", ".gitmodules", "--get-regexp", r"^submodule\..*\.path$"])?;
    let mut refs = Vec::new();
    for line in out.lines() {
        let Some((key, path)) = line.split_once(' ') else { continue };
        let module = key.trim_start_matches("submodule.").trim_end_matches(".path");
        let branch =
            git(&["config", "-f", ".gitmodules", &format!("submodule.{module}.branch")]).unwrap_or_default();
        let branch = if branch.trim().is_empty() { "main".into() } else { branch.trim().to_string() };
        let name = path.rsplit('/').next().unwrap_or(path).to_string();
        refs.push(Reference { name, path: path.to_string(), branch });
    }
    refs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(refs)
}

fn git(args: &[&str]) -> Result<String> {
    let out = Command::new("git").args(args).output().context("running git")?;
    if !out.status.success() {
        bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

fn git_in(dir: &str, args: &[&str]) -> Result<String> {
    let mut full = vec!["-C", dir];
    full.extend_from_slice(args);
    git(&full)
}

/// The commit this repository pins the submodule at. Read from the index rather
/// than from HEAD, so a freshly added or advanced pointer reports correctly
/// before it is committed.
fn pinned(path: &str) -> Result<String> {
    let entry = git(&["ls-files", "-s", "--", path])?;
    entry
        .split_whitespace()
        .nth(1)
        .map(str::to_string)
        .with_context(|| format!("{path} is not a tracked submodule"))
}

fn is_cloned(path: &str) -> bool {
    std::path::Path::new(path).join(".git").exists()
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    match Cli::parse().task {
        Task::Ci => {
            cargo(&["fmt", "--all", "--check"])?;
            cargo(&["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"])?;
            cargo(&["test", "--workspace"])?;
            println!("\nci: ok");
            Ok(())
        }
        Task::Targets => {
            println!("{:<32} {:<16} tier", "target", "platform");
            println!("{}", "─".repeat(58));
            for (triple, platform, tier) in TARGETS {
                println!(
                    "{triple:<32} {platform:<16} {}",
                    if *tier == Tier::One { "1 (built + tested)" } else { "2 (built only)" }
                );
            }
            println!(
                "\nEvery dependency is pure Rust except `zstd-sys` and `ring`, which vendor C.\n\
                 Those two build natively anywhere with a working cc — including FreeBSD — but\n\
                 cross-compiling to a foreign OS needs its sysroot, so FreeBSD is built and tested\n\
                 in a real VM in CI rather than cross-checked. See docs/platforms.md."
            );
            Ok(())
        }
        Task::CrossCheck { all } => {
            let mut failed = Vec::new();
            for (triple, _, tier) in TARGETS {
                if *tier == Tier::Two && !all {
                    continue;
                }
                println!("── {triple}");
                if cargo(&["check", "--workspace", "--target", triple]).is_err() {
                    failed.push(*triple);
                }
            }
            if !failed.is_empty() {
                bail!("failed for: {}", failed.join(", "));
            }
            Ok(())
        }
        Task::Dist { target } => {
            let mut args = vec!["build", "--release", "--workspace"];
            if let Some(t) = &target {
                args.push("--target");
                args.push(t);
            }
            cargo(&args)?;
            let dir = match &target {
                Some(t) => format!("target/{t}/release"),
                None => "target/release".into(),
            };
            println!("\nbinary sizes:");
            for name in ["rook", "rookd", "rook.exe", "rookd.exe"] {
                let path = format!("{dir}/{name}");
                if let Ok(meta) = std::fs::metadata(&path) {
                    println!("  {:<28} {:>8.1} MiB", name, meta.len() as f64 / (1024.0 * 1024.0));
                }
            }
            Ok(())
        }
        Task::Compaction => cargo(&["run", "--release", "-p", "rook-store", "--example", "compaction"]),
        Task::Refs { action } => refs(action),
    }
}

fn refs(action: RefsCmd) -> Result<()> {
    let all = references()?;
    let pick = |name: &Option<String>| -> Vec<&Reference> {
        match name {
            Some(n) => all.iter().filter(|r| &r.name == n).collect(),
            None => all.iter().collect(),
        }
    };

    match action {
        RefsCmd::Init { name } => {
            let selected = pick(&name);
            if selected.is_empty() {
                bail!("no reference named {name:?}; try `cargo xtask refs status`");
            }
            for r in selected {
                println!("── {}", r.name);
                git(&["submodule", "update", "--init", "--depth", "1", "--", &r.path])?;
            }
            Ok(())
        }

        RefsCmd::Status { name } => {
            println!("{:<12} {:<10} {:<10} drift", "reference", "pinned", "upstream");
            println!("{}", "─".repeat(52));
            for r in pick(&name) {
                let pin = pinned(&r.path).unwrap_or_default();
                if !is_cloned(&r.path) {
                    println!(
                        "{:<12} {:<10} {:<10} not cloned — `cargo xtask refs init {}`",
                        r.name,
                        short(&pin),
                        "?",
                        r.name
                    );
                    continue;
                }
                let (head, behind) = upstream(r)?;
                println!("{:<12} {:<10} {:<10} {}", r.name, short(&pin), short(&head), behind);
            }
            println!("\nAdvancing a pointer is how upstream work gets triaged; see references/PORTED.md.");
            Ok(())
        }

        RefsCmd::Advance { name } => {
            let r = all
                .iter()
                .find(|r| r.name == name)
                .with_context(|| format!("no reference named {name:?}"))?;
            if !is_cloned(&r.path) {
                bail!("{} is not cloned; run `cargo xtask refs init {}` first", r.name, r.name);
            }
            let pin = pinned(&r.path)?;
            let (head, _) = upstream(r)?;
            if pin == head {
                println!("{} is already at upstream {}", r.name, short(&head));
                return Ok(());
            }
            println!("incoming since {}:", short(&pin));
            match git_in(&r.path, &["log", "--oneline", "--no-merges", &format!("{pin}..{head}")]) {
                Ok(log) if !log.is_empty() => println!("{log}"),
                _ => println!("  (history too shallow to list; fetch deeper to see it)"),
            }
            git_in(&r.path, &["checkout", "--detach", &head])?;
            git(&["add", &r.path])?;
            println!(
                "\n{} advanced to {} — staged. Triage the above into references/PORTED.md",
                r.name,
                short(&head)
            );
            Ok(())
        }
    }
}

/// Upstream tip and how far the pinned commit is behind it.
///
/// The clones are shallow, so a deeper fetch is needed before the two commits
/// share enough history to be counted. When even that is not enough, say so
/// rather than reporting a wrong number.
fn upstream(r: &Reference) -> Result<(String, String)> {
    let remote = format!("origin/{}", r.branch);
    git_in(&r.path, &["fetch", "--quiet", "--depth", "200", "origin", &r.branch])
        .or_else(|_| git_in(&r.path, &["fetch", "--quiet", "origin", &r.branch]))?;
    let head = git_in(&r.path, &["rev-parse", &remote])
        .or_else(|_| git_in(&r.path, &["rev-parse", "FETCH_HEAD"]))?;
    let pin = pinned(&r.path)?;
    if pin == head {
        return Ok((head, "up to date".into()));
    }
    let drift = match git_in(&r.path, &["rev-list", "--count", &format!("{pin}..{head}")]) {
        Ok(n) => format!("{n} commits behind"),
        Err(_) => "behind (history too shallow to count)".into(),
    };
    Ok((head, drift))
}

fn short(sha: &str) -> String {
    sha.chars().take(9).collect()
}

fn cargo(args: &[&str]) -> Result<()> {
    let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(args)
        .status()
        .with_context(|| format!("running cargo {}", args.join(" ")))?;
    if !status.success() {
        bail!("cargo {} failed", args.join(" "));
    }
    Ok(())
}
