//! Repo automation: `cargo xtask <task>`.
//! Kept as a Rust binary rather than a Makefile or a shell script so it behaves
//! the same on Windows as it does on the BSDs — which is the whole point of a
//! four-platform target list.

mod bench;
mod smoke;

use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

/// Platforms Rook ships for, and what CI actually does with each.
///
/// The last column is the string in `.github/workflows/ci.yml` that backs the
/// claim; `tests/ci_matrix.rs` looks for it. FreeBSD support in other projects
/// rots because a matrix outlives the job that justified it, and nothing notices.
const TARGETS: &[Target] = &[
    Target::tested("x86_64-unknown-linux-gnu", "linux", "ubuntu-latest"),
    Target::tested("aarch64-apple-darwin", "macos", "macos-latest"),
    Target::tested("x86_64-pc-windows-msvc", "windows", "windows-latest"),
    Target::tested("x86_64-unknown-freebsd", "freebsd", "freebsd-vm"),
    Target::checked("aarch64-unknown-linux-gnu", "linux"),
    Target::checked("x86_64-unknown-linux-musl", "linux (static)"),
    Target::checked("x86_64-apple-darwin", "macos"),
    Target::untried("aarch64-pc-windows-msvc", "windows"),
    Target::untried("aarch64-unknown-freebsd", "freebsd"),
];

struct Target {
    triple: &'static str,
    platform: &'static str,
    coverage: Coverage,
}

#[derive(Clone, Copy, PartialEq)]
enum Coverage {
    /// A CI job runs the whole suite on it, on the named runner.
    Tested(&'static str),
    /// A CI job compiles it. Cross-checking never links against the target's
    /// libc, which is the whole reason FreeBSD gets a VM instead.
    Checked,
    /// Supported, but no hosted runner offers it. Best effort.
    Untried,
}

impl Target {
    const fn tested(triple: &'static str, platform: &'static str, runner: &'static str) -> Self {
        Self { triple, platform, coverage: Coverage::Tested(runner) }
    }
    const fn checked(triple: &'static str, platform: &'static str) -> Self {
        Self { triple, platform, coverage: Coverage::Checked }
    }
    const fn untried(triple: &'static str, platform: &'static str) -> Self {
        Self { triple, platform, coverage: Coverage::Untried }
    }

    fn describe_ci(&self) -> String {
        match self.coverage {
            Coverage::Tested(_) => format!("tested on {}", self.witness()),
            Coverage::Checked => "compiled".into(),
            Coverage::Untried => "best effort".into(),
        }
    }

