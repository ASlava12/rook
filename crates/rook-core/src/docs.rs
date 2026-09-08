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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Page {
    /// Where it was read from, which is what an answer cites beside the local
    /// copy: a reading nobody can check against its source is a claim.
    pub url: String,
    pub title: String,
    /// The prose, as `web_fetch` reads a page — not the markup it arrived in.
    pub text: String,
    /// What the server called this version of the page, so asking whether it
    /// has changed is a conditional request that usually answers 304 with no
    /// body. Absent for a page kept before this was recorded, and for a server
    /// that offers neither — those are re-read rather than asked about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    /// When this page was last read, which is not when the set was: a refresh
    /// that changed one page of five leaves the other four as they were.
    #[serde(default)]
    pub fetched_at: i64,
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
    /// answer a sentence is how a context window goes.
    ///
    /// A question is mostly question — "how does persistence work" is one word
    /// about Redis and three about English — and counting matched terms ranked
    /// three copies of "a filter will be created if it does not exist" above
    /// the paragraph about persistence. So a term is worth what it is rare in
    /// this set, and a passage far weaker than the best is left out rather than
    /// padding the answer.
    pub fn passages(&self, question: &str, most: usize) -> Vec<(String, &str)> {
        let every: Vec<String> = crate::memory::terms_of(question).into_iter().collect();
        let asked: Vec<String> = match every.iter().any(|term| !ASKING.contains(&term.as_str())) {
            true => every.into_iter().filter(|term| !ASKING.contains(&term.as_str())).collect(),
            // "what does this do" is all shape and no subject. Better to rank
            // by the shape than to answer nothing.
            false => every,
        };
        if asked.is_empty() {
            return Vec::new();
        }

        // Which of the asked terms each paragraph has, once — the same
        // comparison answers both the ranking and how common a term is, and
        // asking it twice is how the two drift apart.
        let mut found: Vec<(Vec<bool>, &str, &str)> = Vec::new();
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for page in &self.pages {
            for para in page.text.split("\n\n") {
                let para = para.trim();
                // Short enough to be a heading or a link, and the same line
                // twice is a table of contents rather than two answers: an
                // index page repeats "A filter will be created if it does not
                // exist" once per command, and three of five passages came
                // back as the same sentence.
                if para.len() < 40 || !seen.insert(para) {
                    continue;
                }
                let terms = crate::memory::terms_of(para);
                let matched: Vec<bool> =
                    asked.iter().map(|term| terms.iter().any(|word| same_word(term, word))).collect();
                // Every paragraph, matching or not: how common a term is has
                // to be counted against the whole set. Counted against the
                // matching ones only, a term matched by the one paragraph that
                // matched anything is in all of them — so the only term that
                // carried signal was the one thrown away, and a question with a
                // single distinctive word came back empty.
                found.push((matched, para, page.url.as_str()));
            }
        }
        if found.is_empty() {
            return Vec::new();
        }

        // A term is worth what it is rare: the paragraphs in the set over the
        // paragraphs that use it. "does" turns up in a fifth of a command
        // index and "persistence" in three paragraphs of two hundred, so one
        // paragraph with the rare word outranks forty with the common one.
        // That is idf, without a model to run or a stopword list to keep — and
        // it adapts to the set, which a list cannot: "cluster" is noise in the
        // clustering documentation and the answer everywhere else.
        let weight: Vec<usize> = (0..asked.len())
            .map(|term| found.len() / found.iter().filter(|(matched, ..)| matched[term]).count().max(1))
            .collect();

        let mut scored: Vec<(usize, &str, &str)> = found
            .iter()
            .map(|(matched, para, url)| {
                let score: usize =
                    matched.iter().zip(&weight).filter(|(hit, _)| **hit).map(|(_, w)| *w).sum();
                (score, *para, *url)
            })
            .filter(|(score, ..)| *score > 0)
            .collect();
        // And a passage far weaker than the best one is not a second answer,
        // it is the reader's work: three of five passages came back matching
        // only the "does" in "how does persistence work", which is a paragraph
        // about nothing that was asked.
        const AS_GOOD: usize = 4;
        let best = scored.iter().map(|(score, ..)| *score).max().unwrap_or_default();
        scored.retain(|(score, ..)| score * AS_GOOD >= best);
        scored.sort_by_key(|(score, ..)| std::cmp::Reverse(*score));
        scored.into_iter().take(most).map(|(_, text, url)| (text.to_string(), url)).collect()
    }
}

