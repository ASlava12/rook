//! What the agent's per-turn work costs as the things it accumulates grow.
//!
//! Run with `cargo xtask load`, which builds it in release and can put it under
//! a profiler. Each part is something a turn pays for again every step, and
//! each is something that has grown quietly before: a session's log, the
//! catalog of skills, the tool list, the store a search walks. The numbers are
//! printed rather than asserted — this is a measurement, not a gate — and a
//! part that is suddenly an order of magnitude worse is what it exists to make
//! visible.
//!
//! Wall time on one machine says nothing on its own. What it says is which part
//! dominates, and whether the cost per unit is flat: a per-unit figure that
//! climbs with the size is the shape of a quadratic, and that is the finding
//! worth having.

use std::sync::Arc;
use std::time::{Duration, Instant};

use rook_core::agent::AgentLoop;
use rook_core::{Config, Rook};
use rook_llm::{Provider, Request, Response};
use rook_skills::{Environment, SkillIndex, SkillSource};
use rook_store::{EventKind, Kind, NewEvent, SessionMeta, Store};

struct Silent;

#[async_trait::async_trait]
impl Provider for Silent {
    fn id(&self) -> &str {
        "none/none"
    }
    fn context_window(&self) -> usize {
        128_000
    }
    async fn complete(&self, _: Request) -> rook_llm::Result<Response> {
        Err(rook_llm::LlmError::Other("not called".into()))
    }
}

/// One measured part, in the shape the table prints.
struct Measured {
    part: &'static str,
    size: String,
    took: Duration,
    /// What one of whatever was counted cost, and what that unit is called.
    per: Option<(Duration, &'static str)>,
}

impl Measured {
    fn of(part: &'static str, size: String, took: Duration, units: usize, unit: &'static str) -> Self {
        let per = (units > 0).then(|| (took / units.max(1) as u32, unit));
        Self { part, size, took, per }
    }
}

fn took(d: Duration) -> String {
    match d.as_secs_f64() {
        s if s >= 1.0 => format!("{s:.2} s"),
        s if s >= 0.001 => format!("{:.0} ms", s * 1000.0),
        s => format!("{:.0} µs", s * 1_000_000.0),
    }
}

/// A step of a real turn: an assistant message carrying a tool call, and the
/// result that came back. Shaped like the traffic rather than sized like it,
/// because what is being measured is the per-event cost.
fn message(step: usize) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "role": "assistant",
        "content": [{
            "type": "tool_use",
            "id": format!("toolu_{step:08x}"),
            "name": "read_file",
            "input": { "path": format!("crates/rook-core/src/turn_{step}.rs") }
        }],
        "usage": { "input_tokens": 18_000 + step, "output_tokens": 120 }
    }))
    .unwrap()
}

fn result(step: usize) -> Vec<u8> {
    let mut s = String::with_capacity(2_048);
    for line in 0..40 {
        s.push_str(&format!("    pub fn handler_{step}_{line}(&self) -> Result<Response> {{\n"));
        s.push_str("        self.dispatch()\n    }\n");
    }
    s.into_bytes()
}

/// A skill library of `n`, written to disk the way a real one is.
fn skills_in(dir: &std::path::Path, n: usize) {
    for i in 0..n {
        let at = dir.join(format!("skill-{i:04}"));
        let _ = std::fs::create_dir_all(&at);
        let _ = std::fs::write(
            at.join("SKILL.md"),
            format!(
                "---\nname: skill-{i:04}\nversion: 1.0.0\n\
                 description: Use when the {i}th thing is what the workspace needs.\n---\n\
                 Do the {i}th thing, then check that it took.\n"
            ),
        );
    }
}

