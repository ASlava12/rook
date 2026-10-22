use rook_store::{EventKind, Store};
use std::fmt::Write as _;
use std::path::PathBuf;

fn snapshot(store: &Store, tail: u128) -> String {
    let mut sessions = store.list_sessions().unwrap();
    sessions.sort_by_key(|s| s.id);
    let mut text = String::new();
    for session in sessions {
        write!(&mut text, "{session:?}").unwrap();
        for event in store.events(session.id, 0, 16).unwrap() {
            write!(&mut text, "{event:?}").unwrap();
            let body = store.get(&event.record.body).unwrap();
            write!(&mut text, "{}", rook_store::ObjectId::of(&body).to_hex()).unwrap();
        }
    }
    write!(&mut text, "{:?}", store.kv_get(&format!("activity/{tail:032x}")).unwrap()).unwrap();
    rook_store::ObjectId::of(text.as_bytes()).to_hex()
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let mode = args.get(1).expect("seed or verify");
    let root = PathBuf::from(args.get(2).expect("fresh owned state directory"));
    assert!(root.is_absolute());
    assert!(
        root.parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("rook-diagnostic-metadata-")
    );
    assert_eq!(rook_core::paths::home(), root.join("home"));
    if mode == "verify" {
        let ids: Vec<_> = args.iter().skip(3).map(|id| rook_store::parse_session_id(id).unwrap()).collect();
        assert_eq!(ids.len(), 3);
        let store = Store::open(root.join("home/store")).unwrap();
        let events = store.events(ids[0], 0, 16).unwrap();
        assert!(events.iter().any(|e| e.record.label == "private-oversized-label".repeat(1024)));
        assert_eq!(store.get_session(ids[1]).unwrap().unwrap().title, "private-title".repeat(8192));
        assert_eq!(store.get_session(ids[2]).unwrap().unwrap().title, "private-ancestor".repeat(8192));
        let expected = std::fs::read_to_string(root.join("seed-snapshot.id")).unwrap();
        assert_eq!(
            snapshot(&store, ids[0]),
            expected,
            "all seeded session/event metadata, bodies and activity must survive report reads"
        );
        println!("{{\"saved_metadata_unchanged\":true}}");
        return;
    }
    assert_eq!(mode, "seed");
    assert!(!root.exists());
    std::fs::create_dir(&root).unwrap();
    let workspace = root.join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(workspace.join("readme.txt"), b"owned unchanged\n").unwrap();
    let rook = rook_core::Rook::open(Some(workspace)).unwrap();
    let tail = rook.start_session("private-tail").unwrap();
    let before = rook.log(tail, EventKind::UserMessage, "", "private before").unwrap();
    let oversized = rook
        .log(tail, EventKind::Note, &"private-oversized-label".repeat(1024), "private label body")
        .unwrap();
    let after = rook.log(tail, EventKind::AssistantMessage, "", "private after").unwrap();
    let activity = rook_core::activity::Tracker::new(tail, 30).snapshot(false);
    rook_core::activity::begin(&rook.store, tail, &activity).unwrap();
    let primary = rook.start_session("primary").unwrap();
    rook.store.update_session(primary, |meta| meta.title = "private-title".repeat(8192)).unwrap();
    let parent = rook.start_session("parent").unwrap();
    rook.store.update_session(parent, |meta| meta.title = "private-ancestor".repeat(8192)).unwrap();
    let child = rook.start_session("child").unwrap();
    rook.store.update_session(child, |meta| meta.parent = Some(parent)).unwrap();
    rook.log(child, EventKind::UserMessage, "", "private child").unwrap();
    rook.store.flush().unwrap();
    std::fs::write(root.join("seed-snapshot.id"), snapshot(&rook.store, tail)).unwrap();
    println!(
        "{{\"tail\":\"{}\",\"primary\":\"{}\",\"parent\":\"{}\",\"child\":\"{}\",\"missing\":\"{}\",\"before\":{},\"oversized\":{},\"after\":{},\"generation\":\"{}\",\"last_progress_at\":{}}}",
        rook_store::format_session_id(tail),
        rook_store::format_session_id(primary),
        rook_store::format_session_id(parent),
        rook_store::format_session_id(child),
        rook_store::format_session_id(rook_store::new_session_id()),
        before,
        oversized,
        after,
        activity.generation,
        activity.last_progress_at
    );
}
