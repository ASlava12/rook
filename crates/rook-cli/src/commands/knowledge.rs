//! Documentation and memory commands.

use crate::args::{DocsCmd, MemoryCmd};
use crate::{fmt, source::Source};
use anyhow::{Result, bail};
use std::path::Path;

pub(crate) fn cmd_memory(source: &Source, cmd: MemoryCmd, at: &Path, json: bool) -> Result<()> {
    let workspace = at.display().to_string();
    if let MemoryCmd::Ls { all } = cmd {
        let held = source.memory()?;
        let facts: Vec<_> = if all {
            held.iter().collect()
        } else {
            held.iter().filter(|f| f.scope.applies_in(&workspace)).collect()
        };
        return show_memory(&facts, &held, all, json);
    }
    if let MemoryCmd::Rm { id } = &cmd {
        match source.forget(id)? {
            Some(fact) => println!("forgot [{}] {}", fact.id, fact.text),
            None => bail!("no fact {id:?}"),
        }
        return Ok(());
    }
    match cmd {
        MemoryCmd::Ls { .. } | MemoryCmd::Rm { .. } => unreachable!("routed above"),
        MemoryCmd::Search { query } => {
            let hits = source.memory_search(&query.join(" "), at)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&hits)?);
                return Ok(());
            }
            if hits.is_empty() {
                println!("nothing matched");
            }
            for hit in hits {
                println!("[{}] {}", hit.fact.id, hit.fact.text);
                let why = if hit.matched.is_empty() { "pinned".into() } else { hit.matched.join(", ") };
                println!("      score {:.1} · {why}", hit.score);
            }
        }

        MemoryCmd::Add { text, tag, global, pin } => {
            let scope = if global { rook_core::Scope::Global } else { rook_core::Scope::Project(workspace) };
            let mut fact = rook_core::Fact::new(text.join(" "), scope).with_tags(tag);
            fact.pinned = pin;
            let id = fact.id.clone();
            use rook_core::memory::Learned;
            match source.remember(fact, at)? {
                Learned::New | Learned::Merged => println!("remembered as [{id}]"),
                Learned::Unchanged => println!("already remembered as [{id}]"),
                Learned::ScopedElsewhere(scope) => println!(
                    "already remembered as [{id}], scoped to {} — pass --global to widen it",
                    scope.label()
                ),
            }
        }

        MemoryCmd::History => {
            let history = source.memory_history()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&history)?);
                return Ok(());
            }
            let rows: Vec<Vec<String>> = history
                .iter()
                .map(|v| {
                    vec![
                        v.object.chars().take(12).collect(),
                        fmt::timestamp(v.updated_at),
                        v.facts.to_string(),
                        v.note.clone().unwrap_or_default(),
                    ]
                })
                .collect();
            print!("{}", fmt::table(&["object", "when", "facts", "note"], &rows));
        }

        MemoryCmd::Diff { a, b } => {
            let changes = source.memory_diff(&a, &b)?;
            if changes.is_empty() {
                println!("identical");
            }
            for (change, fact) in changes {
                let sigil = if change == rook_core::memory::Change::Learned { '+' } else { '-' };
                println!("{sigil} [{}] {}", fact.id, fact.text);
            }
        }

        MemoryCmd::Since { days } => {
            let changes = source.memory_since(days)?;
            if changes.is_empty() {
                println!("nothing learned or forgotten in the last {days} day(s)");
            }
            for (change, fact) in changes {
                let sigil = if change == rook_core::memory::Change::Learned { '+' } else { '-' };
                println!("{sigil} [{}] {}", fact.id, fact.text);
            }
        }
    }
    Ok(())
}

