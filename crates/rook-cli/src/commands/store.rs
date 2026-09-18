//! Store inspection, maintenance and explicit checkpoints.

use crate::args::{CheckpointCmd, StoreCmd};
use crate::{fmt, source::Source};
use anyhow::{Result, bail};
use rook_core::Rook;
use rook_store::{Kind, StoreStats};

/// How much of one object `store cat` asks the API for. Generous, and finite:
/// the store holds tool output that ran away.
const CAT_BYTES: usize = 4 << 20;

pub(crate) fn cmd_store(source: &Source, cmd: StoreCmd, json: bool) -> Result<()> {
    // Routed before the store is opened, because the daemon may be holding it.
    if let StoreCmd::Stat = cmd {
        return show_stats(&source.stats()?, json);
    }
    if let StoreCmd::Ls { kind, limit } = &cmd {
        let want = kind.as_deref().map(parse_kind).transpose()?;
        return show_objects(&source.objects(want, *limit)?, json);
    }
    if let StoreCmd::Refs { prefix } = &cmd {
        return show_refs(&source.refs(prefix.as_deref().unwrap_or(""))?, json);
    }
    // The one write here the daemon serves. Routed for the reason the reads
    // are: a store the daemon is holding is one this cannot open, and
    // maintenance is what somebody reaches for when the disk is full.
    if let StoreCmd::Maintain { dry_run } = cmd {
        let report = source.maintenance(dry_run)?;
        return show_maintenance(&report, dry_run, json, source.local().ok());
    }
    if let StoreCmd::Cat { id } = &cmd {
        let (bytes, elided) = source.object(id, CAT_BYTES)?;
        std::io::Write::write_all(&mut std::io::stdout(), &bytes)?;
        // The one place a routed read is not the local one: the API windows the
        // payload and decodes it as text, so a large object comes back with its
        // middle marked as elided and a binary one comes back mangled.
        if elided {
            eprintln!("\n[the daemon holds the store; it sent the ends of this object, not all of it]");
        }
        return Ok(());
    }
    match cmd {
        StoreCmd::Stat => unreachable!("routed above"),
        StoreCmd::Ls { .. } | StoreCmd::Cat { .. } | StoreCmd::Refs { .. } | StoreCmd::Maintain { .. } => {
            unreachable!("routed above")
        }
        StoreCmd::Gc { dry_run } => {
            let report = source.collect_garbage(dry_run)?;
            println!(
                "{}scanned {}, reachable {}, collected {} ({} freed), orphan files {}",
                if dry_run { "[dry run] " } else { "" },
                report.scanned,
                report.reachable,
                report.collected,
                fmt::bytes(report.bytes_freed),
                report.orphan_files_removed
            );
            if report.undecodable > 0 {
                println!(
                    "{} object(s) nothing can decode any more removed ({} freed) — they were \
                     compressed with a dictionary this store no longer has, and the events that \
                     named them now read as gone rather than as broken",
                    report.undecodable,
                    fmt::bytes(report.undecodable_bytes)
                );
            }
            // Otherwise a store with garbage in it reports collecting none of
            // it, and the reason is invisible.
            if report.too_new > 0 {
                println!(
                    "{} unreachable object(s) held back for being newly written — a checkpoint \
                     in flight looks exactly like garbage until the event naming it lands",
                    report.too_new
                );
            }
        }
        StoreCmd::Prune { dry_run } => {
            let report = source.prune(dry_run)?;
            println!(
                "{}sessions deleted {}, events deleted {}, protected {}",
                if dry_run { "[dry run] " } else { "" },
                report.sessions_deleted,
                report.events_deleted,
                report.protected
            );
            if !dry_run {
                println!("run `rook store gc` to reclaim the space");
            }
            println!("`store maintain` also enforces the size budget, which needs gc to measure");
        }
        StoreCmd::Verify => {
            let bad = source.verify()?;
            if bad.is_empty() {
                println!("all objects verified");
            } else {
                for (id, reason) in &bad {
                    println!("{}  {reason}", &id[..12.min(id.len())]);
                }
                bail!("{} object(s) failed verification", bad.len());
            }
        }
        StoreCmd::Train => {
            let trained = source.train_dictionaries()?;
            if trained.is_empty() {
                println!("not enough samples yet — dictionaries need at least 32 objects of a kind");
            }
            for (kind, size) in trained {
                println!("trained {kind}: {}", fmt::bytes(size as u64));
            }
            println!("existing objects keep their old encoding; new ones use the dictionary");
        }
    }
    Ok(())
}

