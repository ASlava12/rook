use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use std::io::{BufRead, Read, Write};
const CHUNK: usize = 64 * 1024;
const FRAME: usize = CHUNK + 8192;
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
            let (digest, _) = file_digest(file)?;
            hash.update(b"f");
            hash.update((path.len() as u32).to_be_bytes());
            hash.update(path.as_bytes());
            hash.update(file.mode.to_be_bytes());
            hash.update(digest);
        }
        Ok(format!("{:x}", hash.finalize()))
    }
    pub fn stream(
        &self,
        out: &mut dyn Write,
        s: &Settings,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<()> {
        self.validate(s)?;
        let mut writer = Frames { out, remaining: response_cap(s), cancelled };
        writer.line(&serde_json::json!({"type":"stream","version":2}))?;
        for path in &self.dirs {
            writer.line(&serde_json::json!({"type":"dir","path":path}))?;
        }
        for (path, file) in &self.files {
            let (digest, bytes) = file_digest(file)?;
            let digest = format!("{digest:x}");
            writer.line(&serde_json::json!({"type":"file_start","path":path,"mode":file.mode,"bytes":bytes,"sha256":digest}))?;
            let mut offset = 0usize;
            for chunk in file.data.as_bytes().chunks(CHUNK) {
                let data = std::str::from_utf8(chunk).map_err(|e| invalid(e.to_string()))?;
                writer.line(&serde_json::json!({"type":"chunk","offset":offset,"data":data}))?;
                offset += chunk.len() / 4 * 3 - chunk.iter().rev().take_while(|&&b| b == b'=').count();
            }
            writer.line(&serde_json::json!({"type":"file_end","sha256":digest}))?;
        }
        writer.line(&serde_json::json!({"type":"end","hash":self.hash()?}))
    }
    pub fn parse_reader(reader: &mut dyn Read, s: &Settings) -> Result<(serde_json::Value, Self)> {
        struct Open {
            path: String,
            mode: u32,
            data: String,
            bytes: usize,
            expected: usize,
            hash: String,
            digest: Sha256,
            padded: bool,
        }
        let mut reader = std::io::BufReader::new(reader);
        let mut snapshot = Self { dirs: Vec::new(), files: BTreeMap::new() };
        let mut metadata = None;
        let mut ended = None;
        let mut version = false;
        let mut open: Option<Open> = None;
        let mut total = 0usize;
        let mut entries = 0usize;
        loop {
            let mut raw = Vec::new();
            let read = (&mut reader)
                .take((FRAME + 1) as u64)
                .read_until(b'\n', &mut raw)
                .map_err(|e| invalid(e.to_string()))?;
            if read == 0 {
                break;
            }
            if raw.len() > FRAME {
                return Err(invalid("SSH chunk frame exceeded its byte limit"));
            }
            if raw.iter().all(|b| b.is_ascii_whitespace()) {
                continue;
            }
            if ended.is_some() {
                return Err(invalid("SSH returned frames after its terminal certificate"));
            }
            let frame: serde_json::Value =
                serde_json::from_slice(&raw).map_err(|e| invalid(e.to_string()))?;
            match frame["type"].as_str() {
                Some("meta") if metadata.is_none() && !version && entries == 0 => {
                    if raw.len() > 8192 || frame["stream_version"] != 2 {
                        return Err(invalid("SSH worker does not certify streaming transfer version 2"));
                    }
                    metadata = Some(frame);
                }
                Some("stream") if !version && frame["version"] == 2 && entries == 0 => {
                    version = true;
                }
                Some("dir" | "file_start") if version && open.is_none() => {
                    entries += 1;
                    if entries > s.max_entries {
                        return Err(invalid("remote entries exceed max_entries"));
                    }
                    let path = frame["path"]
                        .as_str()
                        .filter(|path| valid(path) && !excluded(path, s))
                        .ok_or_else(|| invalid("invalid remote transfer path"))?;
                    if snapshot.files.contains_key(path) || snapshot.dirs.iter().any(|p| p == path) {
                        return Err(invalid("duplicate remote transfer path"));
                    }
                    if frame["type"] == "dir" {
                        snapshot.dirs.push(path.into());
                        continue;
                    }
                    let mode = frame["mode"]
                        .as_u64()
                        .filter(|&m| m <= 0o777)
                        .ok_or_else(|| invalid("invalid remote file mode"))?
                        as u32;
                    let expected = frame["bytes"]
                        .as_u64()
                        .and_then(|n| usize::try_from(n).ok())
                        .filter(|&n| n <= s.file_bytes && n <= s.total_bytes.saturating_sub(total))
                        .ok_or_else(|| invalid("remote file bytes exceed configured limits"))?;
                    let hash = frame["sha256"]
                        .as_str()
                        .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
                        .ok_or_else(|| invalid("invalid remote file hash"))?;
                    if snapshot.files.len() >= s.max_files {
                        return Err(invalid("remote files exceed max_files"));
                    }
                    open = Some(Open {
                        path: path.into(),
                        mode,
                        data: String::new(),
                        bytes: 0,
                        expected,
                        hash: hash.into(),
                        digest: Sha256::new(),
                        padded: false,
                    });
                }
                Some("chunk") if open.is_some() => {
                    let file = open.as_mut().ok_or_else(|| invalid("missing SSH file start"))?;
                    if file.padded || frame["offset"].as_u64() != Some(file.bytes as u64) {
                        return Err(invalid("SSH file chunk offset changed or follows padded data"));
                    }
                    let text = frame["data"]
                        .as_str()
                        .filter(|s| !s.is_empty() && s.len() <= CHUNK)
                        .ok_or_else(|| invalid("SSH chunk bytes exceeded their limit"))?;
                    let data = STANDARD.decode(text).map_err(|e| invalid(e.to_string()))?;
                    if data.len() > file.expected.saturating_sub(file.bytes) {
                        return Err(invalid("SSH file grew beyond its admitted size"));
                    }
                    file.digest.update(&data);
                    file.bytes += data.len();
                    file.data.push_str(text);
                    file.padded = text.ends_with('=');
                }
                Some("file_end") if open.is_some() => {
                    let file = open.take().ok_or_else(|| invalid("missing SSH file start"))?;
                    if file.bytes != file.expected
                        || format!("{:x}", file.digest.finalize()) != file.hash
                        || frame["sha256"].as_str() != Some(file.hash.as_str())
                    {
                        return Err(invalid("SSH streamed file certificate mismatch"));
                    }
                    total += file.bytes;
                    snapshot.files.insert(file.path, File { mode: file.mode, data: file.data });
                }
                Some("end") if version && open.is_none() => {
                    ended = frame["hash"].as_str().map(str::to_owned);
                }
                _ => return Err(invalid("unsupported SSH transfer frame; streaming version 2 is required")),
            }
        }
        if open.is_some() {
            return Err(invalid("SSH file stream ended before its certificate"));
        }
        snapshot.dirs.sort();
        snapshot.validate(s)?;
        if !version || ended.as_deref() != Some(&snapshot.hash()?) {
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
            let (_, bytes) = file_digest(file)?;
            if bytes > s.file_bytes || bytes > s.total_bytes.saturating_sub(total) {
                return Err(invalid("SSH source exceeds its total byte limit"));
            }
            total += bytes;
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
fn file_digest(file: &File) -> Result<(sha2::digest::Output<Sha256>, usize)> {
    let mut reader = base64::read::DecoderReader::new(file.data.as_bytes(), &STANDARD);
    let mut digest = Sha256::new();
    let mut bytes = 0usize;
    let mut chunk = [0u8; 16384];
    loop {
        let n = reader.read(&mut chunk).map_err(|e| invalid(e.to_string()))?;
        if n == 0 {
            break;
        }
        bytes += n;
        digest.update(&chunk[..n]);
    }
    Ok((digest.finalize(), bytes))
}
struct Frames<'a> {
    out: &'a mut dyn Write,
    remaining: usize,
    cancelled: &'a std::sync::atomic::AtomicBool,
}
impl Write for Frames<'_> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.cancelled.load(std::sync::atomic::Ordering::Acquire) {
            return Err(std::io::Error::other("SSH transfer cancelled"));
        }
        if data.len() > self.remaining {
            return Err(std::io::Error::other("SSH encoded source exceeded transfer bounds"));
        }
        self.out.write_all(data)?;
        self.remaining -= data.len();
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}
impl Frames<'_> {
    fn line(&mut self, value: &serde_json::Value) -> Result<()> {
        serde_json::to_writer(&mut *self, value).map_err(|e| invalid(e.to_string()))?;
        self.write_all(b"\n").map_err(|e| invalid(e.to_string()))
    }
}
pub(super) fn response_cap(s: &Settings) -> usize {
    s.total_bytes.div_ceil(3) * 4
        + s.max_entries * 8192
        + (s.total_bytes.div_ceil(CHUNK / 4 * 3) + s.max_files) * 256
        + 16384
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chunked_snapshots_round_trip_binary_and_empty_files_and_reject_partial_or_reordered_payloads() {
        let settings = Settings::default();
        let bytes: Vec<u8> = (0..200000).map(|n| (n % 251) as u8).collect();
        let source = Snapshot {
            dirs: vec!["dir".into()],
            files: BTreeMap::from([
                ("dir/binary".into(), File { mode: 0o600, data: STANDARD.encode(&bytes) }),
                ("empty".into(), File { mode: 0o644, data: String::new() }),
            ]),
        };
        let mut wire = b"{\"type\":\"meta\",\"stream_version\":2}\n".to_vec();
        source.stream(&mut wire, &settings, &std::sync::atomic::AtomicBool::new(false)).unwrap();
        assert!(wire.len() > CHUNK * 3, "fixture must reach multiple chunks");
        assert!(wire.split(|&b| b == b'\n').all(|line| line.len() <= FRAME));
        let (_, restored) = Snapshot::parse_reader(&mut wire.as_slice(), &settings).unwrap();
        assert_eq!(restored.hash().unwrap(), source.hash().unwrap());
        assert_eq!(STANDARD.decode(&restored.files["dir/binary"].data).unwrap(), bytes);
        assert!(restored.files["empty"].data.is_empty());
        let mut rows: Vec<serde_json::Value> = wire
            .split(|&b| b == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        let original = rows.clone();
        let chunk = rows.iter().position(|row| row["type"] == "chunk").unwrap();
        rows[chunk]["offset"] = serde_json::json!(1);
        let encode = |rows: &[serde_json::Value]| {
            rows.iter()
                .flat_map(|row| {
                    let mut line = serde_json::to_vec(row).unwrap();
                    line.push(b'\n');
                    line
                })
                .collect::<Vec<_>>()
        };
        assert!(Snapshot::parse_reader(&mut encode(&rows).as_slice(), &settings).is_err());
        assert!(Snapshot::parse_reader(&mut encode(&original[..chunk + 1]).as_slice(), &settings).is_err());
        let mut hash_mismatch = original.clone();
        hash_mismatch[chunk]["data"] = serde_json::json!(STANDARD.encode(vec![42u8; CHUNK / 4 * 3]));
        assert!(Snapshot::parse_reader(&mut encode(&hash_mismatch).as_slice(), &settings).is_err());
        let mut old_worker = original.clone();
        old_worker[0]["stream_version"] = serde_json::json!(1);
        assert!(Snapshot::parse_reader(&mut encode(&old_worker).as_slice(), &settings).is_err());
        let mut over = settings.clone();
        over.file_bytes = bytes.len() - 1;
        assert!(Snapshot::parse_reader(&mut wire.as_slice(), &over).is_err());
        let cancelled = std::sync::atomic::AtomicBool::new(true);
        let mut no_bytes = Vec::new();
        assert!(source.stream(&mut no_bytes, &settings, &cancelled).is_err());
        assert!(no_bytes.is_empty());
    }
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
    #[cfg(unix)] // Managed SSH execution requires a Unix client and Unix path/mode sources.
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
