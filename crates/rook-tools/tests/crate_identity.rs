//! The real tool, each call in a child with a private Cargo home.
use rook_tools::{Tool, ToolContext, ToolOutcome, crates::CrateApi};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[test]
fn isolated_tool_child() {
    let Some(root) = std::env::var_os("ROOK_OWNED_CRATE_IDENTITY") else { return };
    let root = PathBuf::from(root);
    let args: Value = serde_json::from_slice(&std::fs::read(root.join("args.json")).unwrap()).unwrap();
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(CrateApi.call(&ToolContext::new(root.join("workspace")), &args));
    let value = match result {
        Ok(out) => serde_json::to_value(out).unwrap(),
        Err(error) => serde_json::to_value(ToolOutcome::error(error.to_string())).unwrap(),
    };
    std::fs::write(root.join("result.json"), serde_json::to_vec(&value).unwrap()).unwrap();
}

fn package(version: &str, source: &str, checksum: &str) -> String {
    format!(
        "\n[[package]]\nname = \"owned-example\"\nversion = \"{version}\"\nsource = \"{source}\"\nchecksum = \"{checksum}\"\n"
    )
}

fn seed(root: &Path, registry: &str, version: &str, checksum: &str, marker: &str) {
    let source = root.join("home/registry/src").join(registry).join(format!("owned-example-{version}"));
    std::fs::create_dir_all(source.join("src")).unwrap();
    std::fs::write(source.join("src/lib.rs"), format!("pub struct {marker};\n")).unwrap();
    let cache = root.join("home/registry/cache").join(registry);
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(
        cache.join(format!("owned-example-{version}.crate")),
        if checksum == A { b"owned package A" } else { b"owned package B" },
    )
    .unwrap();
}

fn invoke(versions: &[(&str, &str, &str)], args: Value, prepare: impl FnOnce(&Path)) -> ToolOutcome {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("workspace")).unwrap();
    std::fs::write(
        root.path().join("workspace/Cargo.toml"),
        "[package]\nname=\"owned-project\"\nversion=\"0.1.0\"\n[dependencies]\nowned-example=\"=2.0.0\"\n",
    )
    .unwrap();
    let lock =
        "version = 4\n".to_owned() + &versions.iter().map(|(v, s, c)| package(v, s, c)).collect::<String>();
    std::fs::write(root.path().join("workspace/Cargo.lock"), lock).unwrap();
    std::fs::write(root.path().join("args.json"), serde_json::to_vec(&args).unwrap()).unwrap();
    prepare(root.path());
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    rook_contain::quietly(&mut command);
    let mut child = command
        .args(["--exact", "isolated_tool_child", "--nocapture"])
        .env("ROOK_OWNED_CRATE_IDENTITY", root.path())
        .env("CARGO_HOME", root.path().join("home"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "isolated tool child failed: {status}");
            break;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("isolated tool child exceeded 30 seconds");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    serde_json::from_slice(&std::fs::read(root.path().join("result.json")).unwrap()).unwrap()
}

const REGISTRY: &str = "registry+https://github.com/rust-lang/crates.io-index";
const A: &str = "2f168479007928ff3b4446b59ff7ddb8818532f83d944bc83eb2ab3c50514c30";
const B: &str = "3878b574e63c8d391e0404c7245695906bc4bf2aee890e18b1061f76d323301e";

#[test]
fn multiple_locked_versions_require_an_explicit_choice() {
    let out =
        invoke(&[("1.0.0", REGISTRY, A), ("2.0.0", REGISTRY, B)], json!({"crate":"owned-example"}), |r| {
            seed(r, "registry-a", "1.0.0", A, "WrongVersion");
            seed(r, "registry-a", "2.0.0", B, "RightVersion");
        });
    assert!(out.is_error && out.content.contains("version"), "{}", out.content);
    assert!(!out.content.contains("pub struct"));
}

#[test]
fn explicit_version_reads_that_version_and_not_the_first() {
    let out = invoke(
        &[("1.0.0", REGISTRY, A), ("2.0.0", REGISTRY, B)],
        json!({"crate":"owned-example","version":"2.0.0"}),
        |r| {
            seed(r, "registry-a", "1.0.0", A, "WrongVersion");
            seed(r, "registry-a", "2.0.0", B, "RightVersion");
        },
    );
    assert!(
        !out.is_error && out.content.contains("RightVersion") && !out.content.contains("WrongVersion"),
        "{}",
        out.content
    );
    assert_eq!(out.meta["version"], "2.0.0");
}

