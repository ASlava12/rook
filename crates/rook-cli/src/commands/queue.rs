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
        Some(QueueCmd::Submit { id, target, text, follow_up }) => {
            let target = match target {
                Some(target) => target.clone(),
                None => {
                    let page = source.queue_page(session, &Query::default())?;
                    if *follow_up {
                        page.follow_up_target.ok_or_else(|| {
                            anyhow::anyhow!("start a turn or goal before queuing a follow-up")
                        })?
                    } else {
                        page.submission_target
                    }
                }
            };
            anyhow::ensure!(
                !target.is_empty(),
                "daemon does not advertise scoped queue submission; restart it with the current build"
            );
            let id =
                id.clone().unwrap_or_else(|| rook_store::format_session_id(rook_store::new_session_id()));
            // Keep the retry identity visible even if this process is stopped
            // after the server commits but before its response arrives.
            eprintln!("Submission ID {id}; target {target}");
            let change = if *follow_up {
                Change::FollowUp { target: target.clone(), id: id.clone(), text: text.join(" ") }
            } else {
                Change::Submit { target: target.clone(), id: id.clone(), text: text.join(" ") }
            };
            let entry = source.queue_change(session, change).map_err(|error| anyhow::anyhow!(
                "{error}; repeat the identical text and submission mode with --id {id} --target {target} to resolve this submission without duplicating it"
            ))?;
            serde_json::to_value(entry)?
        }
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
        let mut text = format!("Submission target: {}\n", page.submission_target);
        text.push_str(&page.items.iter().map(describe_entry).collect::<Vec<_>>().join("\n\n"));
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
        if let Some(follow) = &entry.receipt.follow_up {
            format!(
                "follow-up after {} · {}{}",
                follow.after,
                status(&entry.receipt),
                follow.blocked.as_ref().map(|reason| format!(" · stopped: {reason}")).unwrap_or_default()
            )
        } else {
            status(&entry.receipt).into()
        },
        entry.receipt.revision,
        entry.receipt.text,
        if entry.truncated { "\n[preview shortened; use show to read the complete message]" } else { "" }
    )
}
