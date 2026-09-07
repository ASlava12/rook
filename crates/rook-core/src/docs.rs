//! Documentation the agent has read, kept.
//!
//! A model asked about a technology answers from what it was trained on, which
//! was a year old the day it shipped and says so nowhere. The alternative it
//! has is the web, which is a page at a time and gone again by the next turn.
//!
//! So what it reads is kept: one set per topic and version, made of pages that
//! were fetched, turned into prose and filed with where each came from. The
//! source is not the page — nobody wants a copy of somebody's HTML — it is the
//! reading, and the address it was read from, so an answer can carry both: the
//! local copy, which is what the answer is made of, and the original, which is
//! what somebody else can check.
//!
//! Versions are part of the name because documentation is: `latest` when
//! nothing is said, and whatever was asked for when something is.

use serde::{Deserialize, Serialize};

use rook_store::{Kind, ObjectId, Store};

use crate::error::Result;

/// What `latest` means: the newest the sources had when it was read.
pub const LATEST: &str = "latest";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page {
    /// Where it was read from, which is what an answer cites beside the local
    /// copy: a reading nobody can check against its source is a claim.
    pub url: String,
    pub title: String,
    /// The prose, as `web_fetch` reads a page — not the markup it arrived in.
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DocSet {
    pub topic: String,
    pub version: String,
    pub pages: Vec<Page>,
    pub fetched_at: i64,
}

impl DocSet {
    pub fn new(topic: &str, version: &str, pages: Vec<Page>) -> Self {
        Self {
            topic: topic.trim().to_lowercase(),
            version: version.trim().to_lowercase(),
            pages,
            fetched_at: rook_store::now_unix(),
        }
    }

    pub fn bytes(&self) -> usize {
        self.pages.iter().map(|p| p.text.len()).sum()
    }

    pub fn load(store: &Store, id: &ObjectId) -> Result<Self> {
        Ok(serde_json::from_slice(&store.get(id)?)?)
    }

    pub fn store(&self, store: &Store) -> Result<ObjectId> {
        Ok(store.put(Kind::Docs, &serde_json::to_vec(self)?)?)
    }

    /// The passages that answer a question, each with the address it was read
    /// from.
    ///
    /// Paragraphs rather than pages: a documentation page is mostly navigation
    /// and examples of other things, and handing a model the whole of one to
    /// answer a sentence is how a context window goes. Scored the way memory
    /// is — the terms the question uses, counted — because the alternative is
    /// an embedding model to run and a second thing to keep current.
    pub fn passages(&self, question: &str, most: usize) -> Vec<(String, &str)> {
        let asked = crate::memory::terms_of(question);
        if asked.is_empty() {
            return Vec::new();
        }
        let mut scored: Vec<(usize, String, &str)> = Vec::new();
        for page in &self.pages {
            for para in page.text.split("\n\n") {
                let para = para.trim();
                if para.len() < 40 {
                    continue;
                }
                let terms = crate::memory::terms_of(para);
                let hits = asked.iter().filter(|term| terms.iter().any(|word| same_word(term, word))).count();
                if hits > 0 {
                    scored.push((hits, para.to_string(), page.url.as_str()));
                }
            }
        }
        scored.sort_by_key(|(hits, ..)| std::cmp::Reverse(*hits));
        scored.into_iter().take(most).map(|(_, text, url)| (text, url)).collect()
    }
}

/// Two words that are the same word.
///
/// Exactly, or by six characters of shared prefix: a question says
/// "persistence" and the page says "persists", which is the ordinary case
/// rather than the clever one. Six because five matches "config" to
/// "confidence", and a scorer that does that ranks the wrong paragraph first.
/// This is not a stemmer and is not trying to be — it is the cheapest rule that
/// stops an exact-match search from answering "nothing in it is about that"
/// about a page that plainly is.
fn same_word(a: &str, b: &str) -> bool {
    const ENOUGH: usize = 6;
    if a == b {
        return true;
    }
    let shared = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count();
    shared >= ENOUGH && a.chars().count() >= ENOUGH && b.chars().count() >= ENOUGH
}

/// The reference a set is kept under. One per topic and version, so reading
/// the same documentation again replaces the copy rather than growing a second.
pub fn reference(topic: &str, version: &str) -> String {
    format!("docs/{}/{}", slug(topic), slug(version))
}

/// What a topic or a version is called in a reference: the characters a name
/// can carry without becoming a second question about how it was spelled.
fn slug(text: &str) -> String {
    let cleaned: String = text
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '-' })
        .collect();
    cleaned.trim_matches('-').to_string()
}

