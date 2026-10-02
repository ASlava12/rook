//! Coalesced invalidation carries no producer data. Each observer folds its own
//! fixed saved prefix, retaining the same bounds/ownership as Context.
use super::*;

#[derive(Default)]
pub(crate) struct Cursor {
    next: u64,
    loaded: bool,
    state: State,
}

impl Cursor {
    pub(crate) fn advance(&mut self, rook: &crate::Rook, session: u128) -> crate::Result<bool> {
        let through = rook.store.get_session(session)?.map_or(0, |m| m.next_seq);
        let mut changed = !self.loaded;
        self.loaded = true;
        if through < self.next {
            self.next = 0;
            self.state = State::default();
            changed = true;
        }
        while self.next < through {
            let events = rook.store.events(session, self.next, 256)?;
            let Some(last) = events.last() else {
                self.next = through;
                break;
            };
            self.next = last.seq.saturating_add(1).min(through);
            for event in events.into_iter().take_while(|e| e.seq < through) {
                if event.record.kind == rook_store::EventKind::Note && event.record.label == LABEL {
                    changed = true;
                    let read = rook
                        .store
                        .stat_object(&event.record.body)
                        .map_err(crate::error::CoreError::from)
                        .and_then(|meta| {
                            self.state.include(
                                &rook.store,
                                &event,
                                meta.map_or(0, |m| m.size_raw),
                                &rook.config.extension_ui,
                            )
                        });
                    if read.is_err() {
                        self.state.invalid_records = self.state.invalid_records.saturating_add(1);
                    }
                }
            }
        }
        Ok(changed)
    }

    pub(crate) fn event(&self, rook: &crate::Rook, session: u128) -> crate::Result<rook_proto::ChatEvent> {
        // The structured state and legacy text share a frame. Leave room for
        // escaping, the envelope and the socket's own admission accounting.
        let limit = rook
            .config
            .extension_ui
            .max_state_bytes
            .min(
                rook.config
                    .server
                    .chat_queue_bytes
                    .min(rook.config.server.chat_replay_bytes)
                    .saturating_sub(1024)
                    / 4,
            )
            .max(512);
        let state = self.state.for_display(limit)?;
        let bytes = encoded(&state, limit)?;
        let value = serde_json::from_slice(&bytes)?;
        Ok(rook_proto::ChatEvent::Agent {
            text: state.describe(),
            receipt: None,
            admission: None,
            extension_ui: Some(rook_proto::ExtensionUi {
                session: rook_store::format_session_id(session),
                through: self.next,
                state: value,
            }),
        })
    }
}

impl State {
    fn for_display(&self, limit: usize) -> Result<Self, serde_json::Error> {
        #[derive(Serialize)]
        struct Borrowed<'a> {
            reports: Vec<&'a Report>,
            omitted_updates: usize,
            invalid_records: usize,
        }
        let mut borrowed = Borrowed {
            reports: Vec::new(),
            omitted_updates: self.omitted_updates,
            invalid_records: self.invalid_records,
        };
        // Admission uses references, before any additional report/string copy.
        for report in &self.reports {
            borrowed.reports.push(report);
            if encoded(&borrowed, limit.saturating_sub(128)).is_err() {
                borrowed.reports.pop();
                borrowed.omitted_updates = borrowed.omitted_updates.saturating_add(1);
            }
        }
        let bytes = encoded(&borrowed, limit)?;
        serde_json::from_slice(&bytes)
    }

    /// Admit a display payload before decoding/copying its reports. Receivers
    /// use the contract's hard maxima, independent of another machine's config.
    pub fn from_display(value: &serde_json::Value) -> std::io::Result<Self> {
        let reports = value.get("reports").and_then(serde_json::Value::as_array);
        if reports.is_some_and(|r| r.len() > 128) {
            return Err(std::io::Error::other("extension display entry limit"));
        }
        let bytes = encoded(value, 1048576).map_err(std::io::Error::other)?;
        let state: Self = serde_json::from_slice(&bytes)?;
        if !state.reports.iter().all(|r| r.source.valid() && r.item.valid()) {
            return Err(std::io::Error::other("invalid extension display"));
        }
        Ok(state)
    }
}

