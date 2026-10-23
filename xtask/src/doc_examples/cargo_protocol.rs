//! Cargo's actual verdict, executable identity and selected-source diagnostics.

use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

const MAX_JSON_BYTES: usize = 4 * 1024 * 1024;
const MAX_EVENTS: usize = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildMode {
    Main,
    Test,
}

pub struct Identity {
    pub package_id: String,
    pub manifest: PathBuf,
    pub source: PathBuf,
    pub target_root: PathBuf,
    pub name: String,
    pub source_lines: usize,
}

#[derive(Debug)]
pub struct Diagnostic {
    pub code: Option<String>,
    pub line: usize,
    pub message: String,
}

#[derive(Debug)]
pub struct Evidence {
    pub artifact: Option<PathBuf>,
    pub codes: BTreeSet<String>,
    pub source_errors: usize,
    pub source_warnings: usize,
    pub diagnostics: Vec<Diagnostic>,
}

// Compare protocol paths without opening foreign files or following a path
// supplied by an unrelated Cargo event. Filesystem admission follows separately.
pub(super) fn portable(path: &Path) -> Option<PathBuf> {
    let value = path.to_str()?;
    if value.contains('\0') {
        return None;
    }
    #[cfg(windows)]
    let adjusted = if let Some(unc) = value.strip_prefix("\\\\?\\UNC\\") {
        PathBuf::from(format!("\\\\{unc}"))
    } else {
        PathBuf::from(value.strip_prefix("\\\\?\\").unwrap_or(value))
    };
    #[cfg(not(windows))]
    let adjusted = PathBuf::from(value);
    Some(adjusted)
}

fn normalized(path: &Path, base: &Path) -> Option<PathBuf> {
    let adjusted = portable(path)?;
    let candidate = if adjusted.is_absolute() {
        adjusted
    } else {
        if adjusted.components().any(|c| !matches!(c, Component::Normal(_) | Component::CurDir)) {
            return None;
        }
        portable(base)?.join(adjusted)
    };
    let mut output = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::ParentDir => return None,
            Component::CurDir => {}
            component => output.push(component.as_os_str()),
        }
    }
    Some(output)
}

fn same_path(value: &Value, expected: &Path, base: &Path) -> bool {
    match (value.as_str().and_then(|raw| normalized(Path::new(raw), base)), normalized(expected, base)) {
        (Some(actual), Some(expected)) => actual == expected,
        _ => false,
    }
}

fn selected(row: &Value, identity: &Identity, base: &Path) -> bool {
    row["package_id"].as_str() == Some(identity.package_id.as_str())
        && same_path(&row["manifest_path"], &identity.manifest, base)
        && row["target"]["name"].as_str() == Some(identity.name.as_str())
        && same_path(&row["target"]["src_path"], &identity.source, base)
        && row["target"]["kind"].as_array().is_some_and(|kinds| kinds.len() == 1 && kinds[0] == "example")
}

fn error_code(value: &Value) -> Option<String> {
    let code = value.as_str()?;
    (code.len() == 5 && code.as_bytes()[0] == b'E' && code.as_bytes()[1..].iter().all(u8::is_ascii_digit))
        .then(|| code.to_owned())
}

fn message(text: &str) -> String {
    if text.len() <= 2048 {
        return text.into();
    }
    let mut end = 2045;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &text[..end])
}

fn refusal(reason: &str, evidence: &Evidence) -> String {
    let mut output = reason.to_owned();
    for diagnostic in evidence.diagnostics.iter().take(3) {
        use std::fmt::Write;
        let _ = write!(
            output,
            "; source line {} {}: {}",
            diagnostic.line,
            diagnostic.code.as_deref().unwrap_or("uncoded"),
            diagnostic.message
        );
    }
    output
}

