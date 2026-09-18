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