    /// What must appear in the workflow for `describe_ci` to be true — a cross-
    /// checked target is named there by its own triple.
    fn witness(&self) -> &'static str {
        match self.coverage {
            Coverage::Tested(runner) => runner,
            Coverage::Checked => self.triple,
            Coverage::Untried => "",
        }
    }
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
    /// Compile the targets CI cross-checks. Needs the toolchains installed.
    CrossCheck {
        /// Include the targets CI does not build either.
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
    /// Report what `target/` is costing and reclaim it.
    Clean {
        /// Remove everything, not just the parts that regenerate cheaply.
        #[arg(long)]
        all: bool,
    },
    /// Run a few real turns against a real model, and check what came back.
    Smoke {
        /// `provider/model`, else `ROOK_SMOKE_MODEL`, else the configured one.
        #[arg(long)]
        model: Option<String>,
    },
    /// Measure whether a checklist tool earns its keep, against ADR-0010.
    Bench {
        /// `provider/model`, else `ROOK_BENCH_MODEL`.
        #[arg(long)]
        model: Option<String>,
        /// Runs per task per arm. Three is what the reference used.
        #[arg(long, default_value_t = 3)]
        repeats: usize,
        /// Only these arms, comma separated: plan-line, nothing, todo-tool.
        #[arg(long)]
        arms: Option<String>,
    },
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
            println!("{:<32} {:<16} ci", "target", "platform");
            println!("{}", "─".repeat(58));
            for t in TARGETS {
                println!("{:<32} {:<16} {}", t.triple, t.platform, t.describe_ci());
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
            let wanted = |c: Coverage| c == Coverage::Checked || (all && c == Coverage::Untried);
            let mut failed = Vec::new();
            for t in TARGETS.iter().filter(|t| wanted(t.coverage)) {
                println!("── {}", t.triple);
                if cargo(&["check", "--workspace", "--target", t.triple]).is_err() {
                    failed.push(t.triple);
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

            // Next to the binary, which is the first place `builtin_skills_dir`
            // looks. Without this a release ships an agent with no skills at
            // all, and nothing in a dev build would ever notice.
            let skills = std::path::Path::new(&dir).join("skills");
            let _ = std::fs::remove_dir_all(&skills);
            copy_tree(std::path::Path::new("skills"), &skills)?;
            println!("packaged {} built-in skill(s)", count_dirs(&skills));

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
        Task::Clean { all } => clean(all),
        Task::Smoke { model } => smoke::smoke(model),
        Task::Bench { model, repeats, arms } => bench::bench(model, repeats, arms),
        Task::Refs { action } => refs(action),
    }
}

fn copy_tree(from: &std::path::Path, to: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn count_dirs(path: &std::path::Path) -> usize {
    std::fs::read_dir(path).into_iter().flatten().flatten().filter(|e| e.path().is_dir()).count()
}

/// Incremental state and cross-target artifacts dominate `target/` and rebuild
/// cheaply, so they go first; `--all` is for when the disk is actually full.
fn clean(all: bool) -> Result<()> {
    let before = dir_size(std::path::Path::new("target"));
    if all {
        cargo(&["clean"])?;
    } else {
        for path in ["target/debug/incremental", "target/release/incremental"] {
            let _ = std::fs::remove_dir_all(path);
        }
        for entry in std::fs::read_dir("target").into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.contains('-') && entry.path().is_dir() {
                println!("removing cross-target artifacts: {name}");
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
    let after = dir_size(std::path::Path::new("target"));
    println!("target/: {} -> {} ({} reclaimed)", gib(before), gib(after), gib(before - after));
    if !all && after > 4 << 30 {
        println!("still large — `cargo xtask clean --all` removes the rest");
    }
    Ok(())
}

/// What the disk actually gives up, which is not what the file lengths add up
/// to: cargo hardlinks one built artifact into several places, so summing
/// lengths counted a `target/` directory at more than twice its real size — and
/// the whole point of this command is to answer "how much will I get back".
fn dir_size(path: &std::path::Path) -> u64 {
    let mut counted = std::collections::HashSet::new();
    walk(path, &mut counted)
}

type Inode = (u64, u64);

fn walk(path: &std::path::Path, counted: &mut std::collections::HashSet<Inode>) -> u64 {
    let mut total = 0;
    for entry in std::fs::read_dir(path).into_iter().flatten().flatten() {
        match entry.metadata() {
            Ok(m) if m.is_dir() => total += walk(&entry.path(), counted),
            Ok(m) => total += charge(&m, counted),
            Err(_) => {}
        }
    }
    total
}

#[cfg(unix)]
fn charge(m: &std::fs::Metadata, counted: &mut std::collections::HashSet<Inode>) -> u64 {
    use std::os::unix::fs::MetadataExt;
    if m.nlink() > 1 && !counted.insert((m.dev(), m.ino())) {
        return 0;
    }
    m.blocks() * 512
}

#[cfg(not(unix))]
fn charge(m: &std::fs::Metadata, _counted: &mut std::collections::HashSet<Inode>) -> u64 {
    m.len()
}

fn gib(bytes: u64) -> String {
    format!("{:.2} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
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
            // Forced because a reference is read-only to us and nothing in one
            // is ours to keep. It is not hypothetical: hermes tracks two paths
            // that differ only in case, which a case-insensitive filesystem
            // cannot hold both of, so its tree is dirty the moment it is
            // checked out — and an ordinary checkout then refuses, at the tail
            // of a two-hundred-line log where nobody is looking.
            git_in(&r.path, &["checkout", "--detach", "--force", &head])?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn workflow() -> String {
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../.github/workflows/ci.yml"))
            .expect("the workflow the target matrix claims to describe")
    }

    #[test]
    fn every_target_gets_the_coverage_the_matrix_claims_for_it() {
        let ci = workflow();
        for t in TARGETS {
            match t.coverage {
                Coverage::Untried => assert!(
                    !ci.contains(t.triple),
                    "{} is in the workflow, so the matrix understates it as {}",
                    t.triple,
                    t.describe_ci()
                ),
                _ => assert!(
                    ci.contains(t.witness()),
                    "the matrix says {} is {}, but `{}` is nowhere in ci.yml",
                    t.triple,
                    t.describe_ci(),
                    t.witness()
                ),
            }
        }
    }

    /// cargo hardlinks one built artifact into `deps/` and the profile root, so
    /// charging for every link reported `target/` at more than twice its size —
    /// in a command whose entire job is to say how much the disk will get back.
    #[cfg(unix)]
    #[test]
    fn a_hardlinked_artifact_costs_the_disk_once() {
        let dir = tempfile::tempdir().unwrap();
        let artifact = dir.path().join("libfoo.rlib");
        std::fs::write(&artifact, vec![7u8; 64 * 1024]).unwrap();
        std::fs::create_dir(dir.path().join("deps")).unwrap();
        std::fs::hard_link(&artifact, dir.path().join("deps/libfoo.rlib")).unwrap();

        let size = dir_size(dir.path());
        assert!(
            (64 * 1024..96 * 1024).contains(&size),
            "one 64 KiB artifact under two names should cost about 64 KiB, not {size} bytes"
        );
    }

    #[test]
    fn the_gate_is_defined_once_and_ci_runs_that_one() {
        assert!(
            workflow().contains("cargo xtask ci"),
            "ci.yml spells the gate out itself instead of running `cargo xtask ci`, \
             so the two can drift"
        );
    }
}
