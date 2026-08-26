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
    }
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
