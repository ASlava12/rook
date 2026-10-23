//! Compiler-checked, explicitly indexed repository documentation.

mod cargo_protocol;
mod inputs;
mod markdown;

use anyhow::{Context, Result, ensure};
use clap::Args;
use inputs::{Article, Inputs, hash};
use markdown::Expectation;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

#[derive(Args)]
pub struct Options {
    #[arg(long, default_value = "docs/examples/checks.json")]
    index: String,
    /// Article IDs, comma separated. Without this, check every indexed article.
    #[arg(long, value_delimiter = ',')]
    only: Vec<String>,
    /// Execute compiled main programs and test harnesses, except no_run blocks.
    #[arg(long)]
    run: bool,
    /// Write a JSON receipt to a new file, including on validation failure.
    #[arg(long)]
    output: Option<PathBuf>,
    /// Parent for a fresh temporary workspace; existing contents are retained.
    #[arg(long)]
    work_root: Option<PathBuf>,
    #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u64).range(1..=600))]
    compile_seconds: u64,
    #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u64).range(1..=60))]
    runtime_seconds: u64,
    #[arg(long, default_value_t = 600, value_parser = clap::value_parser!(u64).range(1..=3600))]
    total_seconds: u64,
    #[arg(long, default_value_t = 524_288, value_parser = clap::value_parser!(u64).range(4096..=4_194_304))]
    capture_bytes: u64,
}

struct Checker<'a> {
    options: &'a Options,
    started: Instant,
    cargo: std::ffi::OsString,
    commands: Vec<Value>,
    blocks: Vec<Value>,
    /// Owned generated input files and already admitted executable artifacts.
    sealed: BTreeMap<PathBuf, String>,
}

fn bounded_text(bytes: &[u8], limit: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].into()
}

fn file_hash(root: &Path, path: &Path, limit: usize) -> Result<String> {
    let root = cargo_protocol::portable(root).context("invalid owned root")?;
    let path = cargo_protocol::portable(path).context("invalid owned path")?;
    let relative = path.strip_prefix(&root).context("artifact escaped owned directory")?;
    let relative = relative.to_str().context("non-UTF-8 generated path")?.replace('\\', "/");
    let checked = inputs::regular(&root, &relative)?;
    let mut file = File::open(checked)?;
    let length = file.metadata()?.len();
    ensure!(length <= limit as u64, "owned file exceeds hashing limit");
    let mut bytes = Vec::new();
    (&mut file).take(limit as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= limit && bytes.len() as u64 == length, "owned file changed size while hashing");
    Ok(hash(&bytes))
}

