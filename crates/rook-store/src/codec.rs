use crate::error::{Result, StoreError};
use crate::object::Kind;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

pub const CODEC_RAW: u8 = 0;
pub const CODEC_ZSTD: u8 = 1;
pub const CODEC_ZSTD_DICT: u8 = 2;

/// Default compression level. 9 sits at the knee of the ratio/CPU curve for the
/// small JSON payloads that dominate an agent store.
pub const DEFAULT_LEVEL: i32 = 9;

/// Payloads at or below this size are stored verbatim: zstd framing costs more
/// than it saves, and the value lives inline in the index either way.
const MIN_COMPRESS: usize = 64;

/// Zstd dictionaries, one per object kind, trained from real samples.
///
/// This is where most of the compactness comes from. An agent writes an endless
/// stream of small, structurally near-identical JSON blobs; compressed one at a
/// time they barely shrink, because zstd never sees enough context to build a
/// model. A 16 KiB dictionary trained on a few hundred of them turns each
/// 400-byte message into a few dozen bytes.
pub struct DictSet {
    dir: PathBuf,
    training: std::sync::Mutex<()>,
    /// Current dictionary first, followed by immutable previous generations.
    dicts: RwLock<HashMap<u8, Vec<Vec<u8>>>>,
}

impl DictSet {
    pub fn load(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir).map_err(|e| StoreError::io(&dir, e))?;
        let mut dicts: HashMap<u8, Vec<Vec<u8>>> = HashMap::new();
        for kind in Kind::ALL {
            let path = dir.join(format!("{}.zdict", kind.as_str()));
            match std::fs::read(&path) {
                Ok(bytes) => {
                    dicts.insert(kind as u8, vec![bytes]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(StoreError::io(&path, e)),
            }
            // The ones it replaced, newest first, so a decode tries the likely
            // one before the rest.
            for (_, bytes) in retired(&dir, kind)? {
                dicts.entry(kind as u8).or_default().push(bytes);
            }
        }
        Ok(Self { dir, dicts: RwLock::new(dicts), training: Default::default() })
    }

    /// The one new objects are compressed with.
    pub fn get(&self, kind: Kind) -> Option<Vec<u8>> {
        self.dicts.read().ok()?.get(&(kind as u8))?.first().cloned()
    }

    /// Every one an object of this kind might have been compressed with, the
    /// current one first.
    pub(crate) fn all(&self, kind: Kind) -> Vec<Vec<u8>> {
        self.dicts.read().ok().and_then(|d| d.get(&(kind as u8)).cloned()).unwrap_or_default()
    }

    pub fn has(&self, kind: Kind) -> bool {
        self.dicts.read().map(|d| d.contains_key(&(kind as u8))).unwrap_or(false)
    }

    /// Train a dictionary for `kind` from observed payloads and install it.
    ///
    /// Preserve the previous generation before atomically publishing a new one.
    /// Readers retain every generation across restarts; existing frames name
    /// their zstd dictionary, so no object metadata needs to be rewritten.
    pub fn train(&self, kind: Kind, samples: &[Vec<u8>], max_size: usize) -> Result<usize> {
        if samples.len() < MIN_SAMPLES {
            return Ok(0);
        }
        let _training = self.training.lock().unwrap_or_else(|e| e.into_inner());
        let refs: Vec<&[u8]> = samples.iter().map(|s| s.as_slice()).collect();
        let dict = zstd::dict::from_samples(&refs, max_size)
            .map_err(|e| StoreError::Encoding(format!("dictionary training failed: {e}")))?;
        let path = self.dir.join(format!("{}.zdict", kind.as_str()));
        let old = self.get(kind);
        if old.as_deref() == Some(dict.as_slice()) {
            return Ok(dict.len());
        }
        if let Some(old) = old {
            let next = retired(&self.dir, kind)?
                .first()
                .map(|(n, _)| n.checked_add(1))
                .unwrap_or(Some(1))
                .ok_or_else(|| StoreError::Encoding("dictionary generations exhausted".into()))?;
            let kept = self.dir.join(format!("{}.{next}.zdict", kind.as_str()));
            atomic_write(&kept, &old)?;
        }
        atomic_write(&path, &dict)?;
        let len = dict.len();
        self.dicts.write().unwrap_or_else(|e| e.into_inner()).entry(kind as u8).or_default().insert(0, dict);
        Ok(len)
    }
}

/// The dictionaries this kind has retired, highest generation first.
fn retired(dir: &Path, kind: Kind) -> Result<Vec<(u32, Vec<u8>)>> {
    let prefix = format!("{}.", kind.as_str());
    let mut found = Vec::new();
    let listing = match std::fs::read_dir(dir) {
        Ok(listing) => listing,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(found),
        Err(e) => return Err(StoreError::io(dir, e)),
    };
    for entry in listing {
        let entry = entry.map_err(|e| StoreError::io(dir, e))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(rest) = name.strip_prefix(&prefix) else { continue };
        let Some(generation) = rest.strip_suffix(".zdict").and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        let path = entry.path();
        found.push((generation, std::fs::read(&path).map_err(|e| StoreError::io(&path, e))?));
    }
    found.sort_by_key(|(generation, _)| std::cmp::Reverse(*generation));
    Ok(found)
}

/// Training on fewer samples than this produces a dictionary that is worse than
/// none at all.
pub const MIN_SAMPLES: usize = 32;

/// Compress `data`, choosing the cheapest encoding that actually wins.
pub fn encode(dicts: &DictSet, kind: Kind, data: &[u8], level: i32) -> Result<(u8, Vec<u8>)> {
    if data.len() <= MIN_COMPRESS {
        return Ok((CODEC_RAW, data.to_vec()));
    }

    let (codec, out) = match dicts.get(kind) {
        Some(dict) => {
            let mut c = zstd::bulk::Compressor::with_dictionary(level, &dict)
                .map_err(|e| StoreError::Encoding(e.to_string()))?;
            let out = c.compress(data).map_err(|e| StoreError::Encoding(e.to_string()))?;
            (CODEC_ZSTD_DICT, out)
        }
        None => {
            let out = zstd::bulk::compress(data, level).map_err(|e| StoreError::Encoding(e.to_string()))?;
            (CODEC_ZSTD, out)
        }
    };

    // Already-compressed payloads (images, archives, some tool output) come back
    // bigger. Storing those raw keeps the store honest about its own size.
    if out.len() >= data.len() {
        return Ok((CODEC_RAW, data.to_vec()));
    }
    Ok((codec, out))
}

pub fn decode(dicts: &DictSet, kind: Kind, codec: u8, data: &[u8], raw_size: usize) -> Result<Vec<u8>> {
    match codec {
        CODEC_RAW => Ok(data.to_vec()),
        CODEC_ZSTD => zstd::bulk::decompress(data, raw_size).map_err(|e| StoreError::Encoding(e.to_string())),
        CODEC_ZSTD_DICT => {
            // Which dictionary is not recorded anywhere, so every one this
            // store has is tried. Safe to try: a zstd frame names the
            // dictionary it was written with, so the wrong one is refused
            // rather than decoded into something else.
            let held = dicts.all(kind);
            if held.is_empty() {
                return Err(StoreError::Encoding(format!(
                    "object needs the {} dictionary but it is missing from the store",
                    kind.as_str()
                )));
            }
            let mut last = String::new();
            for dict in &held {
                let decoded = zstd::bulk::Decompressor::with_dictionary(dict)
                    .and_then(|mut d| d.decompress(data, raw_size));
                match decoded {
                    Ok(bytes) => return Ok(bytes),
                    Err(e) => last = e.to_string(),
                }
            }
            Err(StoreError::Encoding(format!(
                "{last}: none of the {} {} dictionaries this store has decode it, so the one it \
                 was written with is gone",
                held.len(),
                kind.as_str()
            )))
        }
        other => Err(StoreError::Encoding(format!("unknown codec {other}"))),
    }
}

/// Publish only complete, durable files. Temporary files are never loaded as dictionaries.
pub(crate) fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    atomic_write_in(path, data, path.parent().unwrap_or(Path::new(".")))
}

pub(crate) fn atomic_write_in(path: &Path, data: &[u8], staging: &Path) -> Result<()> {
    use std::io::Write;
    let tmp = staging.join(format!("{}.tmp", crate::new_session_id()));
    // Cleanup owns this name only after create_new succeeded.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|e| StoreError::io(&tmp, e))?;
    let result = (|| {
        file.write_all(data).and_then(|()| file.sync_all()).map_err(|e| StoreError::io(&tmp, e))?;
        drop(file);
        std::fs::rename(&tmp, path).map_err(|e| StoreError::io(path, e))?;
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            std::fs::File::open(parent)
                .and_then(|dir| dir.sync_all())
                .map_err(|e| StoreError::io(parent, e))?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}
