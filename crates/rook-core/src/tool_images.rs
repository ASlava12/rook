//! Tool images are companion events, never new instructions from the user.
use rook_llm::Image;
use rook_store::{Event, EventKind, Kind, NewEvent, ObjectId};

use crate::{CoreError, Result, Rook};

pub(crate) const LABEL: &str = "rook:tool-images:v1";
const MAX_PAYLOAD: u64 =
    (rook_llm::images::MAX_IMAGE_BYTES.div_ceil(3) * 4 * rook_llm::images::MAX_IMAGES_PER_MESSAGE + 4096)
        as u64;

pub(crate) fn record(
    rook: &Rook,
    session: u128,
    name: &str,
    text: &mut String,
    images: &[Image],
) -> Result<u64> {
    record_with_changes(rook, session, name, text, images, None)
}

pub(crate) fn record_with_changes(
    rook: &Rook,
    session: u128,
    name: &str,
    text: &mut String,
    images: &[Image],
    changes: Option<&str>,
) -> Result<u64> {
    record_with_preview(
        rook,
        session,
        name,
        text,
        images,
        changes.map(|text| (crate::tool_changes::LABEL, text)),
    )
}

pub(crate) fn record_with_preview(
    rook: &Rook,
    session: u128,
    name: &str,
    text: &mut String,
    images: &[Image],
    preview: Option<(&str, &str)>,
) -> Result<u64> {
    if let Some((label, text)) = preview {
        let limit = match label {
            crate::tool_changes::LABEL => crate::tool_changes::MAX_BYTES,
            crate::tool_details::LABEL => crate::tool_details::MAX_BYTES,
            _ => return Err(CoreError::Other("unknown saved tool preview type".into())),
        };
        if text.len() > limit {
            return Err(CoreError::Other("saved tool preview exceeds its byte limit".into()));
        }
    }
    if images.is_empty() {
        let details = preview
            .filter(|(label, _)| *label == crate::tool_details::LABEL)
            .map(|(_, preview)| crate::tool_details::bind(preview, text))
            .transpose()?;
        let preview = details.as_deref().map(|text| (crate::tool_details::LABEL, text)).or(preview);
        if let Some((label, preview)) = preview {
            let [_, result] = rook.store.append_event_pair(
                session,
                NewEvent::new(EventKind::Note, Kind::Message, preview.as_bytes()).label(label),
                NewEvent::new(EventKind::ToolResult, Kind::ToolResult, text.as_bytes()).label(name),
            )?;
            return Ok(result);
        }
        return rook.log(session, EventKind::ToolResult, name, text);
    }
    if images.len() > rook_llm::images::MAX_IMAGES_PER_MESSAGE {
        return Err(CoreError::Other("at most 4 images per tool result".into()));
    }
    // Third-party Tool implementations must obey the same bound as MCP.
    for image in images {
        Image::from_base64(&image.mime_type, &image.data).map_err(CoreError::Other)?;
    }
    let data = serde_json::to_vec(images)?;
    // Screenshots of two different states can have identical captions and
    // lengths. The loop detector must still see that the answer changed.
    text.push_str(&format!("\n[{} tool image(s); content {}]", images.len(), ObjectId::of(&data).to_hex()));
    let details = preview
        .filter(|(label, _)| *label == crate::tool_details::LABEL)
        .map(|(_, preview)| crate::tool_details::bind(preview, text))
        .transpose()?;
    let preview = details.as_deref().map(|text| (crate::tool_details::LABEL, text)).or(preview);
    if let Some((label, preview)) = preview {
        let [_, _, result] = rook.store.append_events_with_values(
            session,
            [
                NewEvent::new(EventKind::Note, Kind::Message, preview.as_bytes()).label(label),
                NewEvent::new(EventKind::Note, Kind::Message, &data).label(LABEL),
                NewEvent::new(EventKind::ToolResult, Kind::ToolResult, text.as_bytes()).label(name),
            ],
            &[],
        )?;
        return Ok(result);
    }
    let [_, result] = rook.store.append_event_pair(
        session,
        NewEvent::new(EventKind::Note, Kind::Message, &data).label(LABEL),
        NewEvent::new(EventKind::ToolResult, Kind::ToolResult, text.as_bytes()).label(name),
    )?;
    Ok(result)
}

fn decode(rook: &Rook, event: &Event) -> Result<Vec<Image>> {
    let size = rook.store.stat_object(&event.record.body)?.map(|m| m.size_raw).unwrap_or(0);
    if size > MAX_PAYLOAD {
        return Err(CoreError::Other("stored tool images exceed the payload limit".into()));
    }
    let images: Vec<Image> = serde_json::from_slice(&rook.store.get(&event.record.body)?)?;
    if images.len() > rook_llm::images::MAX_IMAGES_PER_MESSAGE {
        return Err(CoreError::Other("stored tool result has more than 4 images".into()));
    }
    images
        .into_iter()
        .map(|image| Image::from_base64(&image.mime_type, &image.data).map_err(CoreError::Other))
        .collect()
}

pub(crate) fn load(rook: &Rook, result: &Event) -> Result<Vec<Image>> {
    if result.record.kind != EventKind::ToolResult {
        return Ok(Vec::new());
    }
    let Some(previous) = result.seq.checked_sub(1) else { return Ok(Vec::new()) };
    let event = rook.store.events(result.session, previous, 1)?.into_iter().next();
    match event {
        Some(event)
            if event.seq == previous
                && event.record.kind == EventKind::Note
                && event.record.label == LABEL =>
        {
            decode(rook, &event)
        }
        _ => Ok(Vec::new()),
    }
}

pub(crate) fn preview(rook: &Rook, event: &Event) -> Result<String> {
    let images = decode(rook, event)?;
    Ok(images
        .iter()
        .map(|image| {
            format!("[tool image: {} {}×{}; inline data omitted]", image.mime_type, image.width, image.height)
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

pub(crate) fn tokens(rook: &Rook, event: &Event) -> Result<usize> {
    Ok(decode(rook, event)?.iter().map(Image::estimated_tokens).sum())
}

/// A model can request many screenshots in one batch. Bound the live request
/// during admission, not only after all those calls have accumulated images.
/// The newest pixels win; omitted originals stay in their companion events.
pub(crate) fn bound(messages: &mut [rook_llm::Message]) -> usize {
    let mut room = crate::attachments::MAX_ATTACHMENTS;
    let mut omitted = 0;
    for message in messages.iter_mut().rev() {
        let keep = room.min(message.images.len());
        let dropped = message.images.len() - keep;
        room -= keep;
        if dropped == 0 {
            continue;
        }
        message.images.truncate(keep);
        omitted += dropped;
        let recovery = if message.role == rook_llm::Role::Tool {
            "Use read_result with this result_id and include_images=true if those pixels are needed."
        } else {
            "Ask for the original image to be attached again if those pixels are needed."
        };
        let note = format!(
            "\n[{dropped} image(s) omitted from this request to keep at most {} recent images in context. {recovery}]",
            crate::attachments::MAX_ATTACHMENTS
        );
        // Preserve result_id and the provenance envelope for pruning and
        // retrieval. Appending outside its JSON would make both unreadable.
        if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&message.content)
            && let Some(text) = value.get_mut("rook_source").and_then(|v| v.get_mut("content"))
            && let Some(original) = text.as_str()
        {
            *text = serde_json::Value::String(format!("{original}{note}"));
            message.content = value.to_string();
        } else {
            message.content.push_str(&note);
        }
    }
    omitted
}