#[test]
fn a_local_copy_with_a_different_package_checksum_is_not_used() {
    let out = invoke(&[("2.0.0", REGISTRY, B)], json!({"crate":"owned-example"}), |r| {
        seed(r, "registry-a", "2.0.0", A, "WrongSource")
    });
    assert!(out.is_error && !out.content.contains("WrongSource"), "{}", out.content);
}

#[test]
fn same_version_from_different_locked_sources_is_ambiguous() {
    let out = invoke(
        &[("2.0.0", REGISTRY, A), ("2.0.0", "registry+https://example.invalid/index", B)],
        json!({"crate":"owned-example","version":"2.0.0"}),
        |r| seed(r, "registry-a", "2.0.0", A, "WrongSource"),
    );
    assert!(out.is_error && out.content.contains("source"), "{}", out.content);
}

#[test]
fn a_matching_unique_local_package_still_works() {
    let out = invoke(&[("2.0.0", REGISTRY, B)], json!({"crate":"owned-example"}), |r| {
        seed(r, "registry-a", "2.0.0", B, "RightVersion")
    });
    assert!(!out.is_error && out.content.contains("RightVersion"), "{}", out.content);
}

#[test]
fn an_unknown_explicit_version_does_not_fall_back() {
    let out = invoke(&[("2.0.0", REGISTRY, B)], json!({"crate":"owned-example","version":"3.0.0"}), |r| {
        seed(r, "registry-a", "2.0.0", B, "WrongVersion");
    });
    assert!(
        out.is_error && out.content.contains("version") && !out.content.contains("WrongVersion"),
        "{}",
        out.content
    );
}

#[test]
fn an_invalid_nearest_lock_does_not_use_a_parent_lock() {
    let out = invoke(&[("2.0.0", REGISTRY, B)], json!({"crate":"owned-example"}), |r| {
        seed(r, "registry-a", "2.0.0", B, "WrongWorkspace");
        std::fs::copy(r.join("workspace/Cargo.lock"), r.join("Cargo.lock")).unwrap();
        std::fs::write(r.join("workspace/Cargo.lock"), "not valid TOML").unwrap();
    });
    assert!(
        out.is_error && out.content.contains("invalid") && !out.content.contains("WrongWorkspace"),
        "{}",
        out.content
    );
}

#[test]
fn a_git_package_never_uses_a_same_name_registry_package() {
    let out = invoke(
        &[("2.0.0", "git+https://example.invalid/owned#012345", B)],
        json!({"crate":"owned-example"}),
        |r| {
            seed(r, "registry-a", "2.0.0", B, "WrongSource");
        },
    );
    assert!(
        out.is_error && out.content.contains("path/git") && !out.content.contains("WrongSource"),
        "{}",
        out.content
    );
}

#[test]
fn oversized_lock_is_refused_before_source_lookup() {
    let out = invoke(&[("2.0.0", REGISTRY, B)], json!({"crate":"owned-example"}), |r| {
        seed(r, "registry-a", "2.0.0", B, "Unadmitted");
        let mut lock = std::fs::read(r.join("workspace/Cargo.lock")).unwrap();
        lock.resize(4 * 1024 * 1024 + 1, b' ');
        std::fs::write(r.join("workspace/Cargo.lock"), lock).unwrap();
    });
    assert!(
        out.is_error && out.content.contains("4 MiB") && !out.content.contains("Unadmitted"),
        "{}",
        out.content
    );
}

#[test]
fn oversized_source_is_reported_as_incomplete() {
    let out = invoke(&[("2.0.0", REGISTRY, B)], json!({"crate":"owned-example"}), |r| {
        seed(r, "registry-a", "2.0.0", B, "NotAdmitted");
        let mut body = b"pub struct NotAdmitted;\n".to_vec();
        body.resize(256 * 1024 + 1, b' ');
        std::fs::write(r.join("home/registry/src/registry-a/owned-example-2.0.0/src/lib.rs"), body).unwrap();
    });
    assert!(!out.is_error && out.truncated && out.meta["scan_incomplete"] == true, "{}", out.content);
    assert!(!out.content.contains("pub struct NotAdmitted"));
}
