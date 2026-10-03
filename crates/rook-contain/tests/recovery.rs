use rook_contain::files::RecoveryFiles;
use std::path::Path;

#[test]
fn recovery_publication_preserves_existing_entries_and_refuses_input_before_copy() {
    let root = tempfile::tempdir().unwrap();
    assert!(RecoveryFiles::open(root.path(), Path::new("missing"), false).is_err());
    assert!(!root.path().join("missing").exists());
    let boundary = RecoveryFiles::open(root.path(), Path::new("checkout"), true).unwrap();
    assert!(boundary.write_new(Path::new("nested/note"), b"first").unwrap());
    assert!(!boundary.write_new(Path::new("nested/note"), b"replacement").unwrap());
    assert_eq!(boundary.read(Path::new("nested/note"), 5).unwrap(), b"first");
    assert!(boundary.read(Path::new("nested/note"), 4).is_err());
    assert_eq!(boundary.metadata(Path::new("nested/note")).unwrap().len(), 5);
    assert!(boundary.write_new(Path::new("../outside"), b"escape").is_err());
    let mut source = tempfile::tempfile().unwrap();
    use std::io::{Seek, Write};
    source.write_all(b"sixsix").unwrap();
    source.rewind().unwrap();
    assert!(source.metadata().unwrap().len() > 5, "input actually exceeds admission");
    assert!(boundary.copy_new(Path::new("too-large"), &mut source, 5, false).is_err());
    assert!(!boundary.exists(Path::new("too-large")).unwrap());
    assert!(boundary.copy_new(Path::new("complete"), &mut source, 6, true).unwrap());
    assert_eq!(boundary.read(Path::new("complete"), 6).unwrap(), b"sixsix");
    assert!(
        std::fs::read_dir(root.path().join("checkout"))
            .unwrap()
            .all(|e| !rook_contain::files::is_write_temporary(&e.unwrap().path()))
    );
}

#[test]
fn recovery_never_follows_linked_prefixes_or_replaces_a_final_link() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("held"), b"held").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), root.path().join("linked")).unwrap();
    #[cfg(windows)]
    if let Err(e) = std::os::windows::fs::symlink_dir(outside.path(), root.path().join("linked")) {
        assert_eq!(e.raw_os_error(), Some(1314), "only unavailable link privilege may skip this setup");
        return;
    }
    assert!(RecoveryFiles::open(root.path(), Path::new("linked"), false).is_err());
    let boundary = RecoveryFiles::open(root.path(), Path::new("."), false).unwrap();
    assert!(boundary.write_new(Path::new("linked/held"), b"replacement").is_err());
    assert_eq!(std::fs::read(outside.path().join("held")).unwrap(), b"held");
    assert!(boundary.symlink_new(Path::new("final"), Path::new("../outside-held")).unwrap());
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path().join("held"), root.path().join("outside-final")).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(outside.path().join("held"), root.path().join("outside-final"))
        .unwrap();
    assert!(!boundary.write_new(Path::new("outside-final"), b"replacement").unwrap());
    assert!(boundary.read(Path::new("outside-final"), 10).is_err());
    assert!(!boundary.write_new(Path::new("final"), b"replacement").unwrap());
    assert!(boundary.exists(Path::new("final")).unwrap());
    assert!(boundary.read(Path::new("final"), 10).is_err());
    assert_eq!(std::fs::read(outside.path().join("held")).unwrap(), b"held");
}