fn show_memory(facts: &[&rook_core::Fact], held: &[rook_core::Fact], all: bool, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&facts)?);
        return Ok(());
    }
    let rows: Vec<Vec<String>> = facts
        .iter()
        .map(|f| {
            vec![
                f.id.clone(),
                if f.pinned { "pin".into() } else { String::new() },
                f.scope.label().rsplit('/').next().unwrap_or("global").to_string(),
                f.tags.join(","),
                f.text.chars().take(70).collect(),
            ]
        })
        .collect();
    print!("{}", fmt::table(&["id", "", "scope", "tags", "fact"], &rows));
    if !all && facts.len() < held.len() {
        println!("\n{} more scoped to other workspaces (--all)", held.len() - facts.len());
    }
    // Pinning wins over relevance and not over the budget, so past this point
    // pinning one more fact costs another one its place. Read from the
    // configuration rather than from an engine, which a routed listing has not
    // opened.
    let budget = rook_core::Config::load()?.memory.context_budget_tokens;
    let pinned: usize = facts.iter().filter(|f| f.pinned).map(|f| f.tokens()).sum();
    if pinned > budget {
        println!(
            "\npinned facts come to ~{pinned} tokens against a recall budget of {budget} \
             — some of them will not reach the model"
        );
    }
    Ok(())
}

pub(crate) fn cmd_docs(source: &Source, cmd: DocsCmd, json: bool) -> Result<()> {
    match cmd {
        DocsCmd::Ls => {
            let kept = source.docs_kept()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&kept)?);
                return Ok(());
            }
            if kept.is_empty() {
                println!(
                    "nothing kept yet. `rook docs add <topic>` reads a technology's \
                     documentation and keeps it; the agent does the same on its own when it is \
                     asked about something it has no copy of."
                );
                return Ok(());
            }
            for set in &kept {
                println!(
                    "{} {} · {} page(s) · {} · read {}",
                    set.topic,
                    set.version,
                    set.pages,
                    fmt::bytes(set.bytes as u64),
                    fmt::ago(set.fetched_at)
                );
            }
        }
        DocsCmd::Show { topic, version, question, page } => {
            let Some(set) = source.docs(&topic, version.as_deref())? else {
                // What is here under a name near the one asked for, because
                // that is usually the answer: a narrower question gathered an
                // hour ago is a set nobody remembers the exact name of.
                let near = source.docs_like(&topic)?;
                let beside = match near.is_empty() {
                    true => String::new(),
                    false => format!(
                        " Kept under a name near it: {}.",
                        near.iter().map(|s| s.topic.clone()).collect::<Vec<_>>().join(", ")
                    ),
                };
                bail!("nothing kept for {topic:?}. `rook docs add {topic}` gathers it.{beside}");
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&set)?);
                return Ok(());
            }
            if let Some(n) = page {
                let Some(page) = set.pages.get(n.saturating_sub(1)) else {
                    bail!("that set has {} page(s)", set.pages.len());
                };
                println!("{}\n{}\n", page.title, page.url);
                println!("{}", page.text);
                return Ok(());
            }
            println!(
                "{} {} · kept as {} · read {}",
                set.topic,
                set.version,
                rook_core::docs::reference(&set.topic, &set.version),
                fmt::ago(set.fetched_at)
            );
            match question {
                Some(question) => {
                    let found = set.passages(&question, 5);
                    if found.is_empty() {
                        println!("\nnothing in it is about that.");
                    }
                    for (text, url) in found {
                        println!("\n[from {url}]\n{text}");
                    }
                }
                None => {
                    for (n, page) in set.pages.iter().enumerate() {
                        println!("\n{}. {}\n   {}", n + 1, page.title, page.url);
                    }
                    println!("\n`--question` reads it; `--page N` prints one whole.");
                }
            }
        }
        DocsCmd::Add { topic, version, refresh } => {
            let version = version.unwrap_or_else(|| rook_core::docs::LATEST.into());
            if !refresh && let Some(set) = source.docs(&topic, Some(&version))? {
                println!(
                    "already kept: {} {} · {} page(s), read {}. `--refresh` asks the sources \
                     what changed.",
                    set.topic,
                    set.version,
                    set.pages.len(),
                    fmt::ago(set.fetched_at)
                );
                return Ok(());
            }
            let (reference, set, notes) = source.gather_docs(&topic, &version)?;
            for note in &notes {
                println!("{note}");
            }
            println!("{} page(s) in {reference}:", set.pages.len());
            for page in &set.pages {
                println!("- {} — {}", page.title, page.url);
            }
        }
        DocsCmd::Rm { topic, version } => match source.forget_docs(&topic, version.as_deref())? {
            0 => bail!("nothing kept for {topic:?}"),
            gone => println!("dropped {gone} set(s). `store maintain` reclaims the space."),
        },
    }
    Ok(())
}
