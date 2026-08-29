use rook_store::{EventKind, Kind, NewEvent, ObjectId, SessionMeta, Store};

/// xorshift64*, so "incompressible" test data really is incompressible.
fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            x.wrapping_mul(0x2545F4914F6CDD1D) as u8
        })
        .collect()
}

fn tmp_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    (dir, store)
}

/// A payload that looks like what an agent actually writes: small, structured,
/// and nearly identical to its neighbours.
fn message(i: usize) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "role": "assistant",
        "model": "local/qwen3-coder-30b",
        "stop_reason": "tool_use",
        "content": [{
            "type": "tool_use",
            "name": "read_file",
            "input": { "path": format!("crates/rook-core/src/agent_{i}.rs"), "offset": i }
        }],
        "usage": { "input_tokens": 12000 + i, "output_tokens": 64 }
    }))
    .unwrap()
}

#[test]
fn roundtrip_preserves_bytes() {
    let (_d, s) = tmp_store();
    for payload in [b"".as_slice(), b"short", &vec![0xABu8; 300_000]] {
        let id = s.put(Kind::Other, payload).unwrap();
        assert_eq!(s.get(&id).unwrap(), payload, "roundtrip failed for {} bytes", payload.len());
    }
}

#[test]
fn identical_content_is_stored_once() {
    let (_d, s) = tmp_store();
    let body = message(1);
    let a = s.put(Kind::Message, &body).unwrap();
    let b = s.put(Kind::Message, &body).unwrap();
    assert_eq!(a, b, "same bytes must yield the same object id");
    assert_eq!(s.stats().unwrap().objects, 1, "duplicate content must not add an object");
}

#[test]
fn large_objects_spill_to_files_small_ones_stay_inline() {
    let (_d, s) = tmp_store();
    // Incompressible, so it stays above the inline threshold after encoding.
    let big = noise(3_000_000, 0xC0FFEE);
    let big_id = s.put(Kind::FileBlob, &big).unwrap();
    let small_id = s.put(Kind::Message, &message(2)).unwrap();

    assert!(s.stat_object(&big_id).unwrap().unwrap().external, "3 MB object should be external");
    assert!(!s.stat_object(&small_id).unwrap().unwrap().external, "small object should be inline");
    assert_eq!(s.get(&big_id).unwrap(), big);
}

#[test]
fn incompressible_data_is_not_stored_larger_than_the_original() {
    let (_d, s) = tmp_store();
    let data = noise(100_000, 0xDEADBEEF);
    let id = s.put(Kind::FileBlob, &data).unwrap();
    let meta = s.stat_object(&id).unwrap().unwrap();
    assert!(
        meta.size_stored <= meta.size_raw,
        "stored {} > raw {}: compression must never inflate",
        meta.size_stored,
        meta.size_raw
    );
}

#[test]
fn session_log_is_append_only_and_ordered() {
    let (_d, s) = tmp_store();
    let id = rook_store::new_session_id();
    s.create_session(&SessionMeta::new(id, "test", "/tmp/ws", rook_store::now_unix())).unwrap();

    for i in 0..50 {
        let seq = s
            .append_event(
                id,
                NewEvent::new(EventKind::AssistantMessage, Kind::Message, &message(i))
                    .label("model")
                    .usage(10, 5),
            )
            .unwrap();
        assert_eq!(seq, i as u64, "sequence numbers must be dense and increasing");
    }

    let events = s.events(id, 0, usize::MAX).unwrap();
    assert_eq!(events.len(), 50);
    assert!(events.windows(2).all(|w| w[0].seq < w[1].seq), "events must come back in order");

    let meta = s.get_session(id).unwrap().unwrap();
    assert_eq!(meta.event_count, 50);
    assert_eq!(meta.tokens_in, 500);
    assert_eq!(meta.tokens_out, 250);

    // A replayed payload costs a log record, not another copy of the body.
    let before = s.stats().unwrap().objects;
    s.append_event(id, NewEvent::new(EventKind::AssistantMessage, Kind::Message, &message(0)).label("model"))
        .unwrap();
    assert_eq!(s.stats().unwrap().objects, before, "repeated body must not allocate a new object");
}