impl Checker<'_> {
    fn verify_owned(&self, root: &Path) -> Result<()> {
        for (path, expected) in &self.sealed {
            ensure!(
                file_hash(root, path, 64 * 1024 * 1024)? == *expected,
                "owned input/artifact changed: {}",
                path.display()
            );
        }
        Ok(())
    }

    fn patience(&self, seconds: u64) -> Result<Duration> {
        let remaining = Duration::from_secs(self.options.total_seconds)
            .checked_sub(self.started.elapsed())
            .context("documentation check admission budget exhausted")?;
        ensure!(!remaining.is_zero(), "documentation check admission budget exhausted");
        Ok(Duration::from_secs(seconds).min(remaining))
    }

    fn command(
        &mut self,
        root: &Path,
        command: &mut Command,
        phase: &str,
        seconds: u64,
    ) -> Result<std::process::Output> {
        self.verify_owned(root)?;
        let patience = self.patience(seconds)?;
        let result =
            rook_contain::captured_with_cleanup(command, patience, self.options.capture_bytes as usize);
        match result {
            Ok(captured) => {
                self.commands.push(json!({"phase":phase,"exit_code":captured.output.status.code(),"cleanup_signalled":captured.cleanup_signalled,"observed_group_alive":captured.observed_group_alive,"stdout_bytes":captured.output.stdout.len(),"stderr_bytes":captured.output.stderr.len()}));
                self.verify_owned(root)?;
                Ok(captured.output)
            }
            Err(error) => {
                self.commands.push(
                    json!({"phase":phase,"capture_error":bounded_text(error.to_string().as_bytes(),2048)}),
                );
                Err(error).with_context(|| format!("{phase}: process/capture failed"))
            }
        }
    }

    fn cargo_command(&self, dir: &Path, target: &Path) -> Command {
        let mut command = Command::new(&self.cargo);
        command
            .current_dir(dir)
            .env("CARGO_TARGET_DIR", target)
            .env("CARGO_NET_OFFLINE", "true")
            .env("CARGO_TERM_COLOR", "never");
        command
    }

    fn tool_version(&mut self, root: &Path, tool: &str) -> Result<String> {
        let mut command = if tool == "cargo" {
            Command::new(&self.cargo)
        } else {
            Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
        };
        command.arg("--version").current_dir(root);
        let output = self.command(root, &mut command, tool, self.options.compile_seconds)?;
        ensure!(output.status.success(), "{tool} version failed");
        Ok(bounded_text(&output.stdout, 1024).trim().into())
    }

    fn write_sealed(&mut self, root: &Path, path: PathBuf, bytes: &[u8]) -> Result<()> {
        fs::write(&path, bytes)?;
        let digest = file_hash(root, &path, 512 * 1024)?;
        ensure!(digest == hash(bytes), "generated input changed while writing");
        self.sealed.insert(path, digest);
        Ok(())
    }

    fn article(&mut self, root: &Path, article: &Article) -> Result<()> {
        let dir = root.join(&article.id);
        fs::create_dir(&dir)?;
        let examples = dir.join("examples");
        fs::create_dir(&examples)?;
        let target = dir.join("target");
        fs::create_dir(&target)?;
        let manifest = dir.join("Cargo.toml");
        self.write_sealed(root, manifest.clone(), &article.manifest.bytes)?;
        self.write_sealed(root, dir.join("Cargo.lock"), &article.lock.bytes)?;
        let mut expected = BTreeMap::new();
        for (index, block) in article.blocks.iter().enumerate() {
            if block.expectation == Expectation::Ignore {
                continue;
            }
            let name = format!("block_{index}");
            let path = examples.join(format!("{name}.rs"));
            let code = markdown::program(block);
            self.write_sealed(root, path.clone(), code.as_bytes())?;
            expected.insert(name, path);
        }
        let mut metadata = self.cargo_command(&dir, &target);
        metadata.args(["metadata", "--format-version", "1", "--locked", "--offline"]);
        let output = self.command(
            root,
            &mut metadata,
            &format!("{}:metadata", article.id),
            self.options.compile_seconds,
        )?;
        ensure!(
            output.status.success(),
            "{}: locked/offline metadata failed: {}",
            article.id,
            bounded_text(&output.stderr, 2048)
        );
        let meta: Value = serde_json::from_slice(&output.stdout).context("invalid Cargo metadata")?;
        let package_id = metadata_identity(&meta, article, &dir, &target, &expected)?;
        for (index, block) in article.blocks.iter().enumerate() {
            let mut record = json!({"article":article.id,"path":article.article.relative,"line":block.line,"source_sha256":article.article.sha256,"block":index,"code_sha256":hash(block.code.as_bytes()),"no_run":block.no_run});
            if block.expectation == Expectation::Ignore {
                record["status"] = json!("ignored");
                self.blocks.push(record);
                continue;
            }
            record["status"] = json!("incomplete");
            self.blocks.push(record);
            let record_index = self.blocks.len() - 1;
            let name = format!("block_{index}");
            let identity = cargo_protocol::Identity {
                package_id: package_id.clone(),
                manifest: manifest.clone(),
                source: expected[&name].clone(),
                target_root: target.clone(),
                name,
                source_lines: block.code.lines().count(),
            };
            let required = match &block.expectation {
                Expectation::CompileFail { codes } => Some(codes),
                _ => None,
            };
            let modes: &[cargo_protocol::BuildMode] = if required.is_some() {
                &[cargo_protocol::BuildMode::Test]
            } else {
                &[cargo_protocol::BuildMode::Main, cargo_protocol::BuildMode::Test]
            };
            let mut artifacts = Vec::new();
            for mode in modes {
                let mut command = self.cargo_command(&dir, &target);
                if *mode == cargo_protocol::BuildMode::Main {
                    command.arg("build");
                } else {
                    command.args(["test", "--no-run"]);
                }
                command.args(["--locked", "--offline", "--message-format=json", "--example", &identity.name]);
                let phase = format!("{}:{}:{mode:?}", article.id, identity.name);
                let output = self.command(root, &mut command, &phase, self.options.compile_seconds)?;
                let evidence =
                    cargo_protocol::inspect(&output.stdout, output.status.code(), &identity, *mode, required)
                        .map_err(anyhow::Error::msg)
                        .with_context(|| format!("{phase}: {}", bounded_text(&output.stderr, 2048)))?;
                self.blocks[record_index]["diagnostics"] = json!(
                    evidence
                        .diagnostics
                        .iter()
                        .map(|d| json!({"code":d.code,"article_line":block.line+d.line,"message":d.message}))
                        .collect::<Vec<_>>()
                );
                self.blocks[record_index]["codes"] = json!(evidence.codes);
                self.blocks[record_index]["source_errors"] = json!(evidence.source_errors);
                self.blocks[record_index]["source_warnings"] = json!(evidence.source_warnings);
                if let Some(artifact) = evidence.artifact {
                    let digest = file_hash(root, &artifact, 64 * 1024 * 1024)?;
                    ensure!(
                        !artifacts.iter().any(|(path, _)| path == &artifact),
                        "main/test artifacts must be distinct"
                    );
                    self.sealed.insert(artifact.clone(), digest.clone());
                    artifacts.push((artifact, digest));
                }
            }
            self.blocks[record_index]["artifacts"] = json!(
                artifacts
                    .iter()
                    .map(|(path, digest)| json!({"path":path,"sha256":digest}))
                    .collect::<Vec<_>>()
            );
            self.blocks[record_index]["status"] = json!(if required.is_some() {
                "expected_compiler_failure"
            } else if block.no_run {
                "no_run"
            } else {
                "compiled_main_and_test"
            });
            if self.options.run && !block.no_run && required.is_none() {
                self.blocks[record_index]["status"] = json!("incomplete_runtime");
                for (number, (artifact, _)) in artifacts.iter().enumerate() {
                    let working = dir.join(format!("run_{index}_{number}"));
                    fs::create_dir(&working)?;
                    let mut command = Command::new(artifact);
                    command.current_dir(&working);
                    let output = self.command(
                        root,
                        &mut command,
                        &format!("{}:{}:run_{number}", article.id, identity.name),
                        self.options.runtime_seconds,
                    )?;
                    ensure!(
                        output.status.success(),
                        "{}:{} runtime {number} failed: {} {}",
                        article.id,
                        identity.name,
                        bounded_text(&output.stdout, 1024),
                        bounded_text(&output.stderr, 1024)
                    );
                }
                self.blocks[record_index]["status"] = json!("ran_main_and_test");
            }
        }
        Ok(())
    }
}

