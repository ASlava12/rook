//! Compact, content-addressed storage for an autonomous agent.
//!
//! # Why this exists
//!
//! Agent transcripts are the most redundant data a developer tool produces. The
//! same system prompt, the same file re-read, the same directory listing, the
//! same failing test output — over and over, across thousands of turns. Tools in
//! this space routinely turn that into gigabytes: an unbounded SQLite log, or a
//! `git add .` over the whole workspace on every checkpoint.
//!
//! Three mechanisms keep this store small:
//!
//! 1. **Content addressing.** Objects are keyed by blake3 hash, so repeated
//!    payloads are stored exactly once no matter how many sessions reference them.
//! 2. **Trained zstd dictionaries per object kind.** Small JSON blobs barely
//!    compress alone; against a dictionary trained on their own shape they shrink
//!    by an order of magnitude. See [`codec::DictSet`].
//! 3. **Inlining.** Anything under [`schema::INLINE_MAX`] lives inside the redb
//!    index instead of becoming its own file, so millions of small objects do not
//!    become millions of inodes.
//!
//! Growth is bounded on purpose: [`Store::prune`] enforces a retention policy and
//! [`Store::gc`] reclaims whatever is no longer reachable.

pub mod codec;
pub mod error;
pub mod maintenance;
pub mod object;
pub mod schema;
pub mod stats;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use redb::{Database, ReadableDatabase, ReadableTable, WriteTransaction};

pub use error::{Result, StoreError};
pub use maintenance::{GcOptions, GcReport, PruneReport, RetentionPolicy};
pub use object::{Kind, ObjectId, ObjectMeta};
pub use schema::{Event, EventKind, EventRecord, FORMAT_VERSION, NewEvent, SessionMeta};
pub use stats::{KindStats, StoreStats};

/// Generate a fresh session id.
///
/// Monotonic within the process, which plain ULIDs are not: they order by
/// millisecond and then by random bits, so two sessions started in the same
/// millisecond sort in a random order — and that ordering is what decides which
/// one `--session last` means. The generator keeps the timestamp and increments
/// the random part instead when the millisecond repeats.
pub fn new_session_id() -> u128 {
    monotonic().0
}

/// A name for one entry of an appended-to history, ordered by when it was made.
///
/// Those refs are read back in the order their names sort, and the name used to
/// be a millisecond stamp with the object's hash after it. Two changes in the
/// same millisecond — which two tool calls in a row are — then tied, and the tie
/// resolved by the hash, which is to say arbitrarily: `memory history` reported
/// the older change first. A monotonic ULID cannot tie.
///
/// Sorts after the old format for as long as both are in a store, which is the
/// right way round: `0000017…` is smaller than `01…` and is also older.
pub fn history_key() -> String {
    monotonic().to_string()
}

fn monotonic() -> ulid::Ulid {
    static GENERATOR: std::sync::Mutex<Option<ulid::Generator>> = std::sync::Mutex::new(None);
    let mut held = GENERATOR.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let generator = held.get_or_insert_with(ulid::Generator::new);
    // Only after 2^80 ids inside one millisecond, and a fresh random one is
    // still ordered correctly against every other millisecond.
    generator.generate().unwrap_or_else(|_| ulid::Ulid::generate())
}

pub fn format_session_id(id: u128) -> String {
    ulid::Ulid(id).to_string()
}

pub fn parse_session_id(s: &str) -> Option<u128> {
    ulid::Ulid::from_string(s).ok().map(|u| u.0)
}

