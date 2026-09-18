//! Directory-relative operations keep validation and I/O in the same boundary.
use cap_std::fs::{Dir, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;

fn directory(root: &Path) -> io::Result<Dir> {
    Dir::open_ambient_dir(root, cap_std::ambient_authority())
}

fn relative(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_) | std::path::Component::CurDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "expected a relative path inside the directory",
        ));
    }
    Ok(())
}

/// Open a file without allowing path traversal outside `root`.
pub fn open(root: &Path, path: &Path) -> io::Result<std::fs::File> {
    relative(path)?;
    Ok(directory(root)?.open(path)?.into_std())
}

/// Read at most `limit` bytes; large bodies are refused before being retained.
pub fn read_text(root: &Path, path: &Path, limit: usize) -> io::Result<String> {
    let mut text = String::new();
    open(root, path)?.take(limit.saturating_add(1) as u64).read_to_string(&mut text)?;
    if text.len() > limit {
        return Err(io::Error::other("file exceeds the read budget"));
    }
    Ok(text)
}

/// Replace a file atomically without following its final symlink or hard link.
pub fn write(root: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    relative(path)?;
    let dir = directory(root)?;
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let parent = create_parent(&dir, parent)?;
    let name = path.file_name().ok_or_else(|| io::Error::other("file name is missing"))?;
    let permissions = match parent.symlink_metadata(name) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "refusing to replace a symlink"));
        }
        Ok(meta) => Some(meta.permissions()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    let (temp, mut file) = temporary(&parent)?;
    let result = (|| {
        file.write_all(bytes)?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        file.sync_all()?;
        drop(file);
        parent.rename(&temp, &parent, name)?;
        sync_directory(&parent)
    })();
    if result.is_err() {
        let _ = parent.remove_file(&temp);
    }
    result
}

// Keep the temporary file on the destination filesystem for atomic rename.
// A name left by a killed process is occupied, not ours to truncate or unlink.
fn temporary(parent: &Dir) -> io::Result<(String, cap_std::fs::File)> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    for _ in 0..128 {
        let temp = format!(
            ".rook-write-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        match parent.open_with(&temp, OpenOptions::new().write(true).create_new(true)) {
            Ok(file) => return Ok((temp, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "could not reserve a temporary write file"))
}

fn sync_directory(dir: &Dir) -> io::Result<()> {
    // Windows does not support FlushFileBuffers on these directory handles.
    #[cfg(unix)]
    dir.try_clone()?.into_std_file().sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

fn create_parent(root: &Dir, path: &Path) -> io::Result<Dir> {
    root.create_dir_all(path)?;
    // A new nested destination also needs its ancestors' directory entries
    // persisted. The immediate parent is synced after publishing the file.
    #[cfg(unix)]
    for ancestor in path.ancestors().skip(1) {
        let ancestor = if ancestor.as_os_str().is_empty() { Path::new(".") } else { ancestor };
        sync_directory(&root.open_dir(ancestor)?)?;
    }
    root.open_dir(path)
}

/// Private write remnants are not project files, even after a killed process.
pub fn is_write_temporary(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix(".rook-write-"))
        .and_then(|n| n.split_once('-'))
        .is_some_and(|(pid, serial)| {
            !pid.is_empty()
                && !serial.is_empty()
                && pid.bytes().all(|b| b.is_ascii_digit())
                && serial.bytes().all(|b| b.is_ascii_digit())
        })
}

/// Remove a directory entry without following it outside the directory.
pub fn remove(root: &Path, path: &Path) -> io::Result<()> {
    relative(path)?;
    directory(root)?.remove_file(path)
}

/// Check a restore destination before applying any part of a batch.
pub fn validate(root: &Path, path: &Path) -> io::Result<()> {
    relative(path)?;
    let dir = directory(root)?;
    let mut prefix = std::path::PathBuf::new();
    let parts: Vec<_> = path.components().collect();
    for (at, part) in parts.iter().enumerate() {
        prefix.push(part);
        match dir.symlink_metadata(&prefix) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "restore path contains a symlink",
                ));
            }
            Ok(meta) if at + 1 < parts.len() && !meta.is_dir() => {
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    "restore parent is not a directory",
                ));
            }
            Ok(meta) if at + 1 == parts.len() && !meta.is_file() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "restore destination is not a regular file",
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Move one file without overwriting a destination created concurrently.
/// Directory handles keep both operations confined even if parents are renamed.
pub fn move_file(from_root: &Path, from: &Path, to_root: &Path, to: &Path) -> io::Result<u64> {
    relative(from)?;
    relative(to)?;
    let source = directory(from_root)?;
    let destination = directory(to_root)?;
    let parent =
        |p: &Path| p.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_path_buf();
    let source = source.open_dir(parent(from))?;
    let destination = create_parent(&destination, &parent(to))?;
    let from = from.file_name().ok_or_else(|| io::Error::other("source name is missing"))?;
    let to = to.file_name().ok_or_else(|| io::Error::other("destination name is missing"))?;
    if !source.symlink_metadata(from)?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "moving symlinks or special files is not supported",
        ));
    }
    let mut input = source.open(from)?;
    let meta = input.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::other("source is not a file"));
    }
    let mut output = destination.open_with(to, OpenOptions::new().write(true).create_new(true))?;
    let copied: io::Result<u64> = (|| {
        let bytes = io::copy(&mut input, &mut output)?;
        output.set_permissions(meta.permissions())?;
        output.sync_all()?;
        Ok(bytes)
    })();
    drop(output);
    if copied.is_err() {
        let _ = destination.remove_file(to);
    }
    let bytes = copied?;
    sync_directory(&destination)?;
    source.remove_file(from)?;
    sync_directory(&source)?;
    Ok(bytes)
}
