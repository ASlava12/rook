//! Skill catalog, installation and trust decisions.

use crate::args::SkillCmd;
use crate::{fmt, source::Source};
use anyhow::Result;
use rook_core::Refreshed;
use rook_skills::SkillCard;
use std::path::Path;

pub(crate) fn cmd_skills(source: &Source, cmd: SkillCmd, workspace: &Path, json: bool) -> Result<()> {
    if let SkillCmd::Ls { all } = cmd {
        return show_skills(&source.catalog(workspace)?, all, json);
    }
    if let SkillCmd::Show { name } = &cmd {
        return show_skill(&source.skill(name, workspace)?, json);
    }
    if let SkillCmd::Why { name } = &cmd {
        let why = source.why_skill(name, workspace)?;
        if json {
            println!("{}", serde_json::to_string_pretty(&why)?);
            return Ok(());
        }
        let env = &why.environment;
        println!("environment: {} / {} / {} userland\n", env.os, env.arch, env.userland);
        for version in &why.versions {
            if version.mismatches.is_empty() {
                println!("  ✓ {} [{}] applies", version.id, version.source);
            } else {
                println!("  ✗ {} [{}]", version.id, version.source);
                for m in &version.mismatches {
                    println!("      {m}");
                }
            }
        }
        match (&why.chosen, &why.why_not) {
            (Some(chosen), _) => println!("\nchosen: {chosen}"),
            (None, reason) => {
                println!("\nchosen: none — {}", reason.as_deref().unwrap_or("nothing applies"))
            }
        }
        return Ok(());
    }
    if let SkillCmd::History { name } = &cmd {
        let history = source.skill_history(name)?;
        if json {
            println!("{}", serde_json::to_string_pretty(&history)?);
            return Ok(());
        }
        if history.is_empty() {
            println!("no captures yet — `rook skills capture {name}`");
            return Ok(());
        }
        let rows: Vec<Vec<String>> = history
            .iter()
            .map(|h| {
                vec![
                    h.object.chars().take(12).collect(),
                    h.version.clone(),
                    fmt::timestamp(h.captured_at),
                    h.files.to_string(),
                    fmt::bytes(h.bytes),
                    h.note.clone().unwrap_or_default(),
                ]
            })
            .collect();
        print!("{}", fmt::table(&["object", "version", "captured", "files", "size", "note"], &rows));
        return Ok(());
    }
    if let SkillCmd::Diff { a, b } = &cmd {
        let changes = source.skill_diff(a, b)?;
        if changes.is_empty() {
            println!("identical");
        }
        for (path, change) in changes {
            println!("{} {path}", change.sigil());
        }
        return Ok(());
    }
    if let SkillCmd::Sources = cmd {
        // The configuration, which is a file: reading it never needed the store
        // and so never needed the daemon stopped.
        let sources = rook_core::Config::load()?.skill_sources;
        if sources.is_empty() {
            println!("none configured — add them under `[skill_sources]` in config.toml");
        }
        for source in &sources {
            println!("  {source}");
        }
        return Ok(());
    }
    if let SkillCmd::Search { query, refresh } = &cmd {
        let (offered, errors) = source.skills_offered(query, *refresh)?;
        {
            if json {
                let items: Vec<_> = offered
                    .iter()
                    .map(|o| {
                        serde_json::json!({
                            "name": o.name, "description": o.description, "source": o.source,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&items)?);
                return Ok(());
            }
            let installed: Vec<String> = source.catalog(workspace)?.iter().map(|c| c.name.clone()).collect();
            let rows: Vec<Vec<String>> = offered
                .iter()
                .map(|o| {
                    vec![
                        o.name.clone(),
                        if installed.contains(&o.name) { "installed".into() } else { String::new() },
                        o.description.chars().take(72).collect(),
                    ]
                })
                .collect();
            if rows.is_empty() {
                println!("nothing offered matches {query:?} — `rook skills sources` lists where it looked");
            } else {
                print!("{}", fmt::table(&["name", "", "description"], &rows));
                println!("\n`rook skills install <name>` puts one here.");
            }
            for error in &errors {
                println!("✗ {error}");
            }
        }
        return Ok(());
    }
    match cmd {
        SkillCmd::Ls { .. }
        | SkillCmd::Show { .. }
        | SkillCmd::Why { .. }
        | SkillCmd::Sources
        | SkillCmd::Search { .. }
        | SkillCmd::History { .. }
        | SkillCmd::Diff { .. } => unreachable!("routed above"),
        SkillCmd::Update => {
            let refreshed = source.update_skills()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&refreshed)?);
                return Ok(());
            }
            if refreshed.is_empty() {
                println!("no skills in {} yet", rook_core::paths::user_skills_dir().display());
            }
            for (name, outcome) in &refreshed {
                match outcome {
                    Refreshed::Updated { source } => println!("  {name} — updated from {source}"),
                    Refreshed::Current => println!("  {name} — already current"),
                    // Named rather than counted: a skill whose update is being
                    // held back is the one thing somebody running this wants to
                    // know about, and "nothing to do" would be the opposite of
                    // what happened.
                    Refreshed::Edited { source, came_at } => println!(
                        "  {name} — changed here since it came from {source} {}; left as it is. \
                         `rook skills history {name}` shows what it came as.",
                        fmt::ago(*came_at)
                    ),
                    Refreshed::Gone { source } => {
                        println!("  {name} — {source} no longer offers it; left as it is")
                    }
                    Refreshed::Unreachable { why } => {
                        println!("  {name} — could not be checked ({why}); left as it is")
                    }
                    Refreshed::Yours => println!("  {name} — yours, not from a source"),
                }
            }
        }
        SkillCmd::Install { name } => {
            let path = source.install_skill(&name)?;
            println!("installed {}", path.display());
            println!("read it before trusting it: {}", path.join("SKILL.md").display());
        }
        SkillCmd::New { name, description } => {
            let dir = source.new_skill(&name, &description)?;
            println!("created {}", dir.join("SKILL.md").display());
            println!("edit it, then `rook skills capture {name} -m \"first version\"`");
        }
        SkillCmd::Capture { name, message } => {
            let (set, id) = source.capture_skill(&name, message)?;
            println!(
                "captured {} v{} — {} file{}, {} → object {}",
                set.name,
                set.version,
                set.files.len(),
                if set.files.len() == 1 { "" } else { "s" },
                fmt::bytes(set.total_bytes),
                &id[..12]
            );
        }
        SkillCmd::Rollback { name, object } => {
            let result = source.rollback_skill(&name, &object)?;
            println!("restored {} file(s) for {name} from {object}", result.restored);
            match &result.undo {
                Some(undo) => println!("undo with `rook skills rollback {name} {}`", undo.short()),
                None => println!("{name} was not on disk, so there was nothing to capture first"),
            }
            if !result.left_behind.is_empty() {
                println!("\nnot in that capture, left on disk in {}:", result.dir.display());
                for f in &result.left_behind {
                    println!("  {f}");
                }
            }
        }
    }
    Ok(())
}

