//! Finite native-terminal experiment, not an alternate Rook runtime.
use std::{
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    Frame, Terminal, TerminalOptions, Viewport,
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Layout, Rect},
    text::Line,
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};
use unicode_segmentation::UnicodeSegmentation;

const FOOTER: u16 = 10;
const MAX_ROWS: usize = 200;
const MAX_TEXT: usize = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Mode {
    Inline,
    Fullscreen,
}

#[derive(Parser)]
struct Args {
    /// Existing owned evidence directory. No agent store or network is used.
    #[arg(long)]
    root: PathBuf,
    #[arg(long, value_enum, default_value = "inline")]
    mode: Mode,
    #[arg(long, default_value_t = 80, value_parser = clap::value_parser!(u16).range(1..=200))]
    rows: u16,
}

struct Restore {
    raw: bool,
    alternate: bool,
}
impl Drop for Restore {
    fn drop(&mut self) {
        if self.alternate {
            let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
        }
        if self.raw {
            let _ = terminal::disable_raw_mode();
        }
        let _ = execute!(std::io::stdout(), crossterm::cursor::Show);
    }
}

fn geometry(width: u16, height: u16) -> Result<()> {
    if !(24..=160).contains(&width) || !(14..=60).contains(&height) {
        bail!("probe geometry must be 24..160 columns and 14..60 rows");
    }
    Ok(())
}

fn fixture_line(row: usize) -> String {
    format!("ROW_{row:04} · сохранённый отчёт 🙂 · fixture only")
}

fn append<B: Backend>(terminal: &mut Terminal<B>, from: usize, count: usize) -> Result<()>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let through = from.checked_add(count).context("row arithmetic")?;
    if through > MAX_ROWS {
        bail!("probe row budget reached");
    }
    let size = terminal.size()?;
    geometry(size.width, size.height)?;
    let lines: Vec<_> = (from..through).map(|row| Line::from(fixture_line(row))).collect();
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let rows = paragraph.line_count(size.width);
    if rows > 800 {
        bail!("probe inserted-row budget reached");
    }
    terminal.insert_before(u16::try_from(rows)?, |buffer| paragraph.render(buffer.area, buffer))?;
    Ok(())
}

fn draw(frame: &mut Frame, mode: Mode, emitted: usize, draft: &str) {
    let area = frame.area();
    let [history, footer] = Layout::vertical([Constraint::Min(0), Constraint::Length(FOOTER)]).areas(area);
    if mode == Mode::Fullscreen {
        let per_row =
            Paragraph::new(fixture_line(0)).wrap(Wrap { trim: false }).line_count(history.width).max(1);
        let first = emitted.saturating_sub((history.height as usize / per_row).max(1));
        let lines: Vec<_> = (first..emitted).map(|row| Line::from(fixture_line(row))).collect();
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), history);
    }
    let [title, tail, input, help, queue] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(4),
        Constraint::Length(1),
        Constraint::Length(3),
    ])
    .areas(footer);
    frame.render_widget(format!("SCROLLBACK_PROBE {mode:?} · {emitted} fixture rows"), title);
    frame.render_widget("Mutable live tail; no model/files/tests were run", tail);
    frame.render_widget(
        Paragraph::new(draft)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" Draft ")),
        input,
    );
    frame.render_widget("Ctrl+O overlay · Ctrl+N append · Ctrl+Q quit", help);
    frame.render_widget(
        Paragraph::new("NEXT_MESSAGE").block(Block::default().borders(Borders::ALL).title(" Queued next ")),
        queue,
    );
}

