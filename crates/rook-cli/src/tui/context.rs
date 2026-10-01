//! A bounded view of the current context estimate and a saved request attempt.

use std::fmt::Write;

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::widgets::{Paragraph, Wrap};
use rook_core::ContextUsage;

use crate::source::Source;

#[derive(Default)]
pub(super) struct ContextPane {
    session: Option<u128>,
    window: Option<usize>,
    text: String,
    scroll: u16,
}

impl ContextPane {
    pub(super) fn open(&mut self, source: &Source, session: Option<u128>, window: Option<usize>) {
        self.session = session;
        self.window = window;
        self.scroll = 0;
        self.text = match session {
            Some(session) => match source.context_usage(session, window, source.workspace()) {
                Ok(usage) => describe(&usage),
                Err(error) => format!("Could not read session context: {error}"),
            },
            None => "Start or resume a conversation to inspect its context.".into(),
        };
    }

    pub(super) fn key(&mut self, key: KeyEvent, source: &Source) -> bool {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return true,
            KeyCode::Char('r') => self.open(source, self.session, self.window),
            KeyCode::Down | KeyCode::Char('j') => self.scroll = self.scroll.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll = self.scroll.saturating_add(12),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(12),
            KeyCode::Home => self.scroll = 0,
            _ => {}
        }
        false
    }

    pub(super) fn draw(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(
            Paragraph::new(self.text.as_str())
                .block(super::bordered(" context · r refreshes the recorded request "))
                .wrap(Wrap { trim: false })
                .scroll((self.scroll, 0)),
            area,
        );
    }
}

fn describe(usage: &ContextUsage) -> String {
    let mut text = String::new();
    let pct = usage.live_tokens as f64 / usage.usable.max(1) as f64 * 100.0;
    let _ = writeln!(
        text,
        "Current live estimate: ~{} of {} usable tokens ({pct:.0}%)",
        usage.live_tokens, usage.usable
    );
    let _ = writeln!(
        text,
        "Window {} · compact at {} · {} compactions",
        usage.window, usage.compact_at, usage.compactions
    );
    if usage.needs_compaction {
        let _ = writeln!(text, "Next turn will compact this context.");
    }
    let _ = writeln!(text, "Logged over session: ~{} tokens", usage.logged_tokens);
    if usage.replay_from > 0 {
        let _ = writeln!(text, "Live replay starts at event #{} after the last summary", usage.replay_from);
    }
    for (kind, row) in &usage.by_kind {
        let _ = writeln!(text, "  {kind:<16} {:>4} events · ~{} tokens", row.events, row.tokens);
    }
    let Some(saved) = &usage.last_request else {
        text.push_str("\nNo recorded request attempt in this session.\n");
        return text;
    };
    let request = &saved.catalog;
    let _ = writeln!(text, "\nLast request attempt · event #{} · historical snapshot", saved.event_seq);
    let _ = writeln!(
        text,
        "Provider {} · {} tools · {} schemas · ~{} tokens used",
        request.provider_id, request.delivery, request.detail, request.used_tokens
    );
    let _ = writeln!(text, "Offered tools ({} total):", request.tool_count);
    for tool in &request.tools {
        let _ = writeln!(text, "  {} · ~{} schema tokens", tool.name, tool.estimated_tokens);
    }
    if request.omitted_tools > 0 {
        let _ = writeln!(text, "  {} offered names omitted from this view", request.omitted_tools);
    }
    if request.mcp.discovered > 0 {
        let _ = writeln!(
            text,
            "MCP catalog: {} discovered · {} directly offered · {} deferred via mcp_tools / mcp_call",
            request.mcp.discovered, request.mcp.advertised, request.mcp.deferred
        );
        for name in &request.mcp.deferred_names {
            let _ = writeln!(text, "  {name}");
        }
        if request.mcp.omitted_deferred > 0 {
            let _ =
                writeln!(text, "  {} deferred names omitted from this view", request.mcp.omitted_deferred);
        }
    }
    let sources = &request.sources;
    let _ = writeln!(
        text,
        "Sources: {} skills discovered · {} applicable · {} advertised · {} loaded events",
        sources.discovered_skills,
        sources.applicable_skills,
        sources.advertised_skills,
        sources.loaded_skill_events
    );
    for source in &sources.sources {
        let state = if source.complete == Some(false) { " · partial" } else { "" };
        let _ = writeln!(
            text,
            "  {} · {} · {} · {} · ~{} tokens{state}",
            source.kind, source.name, source.inclusion, source.origin, source.estimated_tokens
        );
    }
    if sources.omitted_sources > 0 {
        let _ = writeln!(text, "  {} sources omitted from this view", sources.omitted_sources);
    }
    text.push_str("Recorded sources and tool names may differ from the current workspace or server.\n");
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_reads_a_saved_request_with_deferred_tools_from_the_local_source() {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = rook_store::Store::open(home.path().join("store")).unwrap();
        let rook = rook_core::Rook::from_parts(
            store,
            rook_core::Config::default(),
            rook_skills::Environment::bare("linux", "x86_64", "0.1.0"),
            rook_skills::SkillIndex::default(),
            workspace.path().to_path_buf(),
        );
        let session = rook.start_session("context panel").unwrap();
        let note = serde_json::json!({
            "provider_id":"scripted/test", "delivery":"native", "detail":"stub",
            "used_tokens":210, "tool_count":1, "omitted_tools":0,
            "tools":[{"name":"mcp_tools","estimated_tokens":20}],
            "mcp":{"discovered":1,"advertised":0,"deferred":1,
                   "deferred_names":["camera__shot"],"omitted_deferred":0},
            "sources":{"discovered_skills":1,"applicable_skills":1,"advertised_skills":1,
                       "loaded_skill_events":1,"sources":[{"kind":"skill","name":"camera",
                       "origin":"project/SKILL.md","inclusion":"loaded","estimated_tokens":40,
                       "complete":true}],"omitted_sources":0}
        });
        rook.log(
            session,
            rook_store::EventKind::Note,
            rook_core::context::REQUEST_CATALOG_LABEL,
            &note.to_string(),
        )
        .unwrap();
        let rook = std::sync::Arc::new(rook);
        let source = Source::Local(rook.clone());
        let mut pane = ContextPane::default();
        pane.open(&source, Some(session), None);
        assert!(pane.text.contains("Last request attempt · event #0 · historical snapshot"));
        assert!(pane.text.contains("camera__shot"));
        assert!(pane.text.contains("1 deferred via mcp_tools / mcp_call"));
        assert!(pane.text.contains("skill · camera · loaded · project/SKILL.md"));
        assert!(pane.text.contains("may differ from the current workspace"));

        // Refresh follows the newest saved attempt, including an older note
        // format that predates MCP provenance.
        let older_format = serde_json::json!({
            "provider_id":"scripted/next", "delivery":"prompt", "detail":"full",
            "used_tokens":300, "tool_count":0, "omitted_tools":0, "tools":[]
        });
        rook.log(
            session,
            rook_store::EventKind::Note,
            rook_core::context::REQUEST_CATALOG_LABEL,
            &older_format.to_string(),
        )
        .unwrap();
        pane.key(KeyEvent::new(KeyCode::Char('r'), ratatui::crossterm::event::KeyModifiers::NONE), &source);
        assert!(pane.text.contains("Last request attempt · event #1 · historical snapshot"));
        assert!(pane.text.contains("scripted/next"));
        assert!(!pane.text.contains("camera__shot"));
    }
}
