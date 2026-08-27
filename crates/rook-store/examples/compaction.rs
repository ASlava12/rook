//! Measures what the store's three compaction mechanisms actually buy, on a
//! synthetic transcript shaped like real agent traffic.
//!
//! Run with: `cargo run -p rook-store --example compaction --release`

use rook_store::{EventKind, Kind, NewEvent, SessionMeta, Store};

fn message(turn: usize) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "role": if turn.is_multiple_of(2) { "assistant" } else { "user" },
        "model": "local/qwen3-coder-30b",
        "stop_reason": "tool_use",
        "content": [{
            "type": "tool_use",
            "id": format!("toolu_{turn:08x}"),
            "name": "read_file",
            "input": { "path": format!("crates/rook-core/src/session/turn_{turn}.rs"), "offset": turn * 40 }
        }],
        "usage": { "input_tokens": 18_000 + turn, "output_tokens": 120 }
    }))
    .unwrap()
}

/// A file the agent reads repeatedly across a session — the single biggest
/// source of redundancy in a real transcript.
fn source_file(n: usize) -> Vec<u8> {
    let mut s = String::new();
    for i in 0..400 {
        s.push_str(&format!("    pub fn handler_{n}_{i}(&self, ctx: &Context) -> Result<Response> {{\n"));
        s.push_str("        let span = tracing::info_span!(\"handler\");\n        let _g = span.enter();\n        self.dispatch(ctx)\n    }\n");
    }
    s.into_bytes()
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn main() {
    let turns = 3_000;
    // Above the 32 samples a dictionary needs: at 25 the file blobs never got
    // one, so the headline ratio measured the message dictionary alone while
    // claiming a dictionary per kind.
    let files = 64;
    let rereads = 5;

    let dir = tempfile::tempdir().unwrap();

    // Pass 1: cold store, no dictionaries yet.
    let store = Store::open(dir.path()).unwrap();
    let sid = rook_store::new_session_id();
    store
        .create_session(&SessionMeta::new(sid, "compaction demo", "/tmp/ws", rook_store::now_unix()))
        .unwrap();

    let mut logical_bytes = 0u64;
    for turn in 0..turns {
        let body = message(turn);
        logical_bytes += body.len() as u64;
        store
            .append_event(
                sid,
                NewEvent::new(EventKind::AssistantMessage, Kind::Message, &body)
                    .label("model")
                    .usage(18_000, 120),
            )
            .unwrap();
    }
    for round in 0..rereads {
        for f in 0..files {
            let body = source_file(f);
            logical_bytes += body.len() as u64;
            let _ = round;
            store
                .append_event(
                    sid,
                    NewEvent::new(EventKind::ToolResult, Kind::FileBlob, &body).label("read_file"),
                )
                .unwrap();
        }
    }

    let cold = store.stats().unwrap();
    let trained = store.train_dictionaries(512, 16 * 1024).unwrap();

    // Pass 2: same traffic into a store that already has the dictionaries.
    let dir2 = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir2.path().join("dicts")).unwrap();
    for e in std::fs::read_dir(dir.path().join("dicts")).unwrap().flatten() {
        std::fs::copy(e.path(), dir2.path().join("dicts").join(e.file_name())).unwrap();
    }
    let warm_store = Store::open(dir2.path()).unwrap();
    let sid2 = rook_store::new_session_id();
    warm_store
        .create_session(&SessionMeta::new(sid2, "compaction demo", "/tmp/ws", rook_store::now_unix()))
        .unwrap();
    for turn in 0..turns {
        warm_store
            .append_event(
                sid2,
                NewEvent::new(EventKind::AssistantMessage, Kind::Message, &message(turn))
                    .label("model")
                    .usage(18_000, 120),
            )
            .unwrap();
    }
    for _ in 0..rereads {
        for f in 0..files {
            warm_store
                .append_event(
                    sid2,
                    NewEvent::new(EventKind::ToolResult, Kind::FileBlob, &source_file(f)).label("read_file"),
                )
                .unwrap();
        }
    }
    let warm = warm_store.stats().unwrap();

    println!(
        "transcript: {turns} turns + {} tool results ({} distinct files, {rereads} re-reads each)",
        files * rereads,
        files
    );
    println!("logical bytes written by the agent : {:>8.2} MiB", mib(logical_bytes));
    println!();
    println!(
        "  after dedup (distinct objects)   : {:>8.2} MiB   ({} objects)",
        mib(cold.bytes_raw),
        cold.objects
    );
    println!(
        "  cold store, standalone zstd      : {:>8.2} MiB   ratio {:.1}x",
        mib(cold.bytes_stored),
        cold.compression_ratio()
    );
    println!(
        "  warm store, trained dictionaries : {:>8.2} MiB   ratio {:.1}x",
        mib(warm.bytes_stored),
        warm.compression_ratio()
    );
    println!();
    println!(
        "  end-to-end (logical -> on disk)  : {:>8.1}x",
        logical_bytes as f64 / warm.disk_bytes().max(1) as f64
    );
    println!("  on-disk total (index + objects)  : {:>8.2} MiB", mib(warm.disk_bytes()));
    println!("  dictionaries trained             : {trained:?}");
}
