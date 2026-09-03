use std::path::Path;

use rook_core::fileset::{CaptureLimits, FileSet, capture_paths, gc_expander};
use rook_core::{Change, CoreError};
use rook_store::{GcOptions, Kind, Store};

fn seed(root: &Path, files: &[(&str, &str)]) {
    for (rel, body) in files {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
}

#[test]
fn capture_then_restore_round_trips() {
    let store_dir = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    let store = Store::open(store_dir.path()).unwrap();

    seed(src.path(), &[("SKILL.md", "---\nname: a\n---\nbody"), ("scripts/run.sh", "echo hi")]);
    let (set, _) =
        FileSet::capture(&store, "skill", "a", "1.0.0", src.path(), &CaptureLimits::for_skill(), None)
            .unwrap();
    assert_eq!(set.files.len(), 2);

    let written = set.restore(&store, dest.path()).unwrap();
    assert_eq!(written, 2);
    assert_eq!(std::fs::read_to_string(dest.path().join("scripts/run.sh")).unwrap(), "echo hi");
}

#[test]
fn a_capture_refuses_to_run_away_instead_of_thrashing() {
    let store_dir = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let store = Store::open(store_dir.path()).unwrap();

    for i in 0..40 {
        std::fs::write(src.path().join(format!("f{i}.txt")), "x".repeat(100)).unwrap();
    }
    let limits = CaptureLimits { max_files: 10, ..CaptureLimits::default() };
    let err = FileSet::capture(&store, "checkpoint", "c", "", src.path(), &limits, None).unwrap_err();
    assert!(matches!(err, CoreError::CaptureTooBig { .. }), "{err}");
    // The message has to tell the user what to do about it.
    assert!(err.to_string().contains("Narrow the paths or raise the limit"), "{err}");

    let limits = CaptureLimits { max_total_bytes: 500, ..CaptureLimits::default() };
    assert!(matches!(
        FileSet::capture(&store, "checkpoint", "c", "", src.path(), &limits, None),
        Err(CoreError::CaptureTooBig { .. })
    ));
}

#[test]
fn heavy_directories_are_excluded_before_they_count_against_the_budget() {
    let store_dir = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let store = Store::open(store_dir.path()).unwrap();

    seed(src.path(), &[("src/main.rs", "fn main() {}")]);
    // The shape that makes `git add .` checkpointing pathological.
    for i in 0..200 {
        let p = src.path().join(format!("target/debug/deps/lib{i}.rlib"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![0u8; 4096]).unwrap();
    }

    let limits = CaptureLimits { max_files: 20, ..CaptureLimits::default() };
    let (set, _) = FileSet::capture(&store, "checkpoint", "c", "", src.path(), &limits, None).unwrap();
    assert_eq!(set.files.len(), 1, "only the source file should have been captured");
    assert!(set.files.contains_key("src/main.rs"));
}

#[test]
fn identical_files_across_captures_are_stored_once() {
    let store_dir = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let store = Store::open(store_dir.path()).unwrap();
    seed(src.path(), &[("a.txt", "unchanged content that is long enough to be worth compressing")]);

    let (_, id1) = FileSet::capture(
        &store,
        "checkpoint",
        "c",
        "",
        src.path(),
        &CaptureLimits::default(),
        Some("first".into()),
    )
    .unwrap();
    std::fs::write(src.path().join("b.txt"), "new file").unwrap();
    let (_, id2) = FileSet::capture(
        &store,
        "checkpoint",
        "c",
        "",
        src.path(),
        &CaptureLimits::default(),
        Some("second".into()),
    )
    .unwrap();

    assert_ne!(id1, id2);
    let per_kind = store.stats().unwrap();
    let files = per_kind.per_kind.iter().find(|k| k.kind == "file").unwrap();
    assert_eq!(files.objects, 2, "a.txt must not be stored twice across the two captures");
}

#[test]
fn diff_reports_adds_modifications_and_removals() {
    let store_dir = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let store = Store::open(store_dir.path()).unwrap();

    seed(src.path(), &[("keep.txt", "same"), ("change.txt", "before"), ("gone.txt", "bye")]);
    let (a, _) =
        FileSet::capture(&store, "checkpoint", "c", "", src.path(), &CaptureLimits::default(), None).unwrap();

    std::fs::write(src.path().join("change.txt"), "after").unwrap();
    std::fs::remove_file(src.path().join("gone.txt")).unwrap();
    std::fs::write(src.path().join("added.txt"), "new").unwrap();
    let (b, _) =
        FileSet::capture(&store, "checkpoint", "c", "", src.path(), &CaptureLimits::default(), None).unwrap();

    let changes = a.diff(&b);
    assert!(changes.contains(&("change.txt".into(), Change::Modified)), "{changes:?}");
    assert!(changes.contains(&("gone.txt".into(), Change::Removed)), "{changes:?}");
    assert!(changes.contains(&("added.txt".into(), Change::Added)), "{changes:?}");
    assert!(!changes.iter().any(|(p, _)| p == "keep.txt"));
}

#[test]
fn gc_keeps_files_that_only_a_manifest_references() {
    let store_dir = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let store = Store::open(store_dir.path()).unwrap();
    seed(src.path(), &[("a.txt", "content worth keeping around for a while")]);

    let (_, id) =
        FileSet::capture(&store, "checkpoint", "c", "", src.path(), &CaptureLimits::default(), None).unwrap();
    store.set_ref("checkpoint/c/0", &id).unwrap();

    // Without the expander the file blob looks unreachable and would be lost.
    // `min_age_secs` off, because the grace period would hide that on its own.
    let naive = store.gc(&GcOptions { dry_run: true, min_age_secs: 0, ..Default::default() }).unwrap();
    assert_eq!(naive.collected, 1);

    let report =
        store.gc(&GcOptions { expand: Some(&gc_expander), min_age_secs: 0, ..Default::default() }).unwrap();
    assert_eq!(report.collected, 0, "the manifest's files must survive");
}

#[test]
fn capture_paths_takes_an_explicit_file_list() {
    let store_dir = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let store = Store::open(store_dir.path()).unwrap();
    seed(src.path(), &[("one.txt", "1"), ("two.txt", "2"), ("three.txt", "3")]);

    let picks = vec![src.path().join("one.txt"), src.path().join("three.txt")];
    let (set, _) =
        capture_paths(&store, "checkpoint", "c", src.path(), &picks, &CaptureLimits::default()).unwrap();
    assert_eq!(set.files.len(), 2);
    assert!(set.files.contains_key("one.txt") && set.files.contains_key("three.txt"));
}

/// A tool call naming a directory where a file goes used to fail the capture
/// for everything else in it, and the loop reported that as "no checkpoint was
/// taken" — the one warning that is supposed to mean the work cannot be undone.
/// An ordinary bad argument must not raise it.
#[test]
fn a_directory_among_the_paths_does_not_cost_the_checkpoint() {
    let store_dir = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let store = Store::open(store_dir.path()).unwrap();
    seed(src.path(), &[("one.txt", "1")]);
    std::fs::create_dir(src.path().join("src")).unwrap();

    let picks = vec![src.path().join("src"), src.path().join("one.txt")];
    let (set, _) = capture_paths(&store, "checkpoint", "c", src.path(), &picks, &CaptureLimits::default())
        .expect("a directory in the list is not a failed capture");

    assert!(set.files.contains_key("one.txt"), "the file beside it is still kept: {:?}", set.files);
    assert!(
        !set.absent.iter().any(|p| p == "src"),
        "and the directory is not called absent, or a rewind would delete it: {:?}",
        set.absent
    );
}

#[test]
fn context_windows_keep_the_head_and_the_tail() {
    let data = format!("{}{}{}", "HEAD".repeat(100), "MIDDLE".repeat(1000), "TAIL".repeat(100));
    let (out, truncated) = rook_core::context::window_bytes(data.as_bytes(), 800);
    assert!(truncated);
    let text = String::from_utf8(out).unwrap();
    assert!(text.starts_with("HEAD"), "the head must survive");
    assert!(text.ends_with("TAIL"), "the tail must survive — errors live at the end");
    assert!(text.contains("bytes elided"), "the elision must be visible to the model");
    assert!(text.len() < 1200);
}

#[test]
fn context_windows_never_split_a_utf8_character() {
    // Multi-byte characters straddling the cut points.
    let data = "日本語テキスト".repeat(500);
    for max in [100, 333, 1024, 4097] {
        let (out, _) = rook_core::context::window_bytes(data.as_bytes(), max);
        assert!(String::from_utf8(out).is_ok(), "windowing at {max} produced invalid utf-8");
    }
}

#[test]
fn the_context_budget_compacts_before_the_window_is_full() {
    let budget = rook_core::context::ContextBudget::new(100_000, 0.75);
    assert!(!budget.needs_compaction(1_000));
    assert!(budget.needs_compaction(budget.threshold()));
    assert!(budget.threshold() < budget.window, "compaction must trigger below the hard limit, not at it");
    assert!(budget.reserve_output > 0, "there must always be room left for a reply");
}

#[test]
fn a_capture_of_a_binary_file_still_round_trips() {
    let store_dir = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    let store = Store::open(store_dir.path()).unwrap();

    let blob: Vec<u8> = (0..70_000u32).map(|i| (i.wrapping_mul(2654435761) >> 16) as u8).collect();
    std::fs::write(src.path().join("data.bin"), &blob).unwrap();
    let (set, _) =
        FileSet::capture(&store, "checkpoint", "c", "", src.path(), &CaptureLimits::default(), None).unwrap();
    set.restore(&store, dest.path()).unwrap();
    assert_eq!(std::fs::read(dest.path().join("data.bin")).unwrap(), blob);
    let _ = Kind::FileBlob;
}
