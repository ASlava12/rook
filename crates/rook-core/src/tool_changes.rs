//! Bounded, tool-reported saved changes. Plain companion notes preserve old
//! event schemas, fork semantics and the model's existing result text.
use rook_store::{Event, EventKind};
use serde_json::Value;

use crate::{Result, Rook, secrets::Vault};

pub(crate) const LABEL: &str = "rook:tool-changes:v1";
pub(crate) const MAX_BYTES: usize = 32 * 1024;
const HEADING: &str = "Saved tool-reported file changes";

pub(crate) fn note(name: &str, value: Option<&Value>, vault: &Vault) -> Option<String> {
    if !matches!(name, "edit_file" | "write_file") {
        return None;
    }
    let value = value?;
    let files = value.get("files")?.as_array()?;
    if files.is_empty() || files.len() > 3 {
        return None;
    }
    // Validate borrowed metadata before any cloning or rendering. Extensions
    // cannot turn this display path into an unbounded additional result body.
    for file in files {
        if file.get("path")?.as_str()?.len() > 512 || file.get("diff")?.as_str()?.len() > 8192 {
            return None;
        }
        file.get("limited")?.as_bool()?;
    }
    let omitted = value.get("omitted_files")?.as_u64()?;
    let mut text =
        format!("{HEADING}\nHistorical preview from {name}; current files and tests are not verified.\n");
    for file in files {
        text.push_str("\nFile: ");
        text.push_str(file.get("path")?.as_str()?);
        if file.get("limited")?.as_bool()? {
            text.push_str(" (preview limited or unavailable)");
        }
        text.push('\n');
        text.push_str(file.get("diff")?.as_str()?);
        text.push('\n');
    }
    if omitted > 0 {
        text.push_str(&format!("\n{omitted} other file(s) omitted from this preview.\n"));
    }
    // Redaction can expand short matches. Keep the committed preview bounded
    // even then; original tool text remains the ordinary result.
    Some(rook_llm::truncate(&vault.redact(&text), MAX_BYTES - 3))
}

/// The atomic batch puts changes immediately before the result, or before
/// its image companion. Never search another branch or infer a diff from prose.
pub(crate) fn source(rook: &Rook, result: &Event) -> Result<Option<u64>> {
    if result.record.kind != EventKind::ToolResult {
        return Ok(None);
    }
    let Some(previous) = result.seq.checked_sub(1) else { return Ok(None) };
    let Some(mut note) = rook.store.events(result.session, previous, 1)?.into_iter().next() else {
        return Ok(None);
    };
    if note.record.kind == EventKind::Note && note.record.label == crate::tool_images::LABEL {
        let Some(previous) = previous.checked_sub(1) else { return Ok(None) };
        let Some(before) = rook.store.events(result.session, previous, 1)?.into_iter().next() else {
            return Ok(None);
        };
        note = before;
    }
    if note.record.kind != EventKind::Note || note.record.label != LABEL {
        return Ok(None);
    }
    let size = rook.store.stat_object(&note.record.body)?.map(|m| m.size_raw).unwrap_or(0);
    if size > MAX_BYTES as u64 {
        return Ok(None);
    }
    let prefix = rook.store.get_range(&note.record.body, 0, HEADING.len())?;
    Ok((prefix == HEADING.as_bytes()).then_some(note.seq))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn saved_changes_validate_metadata_before_copying_and_keep_images_attached_to_the_result() {
        let home = tempfile::tempdir().unwrap();
        let rook = Rook::from_parts(
            rook_store::Store::open(home.path().join("store")).unwrap(),
            crate::Config::default(),
            rook_skills::Environment::bare("linux", "x86_64", "0.1.0"),
            rook_skills::SkillIndex::default(),
            home.path().into(),
        );
        let mut vault = Vault::load_from(home.path().join("secrets.toml")).unwrap();
        vault.keep("test", "known-private-key-0123").unwrap();
        assert_eq!(vault.value("test").as_deref(), Some("known-private-key-0123"));
        let valid = json!({"files":[{"path":"src/a.rs","diff":"-old\n+known-private-key-0123\n","limited":false}],"omitted_files":0});
        let text = note("edit_file", Some(&valid), &vault).unwrap();
        assert!(!text.contains("known-private-key-0123"), "{text}");
        assert!(text.contains("Historical preview") && text.len() <= MAX_BYTES);
        assert!(note("untrusted_mcp", Some(&valid), &vault).is_none());
        let mut large = valid.clone();
        large["files"][0]["diff"] = Value::String("x".repeat(8193));
        assert!(note("edit_file", Some(&large), &vault).is_none());
        large = valid.clone();
        large["files"] = json!([valid["files"][0], valid["files"][0], valid["files"][0], valid["files"][0]]);
        assert!(note("edit_file", Some(&large), &vault).is_none());
        let session = rook.start_session("changes and images").unwrap();
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";
        let image = rook_llm::Image::from_base64("image/png", png).unwrap();
        let mut answer = "edited".to_string();
        let seq = crate::tool_images::record_with_changes(
            &rook,
            session,
            "edit_file",
            &mut answer,
            std::slice::from_ref(&image),
            Some(&text),
        )
        .unwrap();
        let result = rook.store.events(session, seq, 1).unwrap().remove(0);
        assert_eq!(source(&rook, &result).unwrap(), Some(seq - 2));
        assert_eq!(crate::tool_images::load(&rook, &result).unwrap().len(), 1);
        let fork = rook.fork_session(session, seq + 1).unwrap().id;
        assert_eq!(rook.transcript_entry(fork, seq, 0).unwrap().entry.change_note, Some(seq - 2));
        let outside = rook.log(session, EventKind::ToolResult, "edit_file", "not actually recorded").unwrap();
        assert!(rook.transcript_entry(session, outside, 0).unwrap().entry.change_note.is_none());
    }
}
