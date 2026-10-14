//! Emit the production lexical ranking for a bounded, synthetic eval corpus.
use rook_core::memory::{Fact, Scope};
use serde::Deserialize;
use std::io::Read;

#[derive(Deserialize)]
struct Corpus {
    facts: Vec<Document>,
    queries: Vec<Query>,
}
#[derive(Deserialize)]
struct Document {
    id: String,
    text: String,
}
#[derive(Deserialize)]
struct Query {
    text: String,
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut bytes = Vec::new();
    std::io::stdin().take(256 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 256 * 1024 {
        return Err("corpus exceeds 256 KiB".into());
    }
    let corpus: Corpus = serde_json::from_slice(&bytes)?;
    if corpus.facts.len() > 128 || corpus.queries.len() > 64 {
        return Err("too many eval records".into());
    }
    let facts: Vec<_> = corpus
        .facts
        .into_iter()
        .map(|doc| {
            let mut fact = Fact::new(doc.text, Scope::Global);
            fact.id = doc.id;
            fact.created_at = 0;
            fact
        })
        .collect();
    let results: Vec<_> = corpus.queries.iter().map(|query| {
        let start = std::time::Instant::now();
        let hits = rook_core::memory::search(facts.iter(), &query.text);
        serde_json::json!({"micros":start.elapsed().as_micros(),"hits":hits.iter().map(|hit| serde_json::json!({"id":hit.fact.id,"score":hit.score})).collect::<Vec<_>>()})
    }).collect();
    println!("{}", serde_json::to_string(&results)?);
    Ok(())
}