#[test]
fn refs_resolve_and_list_by_prefix() {
    let (_d, s) = tmp_store();
    let a = s.put(Kind::Skill, b"skill v1 body ...........................").unwrap();
    let b = s.put(Kind::Skill, b"skill v2 body ...........................").unwrap();
    s.set_ref("skills/pdf@1.0.0", &a).unwrap();
    s.set_ref("skills/pdf@2.0.0", &b).unwrap();
    s.set_ref("memory/head", &a).unwrap();

    assert_eq!(s.get_ref("skills/pdf@2.0.0").unwrap(), Some(b));
    assert_eq!(s.list_refs("skills/").unwrap().len(), 2);
    assert_eq!(s.list_refs("").unwrap().len(), 3);
    assert!(s.delete_ref("memory/head").unwrap());
    assert_eq!(s.get_ref("memory/head").unwrap(), None);
}

#[test]
fn short_hash_prefixes_resolve_like_git() {
    let (_d, s) = tmp_store();
    let id = s.put(Kind::Message, &message(7)).unwrap();
    assert_eq!(s.resolve_prefix(&id.short()).unwrap(), Some(id));
    assert_eq!(s.resolve_prefix(&id.to_hex()).unwrap(), Some(id));
    assert_eq!(s.resolve_prefix("ffffffffff").unwrap(), None);
}

#[test]
fn gc_collects_only_unreachable_objects() {
    let (_d, s) = tmp_store();
    let sid = rook_store::new_session_id();
    s.create_session(&SessionMeta::new(sid, "t", "/tmp", rook_store::now_unix())).unwrap();
    s.append_event(sid, NewEvent::new(EventKind::UserMessage, Kind::Message, &message(1))).unwrap();

    let pinned = s.put(Kind::Skill, b"a skill that a ref points at ...........").unwrap();
    s.set_ref("skills/keep@1.0.0", &pinned).unwrap();
    let loose = s.put(Kind::ToolResult, b"output nobody references ...............").unwrap();

    let dry = s.gc(&rook_store::GcOptions { dry_run: true, min_age_secs: 0, ..Default::default() }).unwrap();
    assert_eq!(dry.collected, 1, "exactly the unreferenced object should be doomed");
    assert!(s.has(&loose).unwrap(), "dry run must not delete");

    let report = s.gc(&rook_store::GcOptions { min_age_secs: 0, ..Default::default() }).unwrap();
    assert_eq!(report.collected, 1);
    assert!(!s.has(&loose).unwrap(), "unreachable object should be gone");
    assert!(s.has(&pinned).unwrap(), "ref-reachable object must survive");
    assert_eq!(s.events(sid, 0, 10).unwrap().len(), 1, "event bodies must survive gc");
}

#[test]
fn gc_honours_the_expander_for_container_objects() {
    let (_d, s) = tmp_store();
    let leaf = s.put(Kind::FileBlob, &vec![7u8; 2048]).unwrap();
    let manifest = s.put(Kind::Snapshot, leaf.to_hex().as_bytes()).unwrap();
    s.set_ref("snapshots/latest", &manifest).unwrap();

    // Without an expander the leaf looks unreachable.
    assert_eq!(
        s.gc(&rook_store::GcOptions { dry_run: true, min_age_secs: 0, ..Default::default() })
            .unwrap()
            .collected,
        1
    );

    let expand = |_kind: Kind, body: &[u8]| -> Vec<ObjectId> {
        std::str::from_utf8(body).ok().and_then(ObjectId::from_hex).into_iter().collect()
    };
    let report = s
        .gc(&rook_store::GcOptions { expand: Some(&expand), min_age_secs: 0, ..Default::default() })
        .unwrap();
    assert_eq!(report.collected, 0, "expander must keep the manifest's children alive");
    assert!(s.has(&leaf).unwrap());
}