fn overlay(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    mode: Mode,
    draft: &str,
) -> Result<()> {
    let mut restore = Restore { raw: false, alternate: mode == Mode::Inline };
    if restore.alternate {
        execute!(std::io::stdout(), EnterAlternateScreen)?;
    }
    let mut screen = Terminal::with_options(
        CrosstermBackend::new(std::io::stdout()),
        TerminalOptions { viewport: Viewport::Fullscreen },
    )?;
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let size = screen.size()?;
        geometry(size.width, size.height)?;
        screen.draw(|frame| frame.render_widget(
            Paragraph::new(format!("OVERLAY_PROBE\nDraft retained: {draft}\nEscape returns to the main buffer; this is a fixture, not Rook history."))
                .wrap(Wrap { trim: false }).block(Block::default().borders(Borders::ALL)), frame.area()))?;
        if Instant::now() >= deadline {
            bail!("probe overlay deadline reached");
        }
        if event::poll(Duration::from_millis(60))?
            && matches!(event::read()?, Event::Key(key) if key.code == KeyCode::Esc)
        {
            break;
        }
    }
    drop(screen);
    if restore.alternate {
        execute!(std::io::stdout(), LeaveAlternateScreen)?;
        restore.alternate = false;
    }
    terminal.clear()?;
    Ok(())
}

