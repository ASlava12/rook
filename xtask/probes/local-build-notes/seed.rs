use rook_store::{Kind, ObjectId, Store};
use std::path::PathBuf;

fn records(url: &str) -> Vec<(&'static str, String)> {
    let doc = |topic: &str, version: &str, text: &str| {
        format!(
            r#"{{"topic":"{topic}","version":"{version}","fetched_at":1,"pages":[{{"url":"{url}","title":"Owned documentation","text":"{text}"}}]}}"#
        )
    };
    vec![
        (
            "docs/rust-errors/latest",
            doc(
                "rust errors",
                "latest",
                "E0502: OWNED_LOCAL_BUILD_HINT. The same value cannot be borrowed mutably while an immutable borrow remains live; inspect the borrow's scope before selecting a change.",
            ),
        ),
        (
            "docs/redis-persistence/6.2",
            doc(
                "redis persistence",
                "6.2",
                "OWNED_OLDER_DOC_SET. Persistence writes an append-only log and replays that log at startup to restore state after restart.",
            ),
        ),
        ("docs/oversized/latest", doc("oversized", "latest", &"x".repeat(600_000))),
    ]
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let mode = args.get(1).expect("seed or verify");
    let home = PathBuf::from(args.get(2).expect("owned fixture home"));
    let url = args.get(3).expect("owned loopback source URL");
    let port = url
        .strip_prefix("http://127.0.0.1:")
        .unwrap()
        .strip_suffix("/owned-docs")
        .unwrap()
        .parse::<u16>()
        .unwrap();
    assert!(port > 0);
    assert!(home.is_absolute());
    assert!(
        home.parent()
            .unwrap()
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("rook-local-build-notes-")
    );
    let path = home.join("store");
    if mode == "seed" {
        assert!(!path.exists());
    }
    let store = Store::open(path).unwrap();
    for (reference, bytes) in records(url) {
        if mode == "seed" {
            let id = store.put(Kind::Docs, bytes.as_bytes()).unwrap();
            store.set_ref(reference, &id).unwrap();
        } else {
            assert_eq!(mode, "verify");
            let id = store.get_ref(reference).unwrap().unwrap();
            assert_eq!(id, ObjectId::of(bytes.as_bytes()));
            assert_eq!(store.get(&id).unwrap(), bytes.as_bytes());
        }
    }
    assert_eq!(store.list_refs("docs/").unwrap().len(), 3);
    println!("OWNED_DOCS_{}", mode.to_uppercase());
}