#[test]
fn prune_drops_old_sessions_but_never_protected_ones() {
    let (_d, s) = tmp_store();
    let now = rook_store::now_unix();

    let mut old = SessionMeta::new(rook_store::new_session_id(), "old", "/tmp", now);
    old.updated_at = now - 400 * 86_400;
    s.create_session(&old).unwrap();

    let mut pinned = SessionMeta::new(rook_store::new_session_id(), "pinned", "/tmp", now);
    pinned.updated_at = now - 400 * 86_400;
    pinned.tags = vec!["keep".into()];
    s.create_session(&pinned).unwrap();

    let fresh = SessionMeta::new(rook_store::new_session_id(), "fresh", "/tmp", now);
    s.create_session(&fresh).unwrap();

    let policy = rook_store::RetentionPolicy {
        max_session_age_days: Some(90),
        max_sessions: None,
        max_total_bytes: None,
        max_history_entries: None,
        protect_tags: vec!["keep".into()],
    };
    let report = s.prune(&policy, false).unwrap();
    assert_eq!(report.sessions_deleted, 1);
    assert_eq!(report.protected, 1);
    assert!(s.get_session(old.id).unwrap().is_none());
    assert!(s.get_session(pinned.id).unwrap().is_some());
    assert!(s.get_session(fresh.id).unwrap().is_some());
}

/// The central compactness claim: for many small, same-shaped payloads a trained
/// dictionary beats per-object zstd by a wide margin.
#[test]
fn trained_dictionaries_beat_standalone_compression() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path()).unwrap();

    let corpus: Vec<Vec<u8>> = (0..400).map(message).collect();
    let raw_total: usize = corpus.iter().map(|c| c.len()).sum();

    for c in &corpus {
        s.put(Kind::Message, c).unwrap();
    }
    let before = s.stats().unwrap();
    let trained = s.train_dictionaries(400, 16 * 1024).unwrap();
    assert!(
        trained.iter().any(|(k, _)| k == "message"),
        "a dictionary should have been trained for the message kind"
    );

    // Re-store the same corpus into a fresh store that already has the dictionary.
    let dir2 = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir2.path().join("dicts")).unwrap();
    for entry in std::fs::read_dir(dir.path().join("dicts")).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), dir2.path().join("dicts").join(entry.file_name())).unwrap();
    }
    let s2 = Store::open(dir2.path()).unwrap();
    for c in &corpus {
        s2.put(Kind::Message, c).unwrap();
    }
    let after = s2.stats().unwrap();

    assert_eq!(after.objects, before.objects);
    assert!(
        after.bytes_stored < before.bytes_stored,
        "dictionary compression ({} B) should beat standalone zstd ({} B)",
        after.bytes_stored,
        before.bytes_stored
    );
    assert!(
        after.compression_ratio() > 8.0,
        "expected >8x on same-shaped messages, got {:.1}x ({} B raw -> {} B stored)",
        after.compression_ratio(),
        raw_total,
        after.bytes_stored
    );
    // And the data must still come back intact.
    for c in &corpus {
        let id = ObjectId::of(c);
        assert_eq!(&s2.get(&id).unwrap(), c);
    }
}

#[test]
fn verify_reports_a_clean_store() {
    let (_d, s) = tmp_store();
    for i in 0..20 {
        s.put(Kind::Message, &message(i)).unwrap();
    }
    assert!(s.verify().unwrap().is_empty());
}

#[test]
fn opening_a_newer_format_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    {
        let _ = Store::open(dir.path()).unwrap();
    }
    std::fs::write(dir.path().join("format.json"), br#"{"format": 99999, "created_at": 0}"#).unwrap();
    assert!(matches!(Store::open(dir.path()), Err(rook_store::StoreError::FormatTooNew { .. })));
}

