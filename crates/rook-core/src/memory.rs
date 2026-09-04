//! What the agent knows between sessions.
//!
//! Every reference implementation stores memory as flat text files with an
//! add/retrieve/remove API and a character cap. That works, and it loses two
//! things Rook's store already provides for free: where a fact came from, and
//! what changed since yesterday.
//!
//! So memory is a single content-addressed [`MemoryBook`] per version, pointed
//! at by the `memory/head` ref and recorded in `memory/h/…`. Editing memory
//! writes a new version rather than mutating one, which makes `memory history`,
//! `memory diff` and rollback the same operations skills already have.
//!
//! Whole-book versions rather than per-fact objects: a book is a few kilobytes,
//! the dictionary compresses near-identical versions hard, and it keeps diffing
//! to a set comparison instead of a graph walk.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use rook_store::{Kind, ObjectId, Store};

use crate::error::Result;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "path", rename_all = "lowercase")]
pub enum Scope {
    /// Applies wherever the agent runs.
    Global,
    /// Applies only under this workspace.
    Project(String),
}

impl Scope {
    pub fn applies_in(&self, workspace: &str) -> bool {
        match self {
            Scope::Global => true,
            Scope::Project(path) => workspace.starts_with(path.as_str()),
        }
    }

    /// True when everything this scope covers, the other one covers too.
    pub fn within(&self, other: &Scope) -> bool {
        match (self, other) {
            (_, Scope::Global) => true,
            (Scope::Global, Scope::Project(_)) => false,
            (Scope::Project(mine), Scope::Project(theirs)) => mine.starts_with(theirs.as_str()),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Scope::Global => "global".into(),
            Scope::Project(path) => path.clone(),
        }
    }
}

/// Where a fact came from. Without it, a wrong memory is impossible to trace
/// back to the turn that produced it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub session: String,
    pub seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fact {
    /// First 8 hex of the content hash: stable, short enough to type.
    pub id: String,
    pub text: String,
    pub scope: Scope,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub source: Option<Provenance>,
    pub created_at: i64,
    /// Always included in context, regardless of the retrieval budget.
    #[serde(default)]
    pub pinned: bool,
}

impl Fact {
    pub fn new(text: impl Into<String>, scope: Scope) -> Self {
        let text = text.into().trim().to_string();
        Self {
            id: ObjectId::of(text.as_bytes()).to_hex()[..8].to_string(),
            text,
            scope,
            tags: Vec::new(),
            source: None,
            created_at: rook_store::now_unix(),
            pinned: false,
        }
    }

    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags.into_iter().map(|t| t.trim().to_lowercase()).filter(|t| !t.is_empty()).collect();
        self
    }

    pub fn from_turn(mut self, session: u128, seq: u64) -> Self {
        self.source = Some(Provenance { session: rook_store::format_session_id(session), seq });
        self
    }

    pub fn tokens(&self) -> usize {
        crate::context::estimate_tokens(&self.text)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MemoryBook {
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default)]
    pub facts: Vec<Fact>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Change {
    Learned,
    Forgotten,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "learned", content = "scope", rename_all = "snake_case")]
pub enum Learned {
    New,
    /// The fact was already known; tags, pinning or a wider scope were folded
    /// into it.
    Merged,
    Unchanged,
    /// Already known, but somewhere this one does not reach — and neither scope
    /// contains the other, so widening would either leak it or lose it.
    ScopedElsewhere(Scope),
}

impl MemoryBook {
    pub fn load(store: &Store, id: &ObjectId) -> Result<Self> {
        Ok(serde_json::from_slice(&store.get(id)?)?)
    }

    pub fn store(&self, store: &Store) -> Result<ObjectId> {
        Ok(store.put(Kind::Memory, &serde_json::to_vec(self)?)?)
    }

