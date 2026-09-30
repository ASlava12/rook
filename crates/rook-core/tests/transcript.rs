use rook_core::{
    Config, Rook,
    transcript::{Cursor, PageRequest},
};
use rook_store::{EventKind, Kind, NewEvent};

fn open(root: &std::path::Path, config: Config) -> Rook {
    Rook::from_parts(
        rook_store::Store::open(root.join("store")).unwrap(),
        config,
        rook_skills::Environment::bare("linux", "x86_64", "0.1.0"),
        rook_skills::SkillIndex::default(),
        root.into(),
    )
}
fn add(rook: &Rook, session: u128, text: &str) -> u64 {
    rook.store
        .append_event(session, NewEvent::new(EventKind::UserMessage, Kind::Message, text.as_bytes()))
        .unwrap()
}

#[test]
fn pages_reach_both_ends_of_a_long_session_without_growing_or_skipping_events() {
    let root = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.transcript.page_entries = 7;
    let rook = open(root.path(), config.clone());
    let session = rook.start_session("long").unwrap();
    for n in 0..2105 {
        add(&rook, session, &format!("event {n}"));
    }
    let mut page = rook.transcript_page(session, &PageRequest::default()).unwrap();
    assert_eq!(page.items.len(), 7);
    assert_eq!(page.items.last().unwrap().body, "event 2104");
    assert!(page.next.is_none());
    let mut descending = Vec::new();
    loop {
        assert!(page.items.len() <= 7);
        descending.extend(page.items.iter().rev().map(|e| e.seq));
        match page.previous {
            Some(before) => {
                page = rook
                    .transcript_page(session, &PageRequest { before: Some(before), ..Default::default() })
                    .unwrap()
            }
            None => break,
        }
    }
    assert_eq!(descending, (0..2105).rev().collect::<Vec<u64>>());
    let mut ascending = Vec::new();
    loop {
        ascending.extend(page.items.iter().map(|e| e.seq));
        match page.next {
            Some(from) => {
                page = rook
                    .transcript_page(
                        session,
                        &PageRequest { from: Some(from), limit: Some(99999), ..Default::default() },
                    )
                    .unwrap()
            }
            None => break,
        }
    }
    assert_eq!(ascending, (0..2105).collect::<Vec<u64>>());
    let fork = rook_store::new_session_id();
    rook.store.fork_session(session, fork, 2100, "fork").unwrap();
    drop(rook);
    let rook = open(root.path(), config);
    let page = rook.transcript_page(fork, &PageRequest::default()).unwrap();
    assert_eq!(page.items.last().unwrap().seq, 2099);
    assert_eq!(rook.transcript_entry(session, 2099, 0).unwrap().entry.body, "event 2099");
    assert!(rook.store.events(session, 0, 0).unwrap().is_empty());
    assert!(rook.store.events_before(session, 100, 0).unwrap().is_empty());
    assert!(rook.store.events_before(session, 0, 7).unwrap().is_empty());
}

#[test]
fn serialized_page_limits_count_escaping_and_still_make_progress_in_both_directions() {
    let root = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.transcript.page_bytes = 4096;
    config.transcript.body_bytes = 65536;
    let rook = open(root.path(), config);
    let session = rook.start_session("escaped").unwrap();
    let text = "\0\n\t\"\\".repeat(15000);
    assert!(serde_json::to_vec(&text).unwrap().len() > rook.config.transcript.page_bytes);
    for _ in 0..3 {
        add(&rook, session, &text);
    }
    for request in [PageRequest::default(), PageRequest { from: Some(0), ..Default::default() }] {
        let page = rook.transcript_page(session, &request).unwrap();
        assert!(!page.items.is_empty());
        assert!(page.items.iter().all(|e| e.truncated));
        assert!(serde_json::to_vec(&page).unwrap().len() <= 4096);
        assert_eq!(page.items.len(), 1, "the byte bound, not the item bound, was reached");
        assert!(page.previous.is_some() || page.next.is_some());
    }
}

#[test]
fn search_resumes_inside_large_bodies_and_finds_unicode_across_the_scan_boundary() {
    let root = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.transcript.search_bytes = 4096;
    config.transcript.body_bytes = 128;
    config.transcript.search_events = 2;
    let rook = open(root.path(), config);
    let session = rook.start_session("search").unwrap();
    let text = format!("{}КеЛьВиН{}", "x".repeat(8190), "z".repeat(9000));
    let seq = add(&rook, session, &text);
    add(&rook, session, "another matching кельвин");
    let preview = rook.transcript_page(session, &PageRequest::default()).unwrap();
    assert!(!preview.items[0].body.contains("КеЛьВиН"));
    let first = rook.transcript_search(session, "кельвин", Cursor::default()).unwrap();
    assert!(first.hits.is_empty());
    assert_eq!(first.scanned_bytes, 4096);
    let cursor = first.next.unwrap();
    assert_eq!((cursor.seq, cursor.offset), (seq, 4096));
    add(&rook, session, "кельвин appended after search started");
    let second = rook.transcript_search(session, "кельвин", cursor).unwrap();
    assert_eq!(second.hits.len(), 1);
    assert_eq!(second.hits[0].seq, seq);
    assert!(second.hits[0].snippet.contains("КеЛьВиН"));
    assert!(!second.hits[0].snippet.contains('\u{fffd}'));
    let third = rook.transcript_search(session, "кельвин", second.next.unwrap()).unwrap();
    assert_eq!(third.hits.len(), 1);
    assert_eq!(third.hits[0].seq, seq + 1);
    assert!(third.next.is_none(), "appended events cannot extend the search snapshot");
    // Unicode folding can match a code point wider than the query's bytes.
    let other = rook.start_session("wide case fold").unwrap();
    add(&rook, other, &format!("{}K!", "x".repeat(4095)));
    assert_eq!(rook.transcript_search(other, "k!", Cursor::default()).unwrap().hits.len(), 1);
    assert!(rook.transcript_search(session, "[literal]", Cursor::default()).unwrap().hits.is_empty());
}