#[test]
fn session_ids_round_trip_as_ulids_in_json_and_as_integers_on_disk() {
    let id = rook_store::new_session_id();
    let mut meta = SessionMeta::new(id, "t", "/tmp", 0);
    meta.parent = Some(rook_store::new_session_id());

    let json = serde_json::to_value(&meta).unwrap();
    let text = json["id"].as_str().expect("json must carry a ULID string, not a number");
    assert_eq!(rook_store::parse_session_id(text), Some(id), "the printed id must be usable as input");
    assert!(json["parent"].is_string());
    let back: SessionMeta = serde_json::from_value(json).unwrap();
    assert_eq!(back.id, id);
    assert_eq!(back.parent, meta.parent);

    // On disk it stays a compact integer.
    let packed = postcard::to_stdvec(&meta).unwrap();
    assert!(packed.len() < 120, "postcard encoding grew to {} bytes", packed.len());
    assert_eq!(postcard::from_bytes::<SessionMeta>(&packed).unwrap().id, id);
}

#[test]
fn a_fork_keeps_exactly_the_events_before_the_split() {
    let (_d, s) = tmp_store();
    let source = rook_store::new_session_id();
    s.create_session(&SessionMeta::new(source, "src", "/tmp", rook_store::now_unix())).unwrap();
    for i in 0..5 {
        s.append_event(source, NewEvent::new(EventKind::UserMessage, Kind::Message, &message(i))).unwrap();
    }

    for split in [0u64, 1, 3, 5] {
        let fork = rook_store::new_session_id();
        let meta = s.fork_session(source, fork, split, "fork").unwrap();
        assert_eq!(meta.event_count, split, "forking at {split} kept the wrong count");
        assert_eq!(meta.next_seq, split);
        assert_eq!(s.events(fork, 0, usize::MAX).unwrap().len() as u64, split);
        assert_eq!(meta.parent, Some(source));
    }

    // The original is untouched by any of that.
    assert_eq!(s.events(source, 0, usize::MAX).unwrap().len(), 5);
}

/// Sessions of a known age, newest last.
fn aged_sessions(store: &Store, count: usize) -> Vec<u128> {
    let now = rook_store::now_unix();
    (0..count)
        .map(|i| {
            let id = rook_store::new_session_id();
            let mut meta = SessionMeta::new(id, format!("session {i}"), "/tmp", now);
            store.create_session(&meta).unwrap();
            store
                .append_event(id, NewEvent::new(EventKind::UserMessage, Kind::Message, &message(i)))
                .unwrap();
            // After the event, which stamps `updated_at` with the current time.
            meta.updated_at = now - (count - i) as i64 * 3_600;
            store.create_session(&meta).unwrap();
            id
        })
        .collect()
}

fn budget(bytes: Option<u64>) -> rook_store::RetentionPolicy {
    rook_store::RetentionPolicy {
        max_session_age_days: None,
        max_sessions: None,
        max_total_bytes: bytes,
        max_history_entries: None,
        protect_tags: vec!["keep".into()],
    }
}

#[test]
fn the_oldest_unprotected_sessions_come_back_oldest_first() {
    let (_d, s) = tmp_store();
    let sessions = aged_sessions(&s, 8);

    let picked = s.oldest_unprotected(&budget(None), 3).unwrap();
    assert_eq!(picked, sessions[..3], "oldest three, in age order");
}

#[test]
fn a_protecting_tag_keeps_a_session_out_of_the_oldest_batch() {
    let (_d, s) = tmp_store();
    let sessions = aged_sessions(&s, 4);
    let mut oldest = s.get_session(sessions[0]).unwrap().unwrap();
    oldest.tags.push("keep".into());
    s.create_session(&oldest).unwrap();

    let picked = s.oldest_unprotected(&budget(None), 2).unwrap();
    assert_eq!(picked, sessions[1..3], "the protected one is skipped, not kept");
}

