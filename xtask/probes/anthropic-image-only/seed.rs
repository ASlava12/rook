use rook_store::{EventKind, Kind, NewEvent, SessionMeta, Store};
use std::path::PathBuf;
const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let mode = args.get(1).expect("seed or verify");
    let root = PathBuf::from(args.get(2).expect("owned case root"));
    assert!(root.is_absolute());
    assert!(
        root.parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("rook-anthropic-image-only-")
    );
    assert_eq!(PathBuf::from(std::env::var_os("ROOK_HOME").unwrap()), root.join("home"));
    let snapshot = root.join("legacy-input.json");
    if mode == "seed" {
        assert!(!root.join("home/store").exists() && !snapshot.exists());
        let store = Store::open(root.join("home/store")).unwrap();
        let id = rook_store::new_session_id();
        let mut meta = SessionMeta::new(
            id,
            "owned legacy image",
            root.join("workspace").display().to_string(),
            rook_store::now_unix(),
        );
        meta.model = "fixture".into();
        meta.agent = "owned-seed".into();
        store.create_session(&meta).unwrap();
        let text = args.get(3).expect("explicit blank caption");
        let image = serde_json::json!({"mime_type":"image/png","data":PNG,"width":1,"height":1});
        let body = serde_json::to_vec(
            &serde_json::json!({"role":"user","content":text,"images":[image.clone(),image],"cache":true}),
        )
        .unwrap();
        let seq = store
            .append_event(
                id,
                NewEvent::new(EventKind::UserMessage, Kind::Message, &body).label("rook:attachments:v1"),
            )
            .unwrap();
        let event = store.events(id, seq, 1).unwrap().remove(0);
        // Separate the historical image turn from the new frontend prompt.
        // Adjacent user messages are intentionally merged by normal transport preparation.
        store
            .append_event(
                id,
                NewEvent::new(
                    EventKind::AssistantMessage,
                    Kind::Message,
                    b"Owned historical image acknowledged.",
                ),
            )
            .unwrap();
        let saved = serde_json::json!({"session":rook_store::format_session_id(id),"seq":seq,"body_id":event.record.body.to_hex(),"record":format!("{:?}",event.record),"body":String::from_utf8(body).unwrap()});
        store.flush().unwrap();
        std::fs::write(snapshot, serde_json::to_vec(&saved).unwrap()).unwrap();
        println!("{saved}");
    } else {
        assert_eq!(mode, "verify");
        let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(snapshot).unwrap()).unwrap();
        let id = rook_store::parse_session_id(saved["session"].as_str().unwrap()).unwrap();
        let store = Store::open(root.join("home/store")).unwrap();
        let event = store.events(id, saved["seq"].as_u64().unwrap(), 1).unwrap().remove(0);
        assert_eq!(event.seq, saved["seq"].as_u64().unwrap());
        assert_eq!(event.record.body.to_hex(), saved["body_id"].as_str().unwrap());
        assert_eq!(format!("{:?}", event.record), saved["record"].as_str().unwrap());
        assert_eq!(store.get(&event.record.body).unwrap(), saved["body"].as_str().unwrap().as_bytes());
        println!("{{\"original_saved_input_unchanged\":true}}");
    }
}