fn run(args: &Args) -> Result<serde_json::Value> {
    let root = args.root.canonicalize()?;
    if !root.is_dir() {
        bail!("evidence root is not a directory");
    }
    // Refuse replacement before changing terminal state.
    let result = OpenOptions::new().write(true).create_new(true).open(root.join("run-result.json"))?;
    let (width, height) = terminal::size()?;
    geometry(width, height)?;
    let mut restore = Restore { raw: false, alternate: args.mode == Mode::Fullscreen };
    terminal::enable_raw_mode()?;
    restore.raw = true;
    if restore.alternate {
        execute!(std::io::stdout(), EnterAlternateScreen)?;
    }
    let viewport = if args.mode == Mode::Inline { Viewport::Inline(FOOTER) } else { Viewport::Fullscreen };
    let mut terminal =
        Terminal::with_options(CrosstermBackend::new(std::io::stdout()), TerminalOptions { viewport })?;
    let mut emitted = usize::from(args.rows);
    if args.mode == Mode::Inline {
        append(&mut terminal, 0, emitted)?;
    }
    let mut draft = String::new();
    let mut overlays = 0usize;
    let mut resizes = 0usize;
    let mut interactions = 0usize;
    let started = Instant::now();
    let deadline = started + Duration::from_secs(300);
    let final_area = loop {
        let size = terminal.size()?;
        geometry(size.width, size.height)?;
        let mut area = Rect::default();
        terminal.draw(|frame| {
            area = frame.area();
            draw(frame, args.mode, emitted, &draft);
        })?;
        if Instant::now() >= deadline {
            bail!("probe interaction deadline reached");
        }
        if !event::poll(Duration::from_millis(60))? {
            continue;
        }
        interactions += 1;
        if interactions > 2048 {
            bail!("probe interaction budget reached");
        }
        match event::read()? {
            Event::Key(key) if key.is_press() => match (key.code, key.modifiers) {
                (KeyCode::Char('q' | 'c'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
                    break area;
                }
                (KeyCode::Char('o'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
                    if overlays >= 16 {
                        bail!("probe overlay budget reached");
                    }
                    overlay(&mut terminal, args.mode, &draft)?;
                    overlays += 1;
                }
                (KeyCode::Char('n'), modifiers)
                    if modifiers.contains(KeyModifiers::CONTROL) && emitted < MAX_ROWS =>
                {
                    if args.mode == Mode::Inline {
                        append(&mut terminal, emitted, 1)?;
                    }
                    emitted += 1;
                }
                (KeyCode::Char(ch), modifiers)
                    if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        && !ch.is_control()
                        && ch.len_utf8() <= MAX_TEXT.saturating_sub(draft.len()) =>
                {
                    draft.push(ch)
                }
                (KeyCode::Enter, _) if draft.len() < MAX_TEXT => draft.push('\n'),
                (KeyCode::Backspace, _) => {
                    if let Some((at, _)) = draft.grapheme_indices(true).next_back() {
                        draft.truncate(at);
                    }
                }
                _ => {}
            },
            Event::Resize(width, height) => {
                geometry(width, height)?;
                resizes += 1;
            }
            _ => {}
        }
    };
    drop(terminal);
    drop(restore);
    let report = serde_json::json!({"mode":format!("{:?}",args.mode),"emitted":emitted,"draft":draft,"overlays":overlays,"resizes":resizes,"viewport":{"x":final_area.x,"y":final_area.y,"width":final_area.width,"height":final_area.height},"milliseconds":started.elapsed().as_millis()});
    serde_json::to_writer(result, &report)?;
    Ok(report)
}

fn main() -> Result<()> {
    let report = run(&Args::parse())?;
    writeln!(std::io::stdout(), "SCROLLBACK_PROBE_DONE {report}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer.content.iter().map(|cell| cell.symbol()).collect()
    }

    #[test]
    fn inline_commits_completed_rows_once_and_keeps_the_queue_at_the_lower_edge() {
        for width in [40, 80, 120] {
            let mut terminal = Terminal::with_options(
                TestBackend::new(width, 24),
                TerminalOptions { viewport: Viewport::Inline(FOOTER) },
            )
            .unwrap();
            append(&mut terminal, 0, 80).unwrap();
            terminal.draw(|frame| draw(frame, Mode::Inline, 80, "KEEP_DRAFT\nsecond line")).unwrap();
            let backend = terminal.backend();
            let saved = text(backend.scrollback());
            assert!(
                saved.contains("ROW_0000"),
                "fixture must actually reach native scrollback at width {width}"
            );
            let all = saved + &text(backend.buffer());
            for row in 0..80 {
                assert_eq!(
                    all.matches(&format!("ROW_{row:04}")).count(),
                    1,
                    "completed row {row} duplicated or lost at width {width}"
                );
            }
            let queue: String = (0..width).map(|x| backend.buffer()[(x, 22)].symbol()).collect();
            assert!(queue.contains("NEXT_MESSAGE"));
            assert!(text(backend.buffer()).contains("KEEP_DRAFT"));
        }
    }

    #[test]
    fn width_shrink_exposes_the_inline_anchor_problem_without_reinserting_saved_rows() {
        let mut terminal = Terminal::with_options(
            TestBackend::new(80, 24),
            TerminalOptions { viewport: Viewport::Inline(FOOTER) },
        )
        .unwrap();
        append(&mut terminal, 0, 80).unwrap();
        terminal.draw(|frame| draw(frame, Mode::Inline, 80, "KEEP_DRAFT")).unwrap();
        terminal.backend_mut().resize(40, 24);
        let mut area = Rect::default();
        terminal
            .draw(|frame| {
                area = frame.area();
                draw(frame, Mode::Inline, 80, "KEEP_DRAFT");
            })
            .unwrap();
        assert!(
            area.bottom() < 24,
            "record the actual inline shrink behavior rather than claiming a fixed composer"
        );
        assert!(text(terminal.backend().scrollback()).contains("ROW_0000"));
        assert!(text(terminal.backend().buffer()).contains("KEEP_DRAFT"));
    }

    #[test]
    fn geometry_and_fixture_admission_refuse_oversized_buffers_before_insertion() {
        assert!(geometry(161, 24).is_err());
        assert!(geometry(80, 61).is_err());
        let mut terminal = Terminal::with_options(
            TestBackend::new(80, 24),
            TerminalOptions { viewport: Viewport::Inline(FOOTER) },
        )
        .unwrap();
        assert!(append(&mut terminal, 0, MAX_ROWS + 1).is_err());
        assert!(terminal.backend().scrollback().content.is_empty());
    }

    #[test]
    fn fullscreen_reflow_retains_the_latest_row_draft_and_lower_queue() {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, Mode::Fullscreen, 80, "KEEP_DRAFT")).unwrap();
        terminal.backend_mut().resize(40, 24);
        terminal.draw(|frame| draw(frame, Mode::Fullscreen, 80, "KEEP_DRAFT")).unwrap();
        let buffer = terminal.backend().buffer();
        assert!(text(buffer).contains("ROW_0079") && text(buffer).contains("KEEP_DRAFT"));
        let queue: String = (0..40).map(|x| buffer[(x, 22)].symbol()).collect();
        assert!(queue.contains("NEXT_MESSAGE"));
    }
}