#[test]
fn count_and_age_limits_delete_the_oldest_not_the_newest() {
    let (_d, s) = tmp_store();
    let sessions = aged_sessions(&s, 8);
    let policy = rook_store::RetentionPolicy { max_sessions: Some(3), ..budget(None) };

    let report = s.prune(&policy, false).unwrap();

    assert_eq!(report.sessions_deleted, 5);
    for id in &sessions[..5] {
        assert!(s.get_session(*id).unwrap().is_none(), "the oldest five go");
    }
    for id in &sessions[5..] {
        assert!(s.get_session(*id).unwrap().is_some(), "the newest three stay");
    }
}

/// One dictionary per object kind, which is the claim — and a kind below the
/// sample floor gets none rather than a bad one trained on too little.
#[test]
fn each_kind_is_trained_on_its_own_and_only_with_enough_to_learn_from() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path()).unwrap();

    for i in 0..64 {
        s.put(Kind::Message, &message(i)).unwrap();
        s.put(Kind::FileBlob, format!("fn handler_{i}() {{ dispatch(ctx) }}\n").repeat(40).as_bytes())
            .unwrap();
    }
    for i in 0..8 {
        s.put(
            Kind::Snapshot,
            format!("{{\"root\":\"/ws\",\"files\":{{\"a{i}.rs\":\"deadbeef\"}}}}").as_bytes(),
        )
        .unwrap();
    }

    let trained: Vec<String> =
        s.train_dictionaries(400, 16 * 1024).unwrap().into_iter().map(|(k, _)| k).collect();

    assert!(trained.contains(&"message".to_string()), "{trained:?}");
    assert!(
        trained.contains(&"file".to_string()),
        "file blobs are the bulk of a real store and had no dictionary of their own: {trained:?}"
    );
    assert!(
        !trained.contains(&"snapshot".to_string()),
        "eight samples is not enough to learn a dictionary from: {trained:?}"
    );
}

/// An object is unreachable between being written and the event that names it
/// being appended, and a checkpoint writes every captured file before the
/// manifest holding them. The daemon runs maintenance on a timer while turns
/// are running, so a collection can land in that window — and what it would
/// take is live data whose only fault is being new.
#[test]
fn gc_leaves_alone_what_was_only_just_written() {
    let (_d, s) = tmp_store();
    let in_flight = s.put(Kind::FileBlob, b"captured, not yet named by any event .....").unwrap();

    let report = s.gc(&Default::default()).unwrap();

    assert_eq!(report.collected, 0, "nothing old enough to be certain about");
    assert_eq!(report.too_new, 1, "and it says what it held back");
    assert!(s.has(&in_flight).unwrap(), "the checkpoint can still be written");

    let swept = s.gc(&rook_store::GcOptions { min_age_secs: 0, ..Default::default() }).unwrap();
    assert_eq!(swept.collected, 1, "once the window has passed it is ordinary garbage");
}

/// A payload is written to disk *before* the transaction that records it
/// commits, and the staging file exists for the moment between the write and the
/// rename. To an index scan both look exactly like debris from a crash — so a
/// sweep that judges by the index alone deletes the payload of a `put` running
/// beside it and leaves its index entry pointing at nothing.
#[test]
fn the_orphan_sweep_spares_a_file_too_new_to_be_debris() {
    let (dir, s) = tmp_store();
    let name = "ab".repeat(32);
    let holding = dir.path().join("objects").join("ab").join("ab");
    std::fs::create_dir_all(&holding).unwrap();
    let stray = holding.join(&name);
    std::fs::write(&stray, b"a payload whose transaction has not committed yet").unwrap();

    let grace = 600;
    let age = std::fs::metadata(&stray).unwrap().modified().unwrap().elapsed().unwrap().as_secs();
    assert!(age < grace, "the file has to be inside the grace for this to test anything: {age}s");

    let spared = s.gc(&rook_store::GcOptions { min_age_secs: grace as i64, ..Default::default() }).unwrap();
    assert_eq!(spared.orphan_files_removed, 0, "a file this new is a concurrent write, not debris");
    assert!(stray.exists(), "and it is still there");

    let swept = s.gc(&rook_store::GcOptions { min_age_secs: 0, ..Default::default() }).unwrap();
    assert_eq!(swept.orphan_files_removed, 1, "one old enough is still collected");
    assert!(!stray.exists());
}

