//! Guided config editing; the core owns drafts, validation and persistence.
use std::io::IsTerminal;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use rook_core::config::edit::{Editor, Entry, Help};
use serde_json::Value;

const MAX_INPUT: usize = 16 * 1024;

pub(super) fn run() -> Result<()> {
    anyhow::ensure!(
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        "rook config edit needs an interactive terminal; use rook config show/set in scripts"
    );
    let editor = Editor::open(rook_core::paths::config_file()).map_err(anyhow::Error::msg)?;
    let mut app = App::new(editor)?;
    let mut terminal = ratatui::try_init()?;
    let _restore = Restore;
    execute!(std::io::stdout(), event::EnableBracketedPaste)?;
    while !app.quit {
        terminal.draw(|frame| app.draw(frame))?;
        if event::poll(Duration::from_millis(100))? {
            app.event(event::read()?)?;
        }
    }
    Ok(())
}
struct Restore;
impl Drop for Restore {
    fn drop(&mut self) {
        let _ = execute!(std::io::stdout(), event::DisableBracketedPaste);
        ratatui::restore();
    }
}

#[derive(Default)]
struct Input {
    text: String,
    at: usize,
}
impl Input {
    fn new(text: String) -> Self {
        let at = text.len();
        Self { text, at }
    }
    fn insert(&mut self, text: &str) -> bool {
        if self.text.len() + text.len() > MAX_INPUT {
            return false;
        }
        self.text.insert_str(self.at, text);
        self.at += text.len();
        true
    }
    fn key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.text.clear();
                self.at = 0;
            }
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => self.at = 0,
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => self.at = self.text.len(),
            KeyCode::Char(c) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                return self.insert(&c.to_string());
            }
            KeyCode::Home => self.at = 0,
            KeyCode::End => self.at = self.text.len(),
            KeyCode::Left => {
                if let Some(c) = self.text[..self.at].chars().next_back() {
                    self.at -= c.len_utf8();
                }
            }
            KeyCode::Right => {
                if let Some(c) = self.text[self.at..].chars().next() {
                    self.at += c.len_utf8();
                }
            }
            KeyCode::Backspace => {
                if let Some(c) = self.text[..self.at].chars().next_back() {
                    let start = self.at - c.len_utf8();
                    self.text.drain(start..self.at);
                    self.at = start;
                }
            }
            KeyCode::Delete => {
                if let Some(c) = self.text[self.at..].chars().next() {
                    self.text.drain(self.at..self.at + c.len_utf8());
                }
            }
            _ => {}
        }
        true
    }
}