fn show_stats(s: &StoreStats, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&s)?);
        return Ok(());
    }
    println!(
        "objects        {:>12}  ({} inline, {} external)",
        s.objects, s.inline_objects, s.external_objects
    );
    println!("logical size   {:>12}", fmt::bytes(s.bytes_raw));
    println!(
        "stored size    {:>12}  ({:.1}x compression)",
        fmt::bytes(s.bytes_stored),
        s.compression_ratio()
    );
    println!("saved by dedup {:>12}", fmt::bytes(s.dedup_saved_hint));
    println!(
        "on disk        {:>12}  (index {}, objects {})",
        fmt::bytes(s.disk_bytes()),
        fmt::bytes(s.index_bytes),
        fmt::bytes(s.external_bytes)
    );
    println!("sessions       {:>12}", s.sessions);
    println!("events         {:>12}", s.events);
    println!("refs           {:>12}", s.refs);
    if !s.dictionaries.is_empty() {
        let d: Vec<String> = s.dictionaries.iter().map(|(k, v)| format!("{k} {}", fmt::bytes(*v))).collect();
        println!("dictionaries   {}", d.join(", "));
    } else {
        println!("dictionaries   none yet — run `rook store train` once you have some history");
    }
    println!();
    let max = s.per_kind.iter().map(|k| k.bytes_stored).max().unwrap_or(0);
    let rows: Vec<Vec<String>> = s
        .per_kind
        .iter()
        .map(|k| {
            vec![
                k.kind.clone(),
                k.objects.to_string(),
                fmt::bytes(k.bytes_raw),
                fmt::bytes(k.bytes_stored),
                format!("{:.1}x", k.ratio()),
                fmt::bar(k.bytes_stored, max, 20),
            ]
        })
        .collect();
    print!("{}", fmt::table(&["kind", "objects", "logical", "stored", "ratio", ""], &rows));
    Ok(())
}

fn show_objects(objects: &[rook_store::ObjectRow], json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&objects)?);
        return Ok(());
    }
    let rows: Vec<Vec<String>> = objects
        .iter()
        .map(|o| {
            vec![
                o.short.clone(),
                o.kind.clone(),
                fmt::bytes(o.size_raw),
                fmt::bytes(o.size_stored),
                if o.external { "file".into() } else { "inline".into() },
                fmt::timestamp(o.created_at),
            ]
        })
        .collect();
    print!("{}", fmt::table(&["id", "kind", "logical", "stored", "where", "created"], &rows));
    Ok(())
}

fn show_refs(refs: &[rook_store::RefRow], json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&refs)?);
        return Ok(());
    }
    let rows: Vec<Vec<String>> = refs.iter().map(|r| vec![r.name.clone(), r.short.clone()]).collect();
    print!("{}", fmt::table(&["ref", "object"], &rows));
    Ok(())
}

fn parse_kind(s: &str) -> Result<Kind> {
    Ok(match s {
        "message" => Kind::Message,
        "tool-result" | "tool_result" => Kind::ToolResult,
        "file" => Kind::FileBlob,
        "skill" => Kind::Skill,
        "memory" => Kind::Memory,
        "snapshot" => Kind::Snapshot,
        "docs" => Kind::Docs,
        "other" => Kind::Other,
        other => bail!(
            "unknown kind {other:?}; expected one of message, tool-result, file, skill, memory, snapshot, docs, other"
        ),
    })
}

