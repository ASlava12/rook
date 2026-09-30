//! One layout for draft height, cursor movement and drawing. Soft wraps never
//! enter the submitted text; only viewport cells are copied for rendering.
use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy)]
pub(super) struct Geometry {
    width: usize,
    gutter: usize,
}

impl Default for Geometry {
    fn default() -> Self {
        Self { width: usize::MAX, gutter: 0 }
    }
}

impl Geometry {
    pub(super) fn new(prompt: &str, width: u16) -> Self {
        let width = usize::from(width);
        // Leave room for a wide grapheme and the insertion cursor even when
        // the working-status prefix is wider than a small terminal.
        Self { width, gutter: Line::from(prompt).width().min(width.saturating_sub(3)) }
    }

    fn walk(&self, text: &str, mut visit: impl FnMut(usize, usize, usize, &str, usize) -> bool) {
        let capacity = self.width.saturating_sub(self.gutter + 1).max(1);
        let (mut row, mut column) = (0, 0);
        for (byte, symbol) in text.grapheme_indices(true) {
            let cells = if symbol == "\t" {
                4.min(capacity)
            } else if symbol.contains(char::is_control) {
                0
            } else {
                Span::raw(symbol).width().min(capacity)
            };
            if column > 0 && column + cells > capacity {
                row += 1;
                column = 0;
            }
            if !visit(byte, row, column, symbol, cells) {
                return;
            }
            if matches!(symbol, "\n" | "\r\n") {
                row += 1;
                column = 0;
            } else {
                column += cells;
            }
        }
        visit(text.len(), row, column, "", 0);
    }

    pub(super) fn measure(&self, text: &str, at: usize) -> (usize, (usize, usize)) {
        let (mut rows, mut caret) = (1, (0, 0));
        self.walk(text, |byte, row, column, _, _| {
            rows = row + 1;
            if byte <= at {
                caret = (row, column);
            }
            true
        });
        (rows, caret)
    }

    pub(super) fn vertical(&self, text: &str, at: usize, down: bool) -> Option<usize> {
        let (rows, (row, column)) = self.measure(text, at);
        let target = if down { row.checked_add(1)? } else { row.checked_sub(1)? };
        if target >= rows {
            return None;
        }
        let mut found = None;
        self.walk(text, |byte, row, col, _, _| {
            if row == target && (found.is_none() || col <= column) {
                found = Some(byte);
            }
            row <= target
        });
        found
    }

    pub(super) fn view(
        &self,
        text: &str,
        caret: (usize, usize),
        prompt: &str,
        height: u16,
    ) -> (Vec<Line<'static>>, (u16, u16)) {
        if self.width == 0 || height == 0 {
            return (Vec::new(), (0, 0));
        }
        let (row, column) = caret;
        let scroll = row.saturating_sub(usize::from(height - 1));
        let mut lines: Vec<Line<'static>> = Vec::new();
        self.walk(text, |_, row, _, symbol, cells| {
            if row < scroll {
                return true;
            }
            if row >= scroll + usize::from(height) {
                return false;
            }
            if lines.len() <= row - scroll {
                let mut mark = String::new();
                if row == 0 {
                    let mut used = 0;
                    for g in prompt.graphemes(true) {
                        used += Span::raw(g).width();
                        if used > self.gutter {
                            break;
                        }
                        mark.push_str(g);
                    }
                }
                let padding = self.gutter.saturating_sub(Line::from(mark.as_str()).width());
                mark.push_str(&" ".repeat(padding));
                lines.push(Line::from(Span::styled(mark, Style::default().fg(Color::DarkGray))));
            }
            if cells > 0 {
                let visible = if symbol == "\t" {
                    " ".repeat(cells)
                } else if Span::raw(symbol).width() > cells {
                    "�".into()
                } else {
                    symbol.to_owned()
                };
                lines.last_mut().expect("visible row").spans.push(Span::raw(visible));
            }
            true
        });
        (lines, ((row - scroll) as u16, (self.gutter + column).min(self.width - 1) as u16))
    }
}