fn show_skills(catalog: &[SkillCard], all: bool, json: bool) -> Result<()> {
    let cards: Vec<_> = catalog.iter().filter(|c| all || c.applicable).collect();
    if json {
        println!("{}", serde_json::to_string_pretty(&cards)?);
        return Ok(());
    }
    let rows: Vec<Vec<String>> = cards
        .iter()
        .map(|c| {
            vec![
                if c.applicable { "✓".into() } else { "·".into() },
                c.name.clone(),
                c.version.clone(),
                c.source.clone(),
                format!("~{}", c.body_tokens),
                c.description.chars().take(60).collect(),
            ]
        })
        .collect();
    print!("{}", fmt::table(&["", "name", "version", "source", "tokens", "description"], &rows));
    if !all {
        println!(
            "\n(`--all` also shows skills blocked by this environment; `rook skills why <name>` explains one)"
        );
    }
    Ok(())
}

fn show_skill(skill: &rook_skills::SkillDetail, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(skill)?);
        return Ok(());
    }
    println!("# {} {} ({})", skill.name, skill.version, skill.source);
    if let Some(variant) = &skill.variant {
        println!("variant: {} — selected for this environment", variant.display());
    }
    println!("dir: {}\n", skill.dir.display());
    println!("{}", skill.body);
    Ok(())
}