fn main() {
    let scale: usize =
        std::env::args().skip_while(|a| a != "--scale").nth(1).and_then(|n| n.parse().ok()).unwrap_or(1);
    let only: Option<String> = std::env::args().skip_while(|a| a != "--part").nth(1);
    let wanted = |part: &str| only.as_deref().is_none_or(|p| p == part);

    let steps = 2_000 * scale;
    let skills = 200 * scale;
    let mut out: Vec<Measured> = Vec::new();

    let store_dir = tempfile::tempdir().expect("a scratch store");
    let skill_dir = tempfile::tempdir().expect("a scratch skill library");
    let workspace = tempfile::tempdir().expect("a scratch workspace");

    // The store is filled whether or not `append` is the part asked for:
    // everything below reads it, and a part measured against an empty store
    // would be measuring nothing.
    let store = Store::open(store_dir.path()).expect("a store");
    let session = rook_store::new_session_id();
    store
        .create_session(&SessionMeta::new(session, "load", "/tmp/ws", rook_store::now_unix()))
        .expect("a session");

    let began = Instant::now();
    for step in 0..steps {
        store
            .append_event(session, NewEvent::new(EventKind::AssistantMessage, Kind::Message, &message(step)))
            .expect("a message");
        store
            .append_event(
                session,
                NewEvent::new(EventKind::ToolResult, Kind::ToolResult, &result(step)).label("read_file"),
            )
            .expect("a result");
    }
    let events = steps * 2;
    if wanted("append") {
        out.push(Measured::of("append", format!("{events} events"), began.elapsed(), events, "event"));
    }

    skills_in(skill_dir.path(), skills);
    let began = Instant::now();
    let (index, errors) = SkillIndex::discover(&[(skill_dir.path().to_path_buf(), SkillSource::User)]);
    let discovered = began.elapsed();
    assert!(errors.is_empty(), "the synthetic library must parse: {errors:?}");

    let env = Environment::bare("linux", "x86_64", "0.1.0").with_language("rust", "1.97.1");
    let rook = Rook::from_parts(store, Config::default(), env, index, workspace.path().to_path_buf());

    if wanted("discover") {
        out.push(Measured::of("discover", format!("{skills} skills"), discovered, skills, "skill"));
    }

    if wanted("catalog") {
        let began = Instant::now();
        let cards = rook.catalog();
        let took = began.elapsed();
        assert_eq!(cards.len(), skills, "every skill is a card");
        out.push(Measured::of("catalog", format!("{skills} skills"), took, skills, "skill"));
    }

    if wanted("prompt") {
        // Built once per turn and paid for on every request after that, so its
        // size matters more than the time it takes to assemble.
        let sid = rook.start_session("load").expect("a session");
        let began = Instant::now();
        let mut agent = AgentLoop::new(&rook, Arc::new(Silent), sid);
        agent.ask_via(Arc::new(rook_tools::ask::NoOne));
        let prompt = agent.system_prompt();
        let took = began.elapsed();
        out.push(Measured::of("prompt", format!("~{} tokens", prompt.len().div_ceil(4)), took, 0, ""));
    }

    if wanted("transcript") {
        // What every step of a long turn replays. `history` is not public, and
        // this is where its time goes: one object read per event.
        let began = Instant::now();
        let entries = rook.transcript(session, 0, usize::MAX, 2_000).expect("the log reads back");
        let took = began.elapsed();
        assert_eq!(entries.len(), events, "the whole log");
        out.push(Measured::of("transcript", format!("{events} events"), took, events, "event"));
    }

    if wanted("context") {
        let began = Instant::now();
        let usage = rook.context_usage(session, Some(128_000)).expect("a usage");
        let took = began.elapsed();
        assert!(usage.live_tokens > 0, "a session this long is not empty");
        out.push(Measured::of("context", format!("{events} events"), took, events, "event"));
    }

    if wanted("search") {
        let options = rook_core::search::Search { budget: usize::MAX, ..Default::default() };
        let began = Instant::now();
        let found = rook.search("dispatch handler", &options).expect("a search");
        let took = began.elapsed();
        out.push(Measured::of(
            "search",
            format!("{} objects", found.objects_scanned),
            took,
            found.objects_scanned as usize,
            "object",
        ));
    }

    if out.is_empty() {
        println!(
            "no part called {:?}. Known: append, discover, catalog, prompt, transcript, context, search",
            only.unwrap_or_default()
        );
        return;
    }

    println!("{:<12} {:<18} {:>10}   per unit", "part", "size", "took");
    println!("{}", "─".repeat(62));
    for m in &out {
        let per = match &m.per {
            Some((d, unit)) => format!("{}/{unit}", took(*d)),
            None => "—".into(),
        };
        println!("{:<12} {:<18} {:>10}   {per}", m.part, m.size, took(m.took));
    }

    // The point of the table. A part that dominates is where to look first, and
    // a per-unit figure that climbs with `--scale` is the shape of a quadratic.
    if let Some(worst) = out.iter().max_by_key(|m| m.took) {
        println!();
        println!("slowest: {} at {}", worst.part, took(worst.took));
        println!(
            "run again with --scale 2: a per-unit cost that rises is not linear, and that is the finding"
        );
    }
}
