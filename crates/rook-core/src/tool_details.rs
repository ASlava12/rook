//! Small, atomic, display-only companions. Never infer structured facts from
//! process output or treat server-returned metadata as configured MCP identity.
use rook_store::{Event, EventKind};
use serde::{Deserialize, Serialize};

use crate::{
    Result, Rook,
    secrets::Vault,
    transcript::{ToolDetails, ToolImage, ToolResultDetails},
};

pub(crate) const LABEL: &str = "rook:tool-details:v1";
pub(crate) const MAX_BYTES: usize = 4096;

#[derive(Serialize, Deserialize)]
struct Note {
    tool: String,
    result_body: String,
    result: ToolResultDetails,
}

pub(crate) fn note(
    name: &str,
    outcome: &rook_tools::ToolOutcome,
    mcp: Option<(&str, &str)>,
    vault: &Vault,
) -> Option<String> {
    if name.len() > 256 {
        return None;
    }
    let result = if let Some((server, remote)) = mcp {
        if server.len() > 512
            || remote.len() > 256
            || outcome.images.len() > 4
            || outcome.images.iter().any(|image| image.mime_type.len() > 64)
        {
            return None;
        }
        ToolResultDetails::Mcp {
            server: identity(vault, server, 512),
            remote_tool: identity(vault, remote, 256),
            text_blocks: outcome.meta.get("text_blocks").and_then(serde_json::Value::as_u64),
            resource_blocks: outcome.meta.get("resource_blocks").and_then(serde_json::Value::as_u64),
            unsupported_blocks: outcome.meta.get("unsupported_blocks").and_then(serde_json::Value::as_u64),
            images: outcome
                .images
                .iter()
                .map(|image| ToolImage {
                    mime_type: image.mime_type.clone(),
                    width: image.width,
                    height: image.height,
                })
                .collect(),
        }
    } else if matches!(name, "run_command" | "job") {
        let exit_code = outcome
            .meta
            .get("exit_code")
            .and_then(serde_json::Value::as_i64)
            .and_then(|code| i32::try_from(code).ok());
        let timed_out = outcome.meta.get("timed_out").and_then(serde_json::Value::as_bool).unwrap_or(false);
        let running = outcome.meta.get("running").and_then(serde_json::Value::as_bool).unwrap_or(false);
        if exit_code.is_none() && !timed_out && !running {
            return None;
        }
        // Sentinel -1 means signal termination or unavailable status, not an
        // exit code supplied by a completed process.
        ToolResultDetails::Command { exit_code: exit_code.filter(|code| *code >= 0), timed_out, running }
    } else if name == "search" {
        ToolResultDetails::Search {
            matches: outcome.meta.get("matches")?.as_u64()?,
            files_scanned: outcome.meta.get("files_scanned")?.as_u64()?,
            complete: outcome.meta.get("search_complete")?.as_bool()?,
        }
    } else {
        return None;
    };
    // Reserve the exact encoded length of the hash before accepting the note.
    encode(&Note { tool: name.into(), result_body: "0".repeat(64), result })
}

fn identity(vault: &Vault, text: &str, limit: usize) -> String {
    let redacted = vault.redact(text);
    if redacted.len() <= limit { redacted } else { rook_llm::truncate(&redacted, limit.saturating_sub(3)) }
}

