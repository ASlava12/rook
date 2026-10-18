//! Metadata-only, lazy family view. Disk markers never prove a live owner.
use crate::{Result, Rook, branches};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub session: String,
    pub activity: Option<rook_proto::activity::Snapshot>,
    /// Saved scheduling state, independent of whether an executor owns it now.
    pub managed_status: Option<rook_proto::work::Status>,
    /// None for an offline reader; false only after consulting current owners.
    pub owner_live: Option<bool>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page {
    pub tree: branches::Page,
    pub entries: Vec<Entry>,
    pub jobs: Vec<rook_tools::jobs::Metadata>,
    pub omitted_jobs: usize,
    pub unassigned_jobs: usize,
    pub jobs_observed: bool,
    pub observed_at: i64,
}

pub fn page(
    rook: &Rook,
    session: u128,
    query: &branches::Query,
    jobs: Option<&rook_tools::jobs::Jobs>,
) -> Result<Page> {
    let tree = branches::page(rook, session, query)?;
    let mut entries = Vec::new();
    let mut ids = BTreeSet::new();
    for node in tree.ancestors.iter().chain(std::iter::once(&tree.selected)).chain(&tree.children) {
        let Some(id) = rook_store::parse_session_id(&node.id) else { continue };
        ids.insert(id);
        entries.push(Entry {
            session: node.id.clone(),
            activity: crate::activity::load(&rook.store, id)?,
            managed_status: crate::work::managed::for_session(rook, id)?.map(|run| run.status),
            owner_live: None,
        });
    }
    // Fixed metadata bound in addition to the configured lazy branch page.
    let overview = jobs.map(|jobs| jobs.overview(&ids, 256));
    Ok(Page {
        tree,
        entries,
        jobs: overview.as_ref().map(|v| v.jobs.clone()).unwrap_or_default(),
        omitted_jobs: overview.as_ref().map_or(0, |v| v.omitted),
        unassigned_jobs: overview.as_ref().map_or(0, |v| v.unassigned),
        jobs_observed: jobs.is_some(),
        observed_at: rook_store::now_unix(),
    })
}

pub fn describe(page: &Page) -> String {
    let mut text = String::from("Background work · lazy session family\n");
    for (index, node) in page
        .tree
        .ancestors
        .iter()
        .chain(std::iter::once(&page.tree.selected))
        .chain(&page.tree.children)
        .enumerate()
    {
        let depth = index.min(page.tree.ancestors.len() + 1);
        text.push_str(&format!("{}{} {}\n", "  ".repeat(depth), node.id, branches::label(node)));
        if let Some(entry) = page.entries.iter().find(|e| e.session == node.id) {
            let owner = match entry.owner_live {
                Some(true) => "live executor",
                Some(false) => "no observed executor",
                None => "executor unverified (saved state)",
            };
            text.push_str(&format!("{}  {owner}", "  ".repeat(depth)));
            if let Some(activity) = &entry.activity {
                text.push_str(&format!(
                    " · {:?} · idle {}s{}",
                    activity.stage,
                    activity.idle_secs,
                    if activity.stalled { " · stalled" } else { "" }
                ));
            }
            if let Some(status) = &entry.managed_status {
                text.push_str(&format!(" · saved goal {status:?}"));
            }
            text.push('\n');
        }
        for job in page.jobs.iter().filter(|job| job.session == node.id) {
            text.push_str(&format!(
                "{}  {} · {}{}\n",
                "  ".repeat(depth),
                job.id,
                job.exit_code.map_or_else(
                    || "running (owned registry)".into(),
                    |code| format!("driver exited {code}")
                ),
                if job.exit_code.is_some() && job.processes_stopped == Some(false) {
                    " · process cleanup unverified"
                } else {
                    ""
                }
            ));
        }
    }
    if let Some(next) = &page.tree.next {
        text.push_str(&format!("More children: --after {next}\n"));
    }
    if let Some(id) = &page.tree.earlier_ancestor {
        text.push_str(&format!("Earlier ancestor: {id}\n"));
    }
    if let Some(id) = &page.tree.missing_parent {
        text.push_str(&format!("Missing parent: {id}\n"));
    }
    text.push_str(&format!(
        "Jobs: registry {} · {} omitted · {} unassigned; explore a child ID to read deeper descendants.\n",
        if page.jobs_observed { "observed" } else { "unavailable" },
        page.omitted_jobs,
        page.unassigned_jobs
    ));
    text
}
