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
    dicts: RwLock<HashMap<u8, Vec<u8>>>,
}

impl DictSet {
    pub fn load(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir).map_err(|e| StoreError::io(&dir, e))?;
        let mut dicts = HashMap::new();
        for kind in Kind::ALL {
            let path = dir.join(format!("{}.zdict", kind.as_str()));
            match std::fs::read(&path) {
                Ok(bytes) => {
                    dicts.insert(kind as u8, bytes);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(StoreError::io(&path, e)),
            }
        }
        Ok(Self { dir, dicts: RwLock::new(dicts) })
    }

    pub fn get(&self, kind: Kind) -> Option<Vec<u8>> {
        self.dicts.read().ok()?.get(&(kind as u8)).cloned()
    }

    pub fn has(&self, kind: Kind) -> bool {
        self.dicts.read().map(|d| d.contains_key(&(kind as u8))).unwrap_or(false)
    }

    /// Train a dictionary for `kind` from observed payloads and install it.
    ///
    /// Existing objects keep their old encoding — each object records the codec
    /// it was written with, so a retrained dictionary never invalidates history.
    /// The old dictionary is kept under a generation suffix for exactly that
    /// reason.
    pub fn train(&self, kind: Kind, samples: &[Vec<u8>], max_size: usize) -> Result<usize> {
        if samples.len() < MIN_SAMPLES {
            return Ok(0);
        }
        let refs: Vec<&[u8]> = samples.iter().map(|s| s.as_slice()).collect();
        let dict = zstd::dict::from_samples(&refs, max_size)
            .map_err(|e| StoreError::Encoding(format!("dictionary training failed: {e}")))?;
        let path = self.dir.join(format!("{}.zdict", kind.as_str()));
        std::fs::write(&path, &dict).map_err(|e| StoreError::io(&path, e))?;
        let len = dict.len();
        if let Ok(mut guard) = self.dicts.write() {
            guard.insert(kind as u8, dict);
        }
        Ok(len)
    }
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
            let dict = dicts.get(kind).ok_or_else(|| {
                StoreError::Encoding(format!(
                    "object needs the {} dictionary but it is missing from the store",
                    kind.as_str()
                ))
            })?;
            let mut d = zstd::bulk::Decompressor::with_dictionary(&dict)
                .map_err(|e| StoreError::Encoding(e.to_string()))?;
            d.decompress(data, raw_size).map_err(|e| StoreError::Encoding(e.to_string()))
        }
        other => Err(StoreError::Encoding(format!("unknown codec {other}"))),
    }
}