#[test]
fn entry_parts_reconstruct_utf8_and_quotes_preserve_provenance_without_writing_history() {
    let root = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.transcript.body_bytes = 128;
    config.transcript.quote_bytes = 128;
    let rook = open(root.path(), config);
    let session = rook.start_session("utf8").unwrap();
    let text = "А🙂界\n".repeat(3000);
    let seq = add(&rook, session, &text);
    let mut offset = 0;
    let mut whole = String::new();
    loop {
        let part = rook.transcript_entry(session, seq, offset).unwrap();
        assert_eq!(part.offset, offset);
        assert!(!part.entry.body.contains('\u{fffd}'));
        assert!(part.entry.body.len() <= 128);
        whole.push_str(&part.entry.body);
        match part.next_offset {
            Some(next) => {
                assert!(next > offset);
                offset = next;
            }
            None => break,
        }
    }
    assert_eq!(whole, text);
    let part = rook.transcript_entry(session, seq, 3).unwrap();
    assert_eq!(part.offset, 6, "jumping into the emoji resumes at the next UTF-8 boundary");
    let before = rook.store.get_session(session).unwrap().unwrap().next_seq;
    let quote = rook.transcript_quote(session, seq, 0).unwrap();
    let data: serde_json::Value = serde_json::from_str(&quote.text).unwrap();
    assert_eq!(data["rook_source"]["authority"], "data");
    assert_eq!(data["rook_source"]["session"], rook_store::format_session_id(session));
    assert_eq!(data["rook_source"]["seq"], seq);
    assert_eq!(data["rook_source"]["complete"], false);
    assert_eq!(data["rook_source"]["content"], rook.transcript_entry(session, seq, 0).unwrap().entry.body);
    assert!(quote.next_offset.is_some());
    assert_eq!(rook.store.get_session(session).unwrap().unwrap().next_seq, before);
}

#[test]
fn hidden_provider_envelopes_are_neither_searchable_nor_quotable_as_raw_bytes() {
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), Config::default());
    let session = rook.start_session("opaque").unwrap();
    for label in ["rook:assistant-state:v1", "rook:call-state:v1"] {
        let seq = rook
            .store
            .append_event(
                session,
                NewEvent::new(EventKind::Note, Kind::Message, b"private-signed-payload").label(label),
            )
            .unwrap();
        assert!(!rook.transcript_quote(session, seq, 0).unwrap().text.contains("private-signed-payload"));
        assert!(
            !rook.transcript_entry(session, seq, 0).unwrap().entry.body.contains("private-signed-payload")
        );
    }
    assert!(
        rook.transcript_search(session, "private-signed-payload", Cursor::default()).unwrap().hits.is_empty()
    );
    let mut attached = rook_llm::Message::user("visible attached prompt");
    let png =
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";
    attached.images.push(rook_llm::Image::from_base64("image/png", png).unwrap());
    let json = serde_json::to_vec(&attached).unwrap();
    let seq = rook
        .store
        .append_event(
            session,
            NewEvent::new(EventKind::UserMessage, Kind::Message, &json).label("rook:attachments:v1"),
        )
        .unwrap();
    assert_eq!(rook.transcript_entry(session, seq, 0).unwrap().entry.body, attached.content);
    assert!(!rook.transcript_quote(session, seq, 0).unwrap().text.contains(png));
    assert!(rook.transcript_search(session, png, Cursor::default()).unwrap().hits.is_empty());
}

#[test]
fn invalid_queries_and_missing_history_are_distinct_from_an_empty_session() {
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), Config::default());
    let session = rook.start_session("empty").unwrap();
    assert!(rook.transcript_page(session, &PageRequest::default()).unwrap().items.is_empty());
    assert!(rook.transcript_search(session, "anything", Cursor::default()).unwrap().next.is_none());
    assert!(rook.transcript_entry(session, 0, 0).is_err());
    assert!(rook.transcript_quote(session, 0, 0).is_err());
    assert!(rook.transcript_page(rook_store::new_session_id(), &PageRequest::default()).is_err());
    assert!(
        rook.transcript_page(session, &PageRequest { from: Some(0), before: Some(1), limit: None }).is_err()
    );
    for query in [" ".to_owned(), "я".repeat(129)] {
        assert!(rook.transcript_search(session, &query, Cursor::default()).is_err());
    }
}
