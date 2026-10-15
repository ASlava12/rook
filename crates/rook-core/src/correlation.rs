//! Session ancestry supplies one local trace across root, delegates and bridges.
use rook_llm::TraceContext;
use rook_store::Store;

impl crate::Rook {
    pub fn session_trace(&self, id: u128) -> crate::Result<Option<TraceContext>> {
        self.store
            .get_session(id)?
            .ok_or_else(|| crate::CoreError::NoSession(rook_store::format_session_id(id)))?;
        Ok(session(&self.store, id))
    }
}

fn span(identity: &str) -> Option<String> {
    rook_store::ObjectId::of(identity.as_bytes()).to_hex().get(..16).map(str::to_owned)
}

pub(crate) fn session(store: &Store, session: u128) -> Option<TraceContext> {
    let mut root = session;
    let mut seen = std::collections::BTreeSet::new();
    let mut settled = false;
    for _ in 0..64 {
        if !seen.insert(root) {
            return None;
        }
        match store.get_session(root).ok()??.parent {
            Some(parent) => root = parent,
            None => {
                settled = true;
                break;
            }
        }
    }
    if !settled {
        return None;
    }
    let trace_id = if root == 0 {
        rook_store::ObjectId::of(b"rook:root:0").to_hex().get(..32)?.to_owned()
    } else {
        format!("{root:032x}")
    };
    let span_id = span(&format!("session:{session:032x}"))?;
    let parent =
        store.get_session(session).ok()??.parent.and_then(|parent| span(&format!("session:{parent:032x}")));
    TraceContext::bounded(&trace_id, &span_id, parent.as_deref())
}

pub(crate) fn operation(store: &Store, owner: u128, id: &str) -> Option<TraceContext> {
    let parent = session(store, owner)?;
    let span = span(&format!("operation:{owner:032x}:{id}"))?;
    TraceContext::bounded(&parent.trace_id, &span, Some(&parent.span_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ancestry_has_a_reached_bound_and_does_not_invent_a_root_for_corrupt_links() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        for id in 1..=65 {
            let mut meta = rook_store::SessionMeta::new(id, "node", "workspace", 1);
            meta.parent = (id > 1).then_some(id - 1);
            store.create_session(&meta).unwrap();
        }
        assert!(session(&store, 64).is_some(), "exactly 64 visited entries can settle a root");
        assert!(session(&store, 65).is_none(), "65 actual entries reach the ancestry bound");
        store.update_session(1, |meta| meta.parent = Some(2)).unwrap();
        assert!(session(&store, 2).is_none(), "a cycle has no root identity");
        store.update_session(1, |meta| meta.parent = Some(99)).unwrap();
        assert!(session(&store, 2).is_none(), "a missing parent is not a new root");
    }
}