    /// Add a fact, or merge into the one that already says it.
    ///
    /// Identity is the text itself, so remembering the same thing twice updates
    /// tags and pinning instead of accumulating duplicates — which is how a
    /// memory that the model writes to on every turn stays finite.
    pub fn learn(&mut self, fact: Fact) -> Learned {
        let Some(existing) = self.facts.iter_mut().find(|f| f.id == fact.id) else {
            self.facts.push(fact);
            self.updated_at = rook_store::now_unix();
            return Learned::New;
        };

        // Identity is the text, so the same sentence learned globally and in a
        // project is one fact. Keeping the first scope silently means a fact
        // asked for globally never applies anywhere else.
        let elsewhere = (!existing.scope.within(&fact.scope) && !fact.scope.within(&existing.scope))
            .then(|| existing.scope.clone());
        let before =
            (existing.tags.clone(), existing.pinned, existing.source.clone(), existing.scope.clone());
        if existing.scope.within(&fact.scope) {
            existing.scope = fact.scope;
        }
        existing.tags.extend(fact.tags);
        existing.tags.sort();
        existing.tags.dedup();
        existing.pinned |= fact.pinned;
        existing.source = existing.source.clone().or(fact.source);

        if let Some(scope) = elsewhere {
            return Learned::ScopedElsewhere(scope);
        }
        if before == (existing.tags.clone(), existing.pinned, existing.source.clone(), existing.scope.clone())
        {
            Learned::Unchanged
        } else {
            self.updated_at = rook_store::now_unix();
            Learned::Merged
        }
    }

    /// Facts that already say close to this, so the caller can supersede one
    /// rather than keep both. Never merged automatically: "prefer tabs" and
    /// "prefer tabs in Makefiles" read alike and mean different things, and a
    /// memory that quietly drops the difference is worse than one that repeats
    /// itself.
    pub fn similar_to(&self, text: &str) -> Vec<&Fact> {
        self.facts.iter().filter(|f| f.text != text && overlap(&f.text, text) >= WORTH_MENTIONING).collect()
    }

    /// Forget by id or by exact text. Returns what went.
    pub fn forget(&mut self, id_or_text: &str) -> Option<Fact> {
        let position = self.facts.iter().position(|f| f.id == id_or_text || f.text == id_or_text)?;
        self.updated_at = rook_store::now_unix();
        Some(self.facts.remove(position))
    }

    pub fn get(&self, id: &str) -> Option<&Fact> {
        self.facts.iter().find(|f| f.id == id)
    }

    pub fn in_scope(&self, workspace: &str) -> impl Iterator<Item = &Fact> {
        self.facts.iter().filter(move |f| f.scope.applies_in(workspace))
    }

    pub fn diff(&self, other: &MemoryBook) -> Vec<(Change, Fact)> {
        let mine: BTreeSet<&str> = self.facts.iter().map(|f| f.id.as_str()).collect();
        let theirs: BTreeSet<&str> = other.facts.iter().map(|f| f.id.as_str()).collect();
        let mut out: Vec<(Change, Fact)> = other
            .facts
            .iter()
            .filter(|f| !mine.contains(f.id.as_str()))
            .map(|f| (Change::Learned, f.clone()))
            .collect();
        out.extend(
            self.facts
                .iter()
                .filter(|f| !theirs.contains(f.id.as_str()))
                .map(|f| (Change::Forgotten, f.clone())),
        );
        out
    }
}

/// A fact and why it was retrieved.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Hit {
    pub fact: Fact,
    pub score: f32,
    /// The query terms and tags that matched, so a surprising result is
    /// explainable rather than mysterious.
    pub matched: Vec<String>,
}

/// Rank `facts` against a query.
///
/// Term overlap rather than embeddings: a personal memory holds hundreds of
/// facts, not millions, and a vector store would be a database and a model to
/// carry around for a ranking that can be read off the screen.
pub fn search<'a>(facts: impl Iterator<Item = &'a Fact>, query: &str) -> Vec<Hit> {
    let terms = terms_of(query);
    let mut hits: Vec<Hit> = facts
        .map(|fact| {
            let text_terms = terms_of(&fact.text);
            let mut matched = Vec::new();
            let mut score = 0.0;
            for term in &terms {
                if fact.tags.iter().any(|t| akin(t, term)) {
                    score += 2.0;
                    matched.push(format!("#{term}"));
                } else if text_terms.iter().any(|t| akin(t, term)) {
                    score += 1.0;
                    matched.push(term.clone());
                }
            }
            if fact.pinned {
                score += 0.5;
            }
            Hit { fact: fact.clone(), score, matched }
        })
        .filter(|hit| hit.score > 0.0 || hit.fact.pinned)
        .collect();
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.fact.created_at.cmp(&a.fact.created_at))
    });
    hits
}

