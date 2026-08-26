//! Finding things in what the agent has already done.
//!
//! A session list stops helping at about thirty sessions. The question people
//! actually have is "when did I work on the parser", and the answer is in the
//! transcripts.
//!
//! Scanning is organised around content addressing rather than around events:
//! distinct objects are matched once, then mapped back to every position that
//! references them. A file re-read twenty times is one decompression, not
//! twenty.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use rook_store::{Kind, ObjectId};

use crate::error::Result;
use crate::memory;
use crate::service::Rook;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Hit {
    pub session: String,
    pub title: String,
    pub seq: u64,
    pub kind: String,
    pub when: i64,
    /// The matching line, with enough either side to recognise it.
    pub snippet: String,
    pub score: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Found {
    pub hits: Vec<Hit>,
    pub objects_scanned: u64,
    /// True when the scan stopped at its budget rather than at the end.
    pub truncated: bool,
}

#[derive(Clone, Debug)]
pub struct Search {
    pub limit: usize,
    /// Objects to look at before giving up. A personal store holds thousands,
    /// not millions, but an unbounded scan of a large one is a hang.
    pub budget: usize,
    /// Restrict to one session.
    pub session: Option<u128>,
    /// Skip file contents, which are rarely what someone is looking for and are
    /// the bulk of a store by size.
    pub conversation_only: bool,
}

impl Default for Search {
    fn default() -> Self {
        Self { limit: 40, budget: 20_000, session: None, conversation_only: false }
    }
}

impl Rook {
    pub fn search(&self, query: &str, options: &Search) -> Result<Found> {
        let terms: Vec<String> = memory::terms_of(query).into_iter().collect();
        if terms.is_empty() {
            return Ok(Found { hits: Vec::new(), objects_scanned: 0, truncated: false });
        }

        let mut matched: HashMap<ObjectId, (f32, String)> = HashMap::new();
        let mut scanned = 0u64;
        let mut truncated = false;

        for (id, meta) in self.store.list_objects(None, options.budget)? {
            let kind = Kind::from_u8(meta.kind);
            if !searchable(kind, options.conversation_only) {
                continue;
            }
            scanned += 1;
            let Ok(body) = self.store.get(&id) else { continue };
            let Ok(text) = std::str::from_utf8(&body) else { continue };
            if let Some(hit) = score(text, &terms) {
                matched.insert(id, hit);
            }
        }
        if scanned as usize >= options.budget {
            truncated = true;
        }

        let mut hits = Vec::new();
        for session in self.sessions()? {
            if options.session.is_some_and(|only| only != session.id) {
                continue;
            }
            for event in self.store.events(session.id, 0, usize::MAX)? {
                let Some((score, snippet)) = matched.get(&event.record.body) else { continue };
                hits.push(Hit {
                    session: rook_store::format_session_id(session.id),
                    title: session.title.clone(),
                    seq: event.seq,
                    kind: event.record.kind.as_str().to_string(),
                    when: event.record.ts,
                    snippet: snippet.clone(),
                    score: *score,
                });
            }
        }

        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.when.cmp(&a.when))
        });
        hits.truncate(options.limit);
        Ok(Found { hits, objects_scanned: scanned, truncated })
    }
}

fn searchable(kind: Kind, conversation_only: bool) -> bool {
    match kind {
        Kind::Message | Kind::ToolResult | Kind::Memory | Kind::Skill => true,
        Kind::FileBlob => !conversation_only,
        Kind::Snapshot | Kind::Other => false,
    }
}

/// Score a body and pick the line worth showing.
///
/// Distinct terms dominate, then how much the line dwells on them, then a bonus
/// for covering the whole query. Repetition is capped so one line saying a word
/// ten times cannot outrank a line that actually answers the question.
fn score(text: &str, terms: &[String]) -> Option<(f32, String)> {
    const REPETITION_CAP: usize = 3;
    let mut total = 0.0;
    let mut best: Option<(f32, &str)> = None;

    for line in text.lines() {
        let lowered = line.to_lowercase();
        let mut distinct = 0;
        let mut repeats = 0;
        for term in terms {
            let count = lowered.matches(term.as_str()).count();
            if count > 0 {
                distinct += 1;
                repeats += count.min(REPETITION_CAP);
            }
        }
        if distinct == 0 {
            continue;
        }

        let line_score = (distinct * 4 + repeats) as f32 + if distinct == terms.len() { 2.0 } else { 0.0 };
        total += line_score;
        if best.is_none_or(|(best_score, _)| line_score > best_score) {
            best = Some((line_score, line));
        }
    }

    let (_, line) = best?;
    Some((total, snippet(line)))
}

fn snippet(line: &str) -> String {
    const WIDTH: usize = 160;
    let trimmed = line.trim();
    if trimmed.chars().count() <= WIDTH {
        return trimmed.to_string();
    }
    let cut = trimmed.char_indices().nth(WIDTH).map(|(i, _)| i).unwrap_or(trimmed.len());
    format!("{}…", &trimmed[..cut])
}