/// `required_codes = Some(..)` means expected compiler failure, including an
/// empty set for an uncoded error. It still needs a real selected-source error.
pub fn inspect(
    stdout: &[u8],
    exit_code: Option<i32>,
    identity: &Identity,
    mode: BuildMode,
    required_codes: Option<&BTreeSet<String>>,
) -> Result<Evidence, String> {
    if stdout.len() > MAX_JSON_BYTES {
        return Err("Cargo JSON exceeds its byte limit".into());
    }
    let text = std::str::from_utf8(stdout).map_err(|_| "Cargo JSON is not UTF8")?;
    let base = identity.manifest.parent().ok_or("missing package directory")?;
    let target_root = normalized(&identity.target_root, base).ok_or("invalid target directory")?;
    let mut finished = None;
    let mut artifact_count = 0;
    let mut error_count = 0usize;
    let mut evidence = Evidence {
        artifact: None,
        codes: BTreeSet::new(),
        source_errors: 0,
        source_warnings: 0,
        diagnostics: Vec::new(),
    };
    for (index, line) in text.lines().enumerate() {
        if index >= MAX_EVENTS {
            return Err("Cargo JSON exceeds its event limit".into());
        }
        if line.trim().is_empty() {
            continue;
        }
        let row: Value = serde_json::from_str(line).map_err(|_| "invalid Cargo JSON event")?;
        if !row.is_object() {
            return Err("Cargo event must be an object".into());
        }
        match row["reason"].as_str() {
            Some("build-finished") => {
                let success = row["success"].as_bool().ok_or("Cargo verdict has no success flag")?;
                if finished.replace(success).is_some() {
                    return Err("more than one Cargo completion verdict".into());
                }
            }
            Some("compiler-artifact") if selected(&row, identity, base) => {
                let test = row["profile"]["test"].as_bool().ok_or("artifact has no test profile flag")?;
                if test != (mode == BuildMode::Test) {
                    continue;
                }
                artifact_count += 1;
                if artifact_count > 1 {
                    return Err("ambiguous selected executable artifacts".into());
                }
                if let Some(raw) = row["executable"].as_str() {
                    let path = normalized(Path::new(raw), base).ok_or("invalid executable path")?;
                    if path == target_root || !path.starts_with(&target_root) {
                        return Err("executable artifact is outside the owned target directory".into());
                    }
                    evidence.artifact = Some(path);
                }
            }
            Some("compiler-message") => {
                let level = row["message"]["level"].as_str();
                if level == Some("error") {
                    error_count += 1;
                }
                if !selected(&row, identity, base) {
                    continue;
                }
                let span = row["message"]["spans"].as_array().and_then(|spans| {
                    spans.iter().find(|span| {
                        span["is_primary"].as_bool() == Some(true)
                            && same_path(&span["file_name"], &identity.source, base)
                            && span["line_start"]
                                .as_u64()
                                .is_some_and(|n| n > 0 && n <= identity.source_lines as u64)
                            && span["line_end"]
                                .as_u64()
                                .is_some_and(|n| n > 0 && n <= identity.source_lines as u64)
                    })
                });
                let Some(span) = span else { continue };
                if level == Some("warning") {
                    evidence.source_warnings += 1;
                }
                if level == Some("error") {
                    evidence.source_errors += 1;
                    let code = error_code(&row["message"]["code"]["code"]);
                    if let Some(code) = &code {
                        evidence.codes.insert(code.clone());
                    }
                    if evidence.diagnostics.len() < 16 {
                        evidence.diagnostics.push(Diagnostic {
                            code,
                            line: span["line_start"].as_u64().unwrap() as usize,
                            message: message(row["message"]["message"].as_str().unwrap_or("compiler error")),
                        });
                    }
                }
            }
            _ => {}
        }
    }
    let completed = finished.ok_or("Cargo did not report a complete build verdict")?;
    if let Some(required) = required_codes {
        if !exit_code.is_some_and(|code| code > 0) || completed {
            return Err("compile_fail needs a known failed Cargo exit and verdict".into());
        }
        if evidence.source_errors == 0 || evidence.source_errors != error_count || evidence.artifact.is_some()
        {
            return Err(
                "compile_fail needs a compiler error in the selected source, without a selected executable"
                    .into(),
            );
        }
        if !required.is_subset(&evidence.codes) {
            return Err(format!("expected compiler codes {required:?}, observed {:?}", evidence.codes));
        }
    } else if exit_code != Some(0)
        || !completed
        || error_count != 0
        || artifact_count != 1
        || evidence.artifact.is_none()
    {
        return Err(refusal(
            "successful compilation needs exit0, a successful Cargo verdict and its exact executable",
            &evidence,
        ));
    }
    Ok(evidence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn identity() -> Identity {
        let root = std::env::temp_dir().join("rook-owned-protocol");
        Identity {
            package_id: "owned-package".into(),
            manifest: root.join("Cargo.toml"),
            source: root.join("examples/owned.rs"),
            target_root: root.join("target"),
            name: "owned".into(),
            source_lines: 8,
        }
    }
    fn target(id: &Identity) -> Value {
        json!({"name":id.name,"kind":["example"],"src_path":id.source})
    }
    fn artifact(id: &Identity, test: bool) -> Value {
        json!({"reason":"compiler-artifact","package_id":id.package_id,"manifest_path":id.manifest,"target":target(id),"profile":{"test":test},"executable":id.target_root.join("debug/examples/owned")})
    }
    fn diagnostic(id: &Identity, level: &str, code: Option<&str>) -> Value {
        json!({"reason":"compiler-message","package_id":id.package_id,"manifest_path":id.manifest,"target":target(id),"message":{"level":level,"code":code.map(|code|json!({"code":code})),"message":"source quotes E0502 but this message has a typed code","spans":[{"is_primary":true,"file_name":"examples/owned.rs","line_start":1,"line_end":1}]}})
    }
    fn lines(events: Vec<Value>, success: bool) -> Vec<u8> {
        events
            .into_iter()
            .chain([json!({"reason":"build-finished","success":success})])
            .map(|v| v.to_string() + "\n")
            .collect::<String>()
            .into_bytes()
    }
    fn codes() -> BTreeSet<String> {
        ["E0502".into()].into()
    }

    #[test]
    fn exact_main_and_test_artifacts_are_separate() {
        let id = identity();
        for mode in [BuildMode::Main, BuildMode::Test] {
            let data = lines(vec![artifact(&id, mode == BuildMode::Test)], true);
            let verdict = inspect(&data, Some(0), &id, mode, None).unwrap();
            assert!(verdict.artifact.unwrap().starts_with(&id.target_root));
            assert!(verdict.codes.is_empty() && verdict.source_errors == 0 && verdict.diagnostics.is_empty());
        }
        assert!(
            inspect(&lines(vec![artifact(&id, false)], true), Some(0), &id, BuildMode::Test, None).is_err()
        );
    }
    #[test]
    fn success_without_an_executable_is_not_a_pass() {
        let id = identity();
        assert!(inspect(&lines(vec![], true), Some(0), &id, BuildMode::Main, None).is_err());
        let mut row = artifact(&id, false);
        row["executable"] = Value::Null;
        assert!(inspect(&lines(vec![row], true), Some(0), &id, BuildMode::Main, None).is_err());
    }
    #[test]
    fn a_partial_artifact_never_hides_a_failed_cargo_verdict() {
        let id = identity();
        assert!(
            inspect(&lines(vec![artifact(&id, false)], false), Some(101), &id, BuildMode::Main, None)
                .is_err()
        );
    }
    #[test]
    fn infrastructure_failure_is_not_an_uncoded_expected_compiler_error() {
        let id = identity();
        let required = BTreeSet::new();
        assert!(inspect(&lines(vec![], false), Some(101), &id, BuildMode::Test, Some(&required)).is_err());
        assert!(inspect(b"", Some(101), &id, BuildMode::Test, Some(&required)).is_err());
    }
    #[test]
    fn expected_codes_come_from_typed_selected_source_errors() {
        let id = identity();
        let data = lines(vec![diagnostic(&id, "error", Some("E0502"))], false);
        let verdict = inspect(&data, Some(101), &id, BuildMode::Test, Some(&codes())).unwrap();
        assert_eq!(verdict.codes, codes());
        assert_eq!(verdict.source_errors, 1);
        assert!(verdict.artifact.is_none());
        assert_eq!(verdict.diagnostics[0].code.as_deref(), Some("E0502"));
        assert_eq!(verdict.diagnostics[0].line, 1);
        assert!(verdict.diagnostics[0].message.contains("typed code"));
    }
    #[test]
    fn source_quotations_and_warning_codes_do_not_satisfy_expected_errors() {
        let id = identity();
        for row in [diagnostic(&id, "error", Some("E0308")), diagnostic(&id, "warning", Some("E0502"))] {
            assert!(
                inspect(&lines(vec![row], false), Some(101), &id, BuildMode::Test, Some(&codes())).is_err()
            );
        }
    }
    #[test]
    fn dependency_and_foreign_file_errors_do_not_validate_the_example() {
        let id = identity();
        for field in ["package_id", "manifest_path", "target", "span"] {
            let mut row = diagnostic(&id, "error", Some("E0502"));
            match field {
                "package_id" => row[field] = json!("foreign-package"),
                "manifest_path" => row[field] = json!(id.manifest.with_file_name("foreign.toml")),
                "target" => row["target"]["src_path"] = json!("examples/foreign.rs"),
                _ => row["message"]["spans"][0]["file_name"] = json!("examples/foreign.rs"),
            }
            assert!(
                inspect(&lines(vec![row], false), Some(101), &id, BuildMode::Test, Some(&codes())).is_err(),
                "{field}"
            );
        }
    }
    #[test]
    fn errors_in_synthetic_appended_lines_do_not_count_as_source_errors() {
        let id = identity();
        let mut row = diagnostic(&id, "error", Some("E0502"));
        row["message"]["spans"][0]["line_end"] = json!(9);
        assert!(inspect(&lines(vec![row], false), Some(101), &id, BuildMode::Test, Some(&codes())).is_err());
    }
    #[test]
    fn an_own_error_cannot_hide_an_additional_foreign_error() {
        let id = identity();
        let own = diagnostic(&id, "error", Some("E0502"));
        let mut foreign = diagnostic(&id, "error", Some("E0308"));
        foreign["package_id"] = json!("dependency");
        assert!(
            inspect(&lines(vec![own, foreign], false), Some(101), &id, BuildMode::Test, Some(&codes()))
                .is_err()
        );
    }
    #[test]
    fn a_mixed_target_kind_is_not_the_selected_example() {
        let id = identity();
        let mut row = artifact(&id, false);
        row["target"]["kind"] = json!(["example", "custom-build"]);
        assert!(inspect(&lines(vec![row], true), Some(0), &id, BuildMode::Main, None).is_err());
    }
    #[test]
    fn uncoded_selected_source_errors_are_supported() {
        let id = identity();
        let data = lines(vec![diagnostic(&id, "error", None)], false);
        let verdict = inspect(&data, Some(101), &id, BuildMode::Test, Some(&BTreeSet::new())).unwrap();
        assert!(verdict.codes.is_empty());
        assert_eq!(verdict.source_errors, 1);
    }
    #[test]
    fn signal_or_incomplete_output_is_not_an_expected_compiler_failure() {
        let id = identity();
        let data = lines(vec![diagnostic(&id, "error", Some("E0502"))], false);
        assert!(inspect(&data, None, &id, BuildMode::Test, Some(&codes())).is_err());
        let incomplete = diagnostic(&id, "error", Some("E0502")).to_string();
        assert!(inspect(incomplete.as_bytes(), Some(101), &id, BuildMode::Test, Some(&codes())).is_err());
    }
    #[test]
    fn duplicate_or_outside_artifacts_are_refused() {
        let id = identity();
        let row = artifact(&id, false);
        assert!(inspect(&lines(vec![row.clone(), row], true), Some(0), &id, BuildMode::Main, None).is_err());
        let mut outside = artifact(&id, false);
        outside["executable"] = json!(id.manifest.with_file_name("outside.exe"));
        assert!(inspect(&lines(vec![outside], true), Some(0), &id, BuildMode::Main, None).is_err());
    }
    #[test]
    fn warnings_can_accompany_a_real_successful_build() {
        let id = identity();
        let data = lines(vec![artifact(&id, false), diagnostic(&id, "warning", None)], true);
        assert_eq!(inspect(&data, Some(0), &id, BuildMode::Main, None).unwrap().source_warnings, 1);
    }
    #[test]
    fn invalid_utf8_json_and_oversized_capture_cannot_be_verdicts() {
        let id = identity();
        for data in [vec![0xff], b"not Cargo JSON".to_vec(), vec![b' '; MAX_JSON_BYTES + 1]] {
            assert!(inspect(&data, Some(0), &id, BuildMode::Main, None).is_err());
        }
    }

    #[test]
    fn long_unicode_diagnostics_keep_a_bounded_valid_message() {
        let id = identity();
        let mut row = diagnostic(&id, "error", Some("E0502"));
        row["message"]["message"] = json!("é".repeat(3000));
        let verdict =
            inspect(&lines(vec![row], false), Some(101), &id, BuildMode::Test, Some(&codes())).unwrap();
        assert!(verdict.diagnostics[0].message.len() <= 2048);
        assert!(verdict.diagnostics[0].message.ends_with("..."));
    }

    #[cfg(windows)]
    #[test]
    fn relative_diagnostic_spans_match_a_verbatim_package_directory() {
        let mut id = identity();
        id.manifest = PathBuf::from(format!("\\\\?\\{}", id.manifest.display()));
        id.source = PathBuf::from(format!("\\\\?\\{}", id.source.display()));
        id.target_root = PathBuf::from(format!("\\\\?\\{}", id.target_root.display()));
        let data = lines(vec![diagnostic(&id, "error", Some("E0502"))], false);
        assert_eq!(inspect(&data, Some(101), &id, BuildMode::Test, Some(&codes())).unwrap().codes, codes());
    }

    #[test]
    fn actual_cargo_binds_executables_and_compiler_errors_to_the_selected_example() {
        use std::process::Command;
        use std::time::Duration;
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let examples = root.join("examples");
        std::fs::create_dir(&examples).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]\nname='owned_doc_protocol'\nversion='0.0.0'\nedition='2024'\npublish=false\nbuild=false\nautolib=false\nautobins=false\nautotests=false\nautobenches=false\n[workspace]\n").unwrap();
        std::fs::write(
            root.join("Cargo.lock"),
            "version=4\n[[package]]\nname='owned_doc_protocol'\nversion='0.0.0'\n",
        )
        .unwrap();
        let good = "fn main(){assert_eq!(2+2,4);}\n#[test] fn owned(){assert_eq!(\"owned\".len(),5);}\n";
        std::fs::write(examples.join("owned.rs"), good).unwrap();
        let cargo = |arguments: &[&str]| {
            let mut command = Command::new("cargo");
            command.args(arguments).current_dir(&root).env("CARGO_TARGET_DIR", root.join("target"));
            rook_contain::captured_with_cleanup(&mut command, Duration::from_secs(60), 512 * 1024)
                .unwrap()
                .output
        };
        let metadata = cargo(&["metadata", "--locked", "--offline", "--format-version", "1", "--no-deps"]);
        assert!(metadata.status.success(), "{}", String::from_utf8_lossy(&metadata.stderr));
        let metadata: Value = serde_json::from_slice(&metadata.stdout).unwrap();
        let mut id = Identity {
            package_id: metadata["packages"][0]["id"].as_str().unwrap().into(),
            manifest: root.join("Cargo.toml"),
            source: examples.join("owned.rs"),
            target_root: root.join("target"),
            name: "owned".into(),
            source_lines: good.lines().count(),
        };
        let mut artifacts = Vec::new();
        for mode in [BuildMode::Main, BuildMode::Test] {
            let mut arguments = vec![if mode == BuildMode::Main { "build" } else { "test" }];
            arguments.extend([
                "--locked",
                "--offline",
                "--example",
                "owned",
                "--message-format=json",
                "--color",
                "never",
            ]);
            if mode == BuildMode::Test {
                arguments.push("--no-run");
            }
            let output = cargo(&arguments);
            let result = inspect(&output.stdout, output.status.code(), &id, mode, None)
                .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&output.stderr)));
            let artifact = result.artifact.unwrap();
            assert!(artifact.is_file());
            let mut command = Command::new(&artifact);
            command.current_dir(&root);
            let run = rook_contain::captured_with_cleanup(&mut command, Duration::from_secs(10), 64 * 1024)
                .unwrap()
                .output;
            assert!(run.status.success());
            if mode == BuildMode::Test {
                assert!(String::from_utf8_lossy(&run.stdout).contains("1 passed"));
            }
            artifacts.push(artifact);
        }
        assert_ne!(artifacts[0], artifacts[1], "main and test harness must be different artifacts");
        let arguments = [
            "test",
            "--locked",
            "--offline",
            "--example",
            "owned",
            "--no-run",
            "--message-format=json",
            "--color",
            "never",
        ];
        std::fs::write(&id.source, "fn main(){let _:i32=\"owned\";} // E0502 is only a quotation\n").unwrap();
        id.source_lines = 1;
        let output = cargo(&arguments);
        let mismatch = ["E0308".into()].into();
        assert_eq!(
            inspect(&output.stdout, output.status.code(), &id, BuildMode::Test, Some(&mismatch))
                .unwrap()
                .codes,
            mismatch
        );
        assert!(inspect(&output.stdout, output.status.code(), &id, BuildMode::Test, Some(&codes())).is_err());
        std::fs::write(&id.source, "compile_error!(\"owned compiler refusal\"); fn main(){}\n").unwrap();
        let output = cargo(&arguments);
        assert!(
            inspect(&output.stdout, output.status.code(), &id, BuildMode::Test, Some(&BTreeSet::new()))
                .is_ok()
        );
        let missing = root.join("absent.toml");
        let output = cargo(&[
            "build",
            "--offline",
            "--locked",
            "--manifest-path",
            missing.to_str().unwrap(),
            "--message-format=json",
        ]);
        assert!(!output.status.success());
        assert!(
            inspect(&output.stdout, output.status.code(), &id, BuildMode::Main, Some(&BTreeSet::new()))
                .is_err()
        );
        std::fs::write(&id.source, good).unwrap();
        let output =
            cargo(&["check", "--offline", "--locked", "--example", "owned", "--message-format=json"]);
        assert!(output.status.success());
        assert!(
            inspect(&output.stdout, output.status.code(), &id, BuildMode::Main, None).is_err(),
            "checking without a linked executable is not the required build proof"
        );
    }
}