fn metadata_identity(
    meta: &Value,
    article: &Article,
    dir: &Path,
    target: &Path,
    expected: &BTreeMap<String, PathBuf>,
) -> Result<String> {
    // Compare exact owned paths without following foreign metadata paths.
    let same = |value: &Value, path: &Path| {
        value.as_str().and_then(|p| cargo_protocol::portable(Path::new(p))).is_some_and(|p| {
            !p.components().any(|c| matches!(c, std::path::Component::ParentDir))
                && cargo_protocol::portable(path).is_some_and(|expected| p == expected)
        })
    };
    ensure!(
        same(&meta["workspace_root"], dir) && same(&meta["target_directory"], target),
        "Cargo workspace/target directory differs from owned layout"
    );
    let packages = meta["packages"].as_array().context("missing metadata packages")?;
    ensure!(packages.len() <= 128, "too many resolved packages");
    let mut root = None;
    for package in packages {
        let name = package["name"].as_str().context("missing resolved package name")?;
        let version = package["version"].as_str().context("missing resolved package version")?;
        ensure!(article.package.locked.contains(&(name.into(), version.into())), "unlocked resolved package");
        if package["source"].is_null() {
            ensure!(
                root.is_none()
                    && name == article.package.name
                    && version == "0.0.0"
                    && same(&package["manifest_path"], &dir.join("Cargo.toml")),
                "unexpected local Cargo package"
            );
            root = Some(package);
        } else {
            ensure!(
                package["source"].as_str() == Some("registry+https://github.com/rust-lang/crates.io-index"),
                "unexpected Cargo dependency source"
            );
        }
    }
    let root = root.context("missing owned root package")?;
    let id = root["id"].as_str().context("missing root package id")?;
    ensure!(meta["resolve"]["root"].as_str() == Some(id), "Cargo resolver root differs");
    for key in ["workspace_members", "workspace_default_members"] {
        ensure!(
            meta[key].as_array().is_some_and(|a| a.len() == 1 && a[0].as_str() == Some(id)),
            "unexpected Cargo workspace members"
        );
    }
    let targets = root["targets"].as_array().context("missing root targets")?;
    ensure!(targets.len() == expected.len(), "unexpected number of generated Cargo targets");
    let mut seen = BTreeSet::new();
    for item in targets {
        let name = item["name"].as_str().context("missing target name")?;
        let path = expected.get(name).context("unexpected generated Cargo target")?;
        ensure!(
            seen.insert(name)
                && same(&item["src_path"], path)
                && item["kind"] == json!(["example"])
                && item["edition"].as_str() == Some(&article.package.edition.to_string()),
            "Cargo target differs from indexed example"
        );
    }
    Ok(id.into())
}

