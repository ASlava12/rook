//! Load streams the fixed saved prefix; it never dispatches saved operations.
use rook_core::Rook;
use rook_store::{Event, EventKind};
use serde::Serialize;

use crate::{Peer, protocol::Error};

const TEXT_BYTES: usize = 512;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Params<'a, U> {
    session_id: &'a str,
    update: U,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Content<'a> {
    Text {
        text: &'a str,
    },
    Image {
        data: &'a str,
        #[serde(rename = "mimeType")]
        mime_type: &'a str,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Message<'a> {
    session_update: &'static str,
    message_id: &'a str,
    content: Content<'a>,
    #[serde(rename = "_meta")]
    meta: Source<'a>,
}

#[derive(Serialize)]
struct Source<'a> {
    rook: Anchor<'a>,
}

#[derive(Serialize)]
struct Anchor<'a> {
    historical: bool,
    event: u64,
    object: &'a str,
    offset: u64,
}

#[derive(Serialize)]
struct ToolContent<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    content: Content<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Tool<'a> {
    session_update: &'static str,
    tool_call_id: &'a str,
    content: [ToolContent<'a>; 1],
    #[serde(rename = "_meta")]
    meta: Source<'a>,
}

enum Target<'a> {
    Message(&'static str, &'a str),
    Tool(&'a str),
}

async fn chunk(
    peer: &Peer,
    session: &str,
    event: &Event,
    offset: u64,
    target: &Target<'_>,
    content: Content<'_>,
) -> Result<(), Error> {
    let object = event.record.body.to_hex();
    let meta = Source { rook: Anchor { historical: true, event: event.seq, object: &object, offset } };
    match target {
        Target::Message(kind, id) => {
            peer.notify_wait(
                "session/update",
                &Params {
                    session_id: session,
                    update: Message { session_update: kind, message_id: id, content, meta },
                },
            )
            .await
        }
        Target::Tool(id) => {
            peer.notify_wait(
                "session/update",
                &Params {
                    session_id: session,
                    update: Tool {
                        session_update: "tool_call_update",
                        tool_call_id: id,
                        content: [ToolContent { kind: "content", content }],
                        meta,
                    },
                },
            )
            .await
        }
    }
}

fn error(error: impl std::fmt::Display) -> Error {
    Error::internal(format!("cannot replay retained history: {error}"))
}

fn prefix(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.get(..end).unwrap_or_default()
}

async fn text(
    peer: &Peer,
    session: &str,
    event: &Event,
    target: &Target<'_>,
    value: &str,
) -> Result<(), Error> {
    if value.is_empty() {
        return chunk(peer, session, event, 0, target, Content::Text { text: "" }).await;
    }
    let mut remaining = value;
    let mut offset = 0;
    while !remaining.is_empty() {
        let size = prefix(remaining, TEXT_BYTES).len();
        let part = remaining.get(..size).ok_or_else(|| error("invalid UTF-8 text boundary"))?;
        chunk(peer, session, event, offset, target, Content::Text { text: part }).await?;
        remaining = remaining.get(size..).ok_or_else(|| error("invalid UTF-8 text boundary"))?;
        offset += size as u64;
    }
    Ok(())
}

fn admitted_body(rook: &Rook, event: &Event, maximum: usize) -> Result<Vec<u8>, Error> {
    let meta = rook
        .store
        .stat_object(&event.record.body)
        .map_err(error)?
        .ok_or_else(|| error("saved object disappeared"))?;
    if meta.size_raw > maximum as u64 || meta.size_stored > maximum.saturating_mul(2) as u64 {
        return Err(error(format!(
            "saved object exceeds server.acp_history_body_bytes ({maximum}); increase the body budget to load the complete entry"
        )));
    }
    // get_range verifies the whole immutable object, including its hash. Read
    // once; repeatedly requesting tiny ranges would decode it once per frame.
    rook.store.get_range(&event.record.body, 0, maximum).map_err(error)
}

async fn raw(
    rook: &Rook,
    peer: &Peer,
    session: &str,
    event: &Event,
    target: &Target<'_>,
) -> Result<(), Error> {
    let bytes = admitted_body(rook, event, rook.config.server.acp_history_body_bytes)?;
    text(peer, session, event, target, std::str::from_utf8(&bytes).map_err(error)?).await
}

async fn attachment(rook: &Rook, peer: &Peer, session: &str, event: &Event, id: &str) -> Result<(), Error> {
    let maximum = rook.config.server.acp_history_body_bytes.min(rook_core::attachments::MAX_FRAME_BYTES);
    let bytes = admitted_body(rook, event, maximum)?;
    let message =
        rook_core::attachments::decode(std::str::from_utf8(&bytes).map_err(error)?).map_err(error)?;
    let target = Target::Message("user_message_chunk", id);
    text(peer, session, event, &target, &message.content).await?;
    for image in &message.images {
        let checked = rook_llm::Image::from_base64(&image.mime_type, &image.data).map_err(error)?;
        if checked.width != image.width || checked.height != image.height {
            return Err(error("retained image dimensions do not match its pixels"));
        }
        chunk(
            peer,
            session,
            event,
            0,
            &target,
            Content::Image { data: &checked.data, mime_type: &checked.mime_type },
        )
        .await?;
    }
    Ok(())
}

