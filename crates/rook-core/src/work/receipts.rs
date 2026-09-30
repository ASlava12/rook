//! Receipt transitions shared by session steering and durable work. Callers hold
//! their queue's mutation lock through persistence; a snapshot is never authority
//! to edit or accept a message.
use rook_proto::work::{EditInstruction, Steer, Steering, WithdrawInstruction};

use crate::{CoreError, Result, Rook};

pub(crate) static WRITING: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn bad(message: impl Into<String>) -> CoreError {
    CoreError::Other(message.into())
}

fn hash(text: &str) -> String {
    use sha2::Digest;
    format!("sha256:{}", hex::encode(sha2::Sha256::digest(text.trim().as_bytes())))
}

fn validate(rook: &Rook, text: &str) -> Result<()> {
    if text.trim().is_empty() || text.len() > rook.config.work.max_message_bytes {
        return Err(bad(format!(
            "instruction must contain text and fit in {} bytes (work.max_message_bytes)",
            rook.config.work.max_message_bytes
        )));
    }
    Ok(())
}

pub(crate) fn submit(
    rook: &Rook,
    messages: &mut Vec<Steering>,
    request: Steer,
    open: bool,
) -> Result<Steering> {
    if request.id.is_empty()
        || request.id.len() > 64
        || !request.id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(bad("instruction id must be 1–64 letters, digits, hyphens or underscores"));
    }
    validate(rook, &request.text)?;
    let submitted_hash = hash(&request.text);
    if let Some(existing) = messages.iter().find(|m| m.id == request.id) {
        let original = existing.submitted_hash.clone().unwrap_or_else(|| hash(&existing.text));
        if original != submitted_hash {
            return Err(bad("instruction id was already used for different text"));
        }
        return Ok(existing.clone());
    }
    if !open {
        return Err(bad("this run has ended; start a new goal"));
    }
    if messages.len() >= rook.config.work.max_messages {
        return Err(bad(
            "instruction receipt limit reached (work.max_messages); start a new session or raise the limit",
        ));
    }
    let instruction = Steering {
        id: request.id,
        text: request.text.trim().into(),
        submitted_at: super::managed::now(),
        applied_at: None,
        session: None,
        revision: 0,
        withdrawn_at: None,
        submitted_hash: Some(submitted_hash),
        follow_up: None,
    };
    messages.push(instruction.clone());
    Ok(instruction)
}

pub(crate) fn edit(
    rook: &Rook,
    messages: &mut [Steering],
    id: &str,
    request: EditInstruction,
    open: bool,
) -> Result<Steering> {
    validate(rook, &request.text)?;
    let message =
        messages.iter_mut().find(|m| m.id == id).ok_or_else(|| bad("unknown instruction receipt"))?;
    // A retry can arrive after acceptance. Confirming the successful edit does
    // not alter the text already admitted to the model.
    if request.revision.checked_add(1) == Some(message.revision)
        && message.text == request.text.trim()
        && message.withdrawn_at.is_none()
    {
        return Ok(message.clone());
    }
    if !open || !message.queued() {
        return Err(bad("only a queued instruction can be edited; send a new correction instead"));
    }
    if message.follow_up.as_ref().is_some_and(|f| f.reserved.is_some()) {
        return Err(bad(
            "follow-up already reserved; withdraw an unaccepted stopped attempt and submit a new message",
        ));
    }
    if message.revision != request.revision {
        return Err(bad("instruction changed in another window; refresh it before editing"));
    }
    message.submitted_hash.get_or_insert_with(|| hash(&message.text));
    message.revision =
        message.revision.checked_add(1).ok_or_else(|| bad("instruction revision exhausted"))?;
    message.text = request.text.trim().into();
    Ok(message.clone())
}

pub(crate) fn withdraw(
    messages: &mut [Steering],
    id: &str,
    request: WithdrawInstruction,
    open: bool,
) -> Result<Steering> {
    let message =
        messages.iter_mut().find(|m| m.id == id).ok_or_else(|| bad("unknown instruction receipt"))?;
    if message.withdrawn_at.is_some() && request.revision.checked_add(1) == Some(message.revision) {
        return Ok(message.clone());
    }
    if !open || !message.queued() {
        return Err(bad("only a queued instruction can be withdrawn; accepted context cannot be recalled"));
    }
    if message.revision != request.revision {
        return Err(bad("instruction changed in another window; refresh it before withdrawing"));
    }
    message.revision =
        message.revision.checked_add(1).ok_or_else(|| bad("instruction revision exhausted"))?;
    message.withdrawn_at = Some(super::managed::now());
    Ok(message.clone())
}