/// The words a question is made of rather than about.
///
/// Rarity cannot see these, which is what makes them worth a list: "work" is a
/// rare word in a set of documentation and carries nothing in "how does
/// persistence work", so scoring by rarity alone put "Redis works in most POSIX
/// systems" above the paragraph about persistence. Memory's own noise list is
/// shorter because a fact is not phrased as a question; this is that shape, and
/// nothing else — a term is dropped here only when no documentation would ever
/// be about it. Prefixes are not stripped, so a set about workers still matches
/// "worker": six characters of shared prefix is what makes two words one, and
/// "work" is four.
const ASKING: &[&str] = &[
    "how", "what", "why", "when", "where", "which", "who", "whose", "does", "do", "did", "done", "can",
    "could", "should", "would", "will", "shall", "may", "might", "must", "has", "have", "had", "been",
    "being", "there", "here", "these", "those", "they", "them", "their", "its", "than", "then", "if", "so",
    "such", "use", "used", "using", "work", "works", "working", "mean", "means", "tell", "show", "explain",
    "need", "want", "get", "got", "make", "makes", "about", "into", "my", "me", "your", "not",
];

/// What a check of a kept set found.
pub struct Checked {
    pub set: DocSet,
    /// Pages the source answered differently, by title.
    pub changed: Vec<String>,
    /// Pages the source no longer serves, or would not answer for.
    pub unreadable: Vec<String>,
}

/// Ask the sources whether a kept set is still what they serve, and re-read
/// only what changed.
///
/// Age is the wrong question and this is the right one. A copy of a version
/// that is pinned — `postgres 16` — does not go stale by getting older, and a
/// copy of `latest` can be wrong the day after it was read. So nothing is
/// decided from a timestamp: each page is asked for with the validator the
/// server gave, and a server that says 304 has said the copy is current for the
/// cost of a round trip and no body. A page whose server offered neither an
/// `ETag` nor a `Last-Modified` is simply read again — there is nothing to ask
/// with, and pretending otherwise would be a check that always passes.
pub async fn recheck(set: &DocSet, from: &Sources) -> Checked {
    let mut pages = Vec::with_capacity(set.pages.len());
    let mut changed = Vec::new();
    let mut unreadable = Vec::new();
    let mut spent = 0usize;

    for page in &set.pages {
        let asked = from.fetch.page_unless(&page.url, page.etag.as_deref(), page.modified.as_deref()).await;
        let fresh = match asked {
            // The source says the copy is current, which is the answer this
            // exists to get cheaply.
            Ok(None) => {
                spent += page.text.len();
                pages.push(page.clone());
                continue;
            }
            Ok(Some(fresh)) if fresh.status < 400 => fresh,
            Ok(Some(fresh)) => {
                unreadable.push(format!("{} answered {}", page.url, fresh.status));
                pages.push(page.clone());
                continue;
            }
            Err(why) => {
                // A page that cannot be reached is not a page that changed: the
                // copy stands, and what could not be checked is said.
                unreadable.push(why);
                pages.push(page.clone());
                continue;
            }
        };

        let text = fresh.text.trim();
        // The same bound as a gathering, applied the same way: a page that grew
        // past what a set may hold is cut where the rest of them are.
        let room = from.bytes.saturating_sub(spent);
        let text = match text.len() > room {
            true => trimmed(text, room),
            false => text.to_string(),
        };
        if text != page.text {
            changed.push(page.title.clone());
        }
        spent += text.len();
        pages.push(Page {
            url: fresh.url,
            title: page.title.clone(),
            text,
            etag: fresh.etag,
            modified: fresh.modified,
            fetched_at: rook_store::now_unix(),
        });
    }

    let mut set = DocSet { pages, ..set.clone() };
    // The set's own stamp is when it was last *checked*, since that is what
    // somebody reading "read 3 days ago" wants to know; a page that did not
    // change keeps its own older one.
    set.fetched_at = rook_store::now_unix();
    Checked { set, changed, unreadable }
}

