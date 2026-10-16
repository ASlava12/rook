//! A bounded saved patch; width changes choose layout again on every draw.
use super::*;
pub(super) struct View {
    pub owner: u128,
    pub review: String,
    pub path: String,
    pub source: String,
    pub patch: String,
    pub files: Vec<String>,
    pub next: Option<u64>,
    scroll: u16,
    split: Option<bool>,
}
impl View {
    pub(super) fn from_reply(owner: u128, reply: &serde_json::Value) -> Option<Self> {
        Some(Self {
            owner,
            review: reply.get("review")?.as_str()?.into(),
            path: reply.get("path")?.as_str()?.into(),
            source: reply.get("source")?.as_str()?.into(),
            patch: reply.get("diff")?.as_str()?.into(),
            files: reply
                .get("files")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect(),
            next: reply.get("next").and_then(|v| v.as_u64()),
            scroll: 0,
            split: None,
        })
    }
    pub(super) fn key(&mut self, key: crossterm::event::KeyEvent) -> Option<serde_json::Value> {
        match key.code {
            KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Down => self.scroll = self.scroll.saturating_add(1),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(20),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(20),
            KeyCode::Char('u') => self.split = Some(false),
            KeyCode::Char('s') => self.split = Some(true),
            KeyCode::Char('a') => self.split = None,
            KeyCode::Char('n') => {
                if let Some(offset) = self.next {
                    return Some(
                        serde_json::json!({"action":"diff","review":self.review,"path":self.path,"offset":offset}),
                    );
                }
            }
            KeyCode::Left | KeyCode::Right if !self.files.is_empty() => {
                let at = self.files.iter().position(|p| p == &self.path).unwrap_or(0);
                let next = if key.code == KeyCode::Left {
                    (at + self.files.len() - 1) % self.files.len()
                } else {
                    (at + 1) % self.files.len()
                };
                return Some(
                    serde_json::json!({"action":"diff","review":self.review,"path":self.files[next],"offset":0}),
                );
            }
            _ => {}
        }
        None
    }
    pub(super) fn draw(&self, f: &mut Frame, area: Rect) {
        let [heading, body, help] =
            Layout::vertical([Constraint::Length(4), Constraint::Min(1), Constraint::Length(2)]).areas(area);
        f.render_widget(
            Paragraph::new(format!("{} · source {}\n{}", self.path, self.source, self.files.join(" · ")))
                .wrap(Wrap { trim: false })
                .block(bordered("saved changes")),
            heading,
        );
        let split = self.split.unwrap_or(area.width >= 100) && area.width >= 100;
        let lines: Vec<_> = self
            .patch
            .lines()
            .map(|line| {
                let color = if line.starts_with('+') && !line.starts_with("+++") {
                    Color::Green
                } else if line.starts_with('-') && !line.starts_with("---") {
                    Color::Red
                } else {
                    Color::Reset
                };
                Line::styled(line.to_owned(), Style::default().fg(color))
            })
            .collect();
        if split {
            let [left, right] =
                Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(body);
            let (old, new) = columns(&self.patch);
            f.render_widget(
                Paragraph::new(old)
                    .wrap(Wrap { trim: false })
                    .scroll((self.scroll, 0))
                    .block(bordered("before")),
                left,
            );
            f.render_widget(
                Paragraph::new(new)
                    .wrap(Wrap { trim: false })
                    .scroll((self.scroll, 0))
                    .block(bordered("after")),
                right,
            );
        } else {
            f.render_widget(
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .scroll((self.scroll, 0))
                    .block(bordered("unified")),
                body,
            );
        }
        f.render_widget(
            Paragraph::new("←/→ files · ↑/↓ scroll · n next page · a auto · u unified · s split · Esc close"),
            help,
        );
    }
}

fn columns(patch: &str) -> (Vec<Line<'static>>, Vec<Line<'static>>) {
    let (mut before, mut after) = (Vec::new(), Vec::new());
    let (mut deleted, mut added) = (Vec::new(), Vec::new());
    let flush = |deleted: &mut Vec<String>,
                 added: &mut Vec<String>,
                 before: &mut Vec<Line<'static>>,
                 after: &mut Vec<Line<'static>>| {
        for index in 0..deleted.len().max(added.len()) {
            before.push(Line::styled(
                deleted.get(index).cloned().unwrap_or_default(),
                Style::default().fg(Color::Red),
            ));
            after.push(Line::styled(
                added.get(index).cloned().unwrap_or_default(),
                Style::default().fg(Color::Green),
            ));
        }
        deleted.clear();
        added.clear();
    };
    for line in patch.lines() {
        if line.starts_with('-') && !line.starts_with("---") {
            deleted.push(line.to_string());
        } else if line.starts_with('+') && !line.starts_with("+++") {
            added.push(line.to_string());
        } else {
            flush(&mut deleted, &mut added, &mut before, &mut after);
            before.push(Line::raw(line.to_string()));
            after.push(Line::raw(line.to_string()));
        }
    }
    flush(&mut deleted, &mut added, &mut before, &mut after);
    (before, after)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resizing_reselects_layout_without_changing_the_saved_patch_or_source() {
        let value = serde_json::json!({"review":"r","source":"saved-hash","path":"f.txt","files":["f.txt","g.txt"],"next":200,"diff":"@@ -1 +1 @@\n-old\n+new\n"});
        let mut view = View::from_reply(7, &value).unwrap();
        for width in [140, 70, 140] {
            let backend = ratatui::backend::TestBackend::new(width, 24);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal.draw(|frame| view.draw(frame, frame.area())).unwrap();
            let rendered =
                terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect::<String>();
            assert!(rendered.contains("saved-hash"));
            assert!(rendered.contains(if width < 100 { "unified" } else { "before" }));
        }
        let next = view.key(crossterm::event::KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)).unwrap();
        assert_eq!(next["path"], "g.txt");
        assert_eq!(next["review"], "r");
        assert_eq!(view.source, "saved-hash");
        assert_eq!(view.owner, 7);
    }
}
