//! A terminal browser for everything the agent has stored.
//!
//! Read-only by design. The point is to make the agent's memory legible — which
//! sessions exist, what a turn actually cost, which skills apply here and why —
//! without needing a database client or a browser.

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph, Tabs, Wrap};
use ratatui::{DefaultTerminal, Frame};

use rook_core::{Rook, TranscriptEntry};
use rook_skills::SkillCard;
use rook_store::{SessionMeta, StoreStats};

use crate::fmt;

const TABS: [&str; 4] = ["Sessions", "Skills", "Store", "Help"];

pub fn run(rook: Rook) -> Result<()> {
    let mut terminal = ratatui::init();
    let result = App::new(rook).run(&mut terminal);
    ratatui::restore();
    result
}

struct App {
    rook: Rook,
    tab: usize,
    sessions: Vec<SessionMeta>,
    session_state: ListState,
    transcript: Vec<TranscriptEntry>,
    transcript_scroll: u16,
    skills: Vec<SkillCard>,
    skill_state: ListState,
    objects: Vec<(String, String, u64, u64)>,
    stats: Option<StoreStats>,
    status: String,
    quit: bool,
}

impl App {
    fn new(rook: Rook) -> Self {
        let mut app = Self {
            rook,
            tab: 0,
            sessions: Vec::new(),
            session_state: ListState::default(),
            transcript: Vec::new(),
            transcript_scroll: 0,
            skills: Vec::new(),
            skill_state: ListState::default(),
            objects: Vec::new(),
            stats: None,
            status: String::new(),
            quit: false,
        };
        app.reload();
        app
    }

    fn reload(&mut self) {
        self.sessions = self.rook.sessions().unwrap_or_default();
        self.skills = self.rook.catalog();
        self.stats = self.rook.stats().ok();
        self.objects = self
            .rook
            .store
            .list_objects(None, 300)
            .unwrap_or_default()
            .into_iter()
            .map(|(id, m)| {
                (
                    id.short(),
                    rook_store::Kind::from_u8(m.kind).as_str().to_string(),
                    m.size_raw,
                    m.size_stored,
                )
            })
            .collect();
        if self.session_state.selected().is_none() && !self.sessions.is_empty() {
            self.session_state.select(Some(0));
        }
        if self.skill_state.selected().is_none() && !self.skills.is_empty() {
            self.skill_state.select(Some(0));
        }
        self.load_transcript();
        self.status = format!(
            "{} sessions · {} skills · {} on disk",
            self.sessions.len(),
            self.skills.len(),
            self.stats.as_ref().map(|s| fmt::bytes(s.disk_bytes())).unwrap_or_default()
        );
    }

    fn load_transcript(&mut self) {
        self.transcript.clear();
        self.transcript_scroll = 0;
        let Some(i) = self.session_state.selected() else { return };
        let Some(session) = self.sessions.get(i) else { return };
        // Bounded on purpose: viewing a session with a huge tool result must not
        // itself become the memory problem.
        self.transcript = self.rook.transcript(session.id, 0, 500, 4_000).unwrap_or_default();
    }