pub fn run(options: Options) -> Result<()> {
    // Reserve the explicitly requested new receipt before doing expensive work.
    let mut output = options
        .output
        .as_ref()
        .map(|p| {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(p)
                .with_context(|| format!("creating new receipt {}", p.display()))
        })
        .transpose()?;
    let mut checker = Checker {
        options: &options,
        started: Instant::now(),
        cargo: std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()),
        commands: Vec::new(),
        blocks: Vec::new(),
        sealed: BTreeMap::new(),
    };
    let mut receipt = json!({"version":1,"status":"incomplete","execute_requested":options.run,"index":options.index,"only":options.only,"limits":{"compile_seconds":options.compile_seconds,"runtime_seconds":options.runtime_seconds,"total_seconds":options.total_seconds,"capture_bytes":options.capture_bytes},"implementation_sha256":hash(concat!(include_str!("mod.rs"),include_str!("inputs.rs"),include_str!("markdown.rs"),include_str!("cargo_protocol.rs")).as_bytes())});
    let result = (|| -> Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().context("finding repository root")?;
        let Inputs { snapshot, articles } = inputs::load(root, &options.index, &options.only)?;
        receipt["inputs"] = json!(
            snapshot
                .files
                .values()
                .map(|i| json!({"path":i.relative,"sha256":i.sha256,"bytes":i.bytes.len()}))
                .collect::<Vec<_>>()
        );
        let mut builder = tempfile::Builder::new();
        builder.prefix("rook-doc-check-");
        let work = match &options.work_root {
            Some(parent) => builder.tempdir_in(parent)?,
            None => builder.tempdir()?,
        };
        let owned = work.path().canonicalize()?;
        receipt["cargo"] = json!(checker.tool_version(&owned, "cargo")?);
        receipt["rustc"] = json!(checker.tool_version(&owned, "rustc")?);
        for article in &articles {
            checker.article(&owned, article)?;
        }
        checker.verify_owned(&owned)?;
        snapshot.verify_unchanged()?;
        Ok(())
    })();
    receipt["blocks"] = json!(checker.blocks);
    receipt["commands"] = json!(checker.commands);
    receipt["elapsed_ms"] = json!(checker.started.elapsed().as_millis());
    receipt["status"] = json!(if result.is_ok() { "passed" } else { "failed" });
    if let Err(error) = &result {
        receipt["error"] = json!(bounded_text(format!("{error:#}").as_bytes(), 4096));
    }
    let bytes = serde_json::to_vec_pretty(&receipt)?;
    ensure!(bytes.len() <= 2 * 1024 * 1024, "documentation receipt exceeded limit");
    if let Some(file) = &mut output {
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.flush()?;
    }
    println!("{}", String::from_utf8(bytes)?);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> Options {
        Options {
            index: "checks.json".into(),
            only: Vec::new(),
            run: true,
            output: None,
            work_root: None,
            compile_seconds: 30,
            runtime_seconds: 2,
            total_seconds: 90,
            capture_bytes: 512 * 1024,
        }
    }

    fn check(article: &str, count: usize, options: &Options) -> (Result<()>, Vec<Value>, Vec<Value>) {
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("article.md"), article).unwrap();
        fs::write(source.path().join("Cargo.toml"),"[package]\nname='owned_checker'\nversion='0.0.0'\nedition='2024'\npublish=false\nbuild=false\nautolib=false\nautobins=false\nautotests=false\nautobenches=false\n[workspace]\n").unwrap();
        fs::write(
            source.path().join("Cargo.lock"),
            "version=4\n[[package]]\nname='owned_checker'\nversion='0.0.0'\n",
        )
        .unwrap();
        fs::write(source.path().join("checks.json"),json!({"version":1,"articles":[{"id":"owned","article":"article.md","manifest":"Cargo.toml","lock":"Cargo.lock","blocks":count}]}).to_string()).unwrap();
        let inputs = inputs::load(source.path(), "checks.json", &[]).unwrap();
        let work = tempfile::tempdir().unwrap();
        let root = work.path().canonicalize().unwrap();
        let mut checker = Checker {
            options,
            started: Instant::now(),
            cargo: std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()),
            commands: Vec::new(),
            blocks: Vec::new(),
            sealed: BTreeMap::new(),
        };
        let result =
            checker.article(&root, &inputs.articles[0]).and_then(|()| inputs.snapshot.verify_unchanged());
        (result, checker.blocks, checker.commands)
    }

    #[test]
    fn actual_checker_runs_both_targets_preserves_hidden_lines_and_skips_no_run() {
        let article = "```rust\n# fn value() -> u32 { 7 }\nfn main() { assert_eq!(value(),7); }\n#[test]\nfn works() { assert_eq!(value(),7); }\n```\n```rust,no_run\nfn main(){panic!();}\n#[test]\nfn must_not_run(){panic!();}\n```\n```rust,ignore\ninvalid sketch\n```\n";
        let (result, blocks, commands) = check(article, 3, &options());
        result.unwrap();
        assert_eq!(
            blocks.iter().map(|b| b["status"].as_str().unwrap()).collect::<Vec<_>>(),
            ["ran_main_and_test", "no_run", "ignored"]
        );
        let runs =
            commands.iter().filter(|c| c["phase"].as_str().unwrap().contains(":run_")).collect::<Vec<_>>();
        assert_eq!(runs.len(), 2);
        assert!(runs.iter().all(|c| c["exit_code"] == 0));
    }

    #[test]
    fn actual_checker_compile_only_mode_does_not_execute_panicking_programs() {
        let mut options = options();
        options.run = false;
        let (result, blocks, commands) =
            check("```rust\nfn main(){panic!();}\n#[test]\nfn refused(){panic!();}\n```\n", 1, &options);
        result.unwrap();
        assert_eq!(blocks[0]["status"], "compiled_main_and_test");
        assert!(commands.iter().all(|c| !c["phase"].as_str().unwrap().contains(":run_")));
    }

    #[test]
    fn actual_checker_typechecks_test_bodies_in_compile_only_mode() {
        let mut options = options();
        options.run = false;
        let (result, blocks, commands) =
            check("```rust\nfn main(){}\n#[test]\nfn wrong(){let _:u8=\"wrong\";}\n```\n", 1, &options);
        assert!(result.is_err());
        assert_eq!(blocks[0]["status"], "incomplete");
        assert_eq!(commands[1]["exit_code"], 0); // main compiled
        assert!(commands.last().unwrap()["exit_code"].as_i64().unwrap() > 0);
    }

    #[test]
    fn actual_runtime_main_and_harness_panics_both_fail_the_article() {
        for program in [
            "fn main(){panic!(\"main refused\");}",
            "fn main(){}\n#[test]\nfn refused(){panic!(\"test refused\");}",
        ] {
            let (result, blocks, commands) = check(&format!("```rust\n{program}\n```\n"), 1, &options());
            assert!(result.is_err());
            assert_eq!(blocks[0]["status"], "incomplete_runtime");
            assert!(commands.last().unwrap()["exit_code"].as_i64().unwrap() > 0);
        }
    }

    #[test]
    fn actual_expected_compiler_error_requires_the_annotated_code() {
        for (code, passes) in [("E0308", true), ("E0502", false)] {
            let (result, blocks, _) = check(
                &format!("```rust,compile_fail,{code}\nfn main(){{let _:u8=\"quoted E0502\";}}\n```\n"),
                1,
                &options(),
            );
            assert_eq!(result.is_ok(), passes);
            if passes {
                assert_eq!(blocks[0]["codes"], json!(["E0308"]));
            }
        }
    }

    #[test]
    fn actual_runtime_timeout_and_capture_overflow_remain_failures() {
        let mut options = options();
        options.runtime_seconds = 1;
        options.capture_bytes = 4096;
        for program in [
            "fn main(){std::thread::sleep(std::time::Duration::from_secs(30));}",
            "fn main(){for _ in 0..10000{println!(\"capture overflow\");}}",
        ] {
            let (result, blocks, commands) = check(&format!("```rust\n{program}\n```\n"), 1, &options);
            assert!(result.is_err());
            assert_eq!(blocks[0]["status"], "incomplete_runtime");
            assert!(commands.last().unwrap()["capture_error"].is_string());
        }
    }

    #[test]
    fn actual_runtime_cannot_silently_change_generated_source_or_manifest() {
        for file in ["Cargo.toml", "examples/block_0.rs"] {
            let (result, blocks, commands) = check(
                &format!("```rust\nfn main(){{std::fs::write(\"../{file}\",b\"changed\").unwrap();}}\n```\n"),
                1,
                &options(),
            );
            assert!(result.unwrap_err().to_string().contains("owned input/artifact changed"));
            assert_eq!(blocks[0]["status"], "incomplete_runtime");
            assert_eq!(commands.last().unwrap()["exit_code"], 0);
        }
    }

    #[test]
    fn expired_global_admission_budget_does_not_launch_another_process() {
        let options = options();
        let checker = Checker {
            options: &options,
            started: Instant::now() - Duration::from_secs(100),
            cargo: "cargo".into(),
            commands: Vec::new(),
            blocks: Vec::new(),
            sealed: BTreeMap::new(),
        };
        assert!(checker.patience(1).is_err());
    }

    #[test]
    fn metadata_requires_exact_root_target_resolver_and_locked_dependencies() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let inputs = inputs::load(repo, "docs/examples/checks.json", &["ownership".into()]).unwrap();
        let article = &inputs.articles[0];
        let work = tempfile::tempdir().unwrap();
        let dir = work.path().canonicalize().unwrap();
        let target = dir.join("target");
        let source = dir.join("examples/block_0.rs");
        let expected = BTreeMap::from([("block_0".into(), source.clone())]);
        let good = json!({"workspace_root":dir,"target_directory":target,"workspace_members":["owned"],"workspace_default_members":["owned"],"resolve":{"root":"owned"},"packages":[{"id":"owned","name":article.package.name,"version":"0.0.0","source":null,"manifest_path":dir.join("Cargo.toml"),"targets":[{"name":"block_0","kind":["example"],"src_path":source,"edition":"2024"}]}]});
        assert_eq!(metadata_identity(&good, article, &dir, &target, &expected).unwrap(), "owned");
        for field in ["root", "target", "resolver", "members", "source", "kind", "edition", "dependency"] {
            let mut bad = good.clone();
            match field {
                "root"=>bad["workspace_root"]=json!(dir.join("other")),
                "target"=>bad["target_directory"]=json!(dir.join("outside")),
                "resolver"=>bad["resolve"]["root"]=json!("foreign"),
                "members"=>bad["workspace_members"]=json!(["owned","foreign"]),
                "source"=>bad["packages"][0]["targets"][0]["src_path"]=json!(dir.join("foreign.rs")),
                "kind"=>bad["packages"][0]["targets"][0]["kind"]=json!(["custom-build"]),
                "edition"=>bad["packages"][0]["targets"][0]["edition"]=json!("2021"),
                _=>bad["packages"].as_array_mut().unwrap().push(json!({"name":"unlocked","version":"1.0.0","source":"registry+https://github.com/rust-lang/crates.io-index"})),
            }
            assert!(metadata_identity(&bad, article, &dir, &target, &expected).is_err(), "{field}");
        }
    }
}