/// The report, and the local detail when there is a local engine to ask.
///
/// Routed through the daemon there is no store here to count sessions in, so
/// the over-budget line says the number and stops rather than guessing at the
/// breakdown — which is the difference between a shorter answer and a wrong one.
fn show_maintenance(
    report: &rook_core::MaintenanceReport,
    dry_run: bool,
    json: bool,
    local: Option<&Rook>,
) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    let tag = if dry_run { "[dry run] " } else { "" };
    println!(
        "{tag}sessions deleted {}, events deleted {}, protected {}",
        report.prune.sessions_deleted, report.prune.events_deleted, report.prune.protected
    );
    println!("{tag}collected {} ({} freed)", report.gc.collected, fmt::bytes(report.gc.bytes_freed));
    if report.undecodable > 0 {
        println!(
            "{tag}removed {} object(s) nothing can decode any more ({} freed)",
            report.undecodable,
            fmt::bytes(report.undecodable_bytes)
        );
    }
    if report.history_dropped > 0 {
        println!(
            "{tag}dropped {} history entr{} past `[storage.retention] max_history_entries`",
            report.history_dropped,
            if report.history_dropped == 1 { "y" } else { "ies" }
        );
    }
    if report.outputs_dropped > 0 {
        println!(
            "{tag}removed {} kept command output(s) past `[sandbox] max_output_files`",
            report.outputs_dropped
        );
    }
    for (kind, samples) in &report.dictionaries_trained {
        println!("trained {kind} dictionary from {samples} objects");
    }
    match (report.over_budget_by, local) {
        (0, Some(rook)) => println!("stored {}", fmt::bytes(rook.content_bytes()?)),
        (0, None) => {}
        (over, local) => {
            let budget = local.map(|r| r.config.storage.retention.max_total_bytes.unwrap_or(0));
            match budget {
                Some(budget) => {
                    println!("still {} over the {} budget", fmt::bytes(over), fmt::bytes(budget))
                }
                None => println!("still {} over the size budget", fmt::bytes(over)),
            }
            let Some(rook) = local else { return Ok(()) };
            let policy = &rook.config.storage.retention;
            let left = rook.store.list_sessions()?;
            let protected =
                left.iter().filter(|s| s.tags.iter().any(|t| policy.protect_tags.contains(t))).count();
            println!(
                "  {} session(s) remain, {protected} protected; the rest is held by refs — the \
                 newest {} entries of each history, which retention keeps",
                left.len(),
                policy.max_history_entries.map(|n| n.to_string()).unwrap_or("all".into())
            );
        }
    }
    Ok(())
}

pub(crate) fn cmd_checkpoint(source: &Source, cmd: CheckpointCmd, json: bool) -> Result<()> {
    match cmd {
        CheckpointCmd::Create { name, path } => {
            let (set, id) = source.checkpoint(&name, path.as_deref())?;
            println!(
                "checkpoint {name}: {} files, {} → {}",
                set.files.len(),
                fmt::bytes(set.total_bytes),
                &id[..12.min(id.len())]
            );
        }
        CheckpointCmd::Ls => {
            let list = source.checkpoints()?;
            if json {
                let items: Vec<_> =
                    list.iter().map(|(n, id)| serde_json::json!({"ref": n, "object": id})).collect();
                println!("{}", serde_json::to_string_pretty(&items)?);
                return Ok(());
            }
            let rows: Vec<Vec<String>> =
                list.iter().map(|(n, id)| vec![n.clone(), id[..12.min(id.len())].to_string()]).collect();
            print!("{}", fmt::table(&["ref", "object"], &rows));
        }
        CheckpointCmd::Restore { object, to } => {
            println!("restored {} file(s) into {}", source.restore_checkpoint(&object, &to)?, to.display());
        }
    }
    Ok(())
}
