//! Real foreground capture, with compiler-shaped output before the display cap.
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rook_tools::{Tool, ToolContext, exec::RunCommand};

fn compiler() -> &'static Path {
    static BUILT: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
    &BUILT
        .get_or_init(|| {
            let root = tempfile::tempdir().unwrap();
            let directory = root.path().join("compiler tools");
            std::fs::create_dir(&directory).unwrap();
            let path = directory.join(if cfg!(windows) { "cargo.exe" } else { "cargo" });
            let mut command = std::process::Command::new("rustc");
            rook_contain::quietly(&mut command);
            let result = command
                .args(["--edition", "2024", "-C", "debuginfo=0"])
                .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../xtask/probes/cargo-output/helper.rs"))
                .arg("-o")
                .arg(&path)
                .output()
                .unwrap();
            assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
            (root, path)
        })
        .1
}

fn context() -> (tempfile::TempDir, ToolContext) {
    let directory = tempfile::tempdir().unwrap();
    let mut context = ToolContext::new(directory.path().to_path_buf());
    context.max_output_bytes = 2048;
    let logs = directory.path().join("logs");
    std::fs::create_dir(&logs).unwrap();
    context.spill_dir = Some(logs);
    context.max_spill_bytes = 4 * 1024 * 1024;
    (directory, context)
}

#[tokio::test]
async fn compiler_errors_survive_noise_before_the_display_limit() {
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };
    let (_directory, mut context) = context();
    let received = Arc::<AtomicU64>::default();
    context.output_activity = Some(received.clone());
    let outcome = RunCommand
        .call(&context, &serde_json::json!({"command":format!("\"{}\" check", compiler().display())}))
        .await
        .unwrap();
    assert!(outcome.is_error);
    assert_eq!(outcome.meta["exit_code"], 101);
    assert!(outcome.content.contains("OWNED_MIDDLE_DIAGNOSTIC"), "{}", outcome.content);
    assert!(outcome.content.contains("OWNED_SOURCE_SPAN"), "{}", outcome.content);
    assert!(outcome.content.contains("OWNED_WARNING_REMAINS"), "{}", outcome.content);
    assert!(outcome.content.contains("test result: FAILED."));
    assert!(!outcome.content.contains("Compiling owned_"));
    assert!(!outcome.content.contains("test owned_test_"));
    assert_eq!(outcome.meta["cargo_output_condensed"], true);
    assert!(!outcome.meta.contains_key("command_result_bytes"));
    assert!(outcome.truncated);
    let raw = std::fs::read_to_string(outcome.meta["output_file"].as_str().unwrap()).unwrap();
    assert!(raw.contains("Compiling owned_4095"));
    assert!(raw.contains("test owned_test_4095 ... ok"));
    assert!(raw.contains("OWNED_MIDDLE_DIAGNOSTIC"));
    assert_eq!(outcome.full_bytes, raw.len());
    assert_eq!(
        received.load(Ordering::Relaxed),
        raw.len() as u64,
        "omitted progress still counts as real activity"
    );
    assert_eq!(outcome.meta["output_complete"], true);
}

#[tokio::test]
async fn a_condensed_short_result_still_keeps_the_original_output() {
    let (_directory, context) = context();
    let outcome = RunCommand
        .call(&context, &serde_json::json!({"command":format!("\"{}\" check --small", compiler().display())}))
        .await
        .unwrap();
    assert!(!outcome.content.contains("Compiling owned_"), "{}", outcome.content);
    assert!(outcome.content.contains("OWNED_MIDDLE_DIAGNOSTIC"));
    assert!(outcome.truncated);
    assert_eq!(outcome.meta["cargo_output_condensed"], true);
    let raw = std::fs::read_to_string(outcome.meta["output_file"].as_str().unwrap()).unwrap();
    assert!(raw.len() < context.max_output_bytes);
    assert!(raw.contains("Compiling owned_0"));
    assert_eq!(outcome.full_bytes, raw.len());
    assert_eq!(outcome.meta["output_complete"], true);
}