    fn run(mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.quit {
            terminal.draw(|f| self.draw(f))?;
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match (key.code, key.modifiers) {
                    (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => self.quit = true,
                    (KeyCode::Char('c'), KeyModifiers::CONTROL) => self.quit = true,
                    (KeyCode::Tab, _) | (KeyCode::Right, _) => self.tab = (self.tab + 1) % TABS.len(),
                    (KeyCode::BackTab, _) | (KeyCode::Left, _) => {
                        self.tab = (self.tab + TABS.len() - 1) % TABS.len()
                    }
                    (KeyCode::Char(c @ '1'..='4'), _) => self.tab = c as usize - '1' as usize,
                    (KeyCode::Char('r'), _) => self.reload(),
                    (KeyCode::Char('j'), _) | (KeyCode::Down, _) => self.move_selection(1),
                    (KeyCode::Char('k'), _) | (KeyCode::Up, _) => self.move_selection(-1),
                    (KeyCode::PageDown, _) | (KeyCode::Char(' '), _) => {
                        self.transcript_scroll = self.transcript_scroll.saturating_add(20)
                    }
                    (KeyCode::PageUp, _) => {
                        self.transcript_scroll = self.transcript_scroll.saturating_sub(20)
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn move_selection(&mut self, delta: isize) {
        let (state, len) = match self.tab {
            0 => (&mut self.session_state, self.sessions.len()),
            1 => (&mut self.skill_state, self.skills.len()),
            _ => {
                self.transcript_scroll = self.transcript_scroll.saturating_add_signed(delta as i16 * 3);
                return;
            }
        };
        if len == 0 {
            return;
        }
        let current = state.selected().unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, len as isize - 1) as usize;
        state.select(Some(next));
        if self.tab == 0 {
            self.load_transcript();
        }
    }

    fn draw(&mut self, f: &mut Frame) {
        let [header, body, footer] =
            Layout::vertical([Constraint::Length(3), Constraint::Min(3), Constraint::Length(1)])
                .areas(f.area());

        let tabs = Tabs::new(TABS.iter().map(|t| Span::raw(*t)).collect::<Vec<_>>())
            .select(self.tab)
            .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD))
            .block(Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).title(format!(
                " rook {} — {} ",
                rook_core::AGENT_VERSION,
                self.rook.workspace.display()
            )));
        f.render_widget(tabs, header);

        match self.tab {
            0 => self.draw_sessions(f, body),
            1 => self.draw_skills(f, body),
            2 => self.draw_store(f, body),
            _ => self.draw_help(f, body),
        }

        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" ↹/1-4 ", Style::default().fg(Color::Cyan)),
                Span::raw("tab  "),
                Span::styled("j/k ", Style::default().fg(Color::Cyan)),
                Span::raw("move  "),
                Span::styled("r ", Style::default().fg(Color::Cyan)),
                Span::raw("reload  "),
                Span::styled("q ", Style::default().fg(Color::Cyan)),
                Span::raw(format!("quit    {}", self.status)),
            ]))
            .style(Style::default().fg(Color::DarkGray)),
            footer,
        );
    }

    fn draw_sessions(&mut self, f: &mut Frame, area: Rect) {
        let [left, right] =
            Layout::horizontal([Constraint::Percentage(34), Constraint::Percentage(66)]).areas(area);

        let items: Vec<ListItem> = self
            .sessions
            .iter()
            .map(|s| {
                ListItem::new(vec![
                    Line::from(Span::styled(
                        s.title.chars().take(40).collect::<String>(),
                        Style::default().add_modifier(Modifier::BOLD),
                    )),
                    Line::from(Span::styled(
                        format!("  {} · {} events · {}", fmt::ago(s.updated_at), s.event_count, s.model),
                        Style::default().fg(Color::DarkGray),
                    )),
                ])
            })
            .collect();

        f.render_stateful_widget(
            List::new(items)
                .block(bordered(" Sessions "))
                .highlight_style(Style::default().bg(Color::Rgb(40, 44, 52)))
                .highlight_symbol("▌"),
            left,
            &mut self.session_state,
        );

        let mut lines: Vec<Line> = Vec::new();
        for e in &self.transcript {
            lines.push(Line::from(vec![
                Span::styled(format!("#{:<4} ", e.seq), Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{:<13}", e.kind), kind_style(&e.kind)),
                Span::styled(
                    format!("{}  {} → {}", e.label, fmt::bytes(e.bytes), fmt::bytes(e.stored_bytes)),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
            for line in e.body.lines().take(40) {
                lines.push(Line::from(Span::raw(format!("  {line}"))));
            }
            if e.truncated {
                lines.push(Line::from(Span::styled(
                    "  … elided; `rook store cat` for the full object",
                    Style::default().fg(Color::Yellow),
                )));
            }
            lines.push(Line::from(""));
        }
        if lines.is_empty() {
            lines.push(Line::from(Span::styled(
                "no events in this session",
                Style::default().fg(Color::DarkGray),
            )));
        }

        f.render_widget(
            Paragraph::new(lines)
                .block(bordered(" Transcript "))
                .wrap(Wrap { trim: false })
                .scroll((self.transcript_scroll, 0)),
            right,
        );
    }

    fn draw_skills(&mut self, f: &mut Frame, area: Rect) {
        let [left, right] =
            Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(area);

        let items: Vec<ListItem> = self
            .skills
            .iter()
            .map(|c| {
                let mark = if c.applicable { "✓" } else { "·" };
                let color = if c.applicable { Color::Green } else { Color::DarkGray };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{mark} "), Style::default().fg(color)),
                    Span::raw(format!("{:<24}", c.name)),
                    Span::styled(format!("{:<9}", c.version), Style::default().fg(Color::DarkGray)),
                    Span::styled(c.source.clone(), Style::default().fg(Color::Blue)),
                ]))
            })
            .collect();

        f.render_stateful_widget(
            List::new(items)
                .block(bordered(" Skills "))
                .highlight_style(Style::default().bg(Color::Rgb(40, 44, 52)))
                .highlight_symbol("▌"),
            left,
            &mut self.skill_state,
        );

        let mut lines = Vec::new();
        if let Some(card) = self.skill_state.selected().and_then(|i| self.skills.get(i)) {
            lines.push(Line::from(Span::styled(
                format!("{} {}", card.name, card.version),
                Style::default().add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(
                format!("source {} · body ~{} tokens", card.source, card.body_tokens),
                Style::default().fg(Color::DarkGray),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(card.description.clone()));
            lines.push(Line::from(""));
            if card.applicable {
                lines.push(Line::from(Span::styled(
                    "applies in this environment",
                    Style::default().fg(Color::Green),
                )));
            } else {
                lines.push(Line::from(Span::styled("blocked here:", Style::default().fg(Color::Yellow))));
                for m in &card.mismatches {
                    lines.push(Line::from(format!("  · {m}")));
                }
            }
            if !card.keywords.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!("keywords: {}", card.keywords.join(", ")),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
        if !self.rook.skill_errors.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("failed to load:", Style::default().fg(Color::Red))));
            for e in &self.rook.skill_errors {
                lines.push(Line::from(format!("  {e}")));
            }
        }
        f.render_widget(Paragraph::new(lines).block(bordered(" Detail ")).wrap(Wrap { trim: false }), right);
    }

    fn draw_store(&mut self, f: &mut Frame, area: Rect) {
        let [top, bottom] = Layout::vertical([Constraint::Length(12), Constraint::Min(3)]).areas(area);

        let mut lines = Vec::new();
        if let Some(s) = &self.stats {
            lines.push(Line::from(vec![
                Span::raw(format!("{:<16}", "logical")),
                Span::styled(fmt::bytes(s.bytes_raw), Style::default().add_modifier(Modifier::BOLD)),
            ]));
            lines.push(Line::from(vec![
                Span::raw(format!("{:<16}", "stored")),
                Span::styled(fmt::bytes(s.bytes_stored), Style::default().fg(Color::Green)),
                Span::styled(
                    format!("   {:.1}x compression", s.compression_ratio()),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
            lines.push(Line::from(format!("{:<16}{}", "saved by dedup", fmt::bytes(s.dedup_saved_hint))));
            lines.push(Line::from(format!(
                "{:<16}{}   (index {}, objects {})",
                "on disk",
                fmt::bytes(s.disk_bytes()),
                fmt::bytes(s.index_bytes),
                fmt::bytes(s.external_bytes)
            )));
            lines.push(Line::from(format!(
                "{:<16}{} objects · {} events · {} refs",
                "counts", s.objects, s.events, s.refs
            )));
            lines.push(Line::from(""));
            let max = s.per_kind.iter().map(|k| k.bytes_stored).max().unwrap_or(0);
            for k in &s.per_kind {
                lines.push(Line::from(vec![
                    Span::raw(format!("{:<14}", k.kind)),
                    Span::styled(fmt::bar(k.bytes_stored, max, 24), Style::default().fg(Color::Cyan)),
                    Span::raw(format!(
                        "  {:>9}  {:>9}  {:.1}x",
                        fmt::bytes(k.bytes_raw),
                        fmt::bytes(k.bytes_stored),
                        k.ratio()
                    )),
                ]));
            }
        }
        f.render_widget(Paragraph::new(lines).block(bordered(" Store ")).wrap(Wrap { trim: false }), top);

        let items: Vec<ListItem> = self
            .objects
            .iter()
            .map(|(id, kind, raw, stored)| {
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{id}  "), Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{kind:<13}"), kind_style(kind)),
                    Span::raw(format!("{:>10} → {:>10}", fmt::bytes(*raw), fmt::bytes(*stored))),
                ]))
            })
            .collect();
        f.render_widget(List::new(items).block(bordered(" Objects (newest 300) ")), bottom);
    }

    fn draw_help(&mut self, f: &mut Frame, area: Rect) {
        let text = vec![
            Line::from(Span::styled("rook", Style::default().add_modifier(Modifier::BOLD))),
            Line::from(""),
            Line::from("This browser is read-only. Everything it shows is also available"),
            Line::from("from the command line, in tables or as JSON with --json:"),
            Line::from(""),
            Line::from("  rook store stat                what memory costs, per kind"),
            Line::from("  rook store ls / cat <id>       list and print raw objects"),
            Line::from("  rook store gc --dry-run        what would be collected"),
            Line::from("  rook session ls / show <id>    transcripts by sequence number"),
            Line::from("  rook skills ls / why <name>    what applies here, and why not"),
            Line::from("  rook skills history <name>     every captured version"),
            Line::from("  rook skills rollback <n> <id>  restore one, undoably"),
            Line::from("  rook checkpoint create <name>  snapshot part of the workspace"),
            Line::from("  rook doctor                    detected toolchains and platform"),
            Line::from(""),
            Line::from(Span::styled("keys", Style::default().add_modifier(Modifier::BOLD))),
            Line::from("  Tab / 1-4   switch tab          j k ↑ ↓   move"),
            Line::from("  Space/PgDn  scroll transcript    r         reload"),
            Line::from("  q / Esc     quit"),
        ];
        f.render_widget(Paragraph::new(text).block(bordered(" Help ")), area);
    }
}

fn bordered(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(title.bold())
}

fn kind_style(kind: &str) -> Style {
    let color = match kind {
        "user" => Color::Cyan,
        "assistant" => Color::White,
        "tool-call" => Color::Magenta,
        "tool-result" | "tool_result" => Color::Blue,
        "skill" => Color::Green,
        "error" => Color::Red,
        "compaction" => Color::Yellow,
        "file" => Color::Blue,
        "snapshot" => Color::Green,
        _ => Color::Gray,
    };
    Style::default().fg(color)
}
