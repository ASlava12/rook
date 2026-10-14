use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use rook_contain::files::{RecoveryFiles, file_mode};
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct File {
    pub mode: u32,
    pub data: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Snapshot {
    pub dirs: Vec<String>,
    pub files: BTreeMap<String, File>,
}
impl Snapshot {
    pub fn capture(root: &Path, s: &Settings) -> Result<Self> {
        let files = RecoveryFiles::open(root, Path::new("."), false).map_err(|e| invalid(e.to_string()))?;
        let mut snap = Self { dirs: Vec::new(), files: BTreeMap::new() };
        let mut todo = vec![PathBuf::from(".")];
        let mut entries = 0usize;
        let mut total = 0usize;
        while let Some(dir) = todo.pop() {
            let names = files
                .entries(&dir, s.max_entries.saturating_sub(entries))
                .map_err(|e| invalid(format!("SSH source capture: {e}")))?;
            for (name, is_file, is_dir) in names {
                entries += 1;
                if s.exclude.contains(&name) {
                    continue;
                }
                let path = if dir == Path::new(".") { PathBuf::from(name) } else { dir.join(name) };
                let text = path
                    .to_str()
                    .filter(|p| valid(p))
                    .ok_or_else(|| invalid("SSH source path is not bounded UTF-8"))?
                    .to_owned();
                if is_dir {
                    snap.dirs.push(text);
                    todo.push(path);
                } else if is_file {
                    if snap.files.len() >= s.max_files {
                        return Err(invalid("execution_environment.max_files exceeded before SSH execution"));
                    }
                    let meta = files.metadata(&path).map_err(|e| invalid(e.to_string()))?;
                    let size = usize::try_from(meta.len()).map_err(|e| invalid(e.to_string()))?;
                    if size > s.file_bytes || size > s.total_bytes.saturating_sub(total) {
                        return Err(invalid(
                            "execution_environment file_bytes/total_bytes exceeded before SSH execution",
                        ));
                    }
                    let data = files.read(&path, size).map_err(|e| invalid(e.to_string()))?;
                    if data.len() != size {
                        return Err(invalid("SSH source changed while reading"));
                    }
                    total += data.len();
                    snap.files.insert(text, File { mode: file_mode(&meta), data: STANDARD.encode(data) });
                } else {
                    return Err(invalid(
                        "SSH source contains a link or nonregular entry; exclude it explicitly before transferring",
                    ));
                }
            }
        }
        snap.dirs.sort();
        Ok(snap)
    }
    pub fn hash(&self) -> Result<String> {
        let mut hash = Sha256::new();
        for path in &self.dirs {
            hash.update(b"d");
            hash.update((path.len() as u32).to_be_bytes());
            hash.update(path.as_bytes());
        }
        for (path, file) in &self.files {
            let data = STANDARD.decode(&file.data).map_err(|e| invalid(e.to_string()))?;
            hash.update(b"f");
            hash.update((path.len() as u32).to_be_bytes());
            hash.update(path.as_bytes());
            hash.update(file.mode.to_be_bytes());
            hash.update(Sha256::digest(data));
        }
        Ok(format!("{:x}", hash.finalize()))
    }
    pub fn frames(&self, s: &Settings) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        for path in &self.dirs {
            line(&mut bytes, &serde_json::json!({"type":"dir","path":path}), s)?;
        }
        for (path, file) in &self.files {
            line(
                &mut bytes,
                &serde_json::json!({"type":"file","path":path,"mode":file.mode,"data":file.data}),
                s,
            )?;
        }
        line(&mut bytes, &serde_json::json!({"type":"end","hash":self.hash()?}), s)?;
        Ok(bytes)
    }
    pub fn parse(bytes: &[u8], s: &Settings) -> Result<(serde_json::Value, Self)> {
        let mut snapshot = Self { dirs: Vec::new(), files: BTreeMap::new() };
        let mut metadata = None;
        let mut ended = None;
        let mut total = 0usize;
        let mut entries = 0usize;
        for raw in bytes.split(|b| *b == b'\n').filter(|line| !line.is_empty()) {
            if raw.len() > s.file_bytes.div_ceil(3) * 4 + 8192 {
                return Err(invalid("SSH file frame exceeded its byte limit"));
            }
            if ended.is_some() {
                return Err(invalid("SSH returned frames after its terminal certificate"));
            }
            let frame: serde_json::Value = serde_json::from_slice(raw).map_err(|e| invalid(e.to_string()))?;
            match frame["type"].as_str() {
                Some("meta") if metadata.is_none() && entries == 0 => {
                    if raw.len() > 8192 {
                        return Err(invalid("SSH metadata exceeds 8 KiB"));
                    }
                    metadata = Some(frame);
                }
                Some("dir") => {
                    entries += 1;
                    let path = frame["path"]
                        .as_str()
                        .filter(|p| valid(p) && !excluded(p, s))
                        .ok_or_else(|| invalid("invalid remote directory path"))?;
                    if snapshot.dirs.iter().any(|p| p == path) {
                        return Err(invalid("duplicate remote directory"));
                    }
                    snapshot.dirs.push(path.into());
                }
                Some("file") => {
                    entries += 1;
                    let path = frame["path"]
                        .as_str()
                        .filter(|p| valid(p) && !excluded(p, s))
                        .ok_or_else(|| invalid("invalid remote file path"))?;
                    let mode = frame["mode"]
                        .as_u64()
                        .filter(|m| *m <= 0o777)
                        .ok_or_else(|| invalid("invalid remote file mode"))?
                        as u32;
                    let text = frame["data"].as_str().ok_or_else(|| invalid("missing remote file bytes"))?;
                    if text.len() > s.file_bytes.div_ceil(3) * 4 {
                        return Err(invalid("remote encoded file exceeds file_bytes"));
                    }
                    let data = STANDARD.decode(text).map_err(|e| invalid(e.to_string()))?;
                    if data.len() > s.file_bytes
                        || data.len() > s.total_bytes.saturating_sub(total)
                        || snapshot.files.len() >= s.max_files
                    {
                        return Err(invalid("remote files exceed configured transfer bounds"));
                    }
                    total += data.len();
                    if snapshot.files.insert(path.into(), File { mode, data: text.into() }).is_some() {
                        return Err(invalid("duplicate remote file"));
                    }
                }
                Some("end") => {
                    ended = frame["hash"].as_str().map(str::to_owned);
                }
                _ => return Err(invalid("invalid SSH transfer frame")),
            }
            if entries > s.max_entries {
                return Err(invalid("remote entries exceed max_entries"));
            }
        }
        snapshot.dirs.sort();
        snapshot.validate(s)?;
        if ended.as_deref() != Some(&snapshot.hash()?) {
            return Err(invalid("SSH snapshot did not end with its matching source certificate"));
        }
        Ok((metadata.ok_or_else(|| invalid("SSH response omitted metadata"))?, snapshot))
    }
    pub fn validate(&self, s: &Settings) -> Result<()> {
        if self.files.len() > s.max_files || self.files.len() + self.dirs.len() > s.max_entries {
            return Err(invalid("SSH source exceeds its entry limits"));
        }
        let dirs = self.dirs.iter().collect::<std::collections::BTreeSet<_>>();
        if dirs.len() != self.dirs.len() {
            return Err(invalid("SSH source has duplicate directories"));
        }
        let mut total = 0usize;
        for path in self.dirs.iter().chain(self.files.keys()) {
            if !valid(path) || excluded(path, s) || (dirs.contains(path) && self.files.contains_key(path)) {
                return Err(invalid("SSH source has an invalid or conflicting path"));
            }
            let mut parent = Path::new(path).parent();
            while let Some(p) = parent.filter(|p| !p.as_os_str().is_empty()) {
                let parent_text = p.to_str().ok_or_else(|| invalid("SSH source parent is not UTF-8"))?;
                if !dirs.contains(&parent_text.to_owned()) {
                    return Err(invalid("SSH source omitted a parent directory"));
                }
                parent = p.parent();
            }
        }
        for file in self.files.values() {
            if file.mode > 0o777 || file.data.len() > s.file_bytes.div_ceil(3) * 4 {
                return Err(invalid("SSH source exceeds its file byte/mode limits"));
            }
            let data = STANDARD.decode(&file.data).map_err(|e| invalid(e.to_string()))?;
            if data.len() > s.file_bytes || data.len() > s.total_bytes.saturating_sub(total) {
                return Err(invalid("SSH source exceeds its total byte limit"));
            }
            total += data.len();
        }
        Ok(())
    }
    pub fn publish(
        &self,
        before: &Self,
        root: &Path,
        s: &Settings,
        recover: bool,
        mut progress: impl FnMut(&str, bool) -> Result<()>,
    ) -> Result<()> {
        self.validate(s)?;
        before.validate(s)?;
        let current = Self::capture(root, s)?;
        let paths = before
            .files
            .keys()
            .chain(self.files.keys())
            .chain(before.dirs.iter())
            .chain(self.dirs.iter())
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        fn state<'a>(snap: &'a Snapshot, path: &str) -> (Option<(&'a String, u32)>, bool) {
            (snap.files.get(path).map(|f| (&f.data, f.mode)), snap.dirs.iter().any(|p| p == path))
        }
        for path in &paths {
            let old = state(before, path);
            let new = state(self, path);
            let observed = state(&current, path);
            if old != new && observed != old && !(recover && observed == new) {
                return Err(invalid(format!(
                    "local SSH writeback conflict at {path}; inspect the retained before/after bytes"
                )));
            }
        }
        let files = RecoveryFiles::open(root, Path::new("."), false).map_err(|e| invalid(e.to_string()))?;
        let deleted_dirs = before.dirs.iter().filter(|path| !self.dirs.contains(path)).collect::<Vec<_>>();
        for path in &deleted_dirs {
            // A previous publication may already have removed this directory.
            if !current.dirs.contains(path) {
                continue;
            }
            for (name, _, _) in
                files.entries(Path::new(path), s.max_entries).map_err(|e| invalid(e.to_string()))?
            {
                let child = format!("{path}/{name}");
                if excluded(&child, s)
                    || (!before.files.contains_key(&child) && !deleted_dirs.contains(&&child))
                {
                    return Err(invalid(
                        "remote directory deletion would remove excluded or unrelated local data; resolve the conflict explicitly",
                    ));
                }
            }
        }
        // Remove obsolete files/directories before creating replacements. Every step
        // accepts an already-published after image only during explicit recovery.
        for (path, old) in &before.files {
            if self.files.contains_key(path) || !current.files.contains_key(path) {
                continue;
            }
            let data = STANDARD.decode(&old.data).map_err(|e| invalid(e.to_string()))?;
            progress(path, false)?;
            if !files
                .replace_checked(Path::new(path), Some(&data), Some(old.mode), None, 0o600)
                .map_err(|e| invalid(e.to_string()))?
            {
                return Err(invalid(format!("local SSH deletion conflict at {path}")));
            }
            progress(path, true)?;
        }
        for path in deleted_dirs.iter().rev() {
            if !current.dirs.contains(path) {
                continue;
            }
            progress(path, false)?;
            files.remove_directory(Path::new(path)).map_err(|e| invalid(e.to_string()))?;
            progress(path, true)?;
        }
        for path in &self.dirs {
            if before.dirs.contains(path) || (recover && current.dirs.contains(path)) {
                continue;
            }
            progress(path, false)?;
            RecoveryFiles::open(root, Path::new(path), true).map_err(|e| invalid(e.to_string()))?;
            progress(path, true)?;
        }
        for (path, new) in &self.files {
            let old = before.files.get(path);
            if old.map(|f| (&f.data, f.mode)) == Some((&new.data, new.mode))
                || (recover
                    && current.files.get(path).map(|f| (&f.data, f.mode)) == Some((&new.data, new.mode)))
            {
                continue;
            }
            let old_data =
                old.map(|f| STANDARD.decode(&f.data)).transpose().map_err(|e| invalid(e.to_string()))?;
            let new_data = STANDARD.decode(&new.data).map_err(|e| invalid(e.to_string()))?;
            progress(path, false)?;
            if !files
                .replace_checked(
                    Path::new(path),
                    old_data.as_deref(),
                    old.map(|f| f.mode),
                    Some(&new_data),
                    new.mode,
                )
                .map_err(|e| invalid(e.to_string()))?
            {
                return Err(invalid(format!(
                    "local SSH writeback conflict at {path}; inspect its retained pending path"
                )));
            }
            progress(path, true)?;
        }
        Ok(())
    }
}
pub(super) fn valid(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 1024
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
        && Path::new(path).components().all(|c| matches!(c, std::path::Component::Normal(_)))
}
fn excluded(path: &str, s: &Settings) -> bool {
    path.split('/').any(|part| s.exclude.iter().any(|n| n == part))
}
fn line(bytes: &mut Vec<u8>, value: &serde_json::Value, s: &Settings) -> Result<()> {
    let data = serde_json::to_vec(value).map_err(|e| invalid(e.to_string()))?;
    if data.len() > response_cap(s).saturating_sub(bytes.len()) {
        return Err(invalid("SSH encoded source exceeded transfer bounds"));
    }
    bytes.extend(data);
    bytes.push(b'\n');
    Ok(())
}
pub(super) fn response_cap(s: &Settings) -> usize {
    s.total_bytes.div_ceil(3) * 4 + s.max_entries * 8192 + 16384
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn directory_deletion_recovery_accepts_already_removed_paths_after_progress_persistence_failed() {
        let root = tempfile::tempdir().unwrap();
        let settings = Settings::default();
        for name in ["a", "z"] {
            std::fs::create_dir(root.path().join(name)).unwrap();
        }
        let before = Snapshot::capture(root.path(), &settings).unwrap();
        let after = Snapshot { dirs: Vec::new(), files: BTreeMap::new() };
        let failed = after.publish(&before, root.path(), &settings, false, |path, done| {
            if path == "z" && done { Err(invalid("lost progress persistence")) } else { Ok(()) }
        });
        assert!(failed.is_err());
        assert!(!root.path().join("z").exists());
        assert!(root.path().join("a").exists());
        after.publish(&before, root.path(), &settings, true, |_, _| Ok(())).unwrap();
        assert!(!root.path().join("a").exists());
    }
    #[test]
    fn publication_handles_file_directory_replacements_and_repeated_observed_recovery() {
        let root = tempfile::tempdir().unwrap();
        let settings = Settings::default();
        std::fs::write(root.path().join("file"), "old").unwrap();
        std::fs::create_dir(root.path().join("directory")).unwrap();
        std::fs::write(root.path().join("directory/child"), "old child").unwrap();
        let before = Snapshot::capture(root.path(), &settings).unwrap();
        let after = Snapshot {
            dirs: vec!["file".into()],
            files: BTreeMap::from([
                ("file/child".into(), File { mode: 0o600, data: STANDARD.encode("new child") }),
                ("directory".into(), File { mode: 0o600, data: STANDARD.encode("new file") }),
            ]),
        };
        after.publish(&before, root.path(), &settings, false, |_, _| Ok(())).unwrap();
        after.publish(&before, root.path(), &settings, true, |_, _| Ok(())).unwrap();
        assert_eq!(std::fs::read_to_string(root.path().join("file/child")).unwrap(), "new child");
        assert_eq!(std::fs::read_to_string(root.path().join("directory")).unwrap(), "new file");
    }
    #[test]
    fn retained_sources_reject_missing_parents_and_reached_byte_limits_before_hashing_or_publication() {
        let settings = Settings { file_bytes: 1024, total_bytes: 1024, ..Default::default() };
        let mut source = Snapshot {
            dirs: Vec::new(),
            files: BTreeMap::from([(
                "missing/child".into(),
                File { mode: 0o600, data: STANDARD.encode("bytes") },
            )]),
        };
        assert!(source.validate(&settings).is_err());
        source.dirs.push("missing".into());
        source.validate(&settings).unwrap();
        source.files.get_mut("missing/child").unwrap().data = STANDARD.encode(vec![0; 1025]);
        assert!(source.validate(&settings).is_err());
    }
}