/// Today, as `YYYY-MM-DD` in UTC.
///
/// Arithmetic rather than a date crate: the whole need is one line in a prompt,
/// and Howard Hinnant's civil-from-days is fifteen lines that have been correct
/// since 1970 and will be until the type overflows.
pub fn today() -> String {
    let (y, m, d) = civil_from_days(now_unix().div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

/// The date a given instant falls on, for a test that must not depend on today.
#[doc(hidden)]
pub fn date_of_unix_for_test(unix: i64) -> String {
    let (y, m, d) = civil_from_days(unix.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days since the epoch to a calendar date, by shifting the year to start in
/// March so the leap day falls at the end and needs no special case.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (era * 400 + yoe + i64::from(month <= 2), month, day)
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// One object as a listing shows it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ObjectRow {
    pub id: String,
    pub short: String,
    pub kind: String,
    pub size_raw: u64,
    pub size_stored: u64,
    pub external: bool,
    pub created_at: i64,
}

/// One name pointing at an object.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RefRow {
    #[serde(rename = "ref")]
    pub name: String,
    pub object: String,
    pub short: String,
}

/// A handle to the on-disk store. Cheap to clone by `Arc`, safe to share across
/// threads; redb serializes writers internally.
pub struct Store {
    db: Database,
    root: PathBuf,
    dicts: codec::DictSet,
    level: i32,
    /// Events committed since the last one that was flushed to disk.
    ///
    /// Not a queue of anything: redb makes an `Immediate` commit durable along
    /// with everything committed before it, so this counts how much a power cut
    /// could take rather than how much is waiting to be written.
    unflushed: AtomicU64,
}

/// How many events may ride on the next durable commit.
///
/// A flush costs about eight milliseconds on an ordinary disk, and paying it
/// per event cost a two-hundred-step turn four seconds of doing nothing but
/// writing down what it had just done — measured by `cargo xtask load`, which
/// found appending an event two orders of magnitude dearer than anything else a
/// turn does.
///
/// So the flush moved to the points a turn can be returned to rather than
/// happening between every pair of them: a checkpoint, a compaction, and the
/// end of a turn. This bounds what is lost when none of those has come round
/// for a while — a long autonomous run that takes no checkpoints is the case —
/// and a flush every two hundred and fifty-six events is thirty microseconds an
/// event, which is nothing beside the work an event records.
const EVENTS_PER_FLUSH: u64 = 256;

/// The last flush, for a process that ends between turns rather than at one.
///
/// `rook run` is one turn and then an exit, and a daemon told to stop is a
/// store dropped with whatever the turn before left behind. Neither goes
/// through [`Store::flush`] on its own, and without this the events would be in
/// the page cache when the machine lost power.
///
/// A release build aborts on panic, so this does not run then. That is the
/// right way round: a process that is already wrong should not be writing.
impl Drop for Store {
    fn drop(&mut self) {
        // Nothing to say and nowhere to say it — the store is going away, and a
        // logger may already have.
        let _ = self.flush();
    }
}

impl Store {
    /// Open (creating if needed) a store rooted at `root`, e.g. `~/.rook`.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        for sub in ["objects", "dicts", "tmp"] {
            let p = root.join(sub);
            std::fs::create_dir_all(&p).map_err(|e| StoreError::io(&p, e))?;
        }

        let index = root.join("index.redb");
        let db = match Database::create(&index) {
            Ok(db) => db,
            // One writer at a time is redb's contract, and the daemon usually
            // holds it. Say so, instead of surfacing a lock error the user has
            // no way to act on.
            Err(redb::DatabaseError::DatabaseAlreadyOpen) => return Err(StoreError::Locked { path: index }),
            Err(e) => return Err(e.into()),
        };
        Self::check_format(&root)?;
        // Create every table up front so read transactions never race a missing
        // table on a fresh store.
        let txn = db.begin_write()?;
        {
            txn.open_table(schema::OBJECTS)?;
            txn.open_table(schema::BLOBS)?;
            txn.open_table(schema::REFS)?;
            txn.open_table(schema::SESSIONS)?;
            txn.open_table(schema::EVENTS)?;
            txn.open_table(schema::KV)?;
        }
        txn.commit()?;

        let dicts = codec::DictSet::load(root.join("dicts"))?;
        Ok(Self { db, root, dicts, level: codec::DEFAULT_LEVEL, unflushed: Default::default() })
    }

    fn check_format(root: &Path) -> Result<()> {
        let path = root.join("format.json");
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let v: serde_json::Value =
                    serde_json::from_str(&text).map_err(|e| StoreError::Encoding(e.to_string()))?;
                let found = v
                    .get("format")
                    .and_then(|f| f.as_u64())
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or_else(|| StoreError::Encoding("invalid store format marker".into()))?;
                if found > FORMAT_VERSION {
                    return Err(StoreError::FormatTooNew { found, supported: FORMAT_VERSION });
                }
                if found < FORMAT_VERSION {
                    let mut v = v;
                    v["format"] = FORMAT_VERSION.into();
                    codec::atomic_write(&path, &serde_json::to_vec_pretty(&v).unwrap_or_default())?;
                }
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let body = serde_json::json!({
                    "format": FORMAT_VERSION,
                    "created_at": now_unix(),
                });
                // `to_string_pretty` over a map of a number and a timestamp
                // has no failing case, and an empty file would be a store that
                // reads as a different format rather than as a missing one.
                let written = serde_json::to_vec_pretty(&body).unwrap_or_default();
                codec::atomic_write(&path, &written)
            }
            Err(e) => Err(StoreError::io(&path, e)),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn dicts(&self) -> &codec::DictSet {
        &self.dicts
    }

    pub fn set_level(&mut self, level: i32) {
        self.level = level;
    }

    /// Sliced by byte because the name is hex: `to_hex` writes nothing but
    /// ASCII, so every byte is a character and the fan-out cannot cut one.
    #[allow(clippy::string_slice)]
    fn object_path(&self, id: &ObjectId) -> PathBuf {
        let hex = id.to_hex();
        self.root.join("objects").join(&hex[0..2]).join(&hex[2..4]).join(&hex)
    }

    // ---------------------------------------------------------------- objects

    /// Store `data`, returning its content id. A duplicate reuses a readable
    /// object; supplying the original bytes repairs an unreadable dedup hit.
    pub fn put(&self, kind: Kind, data: &[u8]) -> Result<ObjectId> {
        let txn = self.db.begin_write()?;
        let id = self.put_tx(&txn, kind, data)?;
        txn.commit()?;
        Ok(id)
    }

    fn put_tx(&self, txn: &WriteTransaction, kind: Kind, data: &[u8]) -> Result<ObjectId> {
        let id = ObjectId::of(data);
        let mut kind = kind;
        {
            let objects = txn.open_table(schema::OBJECTS)?;
            if let Some(existing) = objects.get(id.as_bytes())? {
                let meta: ObjectMeta = postcard::from_bytes(existing.value())?;
                let stored = if meta.external {
                    std::fs::read(self.object_path(&id)).ok()
                } else {
                    txn.open_table(schema::BLOBS)?.get(id.as_bytes())?.map(|blob| blob.value().to_vec())
                };
                if stored
                    .as_deref()
                    .and_then(|bytes| {
                        codec::decode(
                            &self.dicts,
                            Kind::from_u8(meta.kind),
                            meta.codec,
                            bytes,
                            meta.size_raw as usize,
                        )
                        .ok()
                    })
                    .is_some_and(|bytes| ObjectId::of(&bytes) == id)
                {
                    return Ok(id);
                }
                // Supplied original bytes repair an unreadable dedup hit.
                kind = Kind::from_u8(meta.kind);
            }
        }

        let (codec_id, encoded) = codec::encode(&self.dicts, kind, data, self.level)?;
        let external = encoded.len() > schema::INLINE_MAX;

        if external {
            // Write the payload before recording it. A crash between the two
            // leaves an unreferenced file, which `gc` reclaims — the reverse
            // order would leave an index entry pointing at nothing.
            let path = self.object_path(&id);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| StoreError::io(parent, e))?;
            }
            codec::atomic_write_in(&path, &encoded, &self.root.join("tmp"))?;
            // A newly created hash-prefix directory must survive the same
            // power loss as the object and the committed database entry.
            #[cfg(unix)]
            {
                let objects = self.root.join("objects");
                std::fs::File::open(&objects)
                    .and_then(|dir| dir.sync_all())
                    .map_err(|e| StoreError::io(&objects, e))?;
            }
        }

        let meta = ObjectMeta {
            kind: kind as u8,
            codec: codec_id,
            size_raw: data.len() as u64,
            size_stored: encoded.len() as u64,
            created_at: now_unix(),
            external,
        };
        let encoded_meta = postcard::to_stdvec(&meta)?;

        let mut objects = txn.open_table(schema::OBJECTS)?;
        objects.insert(id.as_bytes(), encoded_meta.as_slice())?;
        if !external {
            let mut blobs = txn.open_table(schema::BLOBS)?;
            blobs.insert(id.as_bytes(), encoded.as_slice())?;
        }
        Ok(id)
    }

    pub fn stat_object(&self, id: &ObjectId) -> Result<Option<ObjectMeta>> {
        let txn = self.db.begin_read()?;
        let objects = txn.open_table(schema::OBJECTS)?;
        match objects.get(id.as_bytes())? {
            Some(v) => Ok(Some(postcard::from_bytes(v.value())?)),
            None => Ok(None),
        }
    }

    pub fn has(&self, id: &ObjectId) -> Result<bool> {
        Ok(self.stat_object(id)?.is_some())
    }

    /// Read an object back, decompressing and verifying its hash.
    pub fn get(&self, id: &ObjectId) -> Result<Vec<u8>> {
        let meta = self.stat_object(id)?.ok_or_else(|| StoreError::MissingObject(id.short()))?;

        let stored = if meta.external {
            let path = self.object_path(id);
            std::fs::read(&path).map_err(|e| StoreError::io(&path, e))?
        } else {
            let txn = self.db.begin_read()?;
            let blobs = txn.open_table(schema::BLOBS)?;
            blobs
                .get(id.as_bytes())?
                .ok_or_else(|| StoreError::Corrupt {
                    id: id.short(),
                    reason: "index entry has no inline payload".into(),
                })?
                .value()
                .to_vec()
        };

        let data = codec::decode(
            &self.dicts,
            Kind::from_u8(meta.kind),
            meta.codec,
            &stored,
            meta.size_raw as usize,
        )?;

        if ObjectId::of(&data) != *id {
            return Err(StoreError::Corrupt {
                id: id.short(),
                reason: "content hash mismatch after decode".into(),
            });
        }
        Ok(data)
    }

    /// Read a byte range with bounded memory, still verifying the whole object's
    /// size and hash. Compressed results need a sequential decode; long command
    /// spills use seekable files in the core instead.
    pub fn get_range(&self, id: &ObjectId, offset: u64, limit: usize) -> Result<Vec<u8>> {
        use std::io::{BufReader, Read};
        let meta = self.stat_object(id)?.ok_or_else(|| StoreError::MissingObject(id.short()))?;
        let corrupt = |reason: &str| StoreError::Corrupt { id: id.short(), reason: reason.into() };
        if offset > meta.size_raw {
            return Err(corrupt("range starts past the object's end"));
        }
        let source = || -> Result<Box<dyn Read>> {
            if meta.external {
                let path = self.object_path(id);
                Ok(Box::new(std::fs::File::open(&path).map_err(|e| StoreError::io(&path, e))?))
            } else {
                let txn = self.db.begin_read()?;
                let blobs = txn.open_table(schema::BLOBS)?;
                let bytes = blobs
                    .get(id.as_bytes())?
                    .ok_or_else(|| corrupt("index entry has no inline payload"))?
                    .value()
                    .to_vec();
                Ok(Box::new(std::io::Cursor::new(bytes)))
            }
        };
        let dictionaries = match meta.codec {
            codec::CODEC_ZSTD_DICT => {
                self.dicts.all(Kind::from_u8(meta.kind)).into_iter().map(Some).collect()
            }
            codec::CODEC_RAW | codec::CODEC_ZSTD => vec![None],
            _ => return Err(corrupt("unknown codec")),
        };
        let mut last = "required dictionary is missing".to_string();
        for dict in dictionaries {
            let attempt = (|| -> Result<Vec<u8>> {
                let mut reader: Box<dyn Read> = match meta.codec {
                    codec::CODEC_RAW => source()?,
                    _ => Box::new(
                        zstd::stream::read::Decoder::with_dictionary(
                            BufReader::new(source()?),
                            dict.as_deref().unwrap_or_default(),
                        )
                        .map_err(|e| StoreError::Encoding(e.to_string()))?,
                    ),
                };
                let mut hash = blake3::Hasher::new();
                let mut chunk = [0u8; 64 * 1024];
                let mut seen = 0u64;
                let mut kept = Vec::new();
                loop {
                    let n = reader.read(&mut chunk).map_err(|e| StoreError::Encoding(e.to_string()))?;
                    if n == 0 {
                        break;
                    }
                    let end = seen.saturating_add(n as u64);
                    if end > meta.size_raw {
                        return Err(corrupt("decoded bytes exceed recorded size"));
                    }
                    hash.update(&chunk[..n]);
                    let start = offset.max(seen);
                    let stop = offset.saturating_add(limit as u64).min(end);
                    if start < stop {
                        kept.extend_from_slice(&chunk[(start - seen) as usize..(stop - seen) as usize]);
                    }
                    seen = end;
                }
                if seen != meta.size_raw || hash.finalize().as_bytes() != id.as_bytes() {
                    return Err(corrupt("content size or hash mismatch after decode"));
                }
                Ok(kept)
            })();
            match attempt {
                Ok(bytes) => return Ok(bytes),
                Err(why) => last = why.to_string(),
            }
        }
        Err(corrupt(&last))
    }

    /// Resolve a unique hash prefix, the way `git` resolves a short sha.
    pub fn resolve_prefix(&self, prefix: &str) -> Result<Option<ObjectId>> {
        if let Some(id) = ObjectId::from_hex(prefix) {
            return Ok(self.has(&id)?.then_some(id));
        }
        // A hash prefix is hex, so it is ASCII, and anything else resolves to
        // nothing. Asked before the byte below rather than left to `hex::decode`
        // afterwards: `prefix.len() - 1` is a byte index into whatever a person
        // typed, and one byte back from the `é` in `café` is inside it. That is
        // a panic on an argument, in a library, from a mistyped id.
        if !prefix.is_ascii() {
            return Ok(None);
        }
        // Now a byte is a character. An odd number of hex digits decodes as one
        // fewer byte, and the range narrows by the last digit below.
        #[allow(clippy::string_slice)]
        let head = if prefix.len() % 2 == 1 { &prefix[..prefix.len() - 1] } else { prefix };
        let Ok(raw) = hex::decode(head) else {
            return Ok(None);
        };
        let txn = self.db.begin_read()?;
        let objects = txn.open_table(schema::OBJECTS)?;
        let mut found = None;
        for entry in objects.range(raw.as_slice()..)? {
            let (k, _) = entry?;
            if !k.value().starts_with(&raw) {
                break;
            }
            let hex_full = hex::encode(k.value());
            if !hex_full.starts_with(prefix) {
                continue;
            }
            if found.is_some() {
                return Ok(None); // ambiguous
            }
            found = ObjectId::from_hex(&hex_full);
        }
        Ok(found)
    }

    /// The same listing, in the shape every front end shows it in.
    ///
    /// The CLI, the API and the browser all answer this question, and each had
    /// built its own JSON for it — `raw` in one and `size_raw` in the next, for
    /// the same number. One row type, so the answer does not depend on who was
    /// asked.
    pub fn object_rows(&self, kind: Option<Kind>, limit: usize) -> Result<Vec<ObjectRow>> {
        Ok(self
            .list_objects(kind, limit)?
            .into_iter()
            .map(|(id, m)| ObjectRow {
                short: id.short(),
                id: id.to_hex(),
                kind: Kind::from_u8(m.kind).as_str().to_string(),
                size_raw: m.size_raw,
                size_stored: m.size_stored,
                external: m.external,
                created_at: m.created_at,
            })
            .collect())
    }

    pub fn ref_rows(&self, prefix: &str) -> Result<Vec<RefRow>> {
        Ok(self
            .list_refs(prefix)?
            .into_iter()
            .map(|(name, id)| RefRow { name, short: id.short(), object: id.to_hex() })
            .collect())
    }

    pub fn list_objects(&self, kind: Option<Kind>, limit: usize) -> Result<Vec<(ObjectId, ObjectMeta)>> {
        let txn = self.db.begin_read()?;
        let objects = txn.open_table(schema::OBJECTS)?;
        let mut out = Vec::new();
        for entry in objects.iter()? {
            let (k, v) = entry?;
            let meta: ObjectMeta = postcard::from_bytes(v.value())?;
            if let Some(want) = kind
                && meta.kind != want as u8
            {
                continue;
            }
            let Some(id) = ObjectId::from_hex(&hex::encode(k.value())) else { continue };
            out.push((id, meta));
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    // ------------------------------------------------------------------- refs

    pub fn set_ref(&self, name: &str, id: &ObjectId) -> Result<()> {
        let txn = self.db.begin_write()?;
        {
            let mut refs = txn.open_table(schema::REFS)?;
            refs.insert(name, id.as_bytes())?;
        }
        txn.commit()?;
        Ok(())
    }

    /// Publish a version and its history entry only if the head has not moved.
    pub fn compare_set_ref(
        &self,
        name: &str,
        expected: Option<ObjectId>,
        id: &ObjectId,
        history: &str,
    ) -> Result<bool> {
        let txn = self.db.begin_write()?;
        {
            let mut refs = txn.open_table(schema::REFS)?;
            let actual = refs.get(name)?.map(|v| v.value().to_vec());
            if actual.as_deref() != expected.as_ref().map(|id| id.as_bytes()) {
                return Ok(false);
            }
            refs.insert(name, id.as_bytes())?;
            refs.insert(history, id.as_bytes())?;
        }
        txn.commit()?;
        Ok(true)
    }

    pub fn get_ref(&self, name: &str) -> Result<Option<ObjectId>> {
        let txn = self.db.begin_read()?;
        let refs = txn.open_table(schema::REFS)?;
        Ok(refs.get(name)?.and_then(|v| ObjectId::from_hex(&hex::encode(v.value()))))
    }

    pub fn delete_ref(&self, name: &str) -> Result<bool> {
        let txn = self.db.begin_write()?;
        let existed = {
            let mut refs = txn.open_table(schema::REFS)?;
            refs.remove(name)?.is_some()
        };
        txn.commit()?;
        Ok(existed)
    }

    /// List refs under a `prefix`, e.g. `"skills/"` or `"snapshots/"`.
    pub fn list_refs(&self, prefix: &str) -> Result<Vec<(String, ObjectId)>> {
        let txn = self.db.begin_read()?;
        let refs = txn.open_table(schema::REFS)?;
        let mut out = Vec::new();
        for entry in refs.iter()? {
            let (k, v) = entry?;
            let name = k.value().to_string();
            if !name.starts_with(prefix) {
                continue;
            }
            if let Some(id) = ObjectId::from_hex(&hex::encode(v.value())) {
                out.push((name, id));
            }
        }
        Ok(out)
    }

    // --------------------------------------------------------------- sessions

    pub fn create_session(&self, meta: &SessionMeta) -> Result<()> {
        let txn = self.db.begin_write()?;
        {
            let mut sessions = txn.open_table(schema::SESSIONS)?;
            let encoded = postcard::to_stdvec(meta)?;
            sessions.insert(schema::session_key(meta.id).as_slice(), encoded.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    /// Change a session's record in place.
    ///
    /// The read and the write are one transaction, which reading it, changing a
    /// field and calling `create_session` is not: an event appended between the
    /// two is an event whose counters the write puts back. Everything that
    /// edits an existing record goes through here; `create_session` is for one
    /// that did not exist.
    pub fn update_session<F: FnOnce(&mut SessionMeta)>(&self, id: u128, change: F) -> Result<bool> {
        let txn = self.db.begin_write()?;
        let found = {
            let mut sessions = txn.open_table(schema::SESSIONS)?;
            let key = schema::session_key(id);
            let found = match sessions.get(key.as_slice())? {
                Some(raw) => Some(postcard::from_bytes::<SessionMeta>(raw.value())?),
                None => None,
            };
            match found {
                Some(mut meta) => {
                    change(&mut meta);
                    let encoded = postcard::to_stdvec(&meta)?;
                    sessions.insert(key.as_slice(), encoded.as_slice())?;
                    true
                }
                None => false,
            }
        };
        txn.commit()?;
        Ok(found)
    }

    pub fn get_session(&self, id: u128) -> Result<Option<SessionMeta>> {
        let txn = self.db.begin_read()?;
        let sessions = txn.open_table(schema::SESSIONS)?;
        match sessions.get(schema::session_key(id).as_slice())? {
            Some(v) => Ok(Some(postcard::from_bytes(v.value())?)),
            None => Ok(None),
        }
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionMeta>> {
        let txn = self.db.begin_read()?;
        let sessions = txn.open_table(schema::SESSIONS)?;
        let mut out = Vec::new();
        for entry in sessions.iter()? {
            let (_, v) = entry?;
            out.push(postcard::from_bytes::<SessionMeta>(v.value())?);
        }
        Ok(out)
    }

    /// Append one event. The body is stored as an object, so a repeated payload
    /// costs only the ~50-byte log record.
    pub fn append_event(&self, session: u128, event: NewEvent<'_>) -> Result<u64> {
        // A checkpoint and a compaction are the two events a session can be
        // returned to, so they are the two that have to be on the disk rather
        // than in the page cache — and because an `Immediate` commit carries
        // everything before it, making these durable makes the turn that led up
        // to them durable too. Everything else rides along.
        let ordinary = !matches!(event.kind, EventKind::Checkpoint | EventKind::Compaction)
            && self.unflushed.load(Ordering::Relaxed) < EVENTS_PER_FLUSH;

        let mut txn = self.db.begin_write()?;
        if ordinary {
            // The error is "a write is already in flight on this transaction",
            // which cannot be: nothing has been written to it yet. Nothing to
            // handle, and the cost of being wrong is a flush we did not need.
            let _ = txn.set_durability(redb::Durability::None);
        }
        let body_id = self.put_tx(&txn, event.body_kind, event.body)?;

        let seq;
        {
            let mut sessions = txn.open_table(schema::SESSIONS)?;
            let key = schema::session_key(session);
            let mut meta: SessionMeta = match sessions.get(key.as_slice())? {
                Some(v) => postcard::from_bytes(v.value())?,
                None => return Err(StoreError::MissingSession(format_session_id(session))),
            };
            seq = meta.next_seq;
            meta.next_seq += 1;
            meta.event_count += 1;
            meta.tokens_in += event.tokens_in as u64;
            meta.tokens_out += event.tokens_out as u64;
            meta.updated_at = now_unix();
            let encoded = postcard::to_stdvec(&meta)?;
            sessions.insert(key.as_slice(), encoded.as_slice())?;
        }

        {
            let record = EventRecord {
                ts: event.ts.unwrap_or_else(now_unix),
                kind: event.kind,
                body: body_id,
                label: event.label.to_string(),
                tokens_in: event.tokens_in,
                tokens_out: event.tokens_out,
            };
            let encoded = postcard::to_stdvec(&record)?;
            let mut events = txn.open_table(schema::EVENTS)?;
            events.insert(schema::event_key(session, seq).as_slice(), encoded.as_slice())?;
        }

        if event.kind == EventKind::Checkpoint {
            let mut kv = txn.open_table(schema::KV)?;
            let previous = kv.get("checkpoint-clock")?.map(|v| v.value().to_vec());
            let order = previous
                .as_deref()
                .and_then(|b| b.try_into().ok())
                .map(u64::from_le_bytes)
                .unwrap_or(0)
                .checked_add(1)
                .ok_or_else(|| StoreError::Encoding("checkpoint order exhausted".into()))?;
            kv.insert("checkpoint-clock", order.to_le_bytes().as_slice())?;
            kv.insert(format!("checkpoint-order/{session}/{seq}").as_str(), order.to_le_bytes().as_slice())?;
        }
        txn.commit()?;
        match ordinary {
            true => self.unflushed.fetch_add(1, Ordering::Relaxed),
            false => self.unflushed.swap(0, Ordering::Relaxed),
        };
        Ok(seq)
    }

    /// How many events are not on the disk yet.
    ///
    /// A seam, because the alternative is cutting the power: which events are
    /// durable is not observable from inside the process that wrote them, and
    /// the rule — a checkpoint and a compaction are, the steps between them
    /// ride along — is exactly the sort that is quietly lost in a later edit.
    #[doc(hidden)]
    pub fn not_on_disk_yet(&self) -> u64 {
        self.unflushed.load(Ordering::Relaxed)
    }

    /// Put everything written so far beyond the reach of a power cut.
    ///
    /// Called at the end of a turn, which is the unit a person would miss: a
    /// turn interrupted by the machine losing power is not one that resumes,
    /// and what has to survive is the record of the turns that finished.
    ///
    /// Cheap when there is nothing to do, and about eight milliseconds when
    /// there is, which is why it is a turn's cost rather than an event's.
    pub fn flush(&self) -> Result<()> {
        if self.unflushed.load(Ordering::Relaxed) == 0 {
            return Ok(());
        }
        // An empty transaction at the default durability, which persists itself
        // and every commit behind it. There is nothing to write into it.
        self.db.begin_write()?.commit()?;
        self.unflushed.store(0, Ordering::Relaxed);
        Ok(())
    }

    pub fn events(&self, session: u128, from_seq: u64, limit: usize) -> Result<Vec<Event>> {
        let txn = self.db.begin_read()?;
        let events = txn.open_table(schema::EVENTS)?;
        let start = schema::event_key(session, from_seq);
        let end = schema::event_key(session, u64::MAX);
        let mut out = Vec::new();
        for entry in events.range(start.as_slice()..=end.as_slice())? {
            let (k, v) = entry?;
            let Some((sid, seq)) = schema::parse_event_key(k.value()) else { continue };
            if sid != session {
                break;
            }
            out.push(Event { session: sid, seq, record: postcard::from_bytes(v.value())? });
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    /// Copy the first `upto_seq` events of `source` into a new session.
    ///
    /// Forking rather than truncating: a rewind that destroys the history it
    /// rewound past leaves no way back, which is the complaint against every
    /// undo that does it. Objects are shared, so a fork costs only its records.
    pub fn fork_session(
        &self,
        source: u128,
        new_id: u128,
        upto_seq: u64,
        title: &str,
    ) -> Result<SessionMeta> {
        let txn = self.db.begin_write()?;
        let mut meta = {
            let sessions = txn.open_table(schema::SESSIONS)?;
            let raw = sessions
                .get(schema::session_key(source).as_slice())?
                .ok_or_else(|| StoreError::MissingSession(format_session_id(source)))?;
            postcard::from_bytes::<SessionMeta>(raw.value())?
        };

        meta.id = new_id;
        meta.parent = Some(source);
        meta.title = title.to_string();
        meta.created_at = now_unix();
        meta.updated_at = now_unix();
        meta.next_seq = 0;
        meta.event_count = 0;
        meta.tokens_in = 0;
        meta.tokens_out = 0;

        {
            let mut events = txn.open_table(schema::EVENTS)?;
            // Half-open: the fork keeps seqs [0, upto_seq), so rewinding to 0
            // keeps nothing rather than keeping the event being rewound past.
            let start = schema::event_key(source, 0);
            let end = schema::event_key(source, upto_seq);
            let copied: Vec<(u64, Vec<u8>)> = events
                .range(start.as_slice()..end.as_slice())?
                .filter_map(|e| e.ok())
                .filter_map(|(k, v)| {
                    schema::parse_event_key(k.value()).map(|(_, seq)| (seq, v.value().to_vec()))
                })
                .collect();
            for (seq, raw) in copied {
                let record: EventRecord = postcard::from_bytes(&raw)?;
                meta.tokens_in += record.tokens_in as u64;
                meta.tokens_out += record.tokens_out as u64;
                meta.event_count += 1;
                meta.next_seq = seq + 1;
                events.insert(schema::event_key(new_id, seq).as_slice(), raw.as_slice())?;
                if record.kind == EventKind::Checkpoint {
                    let mut kv = txn.open_table(schema::KV)?;
                    let order = kv
                        .get(format!("checkpoint-order/{source}/{seq}").as_str())?
                        .map(|value| value.value().to_vec());
                    if let Some(order) = order {
                        kv.insert(format!("checkpoint-order/{new_id}/{seq}").as_str(), order.as_slice())?;
                    }
                }
            }
        }
        {
            let mut sessions = txn.open_table(schema::SESSIONS)?;
            let encoded = postcard::to_stdvec(&meta)?;
            sessions.insert(schema::session_key(new_id).as_slice(), encoded.as_slice())?;
        }
        txn.commit()?;
        Ok(meta)
    }

    /// Drop a session and its log. Objects it referenced survive until `gc`,
    /// because other sessions may share them.
    pub fn delete_session(&self, session: u128) -> Result<u64> {
        let txn = self.db.begin_write()?;
        let mut removed = 0u64;
        {
            let mut events = txn.open_table(schema::EVENTS)?;
            let start = schema::event_key(session, 0);
            let end = schema::event_key(session, u64::MAX);
            let keys: Vec<Vec<u8>> = events
                .range(start.as_slice()..=end.as_slice())?
                .filter_map(|e| e.ok().map(|(k, _)| k.value().to_vec()))
                .collect();
            for k in keys {
                events.remove(k.as_slice())?;
                removed += 1;
            }
            let mut sessions = txn.open_table(schema::SESSIONS)?;
            sessions.remove(schema::session_key(session).as_slice())?;

            // Whatever is kept beside a session rather than in it is keyed by
            // the session id, and would otherwise outlive the session forever —
            // an accumulator with no bound, since retention deletes on a timer.
            let mut kv = txn.open_table(schema::KV)?;
            let suffix = format!("/{session:032x}");
            let checkpoints = format!("checkpoint-order/{session}/");
            let orphaned: Vec<String> = kv
                .iter()?
                .filter_map(|entry| entry.ok().map(|(key, _)| key.value().to_string()))
                .filter(|key| key.ends_with(&suffix) || key.starts_with(&checkpoints))
                .collect();
            for key in orphaned {
                kv.remove(key.as_str())?;
            }
        }
        txn.commit()?;
        Ok(removed)
    }

    // ---------------------------------------------------------------- kv pairs

    pub fn kv_set(&self, key: &str, value: &[u8]) -> Result<()> {
        let txn = self.db.begin_write()?;
        {
            let mut kv = txn.open_table(schema::KV)?;
            kv.insert(key, value)?;
        }
        txn.commit()?;
        Ok(())
    }

    pub fn kv_get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let txn = self.db.begin_read()?;
        let kv = txn.open_table(schema::KV)?;
        Ok(kv.get(key)?.map(|v| v.value().to_vec()))
    }
}