/// Whether two words should count as the same one.
///
/// Prefix matching from four characters, rather than a stemmer: it relates
/// `deploy`/`deploys`/`deployment` and `migrate`/`migration` without a
/// language-specific rule table, and the threshold keeps `on` from matching
/// `once`.
pub(crate) fn akin(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let shortest = a.len().min(b.len());
    shortest >= 4 && (a.starts_with(b) || b.starts_with(a))
}

/// Words worth matching on: lowercase, de-punctuated, minus the ones that carry
/// no signal and would match everything.
/// The words in `text` worth matching on: no punctuation, no single letters,
/// and none of the two dozen that appear in every sentence ever written.
///
/// Shared with the skill catalogue rather than written twice — "which words
/// carry meaning" has one answer, and a search that ranks by how often "a"
/// appears ranks nothing.
pub fn terms_of(text: &str) -> BTreeSet<String> {
    const NOISE: &[&str] = &[
        "the", "a", "an", "and", "or", "but", "is", "are", "was", "were", "be", "to", "of", "in", "on", "at",
        "for", "with", "that", "this", "it", "as", "by", "from", "i", "you", "we",
    ];
    text.split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
        .map(|w| w.trim_matches('-').to_lowercase())
        // Characters, not bytes: `len()` here kept any single non-ASCII letter
        // while dropping every single ASCII one, which is a difference nothing
        // wanted.
        .filter(|w| w.chars().nth(1).is_some() && !NOISE.contains(&w.as_str()))
        .collect()
}

/// Pick what fits in `budget` tokens: pinned first, then the best matches.
///
/// Pinning wins over relevance and not over the budget. It used to win over
/// both — a pinned fact went in whatever it cost — and `remember` lets the model
/// pin, so an agent that pinned freely for a month would have spent the whole
/// window on its own memory before reading a word of the prompt.
pub fn select<'a>(facts: impl Iterator<Item = &'a Fact>, query: &str, budget: usize) -> Vec<Fact> {
    let mut chosen: Vec<Fact> = Vec::new();
    let mut used = 0;
    let hits = search(facts, query);
    let by_pin = hits.iter().filter(|h| h.fact.pinned).chain(hits.iter().filter(|h| !h.fact.pinned));
    for hit in by_pin {
        let cost = hit.fact.tokens();
        if used + cost > budget {
            continue;
        }
        // Two ways of saying one thing spend the budget twice and tell the model
        // nothing the first did. The better-scoring one is already first.
        if chosen.iter().any(|kept| overlap(&kept.text, &hit.fact.text) >= SAME_FACT) {
            continue;
        }
        used += cost;
        chosen.push(hit.fact.clone());
    }
    chosen
}

/// High enough that only a sentence reordered collapses.
///
/// Term overlap cannot tell a restatement from a narrowing or a contradiction,
/// because the distinguishing word is exactly the one that differs. Measured:
/// "prefer tabs" against "prefer tabs in Makefiles" scores 0.80, and "the API
/// listens on port 7717" against "port 8080" scores 0.75 — suppressing either
/// would hide the fact that says something new. Only an identical set of terms
/// is safe to drop.
pub const SAME_FACT: f32 = 0.95;

/// Low enough to catch a restatement, which is all it is used for: a fact this
/// close is named when the model writes a new one, and nothing is discarded on
/// the strength of it. Measured: a plain restatement scores 0.67 and two facts
/// about different things score 0.29, so erring low costs a line of text and
/// erring high costs the mention entirely.
pub const WORTH_MENTIONING: f32 = 0.55;

/// Shared terms as a fraction of the terms the two use between them.
///
/// Dice rather than Jaccard — twice the overlap over the sum, not the overlap
/// over the union — which is what the thresholds below were measured against:
/// "prefer tabs" and "prefer tabs in Makefiles" score 0.80 here and would score
/// 0.67 the other way. Over the same terms the ranking matches on, so a fact
/// judged similar here is one that would have matched the same queries.
pub fn overlap(a: &str, b: &str) -> f32 {
    let (a, b) = (terms_of(a), terms_of(b));
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let shared = a.intersection(&b).count() as f32;
    shared / (a.len() + b.len()) as f32 * 2.0
}