/// What is kept, as a reader of the list sees it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Kept {
    pub topic: String,
    pub version: String,
    pub pages: usize,
    pub bytes: usize,
    pub fetched_at: i64,
    pub object: String,
}

/// What a gathering needs and what it may spend.
///
/// The two clients are the ones the `web_search` and `web_fetch` tools use, so
/// documentation an agent files away was read exactly as a page it is shown —
/// same redirect rule, same size cap, same reading of the markup.
pub struct Sources {
    pub search: rook_tools::web::Search,
    pub fetch: rook_tools::web::Fetch,
    /// How many pages one topic may cost. Documentation is a site; this is the
    /// difference between reading a few pages of it and mirroring it.
    pub pages: usize,
    /// The ceiling on what a set holds, applied while the pages arrive rather
    /// than after: a set that has already been downloaded has already been
    /// paid for.
    pub bytes: usize,
}

/// Read a topic's documentation and turn it into a set.
///
/// Search for the official pages, fetch a few, keep the prose and the address
/// each came from. What it does not do is keep the page: a copy of somebody's
/// HTML is a copy nobody reads, and what an answer needs is what the page said
/// plus where to check it.
///
/// Answers with the set and whatever could not be read, because a topic where
/// three of five links were dead is a different result from one where they all
/// worked, and the difference is worth saying out loud.
pub async fn gather(
    topic: &str,
    version: &str,
    from: &Sources,
) -> std::result::Result<(DocSet, Vec<String>), String> {
    let query = match version {
        LATEST => format!("{topic} official documentation"),
        version => format!("{topic} {version} documentation"),
    };
    // Twice the pages asked for: a search result that 404s or answers with a
    // login page costs a slot, and a gathering that comes back empty because
    // two links were dead is a gathering nobody trusts.
    let hits = from.search.hits(&query, from.pages * 2).await?;
    if hits.is_empty() {
        return Err(format!("nothing came back for {query:?}"));
    }

    let mut pages = Vec::new();
    let mut notes = Vec::new();
    let mut spent = 0usize;
    for hit in &hits {
        if pages.len() >= from.pages || spent >= from.bytes {
            break;
        }
        let mut lines = hit.lines();
        let title = lines.next().unwrap_or_default().trim().to_string();
        let Some(url) = lines.next().map(str::trim).filter(|u| u.starts_with("http")) else { continue };
        let page = match from.fetch.page(url).await {
            Ok(page) => page,
            Err(e) => {
                notes.push(e);
                continue;
            }
        };
        if page.status >= 400 {
            notes.push(format!("{url} answered {}", page.status));
            continue;
        }
        let text = page.text.trim();
        if text.len() < 200 {
            notes.push(format!("{url} had almost nothing to read"));
            continue;
        }
        // Cut to what is left of the allowance rather than dropping the page:
        // the first part of a documentation page is the part that says what
        // the thing is.
        const STOPPED: &str = "\n\n[the rest of this page was past the size a set may keep]";
        let room = from.bytes - spent;
        let text = match text.len() > room {
            true => {
                // Room for the marker as well as the prose: a cap that the
                // sentence explaining the cap pushes past is not a cap.
                let mut cut = room.saturating_sub(STOPPED.len());
                while cut > 0 && !text.is_char_boundary(cut) {
                    cut -= 1;
                }
                format!("{}{STOPPED}", &text[..cut])
            }
            false => text.to_string(),
        };
        spent += text.len();
        let title = match title.is_empty() {
            true => url.to_string(),
            false => title,
        };
        pages.push(Page { url: page.url, title, text });
    }

    if pages.is_empty() {
        return Err(match notes.is_empty() {
            true => format!("nothing readable came back for {query:?}"),
            false => format!("nothing readable came back for {query:?}: {}", notes.join("; ")),
        });
    }
    Ok((DocSet::new(topic, version, pages), notes))
}
