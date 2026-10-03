//! Directory-relative operations keep validation and I/O in the same boundary.
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;

fn directory(root: &Path) -> io::Result<Dir> {
    Dir::open_ambient_dir(root, cap_std::ambient_authority())
}

/// UTF-8 Git paths omit the Windows verbatim prefix returned by canonicalize.
/// Git's environment and gitdir marker parser do not accept that Win32 spelling.
pub fn git_path(path: &Path) -> io::Result<String> {
    let text = path.to_str().ok_or_else(|| io::Error::other("Git path is not UTF-8"))?;
    #[cfg(windows)]
    {
        if let Some(unc) = text.strip_prefix("\\\\?\\UNC\\") {
            return Ok(format!("//{}", unc.replace('\\', "/")));
        }
        Ok(text.strip_prefix("\\\\?\\").unwrap_or(text).replace('\\', "/"))
    }
    #[cfg(not(windows))]
    {
        Ok(text.to_owned())
    }
}

/// A retained restore boundary. Every traversed directory is opened without
/// following links; publishing never replaces a concurrently created entry.
pub struct RecoveryFiles(Dir);
impl RecoveryFiles {
    pub fn open(root: &Path, inside: &Path, create: bool) -> io::Result<Self> {
        relative(inside)?;
        Ok(Self(recovery_parent(directory(root)?, inside, create)?))
    }

    pub fn metadata(&self, path: &Path) -> io::Result<std::fs::Metadata> {
        relative(path)?;
        let (parent, name) = self.parent(path, false)?;
        // Opening without following the final link binds the size admission
        // to the actual input rather than a replacement symlink's target.
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        parent.open_with(name, &options)?.into_std().metadata()
    }

