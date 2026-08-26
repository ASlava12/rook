//! Garbage collection, retention and dictionary training.
//!
//! Unbounded growth is the single most common storage failure in this class of
//! tool: a trace log that writes terabytes a year, a backup that grows until the
//! request times out, a workspace snapshot that stages 45 GB of datasets. The
//! store therefore ships with the brakes attached rather than as a later fix.

use std::collections::HashSet;

use redb::{ReadableDatabase, ReadableTable};
use serde::{Deserialize, Serialize};

use crate::Store;
use crate::error::Result;
use crate::object::{Kind, ObjectId, ObjectMeta};
use crate::schema;

/// Extra reachability supplied by higher layers.
///
/// The store does not know that a snapshot manifest names file blobs, or that a
/// skill version points at its assets. Rather than teach it those formats, the
/// caller passes an expander: given a marked object, return the objects it
/// references.
pub type Expander<'a> = &'a dyn Fn(Kind, &[u8]) -> Vec<ObjectId>;

#[derive(Default)]
pub struct GcOptions<'a> {
    /// Objects to treat as reachable regardless of the index, e.g. the working
    /// set of a session currently in flight.
    pub extra_roots: Vec<ObjectId>,
    pub expand: Option<Expander<'a>>,
    /// Report what would be collected without deleting anything.
    pub dry_run: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GcReport {
    pub scanned: u64,
    pub reachable: u64,
    pub collected: u64,
    pub bytes_freed: u64,
    /// Files in `objects/` with no index entry — the residue of a crash between
    /// writing a payload and committing its metadata.
    pub orphan_files_removed: u64,
    pub dry_run: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RetentionPolicy {
    /// Delete sessions whose last activity is older than this.
    pub max_session_age_days: Option<u32>,
    /// Keep at most this many sessions, newest first.
    pub max_sessions: Option<usize>,
    /// Soft cap on total on-disk bytes; oldest sessions go first.
    pub max_total_bytes: Option<u64>,
    /// Sessions carrying any of these tags are never pruned automatically.
    pub protect_tags: Vec<String>,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        // Conservative but bounded. A store left alone for a year should not be
        // a surprise.
        Self {
            max_session_age_days: Some(180),
            max_sessions: Some(2000),
            max_total_bytes: Some(4 << 30), // 4 GiB
            protect_tags: vec!["keep".into(), "pinned".into()],
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PruneReport {
    pub sessions_deleted: u64,
    pub events_deleted: u64,
    pub protected: u64,
    pub dry_run: bool,
}

impl Store {
    /// Mark and sweep. Roots are every ref and every event body; everything else
    /// is fair game.
    ///
    /// Mark-and-sweep rather than refcounting, deliberately: refcounts drift
    /// after a crash or a manual edit, and a store that miscounts silently
    /// deletes live data. A full sweep is O(objects) and runs in well under a
    /// second on stores of realistic size.
    pub fn gc(&self, opts: &GcOptions<'_>) -> Result<GcReport> {
        let mut report = GcReport { dry_run: opts.dry_run, ..Default::default() };
        let mut reachable: HashSet<[u8; 32]> = HashSet::new();
        let mut worklist: Vec<ObjectId> = opts.extra_roots.clone();

        {
            let txn = self.db.begin_read()?;
            for entry in txn.open_table(schema::REFS)?.iter()? {
                let (_, v) = entry?;
                if let Some(id) = ObjectId::from_hex(&hex::encode(v.value())) {
                    worklist.push(id);
                }
            }
            for entry in txn.open_table(schema::EVENTS)?.iter()? {
                let (_, v) = entry?;
                let rec: schema::EventRecord = postcard::from_bytes(v.value())?;
                worklist.push(rec.body);
            }
        }

        while let Some(id) = worklist.pop() {
            if !reachable.insert(id.0) {
                continue;
            }
            let Some(expand) = opts.expand else { continue };
            let Some(meta) = self.stat_object(&id)? else { continue };
            // Only container kinds can name children; skip the read otherwise.
            let kind = Kind::from_u8(meta.kind);
            if !matches!(kind, Kind::Snapshot | Kind::Skill | Kind::Memory) {
                continue;
            }
            if let Ok(body) = self.get(&id) {
                worklist.extend(expand(kind, &body));
            }
        }
        report.reachable = reachable.len() as u64;

        let mut doomed: Vec<(ObjectId, ObjectMeta)> = Vec::new();
        {
            let txn = self.db.begin_read()?;
            let objects = txn.open_table(schema::OBJECTS)?;
            for entry in objects.iter()? {
                let (k, v) = entry?;
                report.scanned += 1;
                let raw: [u8; 32] = match k.value().try_into() {
                    Ok(a) => a,
                    Err(_) => continue,
                };
                if reachable.contains(&raw) {
                    continue;
                }
                doomed.push((ObjectId(raw), postcard::from_bytes(v.value())?));
            }
        }

        for (_, meta) in &doomed {
            report.collected += 1;
            report.bytes_freed += meta.size_stored;
        }

        if !opts.dry_run && !doomed.is_empty() {
            let txn = self.db.begin_write()?;
            {
                let mut objects = txn.open_table(schema::OBJECTS)?;
                let mut blobs = txn.open_table(schema::BLOBS)?;
                for (id, meta) in &doomed {
                    objects.remove(id.as_bytes())?;
                    if !meta.external {
                        blobs.remove(id.as_bytes())?;
                    }
                }
            }
            txn.commit()?;
            for (id, meta) in &doomed {
                if meta.external {
                    let _ = std::fs::remove_file(self.object_path(id));
                }
            }
        }

        report.orphan_files_removed = self.sweep_orphan_files(opts.dry_run)?;
        Ok(report)
    }

    /// Remove payload files that no index entry claims.
    fn sweep_orphan_files(&self, dry_run: bool) -> Result<u64> {
        let objects_dir = self.root.join("objects");
        let mut removed = 0;
        let mut stack = vec![objects_dir];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
                let Some(id) = ObjectId::from_hex(name) else { continue };
                if self.has(&id)? {
                    continue;
                }
                removed += 1;
                if !dry_run {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
        // Also clear the staging directory; anything left there is from a crash.
        if !dry_run && let Ok(entries) = std::fs::read_dir(self.root.join("tmp")) {
            for entry in entries.flatten() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        Ok(removed)
    }

    /// Enforce `policy`, deleting the oldest unprotected sessions first.
    /// Objects are not touched here — run [`Store::gc`] afterwards to reclaim.
    pub fn prune(&self, policy: &RetentionPolicy, dry_run: bool) -> Result<PruneReport> {
        let mut report = PruneReport { dry_run, ..Default::default() };
        let mut sessions = self.list_sessions()?;
        sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_at));

        let now = crate::now_unix();
        let disk = self.stats()?.disk_bytes();
        let mut budget_exceeded = policy.max_total_bytes.map(|cap| disk > cap).unwrap_or(false);

        let mut kept = 0usize;
        let mut doomed = Vec::new();
        for s in &sessions {
            let protected = s.tags.iter().any(|t| policy.protect_tags.contains(t));
            if protected {
                report.protected += 1;
                kept += 1;
                continue;
            }
            let too_old =
                policy.max_session_age_days.map(|d| now - s.updated_at > d as i64 * 86_400).unwrap_or(false);
            let too_many = policy.max_sessions.map(|m| kept >= m).unwrap_or(false);

            if too_old || too_many || budget_exceeded {
                doomed.push(s.id);
                // One pass of oldest-first deletion; re-measuring the directory
                // per session would make this quadratic for no real gain.
                if budget_exceeded && doomed.len() >= sessions.len() / 4 {
                    budget_exceeded = false;
                }
            } else {
                kept += 1;
            }
        }

        for id in doomed {
            report.sessions_deleted += 1;
            if !dry_run {
                report.events_deleted += self.delete_session(id)?;
            }
        }
        Ok(report)
    }

    /// Train a zstd dictionary per kind from objects already in the store.
    ///
    /// Worth running once a store has a few hundred objects, and again after the
    /// agent's usage shape changes. Objects written earlier keep working: each
    /// one records the codec it was written with.
    pub fn train_dictionaries(&self, sample_limit: usize, dict_size: usize) -> Result<Vec<(String, usize)>> {
        let mut out = Vec::new();
        for kind in Kind::ALL {
            let ids = self.list_objects(Some(kind), sample_limit)?;
            if ids.len() < crate::codec::MIN_SAMPLES {
                continue;
            }
            let mut samples = Vec::with_capacity(ids.len());
            for (id, _) in ids {
                if let Ok(data) = self.get(&id) {
                    samples.push(data);
                }
            }
            let size = self.dicts.train(kind, &samples, dict_size)?;
            if size > 0 {
                out.push((kind.as_str().to_string(), size));
            }
        }
        Ok(out)
    }

    /// Re-read and re-hash every object. Reports ids that failed.
    pub fn verify(&self) -> Result<Vec<(ObjectId, String)>> {
        let mut bad = Vec::new();
        for (id, _) in self.list_objects(None, usize::MAX)? {
            if let Err(e) = self.get(&id) {
                bad.push((id, e.to_string()));
            }
        }
        Ok(bad)
    }
}
