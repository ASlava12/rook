//! CLI projection of the same queue operations the two interactive views use.
use crate::{args::QueueCmd, source::Source};
use anyhow::Result;
use rook_proto::queue::{Change, Entry, Page, Query};

pub(crate) fn execute(
    source: &Source,
    session: u128,
    command: Option<&QueueCmd>,
) -> Result<serde_json::Value> {
    Ok(match command {
        None => serde_json::to_value(source.queue_page(session, &Query::default())?)?,
        Some(QueueCmd::List { all, after }) => serde_json::to_value(
            source.queue_page(session, &Query { include_finished: *all, after: after.clone() })?,
        )?,
        Some(QueueCmd::Show { reference }) => serde_json::to_value(source.queue_read(session, reference)?)?,
        Some(QueueCmd::Edit { reference, revision, text }) => serde_json::to_value(source.queue_change(
            session,
            Change::Edit { reference: reference.clone(), revision: *revision, text: text.join(" ") },
        )?)?,
        Some(QueueCmd::Withdraw { reference, revision }) => serde_json::to_value(source.queue_change(
            session,
            Change::Withdraw { reference: reference.clone(), revision: *revision },
        )?)?,
    })
}

pub(crate) fn status(receipt: &rook_proto::work::Steering) -> &'static str {
    if receipt.withdrawn_at.is_some() {
        "withdrawn"
    } else if receipt.applied_at.is_some() {
        "accepted"
    } else {
        "queued"
    }
}

pub(crate) fn describe(value: &serde_json::Value) -> Result<String> {
    if let Ok(page) = serde_json::from_value::<Page>(value.clone()) {
        let mut text = page.items.iter().map(describe_entry).collect::<Vec<_>>().join("\n\n");
        if page.items.is_empty() {
            text.push_str("No queued messages.");
        }
        if let Some(next) = page.next {
            text.push_str(&format!("\nNext page: list --after {next}"));
        }
        Ok(text)
    } else {
        Ok(describe_entry(&serde_json::from_value(value.clone())?))
    }
}

fn describe_entry(entry: &Entry) -> String {
    format!(
        "{} · {} · revision {}\n{}{}",
        entry.reference,
        status(&entry.receipt),
        entry.receipt.revision,
        entry.receipt.text,
        if entry.truncated { "\n[preview shortened; use show to read the complete message]" } else { "" }
    )
}