enum Mode {
    Browse,
    Search(Input),
    Edit { path: Vec<String>, input: Input, help: Help, secret: bool, optional: bool },
    Add(Input),
    Delete(Vec<String>),
    Quit,
}
struct App {
    editor: Editor,
    path: Vec<String>,
    entries: Vec<Entry>,
    selected: usize,
    filter: String,
    mode: Mode,
    status: String,
    quit: bool,
}
impl App {
    fn new(editor: Editor) -> Result<Self> {
        let mut app = Self {
            editor,
            path: Vec::new(),
            entries: Vec::new(),
            selected: 0,
            filter: String::new(),
            mode: Mode::Browse,
            status: "Choose a section. Changes are saved only with Ctrl+S.".into(),
            quit: false,
        };
        app.reload()?;
        Ok(app)
    }
    fn reload(&mut self) -> Result<()> {
        self.entries = self.editor.entries(&self.path).map_err(anyhow::Error::msg)?;
        self.clamp();
        Ok(())
    }
    fn visible(&self) -> Vec<usize> {
        let needle = self.filter.to_lowercase();
        let mut visible: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                entry.path.last().is_some_and(|s| s.to_lowercase().contains(&needle))
                    || entry.help.help.to_lowercase().contains(&needle)
                    || entry
                        .value
                        .as_object()
                        .and_then(|v| v.get("name").or(v.get("language")))
                        .and_then(Value::as_str)
                        .is_some_and(|s| s.to_lowercase().contains(&needle))
            })
            .map(|(i, _)| i)
            .collect();
        visible.sort_by_key(|i| {
            let name = self.entries[*i].path.last().map(|s| s.to_lowercase()).unwrap_or_default();
            if name == needle {
                0
            } else if name.starts_with(&needle) {
                1
            } else {
                2
            }
        });
        visible
    }

    fn clamp(&mut self) {
        self.selected = self.selected.min(self.visible().len().saturating_sub(1));
    }
    fn current(&self) -> Option<&Entry> {
        self.entries.get(*self.visible().get(self.selected)?)
    }
    fn event(&mut self, event: Event) -> Result<()> {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                let gathered = crate::paste::gather(key, &mut crate::paste::Terminal)?;
                match gathered.burst {
                    crate::paste::Burst::Paste(text) => self.paste(&text),
                    crate::paste::Burst::Keys(keys) => {
                        for key in keys {
                            self.key(key)?;
                        }
                    }
                }
                for event in gathered.then {
                    self.event(event)?;
                }
            }
            Event::Paste(text) => self.paste(&text),
            _ => {}
        }
        Ok(())
    }
    fn paste(&mut self, text: &str) {
        let accepted = match &mut self.mode {
            Mode::Edit { input, .. } | Mode::Add(input) | Mode::Search(input) => input.insert(text),
            _ => true,
        };
        if !accepted {
            self.status = "Input is limited to 16 KiB; nothing was pasted.".into();
        }
        if let Mode::Search(input) = &self.mode {
            self.filter = input.text.clone();
            self.selected = 0;
        }
    }
    fn key(&mut self, key: KeyEvent) -> Result<()> {
        let mode = std::mem::replace(&mut self.mode, Mode::Browse);
        match mode {
            Mode::Browse => self.browse(key)?,
            Mode::Search(mut input) => match key.code {
                KeyCode::Esc => {
                    self.filter.clear();
                    self.selected = 0;
                }
                KeyCode::Enter => {}
                _ => {
                    input.key(key);
                    self.filter = input.text.clone();
                    self.selected = 0;
                    self.mode = Mode::Search(input);
                }
            },
            Mode::Edit { path, mut input, help, secret, optional } => {
                if key.code == KeyCode::Esc {
                    return Ok(());
                }
                if key.code == KeyCode::Enter {
                    let result = if optional && input.text.is_empty() {
                        self.editor.remove(&path)
                    } else {
                        self.editor.set(&path, &input.text)
                    };
                    match result {
                        Ok(()) => {
                            self.status = format!("{} updated in draft", path.join("."));
                            self.reload()?;
                            return Ok(());
                        }
                        Err(error) => self.status = error,
                    }
                } else if !help.choices.is_empty()
                    && matches!(key.code, KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right)
                {
                    let at = help.choices.iter().position(|v| v == &input.text).unwrap_or(0);
                    let next = if matches!(key.code, KeyCode::Up | KeyCode::Left) {
                        (at + help.choices.len() - 1) % help.choices.len()
                    } else {
                        (at + 1) % help.choices.len()
                    };
                    input = Input::new(help.choices[next].clone());
                } else if !input.key(key) {
                    self.status = "Input is limited to 16 KiB.".into();
                }
                self.mode = Mode::Edit { path, input, help, secret, optional };
            }
            Mode::Add(mut input) => {
                if key.code == KeyCode::Esc {
                    return Ok(());
                }
                if key.code == KeyCode::Enter {
                    match self.editor.add(&self.path, &input.text) {
                        Ok(child) => {
                            self.filter.clear();
                            self.reload()?;
                            if let Some(index) = self.entries.iter().position(|e| e.path == child) {
                                self.selected = index;
                                if self.entries[index].value.is_object() {
                                    self.open()?;
                                }
                            }
                            self.status = "Entry added to draft. Set its fields, then Ctrl+S to save.".into();
                            return Ok(());
                        }
                        Err(error) => self.status = error,
                    }
                } else if !input.key(key) {
                    self.status = "Input is limited to 16 KiB.".into();
                }
                self.mode = Mode::Add(input);
            }
            Mode::Delete(path) => match key.code {
                KeyCode::Char('y') => match self.editor.remove(&path) {
                    Ok(()) => {
                        self.status = "Removed from draft. Fixed settings now use defaults.".into();
                        self.reload()?;
                    }
                    Err(error) => self.status = error,
                },
                KeyCode::Esc | KeyCode::Char('n') => {}
                _ => self.mode = Mode::Delete(path),
            },
            Mode::Quit => match key.code {
                KeyCode::Char('d') => self.quit = true,
                KeyCode::Char('s') => {
                    if self.save() {
                        self.quit = true;
                    }
                }
                KeyCode::Esc | KeyCode::Char('c') => {}
                _ => self.mode = Mode::Quit,
            },
        }
        Ok(())
    }
    fn browse(&mut self, key: KeyEvent) -> Result<()> {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('s') => {
                    self.save();
                    return Ok(());
                }
                KeyCode::Char('c' | 'q') => {
                    self.close();
                    return Ok(());
                }
                _ => return Ok(()),
            }
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected += 1;
                self.clamp();
            }
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(10),
            KeyCode::PageDown => {
                self.selected += 10;
                self.clamp();
            }
            KeyCode::Home => self.selected = 0,
            KeyCode::End => {
                self.selected = self.visible().len().saturating_sub(1);
            }
            KeyCode::Enter | KeyCode::Right => self.open()?,
            KeyCode::Esc | KeyCode::Left => {
                if !self.filter.is_empty() {
                    self.filter.clear();
                    self.selected = 0;
                } else if self.path.pop().is_some() {
                    self.selected = 0;
                    self.reload()?;
                } else {
                    self.close();
                }
            }
            KeyCode::Char('/') => self.mode = Mode::Search(Input::new(self.filter.clone())),
            KeyCode::Char('a') => {
                if !self.editor.can_add(&self.path) {
                    self.status =
                        "Open models, endpoints, MCP, LSP, hooks, or a list/map to add an entry.".into();
                    return Ok(());
                }
                self.status = "Enter a name for a model/server/map entry, or a value for a list item.".into();
                self.mode = Mode::Add(Input::default());
            }
            KeyCode::Char('d') => {
                if let Some(entry) = self.current() {
                    if entry.path.len() == 1 {
                        self.status = "Open a section to remove its settings or entries.".into();
                    } else {
                        self.mode = Mode::Delete(entry.path.clone());
                    }
                }
            }
            KeyCode::Char('q') => self.close(),
            _ => {}
        }
        Ok(())
    }
    fn open(&mut self) -> Result<()> {
        let Some(entry) = self.current() else {
            return Ok(());
        };
        let path = entry.path.clone();
        if entry.value.is_object() || entry.value.is_array() {
            self.path = path;
            self.selected = 0;
            self.filter.clear();
            self.reload()?;
        } else if entry.help.kind == "boolean" {
            let text = if entry.value.as_bool().unwrap_or(false) { "false" } else { "true" };
            match self.editor.set(&path, text) {
                Ok(()) => {
                    self.status = format!("{} = {text} (draft)", path.join("."));
                    self.reload()?;
                }
                Err(error) => self.status = error,
            }
        } else {
            self.mode = Mode::Edit {
                path,
                input: Input::new(edit_text(&entry.value)),
                help: entry.help.clone(),
                secret: entry.secret,
                optional: entry.default.is_null(),
            };
        }
        Ok(())
    }
    fn save(&mut self) -> bool {
        match self.editor.save() {
            Ok(()) => {
                self.status =
                    "Saved. Reopen standalone TUI; restart rookd for listener or MCP/LSP changes.".into();
                true
            }
            Err(error) => {
                self.status = error;
                false
            }
        }
    }
    fn close(&mut self) {
        if self.editor.dirty() {
            self.mode = Mode::Quit;
        } else {
            self.quit = true;
        }
    }
    fn draw(&mut self, f: &mut Frame) {
        let [header, body, status, footer] = Layout::vertical([
            Constraint::Length(4),
            Constraint::Min(4),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .areas(f.area());
        let where_ = if self.path.is_empty() { "sections".into() } else { self.path.join(" / ") };
        f.render_widget(
            Paragraph::new(format!(
                "{}\n{}{}",
                self.editor.file_path().display(),
                where_,
                if self.editor.dirty() { "  [unsaved]" } else { "  [saved]" }
            ))
            .block(block(" rook config edit ")),
            header,
        );
        let areas = if body.width >= 70 {
            Layout::horizontal([Constraint::Percentage(47), Constraint::Percentage(53)]).split(body)
        } else {
            Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).split(body)
        };
        let visible = self.visible();
        let items: Vec<ListItem> = visible
            .iter()
            .map(|i| {
                let entry = &self.entries[*i];
                let label = entry.path.last().map(String::as_str).unwrap_or_default();
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{label}  "), Style::default().fg(Color::Cyan)),
                    Span::raw(display(&entry.value, entry.secret)),
                    Span::styled(
                        if entry.explicit { "  [set]" } else { "  [default]" },
                        Style::default().fg(Color::DarkGray),
                    ),
                ]))
            })
            .collect();
        let title =
            if self.filter.is_empty() { " settings ".into() } else { format!(" filter: {} ", self.filter) };
        let mut state = ListState::default().with_selected((!visible.is_empty()).then_some(self.selected));
        f.render_stateful_widget(
            List::new(items)
                .block(block(title))
                .highlight_symbol("› ")
                .highlight_style(Style::default().bg(Color::DarkGray)),
            areas[0],
            &mut state,
        );
        let help = if let Some(entry) = self.current() {
            format!(
                "{}\n\n{}\n\nType: {}\nDefault: {}\n{}{}\n\n{}",
                entry.path.join("."),
                entry.help.help,
                entry.help.kind,
                display(&entry.default, entry.secret),
                if entry.explicit {
                    "Written in config.toml."
                } else {
                    "Using the default; no line in config.toml."
                },
                if entry.help.choices.is_empty() {
                    String::new()
                } else {
                    format!("\nChoices: {}", entry.help.choices.join(" | "))
                },
                if entry.value.is_object() || entry.value.is_array() {
                    "Enter opens this section. a adds entries in collections."
                } else {
                    "Enter edits (booleans toggle). d removes the override."
                }
            )
        } else {
            "No entries. Press a to add one, / to filter, Esc to go back.".into()
        };
        f.render_widget(Paragraph::new(help).wrap(Wrap { trim: false }).block(block(" notes ")), areas[1]);
        f.render_widget(
            Paragraph::new(self.status.as_str())
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(Color::Yellow)),
            status,
        );
        f.render_widget(
            Paragraph::new(
                "↑↓ choose · Enter edit/open · / find · a add · d remove · ^S save · Esc back · q quit",
            ),
            footer,
        );
        match &self.mode {
            Mode::Browse => {}
            Mode::Search(input) => popup(
                f,
                " Find in this section ",
                &format!("{}▏\n\nEnter: keep filter · Esc: clear", input.text),
            ),
            Mode::Add(input) => popup(
                f,
                " Add entry ",
                &format!(
                    "{}▏\n\nName for a model/server/map entry; value for a list item.\nEnter: add to draft · Esc: cancel · Ctrl+U: clear",
                    input.text
                ),
            ),
            Mode::Edit { path, input, help, secret, optional } => {
                let shown = if *secret {
                    format!(
                        "{}▏{}",
                        "•".repeat(input.text[..input.at].chars().count()),
                        "•".repeat(input.text[input.at..].chars().count())
                    )
                } else {
                    format!("{}▏{}", &input.text[..input.at], &input.text[input.at..])
                };
                popup(
                    f,
                    " Edit value ",
                    &format!(
                        "{}\n\n{}\n\n{}\n{}\nEnter: apply to draft · Esc: cancel · Ctrl+U: clear{}",
                        path.join("."),
                        help.help,
                        shown,
                        if help.choices.is_empty() {
                            String::new()
                        } else {
                            format!("←/→ choose: {}", help.choices.join(" | "))
                        },
                        if *optional { " · empty: unset" } else { "" }
                    ),
                );
            }
            Mode::Delete(path) => popup(
                f,
                " Remove from configuration? ",
                &format!(
                    "{}\n\nA fixed setting returns to its default.\nA list/map entry is removed. Nothing is saved yet.\n\ny: remove · n/Esc: cancel",
                    path.join(".")
                ),
            ),
            Mode::Quit => popup(
                f,
                " Unsaved changes ",
                "s: save and exit\nd: discard draft and exit\nc/Esc: continue editing",
            ),
        }
    }
}
fn edit_text(value: &Value) -> String {
    if let Value::String(s) = value {
        s.clone()
    } else if value.is_null() {
        String::new()
    } else {
        value.to_string()
    }
}
fn display(value: &Value, secret: bool) -> String {
    match value {
        Value::Null => "unset / inherited".into(),
        Value::Array(a) => format!("{} entries →", a.len()),
        Value::Object(o) => o
            .get("name")
            .or(o.get("language"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(|s| format!("{s} →"))
            .unwrap_or_else(|| format!("{} settings →", o.len())),
        Value::String(s) if s.is_empty() => "(empty)".into(),
        _ if secret => "<hidden>".into(),
        _ => edit_text(value).chars().take(120).collect(),
    }
}
fn block(title: impl Into<Line<'static>>) -> Block<'static> {
    Block::default().borders(Borders::ALL).title(title)
}
fn popup(f: &mut Frame, title: &'static str, text: &str) {
    let area = f.area();
    let width = area.width.saturating_sub(4).min(90);
    let height = area.height.saturating_sub(4).min(16);
    let area = Rect::new((area.width - width) / 2, (area.height - height) / 2, width, height);
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }).block(block(title)), area);
}