fn encode(note: &Note) -> Option<String> {
    struct Bytes(Vec<u8>);
    impl std::io::Write for Bytes {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_BYTES.saturating_sub(self.0.len()) {
                return Err(std::io::Error::other("tool details exceed the encoded byte limit"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut bytes = Bytes(Vec::with_capacity(MAX_BYTES));
    serde_json::to_writer(&mut bytes, note).ok()?;
    String::from_utf8(bytes.0).ok()
}

/// Binding happens after redaction/hooks and the image caption, immediately
/// before the atomic append. A fork cut at the note must not lend it to a
/// different result with the same tool name.
pub(crate) fn bind(preview: &str, text: &str) -> Result<String> {
    if preview.len() > MAX_BYTES {
        return Err(crate::CoreError::Other("tool details exceed the encoded byte limit".into()));
    }
    let mut note: Note = serde_json::from_str(preview)?;
    note.result_body = rook_store::ObjectId::of(text.as_bytes()).to_hex();
    encode(&note).ok_or_else(|| crate::CoreError::Other("tool details exceed the encoded byte limit".into()))
}

pub(crate) fn load(rook: &Rook, result: &Event) -> Result<Option<ToolDetails>> {
    if result.record.kind != EventKind::ToolResult {
        return Ok(None);
    }
    let Some(previous) = result.seq.checked_sub(1) else { return Ok(None) };
    let Some(mut event) = rook.store.events(result.session, previous, 1)?.into_iter().next() else {
        return Ok(None);
    };
    let has_images = event.record.kind == EventKind::Note && event.record.label == crate::tool_images::LABEL;
    if has_images {
        let Some(previous) = previous.checked_sub(1) else { return Ok(None) };
        let Some(before) = rook.store.events(result.session, previous, 1)?.into_iter().next() else {
            return Ok(None);
        };
        event = before;
    }
    if event.record.kind != EventKind::Note || event.record.label != LABEL {
        return Ok(None);
    }
    let size = rook.store.stat_object(&event.record.body)?.map(|meta| meta.size_raw).unwrap_or(0);
    if size > MAX_BYTES as u64 {
        return Ok(None);
    }
    let bytes = rook.store.get_range(&event.record.body, 0, MAX_BYTES)?;
    let Ok(note) = serde_json::from_slice::<Note>(&bytes) else { return Ok(None) };
    if note.tool != result.record.label || note.result_body != result.record.body.to_hex() {
        return Ok(None);
    }
    let valid = match &note.result {
        ToolResultDetails::Command { exit_code, timed_out, running } => {
            matches!(note.tool.as_str(), "run_command" | "job")
                && exit_code.is_none_or(|code| code >= 0)
                && !(*timed_out && *running)
                && (!(*timed_out || *running) || exit_code.is_none())
        }
        ToolResultDetails::Search { .. } => note.tool == "search",
        ToolResultDetails::Mcp { server, remote_tool, images, .. } => {
            !server.is_empty()
                && server.len() <= 512
                && !remote_tool.is_empty()
                && remote_tool.len() <= 256
                && images.len() <= 4
                && (images.is_empty() || has_images)
                && images.iter().all(|image| {
                    matches!(
                        image.mime_type.as_str(),
                        "image/png" | "image/jpeg" | "image/webp" | "image/gif"
                    ) && image.width > 0
                        && image.height > 0
                })
        }
    };
    Ok(valid.then_some(ToolDetails { note_seq: event.seq, result: note.result }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rook_store::{Kind, NewEvent};
    use serde_json::json;

    fn rook(root: &std::path::Path) -> Rook {
        Rook::from_parts(
            rook_store::Store::open(root.join("store")).unwrap(),
            crate::Config::default(),
            rook_skills::Environment::bare("linux", "x86_64", "0.1.0"),
            rook_skills::SkillIndex::default(),
            root.into(),
        )
    }

    #[test]
    fn structured_details_do_not_guess_from_prose_and_bound_names_before_copying() {
        let root = tempfile::tempdir().unwrap();
        let mut vault = Vault::load_from(root.path().join("secrets.toml")).unwrap();
        vault.keep("test", "private-identity-1234").unwrap();
        assert_eq!(vault.value("test").as_deref(), Some("private-identity-1234"));
        let prose = rook_tools::ToolOutcome::ok("exit 0; 999 matches; MCP server other");
        assert!(note("run_command", &prose, None, &vault).is_none());
        assert!(note("search", &prose, None, &vault).is_none());
        let spoofed = prose.clone().with("server", "forged").with("matches", 999);
        assert!(note("external", &spoofed, None, &vault).is_none());
        let saved = note("camera__shot", &spoofed, Some(("private-identity-1234", "shot")), &vault).unwrap();
        assert!(!saved.contains("private-identity-1234") && !saved.contains("forged"), "{saved}");
        assert!(note("camera__shot", &prose, Some((&"s".repeat(513), "shot")), &vault).is_none());
        assert!(note(&"n".repeat(257), &prose, Some(("camera", "shot")), &vault).is_none());
        // Individually admitted strings can exceed the envelope after JSON
        // escaping. The writer stops before collecting those expanded bytes.
        let escaped_name = "\u{1}".repeat(256);
        let escaped_server = "\u{1}".repeat(512);
        assert!(escaped_name.len() * 6 + escaped_server.len() * 6 > MAX_BYTES);
        assert!(note(&escaped_name, &prose, Some((&escaped_server, "shot")), &vault).is_none());
        vault.keep("short", "secret").unwrap();
        assert_eq!(vault.value("short").as_deref(), Some("secret"));
        let raw_server = "secret".repeat(85);
        let raw_remote = "secret".repeat(42);
        assert!(raw_server.len() <= 512 && vault.redact(&raw_server).len() > 512);
        assert!(raw_remote.len() <= 256 && vault.redact(&raw_remote).len() > 256);
        let saved: Note = serde_json::from_str(
            &note("camera__shot", &prose, Some((&raw_server, &raw_remote)), &vault).unwrap(),
        )
        .unwrap();
        let ToolResultDetails::Mcp { server, remote_tool, .. } = saved.result else {
            panic!("MCP identity expected")
        };
        assert!(
            server.len() <= 512 && remote_tool.len() <= 256,
            "the ellipsis counts toward each reader limit"
        );
        let search =
            prose.clone().with("matches", 12).with("files_scanned", 3).with("search_complete", false);
        let saved: Note = serde_json::from_str(&note("search", &search, None, &vault).unwrap()).unwrap();
        let text = ToolDetails { note_seq: 0, result: saved.result }.text();
        assert!(text.contains("12 matching lines") && text.contains("partial scan"), "{text}");
        for (meta, expected) in [
            (json!({"exit_code":7}), "command exit 7"),
            (json!({"timed_out":true}), "timed out; no completed exit status"),
            (json!({"running":true}), "running; no completed exit status"),
            (json!({"exit_code":-1}), "exit status unavailable"),
        ] {
            let mut outcome = prose.clone();
            outcome.meta = serde_json::from_value(meta).unwrap();
            let saved: Note =
                serde_json::from_str(&note("run_command", &outcome, None, &vault).unwrap()).unwrap();
            let text = ToolDetails { note_seq: 0, result: saved.result }.text();
            assert!(text.contains(expected) && !text.contains("exit 0"), "{text}");
        }
    }

    #[test]
    fn exact_details_and_images_survive_reopen_and_fork_but_not_unrelated_or_oversized_notes() {
        let root = tempfile::tempdir().unwrap();
        let agent = rook(root.path());
        let session = agent.start_session("typed details").unwrap();
        let vault = Vault::load_from(root.path().join("secrets.toml")).unwrap();
        let image = rook_llm::Image::from_base64("image/png", "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==").unwrap();
        let mut outcome = rook_tools::ToolOutcome::ok("image-only result")
            .with("text_blocks", 0)
            .with("resource_blocks", 0)
            .with("unsupported_blocks", 0);
        outcome.images.push(image);
        let saved = note("camera__shot", &outcome, Some(("camera", "shot")), &vault).unwrap();
        let seq = crate::tool_images::record_with_preview(
            &agent,
            session,
            "camera__shot",
            &mut outcome.content,
            &outcome.images,
            Some((LABEL, &saved)),
        )
        .unwrap();
        let expected = agent.transcript_entry(session, seq, 0).unwrap().entry.tool_details.unwrap();
        assert_eq!(expected.note_seq, seq - 2);
        assert!(expected.text().contains("image image/png 1×1") && expected.text().contains("0 text block"));
        let event = agent.store.events(session, seq, 1).unwrap().remove(0);
        assert_eq!(crate::tool_images::load(&agent, &event).unwrap().len(), 1);
        let child = agent.fork_session(session, seq + 1).unwrap().id;
        let raw_pair = |body: &str, name: &str| {
            agent
                .store
                .append_event_pair(
                    session,
                    NewEvent::new(EventKind::Note, Kind::Message, body.as_bytes()).label(LABEL),
                    NewEvent::new(EventKind::ToolResult, Kind::ToolResult, b"exit 0").label(name),
                )
                .unwrap()[1]
        };
        let unattached_images = bind(&saved, "exit 0").unwrap();
        for seq in [
            raw_pair(&saved, "other__shot"),
            raw_pair(&unattached_images, "camera__shot"), // Correct hash but no image companion.
            raw_pair("not JSON", "camera__shot"),
            raw_pair(&"x".repeat(MAX_BYTES + 1), "camera__shot"),
        ] {
            assert!(agent.transcript_entry(session, seq, 0).unwrap().entry.tool_details.is_none());
        }
        let loose = agent.log(session, EventKind::ToolResult, "camera__shot", "later same tool").unwrap();
        assert!(agent.transcript_entry(session, loose, 0).unwrap().entry.tool_details.is_none());
        let next = agent.store.get_session(session).unwrap().unwrap().next_seq;
        assert!(
            crate::tool_images::record_with_preview(
                &agent,
                session,
                "camera__shot",
                &mut String::new(),
                &[],
                Some((LABEL, &"x".repeat(MAX_BYTES + 1)))
            )
            .is_err()
        );
        assert_eq!(agent.store.get_session(session).unwrap().unwrap().next_seq, next);
        let command = rook_tools::ToolOutcome::error("exit 7").with("exit_code", 7);
        let draft = note("run_command", &command, None, &vault).unwrap();
        let command_seq = crate::tool_images::record_with_preview(
            &agent,
            session,
            "run_command",
            &mut command.content.clone(),
            &[],
            Some((LABEL, &draft)),
        )
        .unwrap();
        assert!(agent.transcript_entry(session, command_seq, 0).unwrap().entry.tool_details.is_some());
        let cut = agent.fork_session(session, command_seq).unwrap().id;
        let unrelated = agent.log(cut, EventKind::ToolResult, "run_command", "exit 0").unwrap();
        assert!(
            agent.transcript_entry(cut, unrelated, 0).unwrap().entry.tool_details.is_none(),
            "an orphaned note copied at the fork cutoff cannot describe a different result body"
        );
        let cut = agent.fork_session(session, command_seq).unwrap().id;
        let wrong_tool = agent.log(cut, EventKind::ToolResult, "search", "exit 7").unwrap();
        assert!(
            agent.transcript_entry(cut, wrong_tool, 0).unwrap().entry.tool_details.is_none(),
            "a matching body hash cannot lend a command's facts to another tool"
        );
        let mut old = serde_json::to_value(agent.transcript_entry(session, loose, 0).unwrap().entry).unwrap();
        old.as_object_mut().unwrap().remove("tool_details");
        assert!(serde_json::from_value::<crate::TranscriptEntry>(old).unwrap().tool_details.is_none());
        drop(agent);
        let agent = rook(root.path());
        for id in [session, child] {
            assert_eq!(
                agent.transcript_entry(id, seq, 0).unwrap().entry.tool_details,
                Some(expected.clone())
            );
            assert_eq!(agent.transcript(id, seq, 1, 128).unwrap()[0].tool_details, Some(expected.clone()));
            assert_eq!(
                agent
                    .transcript_page(
                        id,
                        &crate::transcript::PageRequest {
                            from: Some(seq),
                            limit: Some(1),
                            ..Default::default()
                        }
                    )
                    .unwrap()
                    .items[0]
                    .tool_details,
                Some(expected.clone())
            );
        }
    }
}
