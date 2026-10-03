//! Operator price form. The core owns admission, reviews and persistence.
use std::io::IsTerminal;
use std::time::Duration;

use anyhow::{Result, anyhow};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use rook_core::price_catalog::{Listing, Rates};

pub(super) fn run(source: Option<String>) -> Result<()> {
    anyhow::ensure!(
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        "rook prices --interactive needs a terminal"
    );
    let mut terminal = ratatui::try_init()?;
    let result = form(&mut terminal, source);
    ratatui::restore();
    result
}

pub(super) fn form(terminal: &mut ratatui::DefaultTerminal, source: Option<String>) -> Result<()> {
    let path = rook_core::paths::config_file();
    let directory = rook_core::paths::home().join("cache");
    let load = || -> Result<Listing> {
        let vault = rook_core::Vault::load()?;
        rook_core::price_catalog::inspect(&path, &directory, &vault, source.as_deref())
            .map_err(anyhow::Error::msg)
    };
    let mut listing = load()?;
    let mut selected = 0usize;
    let mut confirm = false;
    let mut status =
        "Inspection is offline. Enter reviews applying missing rates; r refreshes the public source."
            .to_owned();
    let runtime = tokio::runtime::Runtime::new()?;
    let mut refresh: Option<tokio::task::JoinHandle<std::result::Result<(), String>>> = None;
    loop {
        if refresh.as_ref().is_some_and(|job| job.is_finished())
            && let Some(job) = refresh.take()
        {
            status = match runtime.block_on(job) {
                Ok(Ok(())) => "Reference refreshed; review before applying.".into(),
                Ok(Err(e)) => e,
                Err(e) => e.to_string(),
            };
            listing = load()?;
        }
        terminal.draw(|frame| {
            let [header, rows, detail, footer] = Layout::vertical([
                Constraint::Length(3), Constraint::Percentage(30), Constraint::Min(5), Constraint::Length(4),
            ]).areas(frame.area());
            frame.render_widget(Paragraph::new(format!("Public prices · {} · age {} · {}", listing.source_url,
                listing.age_secs.map(|s| format!("{s}s")).unwrap_or("unknown".into()), if listing.stale { "stale" } else { "reference" }))
                .block(Block::default().borders(Borders::ALL)), header);
            let items = listing.models.iter().map(|m| ListItem::new(format!("{} · {} · {}", m.source, m.model, m.provider.as_deref().unwrap_or("unknown identity"))));
            frame.render_stateful_widget(List::new(items).highlight_symbol("▸ ").block(Block::default().borders(Borders::ALL)), rows,
                &mut ListState::default().with_selected(Some(selected)));
            let text = if let Some(model) = listing.models.get(selected) {
                format!("USD / million tokens\nConfigured: {}\nReference: {}\n{}\nMissing: {}\nReference context: {}; reasoning: {}; tools: {} (display only)\nLast application: {}\n{}",
                    rates(model.configured), model.reference.map(rates).unwrap_or("unknown".into()), model.reason,
                    model.apply_fields.join(", "), model.reference_context.map(|v| v.to_string()).unwrap_or("unknown".into()),
                    model.reference_reasoning.map(|v| v.to_string()).unwrap_or("unknown".into()),
                    model.reference_tools.map(|v| v.to_string()).unwrap_or("unknown".into()),
                    model.last_application, listing.notices.join("\n"))
            } else { format!("No named model sources. Add a source in rook config edit.\n{}", listing.notices.join("\n")) };
            frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }).block(Block::default().borders(Borders::ALL)), detail);
            frame.render_widget(Paragraph::new(format!("{}\n{}", status, if confirm { "Apply these missing rates? y applies; n cancels." } else { "j/k select · r refresh · Enter review application · Esc return" })).wrap(Wrap { trim: false }), footer);
        })?;
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let Event::Key(key) = event::read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if key.code == KeyCode::Esc || key.code == KeyCode::Char('q') {
            if let Some(job) = refresh.take() {
                job.abort();
            }
            return Ok(());
        }
        if refresh.is_some() {
            continue;
        }
        if confirm {
            if key.code == KeyCode::Char('y')
                && let Some(model) = listing.models.get(selected)
            {
                let result =
                    model.review_token.as_deref().ok_or_else(|| anyhow!("inspect again")).and_then(|token| {
                        let vault = rook_core::Vault::load()?;
                        rook_core::price_catalog::apply(&path, &directory, &vault, &model.source, token)
                            .map_err(anyhow::Error::msg)
                    });
                status = match result {
                    Ok(_) => {
                        "Missing rates saved. Future turns read them; existing receipts retain their rates."
                            .into()
                    }
                    Err(e) => e.to_string(),
                };
                listing = load()?;
            }
            confirm = false;
            continue;
        }
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                selected = (selected + 1).min(listing.models.len().saturating_sub(1))
            }
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Enter => {
                confirm = listing.models.get(selected).is_some_and(|m| m.review_token.is_some());
                if !confirm {
                    status = "No safe missing rates to apply; see the reason above.".into();
                }
            }
            KeyCode::Char('r') => {
                let settings = rook_core::Config::load()?.price_catalog;
                let directory = directory.clone();
                status = format!(
                    "Refreshing the public reference (at most {}s)… Esc cancels.",
                    settings.timeout_secs
                );
                refresh = Some(
                    runtime
                        .spawn(async move { rook_core::price_catalog::refresh(&directory, settings).await }),
                );
            }
            _ => {}
        }
    }
}
pub(super) fn rates(rates: Rates) -> String {
    let number = |rate: Option<f64>| rate.map(|v| v.to_string()).unwrap_or("unknown".into());
    format!(
        "input {}, output {}, cache read {}, cache write {}",
        number(rates.input),
        number(rates.output),
        number(rates.cache_read),
        number(rates.cache_write)
    )
}