/// No per-session history count cap: admit only bounded metadata pages and one
/// text/image frame at a time, awaiting the shared queue between chunks.
pub(super) async fn replay(rook: &Rook, peer: &Peer, session: u128) -> Result<u64, Error> {
    let through = rook
        .store
        .get_session_limited(session, 64 * 1024)
        .map_err(error)?
        .ok_or_else(|| error("session disappeared"))?
        .next_seq;
    let wire = rook_store::format_session_id(session);
    let metadata_bytes = rook.config.server.chat_queue_bytes.min(64 * 1024);
    let mut from = 0;
    let mut pending: Option<String> = None;
    while from < through {
        let events = rook.store.events_bounded(session, from, 64, metadata_bytes).map_err(error)?;
        if events.is_empty() {
            return Err(error("session changed during replay"));
        }
        if events.first().is_some_and(|event| event.seq >= through) {
            return Err(error("saved prefix changed during replay"));
        }
        for event in events.iter().take_while(|event| event.seq < through) {
            from = event.seq.saturating_add(1);
            let id = format!("saved_{}_{}", wire, event.seq);
            match event.record.kind {
                EventKind::UserMessage | EventKind::AssistantMessage | EventKind::Reasoning => {
                    pending = None;
                    if event.record.kind == EventKind::UserMessage
                        && event.record.label == rook_core::attachments::LABEL
                    {
                        attachment(rook, peer, &wire, event, &id).await?;
                    } else {
                        let kind = match event.record.kind {
                            EventKind::UserMessage => "user_message_chunk",
                            EventKind::Reasoning => "agent_thought_chunk",
                            _ => "agent_message_chunk",
                        };
                        raw(rook, peer, &wire, event, &Target::Message(kind, &id)).await?;
                    }
                }
                EventKind::ToolCall => {
                    let name = rook_tools::elide_middle(&event.record.label, 256);
                    let mut params = crate::protocol::tool_call(
                        &wire,
                        &id,
                        &name,
                        &name,
                        crate::protocol::tool_kind(&name),
                    );
                    // Saved admission is no claim that a command is live now.
                    params["update"]["status"] = "pending".into();
                    params["update"]["_meta"]["rook"] =
                        serde_json::json!({"historical":true,"event":event.seq});
                    peer.notify_wait("session/update", &params).await?;
                    raw(rook, peer, &wire, event, &Target::Tool(&id)).await?;
                    pending = Some(id);
                }
                EventKind::ToolResult => {
                    let id = if let Some(id) = pending.take() {
                        id
                    } else {
                        let mut params = crate::protocol::tool_call(
                            &wire,
                            &id,
                            "historical result",
                            "historical result",
                            "other",
                        );
                        params["update"]["status"] = "pending".into();
                        params["update"]["_meta"]["rook"] =
                            serde_json::json!({"historical":true,"event":event.seq,"callObserved":false});
                        peer.notify_wait("session/update", &params).await?;
                        id
                    };
                    raw(rook, peer, &wire, event, &Target::Tool(&id)).await?;
                    let details = rook.transcript_entry(session, event.seq, 0).map_err(error)?.entry;
                    if let Some(seq) = details.image_note {
                        let companion =
                            rook.store.events_bounded(session, seq, 1, metadata_bytes).map_err(error)?;
                        let source = companion
                            .first()
                            .filter(|source| source.seq == seq)
                            .ok_or_else(|| error("retained image source disappeared"))?;
                        let meta = rook
                            .store
                            .stat_object(&source.record.body)
                            .map_err(error)?
                            .ok_or_else(|| error("retained image object disappeared"))?;
                        if meta.size_raw > rook.config.server.acp_history_body_bytes as u64
                            || meta.size_stored
                                > rook.config.server.acp_history_body_bytes.saturating_mul(2) as u64
                        {
                            return Err(error(
                                "saved image object exceeds server.acp_history_body_bytes; increase it to read the complete source",
                            ));
                        }
                        let mut index = 0;
                        loop {
                            let image = rook.transcript_image(session, event.seq, index).map_err(error)?;
                            chunk(
                                peer,
                                &wire,
                                event,
                                0,
                                &Target::Tool(&id),
                                Content::Image { data: &image.image.data, mime_type: &image.image.mime_type },
                            )
                            .await?;
                            index += 1;
                            if index >= image.count {
                                break;
                            }
                        }
                    }
                    if let Some(measurement) = details.tool_measurement {
                        peer.notify_wait(
                            "session/update",
                            &crate::protocol::tool_call_done(&wire, &id, measurement.failed),
                        )
                        .await?;
                    }
                }
                // Provider state, checkpoints and receipts are not conversation
                // content. In particular no signed/encrypted state is exposed.
                _ => {}
            }
        }
    }
    Ok(through)
}
