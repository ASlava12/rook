//! A session listing keeps descendants beside their conversation, even when
//! only a child has recent activity. Stored parent links are never rewritten.

use std::collections::HashMap;

use serde::Serialize;

use crate::SessionSummary;

const DISPLAY_DEPTH: usize = 8;

#[derive(Clone, Debug, Serialize)]
pub struct Row {
    #[serde(skip)]
    pub index: usize,
    pub depth: usize,
    pub prefix: String,
    pub continuation: String,
}

/// Parent-first rows, with families ordered by their most recent activity.
/// Sibling order follows the input listing. Missing parents become roots;
/// cycles are shown once without recursive traversal or unbounded indentation.
pub fn rows(sessions: &[SessionSummary]) -> Vec<Row> {
    let by_id: HashMap<_, _> = sessions.iter().enumerate().map(|(i, s)| (s.meta.id, i)).collect();
    let mut children = vec![Vec::new(); sessions.len()];
    let mut roots = Vec::new();
    for (i, session) in sessions.iter().enumerate() {
        match session.meta.parent.and_then(|id| by_id.get(&id).copied()).filter(|&parent| parent != i) {
            Some(parent) => children[parent].push(i),
            None => roots.push(i),
        }
    }
    let mut seen = vec![false; sessions.len()];
    let mut families = Vec::new();
    // The second pass also admits a family whose saved links contain a cycle.
    for root in roots.into_iter().chain(0..sessions.len()) {
        if seen[root] {
            continue;
        }
        let mut family = Vec::new();
        let mut newest = sessions[root].meta.updated_at;
        let mut pending = vec![(root, 0, true)];
        let mut path = Vec::new();
        while let Some((index, depth, last)) = pending.pop() {
            if seen[index] {
                continue;
            }
            seen[index] = true;
            newest = newest.max(sessions[index].meta.updated_at);
            path.truncate(depth);
            let mut prefix = indentation(&path);
            if depth > 0 {
                prefix.push_str(if last { "└─ " } else { "├─ " });
            }
            path.push(!last);
            let continuation = indentation(&path);
            family.push(Row { index, depth, prefix, continuation });
            let unvisited: Vec<_> = children[index].iter().copied().filter(|&i| !seen[i]).collect();
            for (at, child) in unvisited.iter().enumerate().rev() {
                pending.push((*child, depth + 1, at + 1 == unvisited.len()));
            }
        }
        families.push((newest, root, family));
    }
    families.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    families.into_iter().flat_map(|(_, _, family)| family).collect()
}

fn indentation(path: &[bool]) -> String {
    let mut text = String::new();
    let start = path.len().saturating_sub(DISPLAY_DEPTH - 1).max(1);
    if start > 1 {
        text.push_str("… ");
    }
    for &continued in path.iter().skip(start) {
        text.push_str(if continued { "│  " } else { "   " });
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: u128, parent: Option<u128>, updated: i64) -> SessionSummary {
        let mut meta = rook_store::SessionMeta::new(id, "a conversation", "workspace", updated);
        meta.parent = parent;
        SessionSummary { meta, goal: None, forked_at: None }
    }

    #[test]
    fn recent_descendants_raise_the_whole_family_and_stay_under_their_parent() {
        let sessions = vec![
            session(4, Some(2), 100),
            session(5, None, 90),
            session(3, Some(1), 80),
            session(2, Some(1), 70),
            session(1, None, 1),
        ];
        let rows = rows(&sessions);
        assert_eq!(rows.iter().map(|r| sessions[r.index].meta.id).collect::<Vec<_>>(), [1, 3, 2, 4, 5]);
        assert_eq!(
            rows.iter().map(|r| r.prefix.as_str()).collect::<Vec<_>>(),
            ["", "├─ ", "└─ ", "   └─ ", ""]
        );
        assert_eq!(rows[1].continuation, "│  ");
        assert_eq!(rows[3].depth, 2);
    }

    #[test]
    fn missing_parents_self_links_and_cycles_remain_visible_once() {
        let sessions = vec![
            session(1, Some(99), 100),
            session(2, Some(2), 90),
            session(3, Some(4), 80),
            session(4, Some(3), 70),
            session(5, Some(4), 60),
        ];
        let listed = rows(&sessions);
        assert_eq!(listed.len(), sessions.len());
        assert_eq!(listed.iter().map(|r| sessions[r.index].meta.id).collect::<Vec<_>>(), [1, 2, 3, 4, 5]);
        assert_eq!(listed[0].depth, 0);
        assert_eq!(listed[1].depth, 0);
        assert_eq!(sessions[0].meta.parent, Some(99), "listing must retain the stored relationship");
    }

    #[test]
    fn deep_trees_keep_every_session_without_recursive_calls_or_growing_row_width() {
        let sessions: Vec<_> =
            (1..=2_000).map(|id| session(id, (id > 1).then_some(id - 1), id as i64)).collect();
        let listed = rows(&sessions);
        assert_eq!(listed.len(), 2_000);
        assert_eq!(listed.last().unwrap().depth, 1_999);
        assert!(listed.last().unwrap().prefix.starts_with("… "));
        assert!(listed.iter().all(|r| r.prefix.chars().count() <= DISPLAY_DEPTH * 3 + 2));
        assert!(rows(&[]).is_empty());
    }
}
