use rook_core::{
    Config, Rook,
    session_recall::{Request, Response},
};
use rook_store::{EventKind, Store};

fn open(root: &std::path::Path) -> Rook {
    Rook::from_parts(
        Store::open(root.join("store")).unwrap(),
        Config::default(),
        rook_skills::Environment::bare("linux", "x86_64", "0.1.0"),
        rook_skills::SkillIndex::default(),
        root.into(),
    )
}
fn query(text: &str) -> Request {
    Request { query: Some(text.into()), ..Default::default() }
}
fn collect(rook: &Rook, mut request: Request) -> Vec<rook_core::session_recall::Hit> {
    let mut hits = Vec::new();
    for _ in 0..100 {
        let response = rook.session_recall(&request).unwrap();
        assert!(response.hits.len() <= 8);
        assert!(response.scanned_sessions <= 128);
        assert!(response.scanned_events <= 128);
        assert!(response.scanned_bytes <= 256 * 1024);
        let done = response.next.is_none();
        request.cursor = response.next;
        hits.extend(response.hits);
        if done {
            return hits;
        }
    }
    panic!("the bounded scan did not reach its end");
}

#[test]
fn a_lost_conversation_is_discoverable_and_readable_with_exact_event_provenance() {
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path());
    let session = rook.start_session("Linux translation").unwrap();
    let seq =
        rook.log(session, EventKind::UserMessage, "", "Переведи главу LPIC; не перезапускай демон").unwrap();
    rook.log(session, EventKind::AssistantMessage, "", "Сохранена первая глава").unwrap();
    let hit = collect(&rook, query("lpic")).remove(0);
    assert_eq!(hit.session, rook_store::format_session_id(session));
    assert_eq!(hit.event.seq, seq);
    assert!(hit.event.snippet.contains("не перезапускай"));
    let response = rook
        .session_recall(&Request { session: Some(hit.session), around: Some(seq), ..Default::default() })
        .unwrap();
    let page = response.page.unwrap();
    assert_eq!(page.items[0].seq, seq);
    assert!(page.items.iter().any(|entry| entry.body.contains("Сохранена первая глава")));
}

#[test]
fn recall_does_not_read_other_workspaces_or_provider_and_checkpoint_companions() {
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path());
    let local = rook.start_session("local").unwrap();
    rook.log(local, EventKind::Note, "opaque provider data", "LPIC private companion").unwrap();
    rook.log(local, EventKind::Checkpoint, "files", "LPIC captured file").unwrap();
    let foreign = rook.for_workspace(root.path().join("elsewhere"));
    let session = foreign.start_session("other project").unwrap();
    foreign.log(session, EventKind::UserMessage, "", "LPIC another project").unwrap();
    assert!(collect(&rook, query("LPIC")).is_empty());
    let error = rook
        .session_recall(&Request {
            session: Some(rook_store::format_session_id(session)),
            ..Default::default()
        })
        .unwrap_err();
    assert!(error.to_string().contains("restricted to the current workspace"));
    assert_eq!(collect(&foreign, query("LPIC")).len(), 1);
}

#[test]
fn inherited_messages_are_found_once_and_survive_parent_deletion() {
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path());
    let parent = rook.start_session("original").unwrap();
    rook.log(parent, EventKind::UserMessage, "", "LPIC inherited").unwrap();
    let child = rook.fork_session(parent, 1).unwrap().id;
    rook.log(child, EventKind::AssistantMessage, "", "LPIC child-only result").unwrap();
    let response = rook.session_recall(&query("LPIC")).unwrap();
    assert_eq!(response.inherited_prefixes_skipped, 1);
    assert_eq!(response.hits.len(), 2);
    rook.delete_session(parent).unwrap();
    let hits = collect(&rook, query("LPIC"));
    assert_eq!(hits.len(), 2);
    assert!(hits.iter().all(|hit| hit.session == rook_store::format_session_id(child)));
}

#[test]
fn event_and_hit_budgets_continue_without_losing_or_repeating_matches() {
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path());
    let session = rook.start_session("long history").unwrap();
    for _ in 0..129 {
        rook.log(session, EventKind::UserMessage, "", "unrelated text").unwrap();
    }
    for i in 0..20 {
        rook.log(session, EventKind::AssistantMessage, "", &format!("LPIC chapter {i}")).unwrap();
    }
    let first = rook.session_recall(&query("LPIC")).unwrap();
    assert!(first.hits.is_empty());
    assert_eq!(first.scanned_events, 128, "the event bound must actually be reached");
    assert!(first.next.is_some());
    let hits = collect(&rook, query("LPIC"));
    assert_eq!(hits.len(), 20);
    let unique: std::collections::HashSet<_> = hits.iter().map(|h| (&h.session, h.event.seq)).collect();
    assert_eq!(unique.len(), 20);
    let mut changed = query("different");
    changed.cursor = first.next;
    assert!(
        rook.session_recall(&changed).unwrap_err().to_string().contains("another workspace, query or mode")
    );
}

#[test]
fn a_large_event_is_searched_in_byte_pages_and_read_with_visible_elision() {
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path());
    let session = rook.start_session("large result").unwrap();
    let seq = rook
        .log(session, EventKind::ToolResult, "command", &format!("{}LPIC tail", "x".repeat(300_000)))
        .unwrap();
    let first = rook.session_recall(&query("LPIC")).unwrap();
    assert_eq!(first.scanned_bytes, 256 * 1024, "the byte bound must actually be reached");
    assert!(first.hits.is_empty());
    assert!(first.next.is_some());
    let hits = collect(&rook, query("LPIC"));
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].event.seq, seq);
    let response = rook
        .session_recall(&Request {
            session: Some(hits[0].session.clone()),
            around: Some(seq),
            ..Default::default()
        })
        .unwrap();
    let page = response.page.as_ref().unwrap();
    assert!(page.items[0].truncated);
    assert!(page.items[0].body.contains("elided"));
    assert!(serde_json::to_vec(&response).unwrap().len() < 20_000);
    let _: Response = serde_json::from_slice(&serde_json::to_vec(&response).unwrap()).unwrap();
}

#[test]
fn discovery_pages_empty_workspaces_and_rejects_invalid_requests() {
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path());
    for _ in 0..130 {
        let id = rook_store::new_session_id();
        rook.store
            .create_session(&rook_store::SessionMeta::new(id, "other", "/another-workspace", 0))
            .unwrap();
    }
    let response = rook.session_recall(&query("LPIC")).unwrap();
    assert_eq!(response.scanned_sessions, 128);
    assert!(response.next.is_some(), "an empty filtered page must still advance");
    assert!(collect(&rook, query("LPIC")).is_empty());
    for request in [
        Request::default(),
        query(""),
        query(&"x".repeat(257)),
        Request { session: Some("not-an-id".into()), ..Default::default() },
        Request { cursor: Some("{}".into()), ..query("LPIC") },
    ] {
        assert!(rook.session_recall(&request).is_err(), "{request:?}");
    }
}