    pub fn exists(&self, path: &Path) -> io::Result<bool> {
        relative(path)?;
        let (parent, name) = match self.parent(path, false) {
            Ok(pair) => pair,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        };
        match parent.symlink_metadata(name) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    pub fn read(&self, path: &Path, maximum: usize) -> io::Result<Vec<u8>> {
        relative(path)?;
        let (parent, name) = self.parent(path, false)?;
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        let file = parent.open_with(name, &options)?.into_std();
        if !file.metadata()?.is_file() || file.metadata()?.len() > maximum as u64 {
            return Err(io::Error::other("recovery metadata is not a bounded regular file"));
        }
        let mut bytes = Vec::new();
        file.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > maximum {
            return Err(io::Error::other("recovery metadata grew past its byte limit"));
        }
        Ok(bytes)
    }

    pub fn write_new(&self, path: &Path, bytes: &[u8]) -> io::Result<bool> {
        self.publish(path, |file| {
            file.write_all(bytes)?;
            Ok(())
        })
    }

    pub fn copy_new(
        &self,
        path: &Path,
        input: &mut std::fs::File,
        maximum: u64,
        executable: bool,
    ) -> io::Result<bool> {
        if !input.metadata()?.is_file() || input.metadata()?.len() > maximum {
            return Err(io::Error::other("recovery input exceeds its file limit"));
        }
        self.publish(path, |output| {
            let copied = io::copy(&mut input.take(maximum.saturating_add(1)), output)?;
            if copied > maximum {
                return Err(io::Error::other("recovery input grew past its file limit"));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                output.set_permissions(std::fs::Permissions::from_mode(if executable {
                    0o755
                } else {
                    0o644
                }))?;
            }
            #[cfg(not(unix))]
            let _ = executable;
            Ok(())
        })
    }

    pub fn symlink_new(&self, path: &Path, target: &Path) -> io::Result<bool> {
        relative(path)?;
        let (parent, name) = self.parent(path, true)?;
        #[cfg(unix)]
        let created = parent.symlink_contents(target, name);
        #[cfg(not(unix))]
        let created = parent.symlink_file(target, name);
        match created {
            Ok(()) => {
                sync_directory(&parent);
                Ok(true)
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn parent<'a>(&self, path: &'a Path, create: bool) -> io::Result<(Dir, &'a std::ffi::OsStr)> {
        let name = path.file_name().ok_or_else(|| io::Error::other("recovery filename is missing"))?;
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        Ok((recovery_parent(self.0.try_clone()?, parent, create)?, name))
    }
    fn publish(
        &self,
        path: &Path,
        fill: impl FnOnce(&mut std::fs::File) -> io::Result<()>,
    ) -> io::Result<bool> {
        relative(path)?;
        let (parent, name) = self.parent(path, true)?;
        if parent.symlink_metadata(name).is_ok() {
            return Ok(false);
        }
        let (temp, file) = temporary(&parent, true)?;
        let mut file = file.into_std();
        let result = (|| {
            fill(&mut file)?;
            file.sync_all()?;
            drop(file);
            match parent.hard_link(&temp, &parent, name) {
                Ok(()) => {
                    sync_directory(&parent);
                    Ok(true)
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
                Err(e) => Err(e),
            }
        })();
        let _ = parent.remove_file(&temp);
        result
    }
}

fn recovery_parent(mut dir: Dir, path: &Path, create: bool) -> io::Result<Dir> {
    relative(path)?;
    for part in path.components() {
        if part == std::path::Component::CurDir {
            continue;
        }
        let name = part.as_os_str();
        if create {
            match dir.create_dir(name) {
                Ok(()) => sync_directory(&dir),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
        dir = dir.open_dir_nofollow(name)?;
    }
    Ok(dir)
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
    write_with(root, path, bytes, false)
}

/// Atomic replacement with owner-only permissions, including temporary bytes.
pub fn write_private(root: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_with(root, path, bytes, true)
}

/// A stable, directory-relative inode for nonblocking cross-process locking.
/// Opening does not truncate existing data or follow links outside the root.
pub fn lock_file(root: &Path, path: &Path) -> io::Result<std::fs::File> {
    relative(path)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(directory(root)?.open_with(path, &options)?.into_std())
}

fn write_with(root: &Path, path: &Path, bytes: &[u8], private: bool) -> io::Result<()> {
    relative(path)?;
    let dir = directory(root)?;
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let parent = create_parent(&dir, parent)?;
    let name = path.file_name().ok_or_else(|| io::Error::other("file name is missing"))?;
    let permissions = match parent.symlink_metadata(name) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "refusing to replace a symlink"));
        }
        Ok(meta) => {
            if private {
                None
            } else {
                Some(meta.permissions())
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    let (temp, mut file) = temporary(&parent, private)?;
    let result = (|| {
        file.write_all(bytes)?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        file.sync_all()?;
        drop(file);
        parent.rename(&temp, &parent, name)?;
        sync_directory(&parent);
        Ok(())
    })();
    if result.is_err() {
        let _ = parent.remove_file(&temp);
    }
    result
}

// Keep the temporary file on the destination filesystem for atomic rename.
// A name left by a killed process is occupied, not ours to truncate or unlink.
fn temporary(parent: &Dir, private: bool) -> io::Result<(String, cap_std::fs::File)> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    for _ in 0..128 {
        let temp = format!(
            ".rook-write-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt;
            if private {
                options.mode(0o600);
            }
        }
        #[cfg(not(unix))]
        let _ = private;
        match parent.open_with(&temp, &options) {
            Ok(file) => return Ok((temp, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "could not reserve a temporary write file"))
}

/// Persist a directory entry, where the platform allows it at all.
///
/// Never fatal, and that is the point. By the time this is called after a
/// rename, the file is already in place: whether the entry has reached the
/// disk decides what survives a power cut and nothing about whether the write
/// happened. Returning the error made a completed write report as a failed one
/// — `rook skills rollback` wrote every file it meant to and then told the
/// person the skill was "part 3eed63bffed8 and part what was there before",
/// naming a recovery they did not need.
///
/// And it fired on two of the three platforms. A cap-std directory handle is
/// opened for lookup rather than for reading — `O_PATH` on Linux, the same idea
/// on FreeBSD — and `fsync` on such a descriptor is `EBADF`, which is how
/// "Bad file descriptor" came to be the answer to writing a file. macOS has no
/// such mode, so the local gate never saw it and CI saw it every time.
fn sync_directory(dir: &Dir) {
    #[cfg(unix)]
    if let Ok(handle) = dir.try_clone() {
        // A failure here is the platform saying it will not, not a fault to
        // report: the caller has already done what it was asked.
        let _ = handle.into_std_file().sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

fn create_parent(root: &Dir, path: &Path) -> io::Result<Dir> {
    root.create_dir_all(path)?;
    // A new nested destination also needs its ancestors' directory entries
    // persisted. The immediate parent is synced after publishing the file.
    #[cfg(unix)]
    for ancestor in path.ancestors().skip(1) {
        let ancestor = if ancestor.as_os_str().is_empty() { Path::new(".") } else { ancestor };
        sync_directory(&root.open_dir(ancestor)?);
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
    sync_directory(&destination);
    // A move across roots is a copy and then an unlink, because a rename cannot
    // cross them and must not follow a symlink on the way. So this is the one
    // step that can fail with the work half done — and it fails safe, leaving
    // both copies rather than none. Said outright: the raw error is
    // `Permission denied` on a path the caller did not name, which reads as the
    // move having done nothing while the destination sits there.
    if let Err(e) = source.remove_file(from) {
        return Err(io::Error::new(
            e.kind(),
            format!(
                "copied to the destination but could not remove {}: {e}. Both copies exist; \
                 remove the original by hand once you know why",
                from.to_string_lossy()
            ),
        ));
    }
    sync_directory(&source);
    Ok(bytes)
}