/// A model with a training cutoff guesses what "now" is, and guesses low. The
/// arithmetic is Howard Hinnant's and the interesting inputs are the boundaries
/// it was written to get right.
#[test]
fn the_date_is_the_one_the_calendar_says() {
    let on = |unix: i64| {
        // The private conversion is reached the way the caller does, through the
        // clock — so the day is pinned by choosing the second.
        rook_store::date_of_unix_for_test(unix)
    };

    assert_eq!(on(0), "1970-01-01", "the epoch");
    assert_eq!(on(86_399), "1970-01-01", "one second before it rolls over");
    assert_eq!(on(86_400), "1970-01-02");
    assert_eq!(on(951_782_400), "2000-02-29", "a leap day in a century that is a leap year");
    assert_eq!(on(1_709_164_800), "2024-02-29");
    assert_eq!(on(1_709_251_200), "2024-03-01", "the day after one");
    assert_eq!(on(4_107_542_400), "2100-03-01", "and a century that is not");
}

/// History refs are read back in the order their names sort. The name used to be
/// a millisecond stamp with the object's hash after it, so two entries written
/// inside one millisecond tied — and the tie resolved by the hash, which is to
/// say by nothing.
#[test]
fn history_keys_do_not_tie_when_the_clock_does() {
    let keys: Vec<String> = (0..500).map(|_| rook_store::history_key()).collect();

    // The precondition, stated exactly: a ULID's first ten characters are its
    // millisecond, so keys sharing them are keys the old scheme would have
    // collided on.
    let same_millisecond = keys.windows(2).filter(|pair| pair[0][..10] == pair[1][..10]).count();
    assert!(same_millisecond > 0, "the keys have to outrun the clock or this proves nothing");

    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(sorted, keys, "written in order, so they must sort in order");

    let distinct: std::collections::BTreeSet<&String> = keys.iter().collect();
    assert_eq!(distinct.len(), keys.len(), "and no two may be the same name");
}

/// Reading a record, changing a field and writing it back is two transactions,
/// and an event appended between them is an event whose counters the write puts
/// back. The turn that named a session was doing exactly that.
#[test]
fn changing_one_field_does_not_undo_what_landed_meanwhile() {
    let (_d, s) = tmp_store();
    let id = rook_store::new_session_id();
    s.create_session(&SessionMeta::new(id, "", "/tmp", rook_store::now_unix())).unwrap();

    // The stale read a caller would have taken before changing a field.
    let stale = s.get_session(id).unwrap().unwrap();
    for i in 0..4 {
        s.append_event(id, NewEvent::new(EventKind::UserMessage, Kind::Message, format!("{i}").as_bytes()))
            .unwrap();
    }
    let busy = s.get_session(id).unwrap().unwrap();
    assert_eq!(busy.event_count, 4, "the events have to have landed for this to test anything");
    assert!(busy.event_count > stale.event_count);

    s.update_session(id, |meta| meta.title = "named later".into()).unwrap();

    let after = s.get_session(id).unwrap().unwrap();
    assert_eq!(after.title, "named later", "the change is applied");
    assert_eq!(after.event_count, 4, "and nothing else is rolled back to what the reader saw");
    assert_eq!(after.next_seq, busy.next_seq);

    assert!(!s.update_session(rook_store::new_session_id(), |_| {}).unwrap(), "a session that is not there");
}
