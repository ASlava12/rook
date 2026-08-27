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

use redb::{Database, ReadableDatabase, ReadableTable, WriteTransaction};

pub use error::{Result, StoreError};
pub use maintenance::{GcOptions, GcReport, PruneReport, RetentionPolicy};
pub use object::{Kind, ObjectId, ObjectMeta};
pub use schema::{Event, EventKind, EventRecord, FORMAT_VERSION, NewEvent, SessionMeta};
pub use stats::{KindStats, StoreStats};

/// Generate a fresh session id. ULIDs sort chronologically, which means session
/// listings come back newest-last for free.
pub fn new_session_id() -> u128 {
    ulid::Ulid::generate().0
}

pub fn format_session_id(id: u128) -> String {
    ulid::Ulid(id).to_string()
}

pub fn parse_session_id(s: &str) -> Option<u128> {
    ulid::Ulid::from_string(s).ok().map(|u| u.0)
}

/// Millisecond clock, for ordering things that can happen more than once a
/// second. Second granularity is not enough for a history key: two captures in
/// the same second are ordinary, and ordering them by a colliding timestamp
/// makes "the previous version" a coin flip.
pub fn now_unix_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// A handle to the on-disk store. Cheap to clone by `Arc`, safe to share across
/// threads; redb serializes writers internally.
pub struct Store {
    db: Database,
    root: PathBuf,
    dicts: codec::DictSet,
    level: i32,
}

impl Store {
    /// Open (creating if needed) a store rooted at `root`, e.g. `~/.rook`.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        for sub in ["objects", "dicts", "tmp"] {
            let p = root.join(sub);
            std::fs::create_dir_all(&p).map_err(|e| StoreError::io(&p, e))?;
        }
        Self::check_format(&root)?;

        let index = root.join("index.redb");
        let db = match Database::create(&index) {
            Ok(db) => db,
            // One writer at a time is redb's contract, and the daemon usually
            // holds it. Say so, instead of surfacing a lock error the user has
            // no way to act on.
            Err(redb::DatabaseError::DatabaseAlreadyOpen) => return Err(StoreError::Locked { path: index }),
            Err(e) => return Err(e.into()),
        };
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
        Ok(Self { db, root, dicts, level: codec::DEFAULT_LEVEL })
    }

    fn check_format(root: &Path) -> Result<()> {
        let path = root.join("format.json");
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let v: serde_json::Value =
                    serde_json::from_str(&text).map_err(|e| StoreError::Encoding(e.to_string()))?;
                let found = v.get("format").and_then(|f| f.as_u64()).unwrap_or(0) as u32;
                if found > FORMAT_VERSION {
                    return Err(StoreError::FormatTooNew { found, supported: FORMAT_VERSION });
                }
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let body = serde_json::json!({
                    "format": FORMAT_VERSION,
                    "created_at": now_unix(),
                });
                std::fs::write(&path, serde_json::to_vec_pretty(&body).unwrap())
                    .map_err(|e| StoreError::io(&path, e))
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

    fn object_path(&self, id: &ObjectId) -> PathBuf {
        let hex = id.to_hex();
        self.root.join("objects").join(&hex[0..2]).join(&hex[2..4]).join(&hex)
    }

    // ---------------------------------------------------------------- objects

    /// Store `data`, returning its content id. Storing the same bytes twice is a
    /// hash lookup and nothing else.
    pub fn put(&self, kind: Kind, data: &[u8]) -> Result<ObjectId> {
        let txn = self.db.begin_write()?;
        let id = self.put_tx(&txn, kind, data)?;
        txn.commit()?;
        Ok(id)
    }

    fn put_tx(&self, txn: &WriteTransaction, kind: Kind, data: &[u8]) -> Result<ObjectId> {
        let id = ObjectId::of(data);
        {
            let objects = txn.open_table(schema::OBJECTS)?;
            if objects.get(id.as_bytes())?.is_some() {
                return Ok(id);
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
            let tmp = self.root.join("tmp").join(id.to_hex());
            std::fs::write(&tmp, &encoded).map_err(|e| StoreError::io(&tmp, e))?;
            std::fs::rename(&tmp, &path).map_err(|e| StoreError::io(&path, e))?;
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

    /// Resolve a unique hash prefix, the way `git` resolves a short sha.
    pub fn resolve_prefix(&self, prefix: &str) -> Result<Option<ObjectId>> {
        if let Some(id) = ObjectId::from_hex(prefix) {
            return Ok(self.has(&id)?.then_some(id));
        }
        let Ok(raw) = hex::decode(if prefix.len() % 2 == 1 { &prefix[..prefix.len() - 1] } else { prefix })
        else {
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
        let txn = self.db.begin_write()?;
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
                ts: now_unix(),
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

        txn.commit()?;
        Ok(seq)
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
            let orphaned: Vec<String> = kv
                .iter()?
                .filter_map(|entry| entry.ok().map(|(key, _)| key.value().to_string()))
                .filter(|key| key.ends_with(&suffix))
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