/// The results whose host is the project's own, first.
///
/// A search for "redis official documentation" answers with redis.io and with
/// four sites that wrote about redis, and the pages fetched are the first few —
/// so an aggregator's summary of the documentation gets kept as the
/// documentation. The host carrying the topic's name is the cheapest signal
/// that a page is the source rather than a reading of it, and it is only an
/// ordering: nothing is dropped, because a project whose documentation lives on
/// readthedocs is not a mistake.
pub fn most_official_first(topic: &str, hits: Vec<String>) -> Vec<String> {
    let name = slug(topic).replace('-', "");
    if name.len() < 3 {
        return hits;
    }
    let theirs = |hit: &String| {
        hit.lines()
            .nth(1)
            .and_then(|url| url.split("://").nth(1))
            .and_then(|rest| rest.split('/').next())
            .is_some_and(|host| host.replace(['-', '.'], "").contains(&name))
    };
    // Stable, so the engine's own ranking decides everything this does not.
    let (official, rest): (Vec<String>, Vec<String>) = hits.into_iter().partition(theirs);
    official.into_iter().chain(rest).collect()
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

/// How old a copy is, for a model deciding whether to trust it.
///
/// Documentation from a year ago is what a model already has and the whole
/// reason for the copy; a reader who is not told the age cannot tell the two
/// apart, and `refresh` is one argument away.
pub fn age(fetched_at: i64) -> String {
    let days = (rook_store::now_unix() - fetched_at).max(0) / 86_400;
    match days {
        0 => "today".into(),
        1 => "yesterday".into(),
        days => format!("{days} days ago"),
    }
}

/// Whether two topics are about the same thing.
///
/// One word in common, of four characters or more. `redis` and `redis
/// persistence` are the case this exists for — a narrow question gathered an
/// hour ago, and a broad one about to spend five fetches finding the same site.
/// Four characters because `the` and `api` are in half of everything, and one
/// word because two topics that share nothing are two topics.
pub fn relates(asked: &str, kept: &str) -> bool {
    let words = |text: &str| -> Vec<String> {
        slug(text).split('-').filter(|w| w.chars().count() >= 4).map(str::to_string).collect()
    };
    let (asked, kept) = (words(asked), words(kept));
    asked.iter().any(|word| kept.contains(word))
}

/// A page cut to what is left of the allowance, saying where it stopped.
///
/// Room for the marker as well as the prose: a cap that the sentence explaining
/// the cap pushes past is not a cap.
fn trimmed(text: &str, room: usize) -> String {
    const STOPPED: &str = "\n\n[the rest of this page was past the size a set may keep]";
    let mut cut = room.saturating_sub(STOPPED.len());
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{STOPPED}", &text[..cut])
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
    let hits = most_official_first(topic, hits);

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
        let room = from.bytes - spent;
        let text = match text.len() > room {
            true => trimmed(text, room),
            false => text.to_string(),
        };
        spent += text.len();
        let title = match title.is_empty() {
            true => url.to_string(),
            false => title,
        };
        pages.push(Page {
            url: page.url,
            title,
            text,
            etag: page.etag,
            modified: page.modified,
            fetched_at: rook_store::now_unix(),
        });
    }

    if pages.is_empty() {
        return Err(match notes.is_empty() {
            true => format!("nothing readable came back for {query:?}"),
            false => format!("nothing readable came back for {query:?}: {}", notes.join("; ")),
        });
    }
    Ok((DocSet::new(topic, version, pages), notes))
}
