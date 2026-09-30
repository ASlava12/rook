//! Bounded, lazy conversation trees over existing session parent links.
use crate::{CoreError, Result, Rook};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub page_entries: usize,
    pub page_bytes: usize,
    pub scan_sessions: usize,
    pub ancestors: usize,
}
impl Default for Settings {
    fn default() -> Self {
        Self { page_entries: 64, page_bytes: 131072, scan_sessions: 256, ancestors: 32 }
    }
}
impl Settings {
    pub(crate) fn errors(&self) -> Vec<String> {
        [
            ("page_entries", self.page_entries, 1, 256),
            ("page_bytes", self.page_bytes, 16384, 1048576),
            ("scan_sessions", self.scan_sessions, 1, 4096),
            ("ancestors", self.ancestors, 1, 64),
        ]
        .into_iter()
        .filter(|(_, value, low, high)| !(low..=high).contains(&value))
        .map(|(name, _, low, high)| format!("branches.{name}: expected {low}..={high}"))
        .collect()
    }
    fn bounded(self) -> Self {
        Self {
            page_entries: self.page_entries.clamp(1, 256),
            page_bytes: self.page_bytes.clamp(16384, 1048576),
            scan_sessions: self.scan_sessions.clamp(1, 4096),
            ancestors: self.ancestors.clamp(1, 64),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub parent: Option<String>,
    pub title: String,
    pub workspace: String,
    pub title_truncated: bool,
    pub workspace_truncated: bool,
    pub forked_at: Option<u64>,
    pub delegated: bool,
    pub next_seq: u64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Query {
    pub after: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page {
    /// Oldest loaded ancestor first; this may start below the actual root.
    pub ancestors: Vec<Node>,
    pub selected: Node,
    pub children: Vec<Node>,
    pub next: Option<String>,
    pub earlier_ancestor: Option<String>,
    pub missing_parent: Option<String>,
    pub scanned_sessions: usize,
}

fn preview(text: &str, limit: usize) -> String {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].into()
}
fn node(rook: &Rook, meta: &rook_store::SessionMeta) -> Result<Node> {
    let title = preview(&meta.title, 256);
    let workspace = preview(&meta.workspace, 512);
    Ok(Node {
        id: rook_store::format_session_id(meta.id),
        parent: meta.parent.map(rook_store::format_session_id),
        title_truncated: title.len() < meta.title.len(),
        workspace_truncated: workspace.len() < meta.workspace.len(),
        title,
        workspace,
        forked_at: rook.forked_at(meta.id)?,
        delegated: meta.tags.iter().any(|t| t == "subtask"),
        next_seq: meta.next_seq,
        updated_at: meta.updated_at,
    })
}
fn size(node: &Node) -> Result<usize> {
    Ok(crate::persistence::encode_with_limit(node, 8192)?.len() + 1)
}

/// Discover only direct children and a bounded ancestry path. Empty child pages
/// can carry a cursor: unrelated sessions spend the scan allowance too.
pub fn page(rook: &Rook, session: u128, query: &Query) -> Result<Page> {
    let after = query
        .after
        .as_ref()
        .map(|id| {
            rook_store::parse_session_id(id)
                .ok_or_else(|| CoreError::Other("invalid branch scan cursor".into()))
        })
        .transpose()?;
    let meta = rook
        .store
        .get_session(session)?
        .ok_or_else(|| CoreError::NoSession(rook_store::format_session_id(session)))?;
    let limits = rook.config.branches.bounded();
    let selected = node(rook, &meta)?;
    let mut remaining = limits.page_bytes.saturating_sub(1024 + size(&selected)?);
    let mut page = Page {
        ancestors: Vec::new(),
        selected,
        children: Vec::new(),
        next: None,
        earlier_ancestor: None,
        missing_parent: None,
        scanned_sessions: 0,
    };
    let mut parent = meta.parent;
    let mut visited = std::collections::BTreeSet::from([session]);
    while let Some(id) = parent {
        if !visited.insert(id) {
            return Err(CoreError::Other("cycle in session parent links; inspect session metadata".into()));
        }
        if page.ancestors.len() == limits.ancestors {
            page.earlier_ancestor = Some(rook_store::format_session_id(id));
            break;
        }
        let Some(meta) = rook.store.get_session(id)? else {
            page.missing_parent = Some(rook_store::format_session_id(id));
            break;
        };
        let ancestor = node(rook, &meta)?;
        let bytes = size(&ancestor)?;
        // Always leave room for at least one maximally escaped child and cursor.
        if bytes + 8192 > remaining {
            page.earlier_ancestor = Some(ancestor.id);
            break;
        }
        remaining -= bytes;
        page.ancestors.push(ancestor);
        parent = meta.parent;
    }
    page.ancestors.reverse();
    let ids = rook.store.session_ids_after(after, limits.scan_sessions + 1)?;
    let mut cursor = after;
    let mut more = ids.len() > limits.scan_sessions;
    for id in ids.iter().take(limits.scan_sessions).copied() {
        if let Some(meta) = rook.store.get_session(id)?
            && meta.parent == Some(session)
        {
            let child = node(rook, &meta)?;
            let bytes = size(&child)?;
            if page.children.len() == limits.page_entries || bytes > remaining {
                more = true;
                break;
            }
            remaining -= bytes;
            page.children.push(child);
        }
        page.scanned_sessions += 1;
        cursor = Some(id);
    }
    if more {
        page.next = cursor.map(rook_store::format_session_id);
    }
    Ok(page)
}

pub fn label(node: &Node) -> String {
    format!(
        "{}{}{}{}",
        if node.title.is_empty() { "(untitled)" } else { &node.title },
        if node.title_truncated { "…" } else { "" },
        if node.delegated { " · delegated task" } else { "" },
        node.forked_at
            .map(|seq| if node.delegated {
                format!(" · delegated at #{seq}")
            } else {
                format!(" · fork before #{seq}")
            })
            .unwrap_or_else(|| if node.parent.is_some() && !node.delegated {
                " · boundary unknown".into()
            } else {
                String::new()
            })
    )
}

pub fn describe(page: &Page) -> String {
    let mut text =
        String::from("Conversation branches — browsing and switching leave workspace files as they are.\n");
    if let Some(id) = &page.earlier_ancestor {
        text.push_str(&format!("Earlier ancestors: {id}\n"));
    }
    if let Some(id) = &page.missing_parent {
        text.push_str(&format!("Parent no longer available: {id}\n"));
    }
    for (depth, node) in page.ancestors.iter().chain(std::iter::once(&page.selected)).enumerate() {
        text.push_str(&format!(
            "{}{} {} {}\n",
            "  ".repeat(depth),
            if node.id == page.selected.id { ">" } else { "└" },
            node.id,
            label(node)
        ));
    }
    for node in &page.children {
        text.push_str(&format!("{}└ {} {}\n", "  ".repeat(page.ancestors.len() + 1), node.id, label(node)));
    }
    if let Some(next) = &page.next {
        text.push_str(&format!("Scan more children: --after {next}\n"));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    fn engine(path: &std::path::Path) -> Rook {
        Rook::from_parts(
            rook_store::Store::open(path.join("store")).unwrap(),
            crate::Config::default(),
            rook_skills::Environment::bare("linux", "x86_64", "0.10.0"),
            rook_skills::SkillIndex::default(),
            path.into(),
        )
    }
    fn session(rook: &Rook, id: u128, parent: Option<u128>, title: &str) {
        let mut meta = rook_store::SessionMeta::new(id, title, rook.workspace.display().to_string(), 1);
        meta.parent = parent;
        rook.store.create_session(&meta).unwrap();
    }

    #[test]
    fn sparse_child_scans_advance_under_bounds_even_when_the_cursor_session_is_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let mut rook = engine(dir.path());
        rook.config.branches.scan_sessions = 2;
        rook.config.branches.page_entries = 1;
        for id in 1..=12 {
            session(&rook, id, ([5, 9, 12].contains(&id)).then_some(1), &format!("session {id}"));
        }
        let first = page(&rook, 1, &Default::default()).unwrap();
        assert_eq!(first.scanned_sessions, 2);
        assert!(first.children.is_empty());
        assert_eq!(first.next.as_deref(), Some(rook_store::format_session_id(2).as_str()));
        rook.delete_session(2).unwrap();
        let mut query = Query { after: first.next };
        let mut found = Vec::new();
        for n in 0..12 {
            let result = page(&rook, 1, &query).unwrap();
            assert!(result.scanned_sessions <= 2);
            assert!(result.children.len() <= 1);
            found.extend(result.children.iter().map(|node| node.id.clone()));
            if result.next.is_none() {
                break;
            }
            assert!(result.next > query.after);
            query.after = result.next;
            assert!(n < 11);
        }
        assert_eq!(found, [5, 9, 12].map(rook_store::format_session_id));
        assert!(page(&rook, 1, &Query { after: Some("not-a-session".into()) }).is_err());
        assert!(page(&rook, 999, &Default::default()).is_err());
    }

    #[test]
    fn ancestry_reports_its_bound_missing_parents_and_cycles_without_inventing_a_root() {
        let dir = tempfile::tempdir().unwrap();
        let mut rook = engine(dir.path());
        for id in 1..=7 {
            session(&rook, id, (id > 1).then(|| id - 1), "branch");
        }
        rook.config.branches.ancestors = 2;
        let result = page(&rook, 7, &Default::default()).unwrap();
        assert_eq!(result.ancestors.len(), 2);
        assert_eq!(result.ancestors[0].id, rook_store::format_session_id(5));
        assert_eq!(result.earlier_ancestor, Some(rook_store::format_session_id(4)));
        let older = page(&rook, 4, &Default::default()).unwrap();
        assert_eq!(older.earlier_ancestor, Some(rook_store::format_session_id(1)));
        rook.delete_session(6).unwrap();
        assert_eq!(
            page(&rook, 7, &Default::default()).unwrap().missing_parent,
            Some(rook_store::format_session_id(6))
        );
        rook.store.update_session(4, |meta| meta.parent = Some(5)).unwrap();
        assert!(page(&rook, 5, &Default::default()).unwrap_err().to_string().contains("cycle"));
    }

    #[test]
    fn escaped_metadata_reaches_the_byte_bound_and_forks_keep_their_original_event_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let mut rook = engine(dir.path());
        rook.config.branches.page_bytes = 16384;
        session(&rook, 1, None, "root");
        for id in 2..=12 {
            session(&rook, id, Some(1), &"\u{1}".repeat(300));
            rook.store.update_session(id, |meta| meta.workspace = "\u{2}".repeat(600)).unwrap();
        }
        let result = page(&rook, 1, &Default::default()).unwrap();
        assert!(result.children.len() < 11, "escaped metadata must reach the byte cap");
        assert!(!result.children.is_empty());
        assert!(result.next.is_some());
        assert!(result.children.iter().all(|n| n.title_truncated && n.workspace_truncated));
        assert!(serde_json::to_vec(&result).unwrap().len() <= 16384);
        rook.log(1, rook_store::EventKind::UserMessage, "", "first").unwrap();
        rook.log(1, rook_store::EventKind::AssistantMessage, "", "second").unwrap();
        std::fs::write(dir.path().join("untouched"), "workspace stays here").unwrap();
        let child = rook.fork_session(1, 1).unwrap();
        let result = page(&rook, child.id, &Default::default()).unwrap();
        assert_eq!(result.selected.forked_at, Some(1));
        assert_eq!(result.selected.next_seq, 1);
        assert_eq!(result.ancestors[0].id, rook_store::format_session_id(1));
        assert_eq!(std::fs::read_to_string(dir.path().join("untouched")).unwrap(), "workspace stays here");
        assert_eq!(rook.store.get_session(1).unwrap().unwrap().next_seq, 2);
        let all = rook.fork_session(1, u64::MAX).unwrap();
        assert_eq!(rook.forked_at(all.id).unwrap(), Some(2));
        rook.log(1, rook_store::EventKind::UserMessage, "", "later parent event").unwrap();
        let all_page = page(&rook, all.id, &Default::default()).unwrap();
        assert_eq!(all_page.selected.forked_at, Some(2));
        assert_eq!(all_page.selected.next_seq, 2, "later parent writes stay outside the fixed fork");
        let task = rook.fork_for_subtask(1, "delegated").unwrap();
        assert!(page(&rook, task, &Default::default()).unwrap().selected.delegated);
    }

    #[test]
    fn branch_configuration_rejects_unbounded_scans_and_undersized_pages() {
        let limits = Settings { page_entries: 0, page_bytes: 100, scan_sessions: usize::MAX, ancestors: 0 };
        let errors = limits.errors();
        assert_eq!(errors.len(), 4);
    }
}