impl crate::Rook {
    /// Coalesced committed display changes, shared across workspace views.
    pub fn extension_ui_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.extension_changed.subscribe()
    }
    /// Current source-owned reports restored from this conversation's saved
    /// prefix. This does not re-execute hooks or verify files/tests.
    pub fn extension_ui_snapshot(&self, session: u128) -> crate::Result<rook_proto::ChatEvent> {
        let mut cursor = Cursor::default();
        cursor.advance(self, session)?;
        cursor.event(self, session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, crate::Rook) {
        let dir = tempfile::tempdir().unwrap();
        let rook = crate::Rook::from_parts(
            rook_store::Store::open(dir.path().join("store")).unwrap(),
            crate::Config::default(),
            rook_skills::Environment::bare("test", "test", "0.10.0"),
            rook_skills::SkillIndex::default(),
            dir.path().to_path_buf(),
        );
        (dir, rook)
    }
    fn update(rook: &crate::Rook, session: u128, ordinal: usize, raw: &str) {
        let source = Source::hook(
            &crate::hooks::HookConfig { command: "fixture".into(), ..Default::default() },
            ordinal,
        );
        Batch::parse(raw, source, &rook.config.extension_ui)
            .unwrap()
            .record(rook, session, str::to_string)
            .unwrap();
    }
    fn state(event: rook_proto::ChatEvent) -> State {
        let rook_proto::ChatEvent::Agent { extension_ui: Some(ui), .. } = event else {
            panic!("missing display")
        };
        State::from_display(&ui.state).unwrap()
    }
    #[tokio::test]
    async fn coalesced_changes_restore_only_the_observed_session_and_clear_only_their_source() {
        let (_dir, rook) = fixture();
        let session = rook.start_session("observed").unwrap();
        let other = rook.start_session("other").unwrap();
        let mut cursor = Cursor::default();
        assert!(cursor.advance(&rook, session).unwrap());
        let mut changes = rook.extension_ui_changes();
        let scoped = rook.for_workspace(rook.workspace.join("other-project"));
        update(&scoped, session, 0, r#"[{"kind":"status","id":"build","text":"first"}]"#);
        update(&scoped, session, 1, r#"[{"kind":"status","id":"build","text":"other source"}]"#);
        changes.changed().await.unwrap();
        assert!(cursor.advance(&rook, session).unwrap());
        assert_eq!(state(cursor.event(&rook, session).unwrap()).reports.len(), 2);
        update(&rook, session, 0, r#"[{"kind":"clear","id":"build"}]"#);
        changes.changed().await.unwrap();
        assert!(cursor.advance(&rook, session).unwrap());
        let restored = state(rook.extension_ui_snapshot(session).unwrap());
        assert_eq!(restored.reports.len(), 1);
        assert_eq!(restored.reports[0].source.ordinal, 1);
        update(&rook, other, 0, r#"[{"kind":"status","id":"build","text":"foreign"}]"#);
        changes.changed().await.unwrap();
        assert!(!cursor.advance(&rook, session).unwrap(), "another session cannot alter this display");
    }
    #[test]
    fn a_small_socket_budget_reports_omissions_instead_of_retaining_an_oversized_frame() {
        let (_dir, mut rook) = fixture();
        rook.config.server.chat_queue_bytes = 65536;
        rook.config.server.chat_replay_bytes = 4096;
        let session = rook.start_session("bounded").unwrap();
        for ordinal in 0..3 {
            let raw =
                serde_json::json!([{"kind":"result","id":"result","title":"Build","body":"x".repeat(2048)}]);
            update(&rook, session, ordinal, &raw.to_string());
        }
        let mut cursor = Cursor::default();
        cursor.advance(&rook, session).unwrap();
        assert_eq!(cursor.state.reports.len(), 3, "saved state really exceeds the socket display budget");
        assert!(encoded(&cursor.state, 4096).is_err());
        let event = cursor.event(&rook, session).unwrap();
        assert!(encoded(&event, 4096).is_ok(), "both structured and legacy text share the admitted frame");
        let display = state(event);
        assert!(display.reports.is_empty());
        assert_eq!(display.omitted_updates, 3);
    }
    #[test]
    fn display_input_and_saved_records_refuse_excess_entries_and_invalid_fields() {
        let (_dir, mut rook) = fixture();
        rook.config.extension_ui.max_entries = 1;
        let session = rook.start_session("invalid").unwrap();
        let source = Source::hook(&crate::hooks::HookConfig::default(), 0);
        let item = serde_json::json!({"kind":"status","id":"x","text":"x"});
        let raw = serde_json::json!({"source":source,"items":[item,item]}).to_string();
        rook.log(session, rook_store::EventKind::Note, LABEL, &raw).unwrap();
        let result = state(rook.extension_ui_snapshot(session).unwrap());
        assert!(result.reports.is_empty());
        assert_eq!(result.invalid_records, 1);
        assert!(
            State::from_display(&serde_json::json!({"reports":vec![serde_json::Value::Null;129]})).is_err()
        );
        let bad = serde_json::json!({"reports":[{"source":source,"event_seq":1,"item":{"kind":"progress","id":"x","label":"x","done":2,"total":1}}]});
        assert!(State::from_display(&bad).is_err());
        assert!(State::from_display(&serde_json::json!({"extra":"x".repeat(1048577)})).is_err());
    }
}
