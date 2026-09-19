use rook_contain::files;
use std::path::Path;

// One test in its own integration-test process: the private write counter has
// not been used before the occupied names are installed.
#[test]
fn atomic_write_preserves_occupied_temporaries_and_cleans_up_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let occupied: Vec<_> =
        (0..128).map(|n| dir.path().join(format!(".rook-write-{}-{n}", std::process::id()))).collect();
    for path in &occupied {
        std::fs::write(path, "not owned by this write").unwrap();
    }
    std::fs::write(dir.path().join("target"), "original").unwrap();
    let error = files::write(dir.path(), Path::new("target"), b"replacement").unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read_to_string(dir.path().join("target")).unwrap(), "original");
    for path in &occupied {
        assert_eq!(std::fs::read_to_string(path).unwrap(), "not owned by this write");
        std::fs::remove_file(path).unwrap();
    }

    files::write(dir.path(), Path::new("nested/deep/target"), b"created").unwrap();
    files::write(dir.path(), Path::new("target"), b"replacement").unwrap();
    assert_eq!(std::fs::read_to_string(dir.path().join("target")).unwrap(), "replacement");
    assert_eq!(std::fs::read_to_string(dir.path().join("nested/deep/target")).unwrap(), "created");
    // Publication fails when a directory occupies the destination. Only the
    // temporary file created by this operation may be removed.
    assert!(files::write(dir.path(), Path::new("nested"), b"refused").is_err());
    assert!(dir.path().join("nested/deep/target").is_file());
    for parent in [dir.path().to_path_buf(), dir.path().join("nested/deep")] {
        assert!(
            std::fs::read_dir(parent)
                .unwrap()
                .all(|entry| !files::is_write_temporary(&entry.unwrap().path()))
        );
    }
}

/// A move across roots is a copy and then an unlink — a rename cannot cross
/// them and must not follow a symlink on the way — so the unlink is the one
/// step that can fail with the work half done. It fails safe, leaving both
/// copies, and the error has to say so: the raw one is `Permission denied` on
/// a path the caller did not name, which reads as a move that did nothing
/// while the destination is sitting there.
#[test]
#[cfg(unix)]
fn a_move_that_cannot_remove_the_original_says_both_copies_exist() {
    use std::os::unix::fs::PermissionsExt;

    let from = tempfile::tempdir().unwrap();
    let to = tempfile::tempdir().unwrap();
    std::fs::create_dir(from.path().join("held")).unwrap();
    std::fs::write(from.path().join("held/note.txt"), b"keep me").unwrap();
    std::fs::write(from.path().join("held/probe.txt"), b"probe").unwrap();
    // The directory entry is what an unlink changes, so it is the directory
    // that has to be unwritable.
    std::fs::set_permissions(from.path().join("held"), std::fs::Permissions::from_mode(0o500)).unwrap();

    // Whether that bites, asked rather than assumed. Root bypasses directory
    // permissions, so under root the unlink succeeds, the move works, and the
    // failure this test is about cannot be staged at all — which is what
    // happened on the FreeBSD runner, where the tests run as root and this
    // failed on a platform difference that has nothing to do with its subject.
    // A probe rather than a uid check: what matters is whether the permission
    // is enforced for whoever is running, not who that is.
    if std::fs::remove_file(from.path().join("held/probe.txt")).is_ok() {
        std::fs::set_permissions(from.path().join("held"), std::fs::Permissions::from_mode(0o700)).unwrap();
        return;
    }

    let failed = files::move_file(from.path(), Path::new("held/note.txt"), to.path(), Path::new("note.txt"))
        .expect_err("the original cannot be removed");
    let said = failed.to_string();

    std::fs::set_permissions(from.path().join("held"), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(said.contains("Both copies exist"), "it says what the state is: {said}");
    assert!(said.contains("note.txt"), "and which file it is about: {said}");
    assert_eq!(
        std::fs::read(to.path().join("note.txt")).unwrap(),
        b"keep me",
        "the copy really is there, which is why saying nothing was misleading"
    );
    assert!(from.path().join("held/note.txt").exists(), "and so is the original");
}
