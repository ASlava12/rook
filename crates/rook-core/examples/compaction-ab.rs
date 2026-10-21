//! Compare summary output budgets against explicitly exported real transcripts.
//! Run with an isolated ROOK_HOME; see docs/reference-r17-adoption.md.
use rook_core::{Config, Rook, Vault, agent::AgentLoop};
use serde::Deserialize;
use std::{path::PathBuf, sync::Arc};

#[derive(Deserialize)]
struct Fixture {
    name: String,
    entries: Vec<Entry>,
    checks: Vec<String>,
}
#[derive(Deserialize)]
struct Entry {
    kind: String,
    #[serde(default)]
    label: String,
    body: String,
}

#[derive(Deserialize)]
struct CompactionOutcome {
    summarized: bool,
}

#[derive(Deserialize)]
struct Auxiliary {
    purpose: String,
    receipt: rook_core::model_route::Receipt,
}

fn record_body(
    rook: &Rook,
    event: &rook_store::Event,
    maximum: usize,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let body = rook.store.get_range(&event.record.body, 0, maximum + 1)?;
    if body.len() > maximum {
        return Err("comparison metadata exceeds its admission limit".into());
    }
    Ok(body)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    rook_core::config::load_env_file();
    tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(run())
}
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let path =
        PathBuf::from(std::env::args().nth(1).ok_or("pass a JSON fixture array (name, entries, checks)")?);
    let file = std::fs::File::open(&path)?;
    if file.metadata()?.len() > 4 * 1024 * 1024 {
        return Err("fixture file exceeds 4 MiB".into());
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    file.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("fixture grew beyond 4 MiB".into());
    }
    let fixtures: Vec<Fixture> = serde_json::from_slice(&bytes)?;
    if fixtures.is_empty() || fixtures.len() > 8 {
        return Err("use 1..8 real transcript fixtures".into());
    }
    let base = Config::load()?;
    let vault = Vault::load()?;
    for fixture in fixtures {
        if fixture.entries.len() > 512 || fixture.checks.len() > 64 {
            return Err("fixture entry/check ceiling exceeded".into());
        }
        for repeat in 0..2 {
            let arms = if repeat == 0 { [false, true] } else { [true, false] };
            for candidate in arms {
                let mut config = base.clone();
                config.memory.enabled = false;
                config.web.enabled = false;
                config.agent.install_servers = false;
                config.agent.compaction_timeout_secs =
                    Some(config.agent.compaction_timeout_secs.unwrap_or(90).clamp(1, 600));
                let source = if config.agent.compaction_model.is_empty() {
                    config.agent.model.clone()
                } else {
                    config.agent.compaction_model.clone()
                };
                let provider: Arc<dyn rook_llm::Provider> =
                    Arc::from(rook_core::models::provider_for(&config, &vault, &source)?);
                let budget =
                    if candidate { (provider.context_window() / 16).clamp(2048, 8192) as u32 } else { 2048 };
                config.agent.compaction_output_tokens = Some(budget);
                if let Some(model) = config.models.get_mut(&source) {
                    model.compaction.keep_recent_tokens = Some(256);
                }
                let root = tempfile::tempdir()?;
                let rook = Rook::from_parts(
                    rook_store::Store::open(root.path().join("store"))?,
                    config,
                    rook_skills::Environment::bare("experiment", "experiment", "0.16.0"),
                    rook_skills::SkillIndex::default(),
                    root.path().into(),
                );
                let session = rook.start_session("explicit compaction budget A/B")?;
                for entry in &fixture.entries {
                    let Some(kind) = rook_store::EventKind::named(&entry.kind) else { continue };
                    if matches!(
                        kind,
                        rook_store::EventKind::UserMessage
                            | rook_store::EventKind::AssistantMessage
                            | rook_store::EventKind::ToolCall
                            | rook_store::EventKind::ToolResult
                    ) {
                        rook.log(session, kind, &entry.label, &vault.redact(&entry.body))?;
                    }
                }
                let start = std::time::Instant::now();
                AgentLoop::new(&rook, provider, session).compact_now().await;
                let elapsed_ms = start.elapsed().as_millis();
                let (_, summary) = rook.last_compaction(session)?;
                let events = rook.transcript(session, 0, 1024, 65536)?;
                let saved = rook.store.events(session, 0, 1024)?;
                let usage: Vec<_> = saved.iter().filter(|e| e.record.label == "compaction usage")
                    .map(|e| serde_json::json!({"input_tokens":e.record.tokens_in,"output_tokens":e.record.tokens_out})).collect();
                // Usage is retained for incomplete/empty responses too. Read
                // committed metadata, never a display-truncated note or prose.
                let outcome = saved
                    .iter()
                    .rev()
                    .find(|e| e.record.kind == rook_store::EventKind::Compaction)
                    .map(|event| {
                        record_body(&rook, event, 1024 * 1024)
                            .and_then(|body| Ok(serde_json::from_slice::<CompactionOutcome>(&body)?))
                    })
                    .transpose()?;
                let mut model_receipt = None;
                for event in saved.iter().rev().filter(|e| e.record.label == "rook:model-aux:v1") {
                    let auxiliary: Auxiliary = serde_json::from_slice(&record_body(&rook, event, 4096)?)?;
                    if auxiliary.purpose == "compaction" {
                        model_receipt = Some(auxiliary.receipt);
                        break;
                    }
                }
                let compaction_outcome = match outcome.as_ref().map(|note| note.summarized) {
                    Some(true) => "summary",
                    Some(false) => "source_fallback",
                    None => "unrecorded",
                };
                let summary = summary.unwrap_or_default();
                let available = compaction_outcome == "summary"
                    && !summary.trim().is_empty()
                    && !usage.is_empty()
                    && model_receipt
                        .as_ref()
                        .is_some_and(|receipt| receipt.complete && receipt.usage_reported);
                let checks: Vec<_> = if available {
                    fixture.checks.iter().map(|term|serde_json::json!({"term":term,"preserved":summary.to_lowercase().contains(&term.to_lowercase())})).collect()
                } else {
                    Vec::new()
                };
                println!(
                    "{}",
                    serde_json::json!({"fixture":fixture.name,"repeat":repeat,"candidate":candidate,"output_budget":budget,"available":available,"compaction_outcome":compaction_outcome,"model_receipt":model_receipt,"elapsed_ms":elapsed_ms,"usage":usage,"checks":checks,"summary":summary,"notices":events.iter().filter(|e|e.kind=="error").map(|e|e.body.clone()).collect::<Vec<_>>() })
                );
                if !available {
                    return Err("summary comparison is incomplete: a confirmed summary and reported usage are required; stop comparison, inspect the saved source/error, leave the default unchanged".into());
                }
            }
        }
    }
    Ok(())
}
