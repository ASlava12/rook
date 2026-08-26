//! Introspection: what is in the store, and what is it costing.
//!
//! This backs `rook store stat`, the TUI's storage pane, and the web dashboard.
//! Being able to answer "why is this 4 GB" without third-party tooling is a
//! requirement, not a nicety — every reference agent that skipped it grew an
//! issue thread about runaway disk usage.

use serde::{Deserialize, Serialize};

use crate::Store;
use crate::error::Result;
use crate::object::{Kind, ObjectMeta};
use crate::schema;
use redb::{ReadableDatabase, ReadableTable, ReadableTableMetadata};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct KindStats {
    pub kind: String,
    pub objects: u64,
    pub bytes_raw: u64,
    pub bytes_stored: u64,
}

impl KindStats {
    pub fn ratio(&self) -> f64 {
        if self.bytes_stored == 0 {
            return 1.0;
        }
        self.bytes_raw as f64 / self.bytes_stored as f64
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StoreStats {
    pub objects: u64,
    pub inline_objects: u64,
    pub external_objects: u64,
    /// Total size of everything ever handed to `put`, counted once per object.
    pub bytes_raw: u64,
    /// What those objects actually occupy after compression.
    pub bytes_stored: u64,
    /// Size of `index.redb` on disk.
    pub index_bytes: u64,
    /// Size of the `objects/` tree on disk.
    pub external_bytes: u64,
    pub sessions: u64,
    pub events: u64,
    pub refs: u64,
    /// Bytes saved by content addressing across session logs: for every event
    /// whose body was already stored, the raw size that would otherwise have
    /// been written again. Counts event bodies only, so it understates the total
    /// — file captures and refs dedup too and are not included here.
    pub dedup_saved_hint: u64,
    pub per_kind: Vec<KindStats>,
    pub dictionaries: Vec<(String, u64)>,
}

impl StoreStats {
    /// Raw-to-stored ratio across the whole store.
    pub fn compression_ratio(&self) -> f64 {
        if self.bytes_stored == 0 {
            return 1.0;
        }
        self.bytes_raw as f64 / self.bytes_stored as f64
    }

    pub fn disk_bytes(&self) -> u64 {
        self.index_bytes + self.external_bytes
    }
}

impl Store {
    pub fn stats(&self) -> Result<StoreStats> {
        let mut s = StoreStats::default();
        let txn = self.db.begin_read()?;

        {
            let objects = txn.open_table(schema::OBJECTS)?;
            let mut per_kind: std::collections::BTreeMap<u8, KindStats> = Default::default();
            for entry in objects.iter()? {
                let (_, v) = entry?;
                let meta: ObjectMeta = postcard::from_bytes(v.value())?;
                s.objects += 1;
                s.bytes_raw += meta.size_raw;
                s.bytes_stored += meta.size_stored;
                if meta.external {
                    s.external_objects += 1;
                } else {
                    s.inline_objects += 1;
                }
                let k = per_kind.entry(meta.kind).or_insert_with(|| KindStats {
                    kind: Kind::from_u8(meta.kind).as_str().to_string(),
                    ..Default::default()
                });
                k.objects += 1;
                k.bytes_raw += meta.size_raw;
                k.bytes_stored += meta.size_stored;
            }
            s.per_kind = per_kind.into_values().collect();
        }

        {
            let sessions = txn.open_table(schema::SESSIONS)?;
            let mut referenced_raw = 0u64;
            for entry in sessions.iter()? {
                let (_, v) = entry?;
                let meta: schema::SessionMeta = postcard::from_bytes(v.value())?;
                s.sessions += 1;
                referenced_raw += meta.event_count;
            }
            let _ = referenced_raw;
        }
        {
            let events = txn.open_table(schema::EVENTS)?;
            s.events = events.len()?;
        }
        {
            let refs = txn.open_table(schema::REFS)?;
            s.refs = refs.len()?;
        }

        s.index_bytes = std::fs::metadata(self.root.join("index.redb")).map(|m| m.len()).unwrap_or(0);
        s.external_bytes = dir_size(&self.root.join("objects"));

        for kind in Kind::ALL {
            if let Some(d) = self.dicts.get(kind) {
                s.dictionaries.push((kind.as_str().to_string(), d.len() as u64));
            }
        }

        // Every event body is an object reference; the difference between event
        // count and distinct referenced objects is the dedup that happened.
        s.dedup_saved_hint = self.dedup_saved()?;
        Ok(s)
    }

    /// Bytes avoided by content addressing: for each event, the raw size of the
    /// body it points at, minus the one copy actually stored.
    fn dedup_saved(&self) -> Result<u64> {
        let txn = self.db.begin_read()?;
        let events = txn.open_table(schema::EVENTS)?;
        let objects = txn.open_table(schema::OBJECTS)?;
        let mut seen: std::collections::HashSet<[u8; 32]> = Default::default();
        let mut saved = 0u64;
        for entry in events.iter()? {
            let (_, v) = entry?;
            let rec: schema::EventRecord = postcard::from_bytes(v.value())?;
            let raw = match objects.get(rec.body.as_bytes())? {
                Some(m) => postcard::from_bytes::<ObjectMeta>(m.value())?.size_raw,
                None => continue,
            };
            if !seen.insert(rec.body.0) {
                saved += raw;
            }
        }
        Ok(saved)
    }
}

pub(crate) fn dir_size(path: &std::path::Path) -> u64 {
    let mut total = 0;
    let Ok(entries) = std::fs::read_dir(path) else { return 0 };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            total += dir_size(&entry.path());
        } else {
            total += meta.len();
        }
    }
    total
}