#[tokio::test]
async fn raw_mode_keeps_passing_test_names_and_compiler_progress() {
    let (_directory, context) = context();
    let outcome = RunCommand
        .call(&context, &serde_json::json!({"command":format!("\"{}\" check --small", compiler().display()),"output":"raw"}))
        .await
        .unwrap();
    assert!(outcome.content.contains("Compiling owned_0"));
    assert!(outcome.content.contains("test owned_test_3 ... ok"));
    assert!(!outcome.truncated);
    assert!(!outcome.meta.contains_key("cargo_output_condensed"));
    assert!(!outcome.meta.contains_key("output_file"));
    assert!(outcome.meta.contains_key("command_result_bytes"));
}

#[tokio::test]
async fn a_non_cargo_program_is_not_filtered() {
    let (directory, context) = context();
    let path = directory.path().join(if cfg!(windows) { "unrelated.exe" } else { "unrelated" });
    std::fs::copy(compiler(), &path).unwrap();
    let outcome = RunCommand
        .call(&context, &serde_json::json!({"command":format!("\"{}\" check --small", path.display())}))
        .await
        .unwrap();
    assert!(outcome.content.contains("Compiling owned_0"));
    assert!(outcome.content.contains("test owned_test_3 ... ok"));
    assert!(!outcome.truncated);
    assert!(!outcome.meta.contains_key("cargo_output_condensed"));
}

#[tokio::test]
async fn no_raw_capture_means_no_condensation() {
    let (_directory, mut context) = context();
    context.spill_dir = None;
    let outcome = RunCommand
        .call(&context, &serde_json::json!({"command":format!("\"{}\" check --small", compiler().display())}))
        .await
        .unwrap();
    assert!(outcome.content.contains("Compiling owned_0"));
    assert!(outcome.content.contains("test owned_test_3 ... ok"));
    assert!(!outcome.truncated);
}

#[tokio::test]
async fn invalid_output_mode_is_refused_before_the_command_runs() {
    let (directory, context) = context();
    let result = RunCommand
        .call(&context, &serde_json::json!({"command":format!("\"{}\" check --small", compiler().display()),"output":"invalid"}))
        .await;
    assert!(result.is_err(), "invalid output mode must be refused");
    assert!(!directory.path().join("invoked.txt").exists());
}

#[tokio::test]
async fn a_capped_original_capture_is_reported_as_incomplete() {
    let (_directory, mut context) = context();
    context.max_spill_bytes = 64;
    let outcome = RunCommand
        .call(&context, &serde_json::json!({"command":format!("\"{}\" check", compiler().display())}))
        .await
        .unwrap();
    assert!(outcome.content.contains("OWNED_MIDDLE_DIAGNOSTIC"));
    assert!(outcome.content.contains("max_spill_bytes"));
    assert_eq!(outcome.meta["cargo_output_condensed"], true);
    assert_eq!(outcome.meta["output_complete"], false);
    let raw = std::fs::read(outcome.meta["output_file"].as_str().unwrap()).unwrap();
    assert_eq!(raw.len(), 64);
    assert!(outcome.full_bytes > raw.len());
}

#[tokio::test]
async fn a_failed_capture_open_preserves_the_original_presentation() {
    let (directory, mut context) = context();
    let blocked = directory.path().join("not-a-directory");
    std::fs::write(&blocked, b"Owned file, not a capture directory").unwrap();
    context.spill_dir = Some(blocked);
    let outcome = RunCommand
        .call(&context, &serde_json::json!({"command":format!("\"{}\" check --small", compiler().display())}))
        .await
        .unwrap();
    assert!(outcome.content.contains("Compiling owned_0"));
    assert!(!outcome.meta.contains_key("cargo_output_condensed"));
    assert!(!outcome.meta.contains_key("output_file"));
}

#[tokio::test]
async fn timeout_keeps_a_partial_error_line_and_does_not_claim_complete_output() {
    let (_directory, mut context) = context();
    context.require_command_cleanup = true;
    let outcome = RunCommand
        .call(&context, &serde_json::json!({"command":format!("\"{}\" check --small --wait", compiler().display()),"timeout_secs":1}))
        .await
        .unwrap();
    assert!(outcome.is_error);
    assert_eq!(outcome.meta["timed_out"], true);
    assert_eq!(outcome.meta["processes_stopped"], true);
    assert_eq!(outcome.meta["output_complete"], false);
    assert_eq!(outcome.meta["cargo_output_condensed"], true);
    assert!(outcome.content.contains("OWNED_PARTIAL_DIAGNOSTIC"), "{}", outcome.content);
    assert!(outcome.truncated);
}
