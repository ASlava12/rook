//! Bounded acquisition of a frozen stage's evidence, before rendering it.
use crate::{Rook, TranscriptEntry};
use rook_store::EventKind;

const SCAN_EVENTS: usize = 1024;
const KEEP_ENTRIES: usize = 128;
const METADATA_BYTES: usize = 16 * 1024;
const SESSION_BYTES: usize = 64 * 1024;
const BODY_BYTES: usize = 2000;
// Range reads verify the entire object, even when retaining only head/tail.
const PAYLOAD_BYTES: u64 = 2 * 1024 * 1024;

pub(super) struct Excerpt {
    pub(super) entries: Vec<TranscriptEntry>,
    through: Option<u64>,
    unread_before: Option<u64>,
    metadata_steps: usize,
    scanned: usize,
    body_reads: usize,
    body_bytes: usize,
    partial_payloads: usize,
    companion_reads: usize,
    issues: Vec<String>,
}

impl Excerpt {
    fn empty(through: Option<u64>) -> Self {
        Self {
            entries: Vec::new(),
            through,
            unread_before: None,
            metadata_steps: 0,
            scanned: 0,
            body_reads: 0,
            body_bytes: 0,
            partial_payloads: 0,
            companion_reads: 0,
            issues: Vec::new(),
        }
    }

    fn issue(&mut self, text: String) {
        if self.issues.len() < 8 {
            self.issues.push(rook_llm::truncate(&text, 253));
        }
    }

    pub(super) fn notice(&self, session: u128, floor: u64) -> String {
        let session = rook_store::format_session_id(session);
        let coverage = serde_json::json!({
            "scope":"saved_stage_excerpt", "session":session, "from":floor,
            "through_exclusive":self.through, "unread_before_exclusive":self.unread_before,
            "metadata_steps":self.metadata_steps, "metadata_step_limit":SCAN_EVENTS,
            "scanned_events":self.scanned, "retained_entries":self.entries.len(),
            "entry_limit":KEEP_ENTRIES, "body_reads":self.body_reads,
            "body_bytes":self.body_bytes, "body_byte_limit":KEEP_ENTRIES * BODY_BYTES,
            "partial_payloads":self.partial_payloads, "companion_reads":self.companion_reads,
            "payload_raw_byte_limit":PAYLOAD_BYTES, "payload_stored_byte_limit":PAYLOAD_BYTES,
            "issues":self.issues
        });
        // Recall cursors move forwards. Starting at the newest omitted event
        // would strand earlier proof behind the first returned page.
        format!(
            "\n\nSaved-stage acquisition is bounded. The renderer may elide additional older entries. \
             Missing or shortened evidence does not establish that an action never happened and does not \
             narrow the goal or its constraints. The original journal is unchanged. Inspect older or \
             unavailable evidence with session_recall {{\"session\":\"{session}\",\"around\":{floor}}}, \
             following its returned cursors as needed. Each displayed event's own sequence anchors \
             inspection of its saved text and attachment metadata. Recall pages can also shorten \
             payloads; the original journal remains available through history entry navigation.\n{}",
            crate::sources::data(
                "history_coverage",
                "saved-stage reader bounds and omissions",
                &coverage.to_string()
            )
        )
    }
}

pub(super) fn read(rook: &Rook, session: u128, floor: u64) -> Excerpt {
    match rook.store.get_session_limited(session, SESSION_BYTES) {
        Ok(Some(meta)) => read_at(rook, session, floor, meta.next_seq),
        result => {
            let mut out = Excerpt::empty(None);
            out.issue(format!(
                "Unable to capture stage history: {}",
                match result {
                    Err(error) => error.to_string(),
                    _ => "session metadata is unavailable".into(),
                }
            ));
            out
        }
    }
}

fn read_at(rook: &Rook, session: u128, floor: u64, through: u64) -> Excerpt {
    let mut out = Excerpt::empty(Some(through));
    if floor > through {
        out.issue("Stage floor is beyond the captured history boundary".into());
        return out;
    }
    let mut before = through;
    while before > floor && out.metadata_steps < SCAN_EVENTS && out.entries.len() < KEEP_ENTRIES {
        out.metadata_steps += 1;
        // One record at a time preserves a good newest tail even if an older
        // metadata record is too large. Excluded notes never resolve bodies.
        let event = match rook.store.events_before_bounded(session, floor, before, 1, METADATA_BYTES) {
            Ok(mut events) => match events.pop() {
                Some(event) => event,
                None => {
                    before = floor;
                    break;
                }
            },
            Err(error) => {
                out.issue(format!("History metadata unavailable before #{before}: {error}"));
                break;
            }
        };
        before = event.seq;
        out.scanned += 1;
        // Checkpoint object ids are not file hashes. Showing bookkeeping to
        // a checker once turned an unchanged rename into a false finding.
        if !crate::context::reaches_the_model(event.record.kind) {
            continue;
        }
        let meta = match rook.store.stat_object(&event.record.body) {
            Ok(Some(meta)) => Some(meta),
            Ok(None) => {
                out.issue(format!("Payload metadata unavailable at #{}: object is missing", event.seq));
                None
            }
            Err(error) => {
                out.issue(format!("Payload metadata unavailable at #{}: {error}", event.seq));
                None
            }
        };
        let attachment =
            event.record.kind == EventKind::UserMessage && event.record.label == crate::attachments::LABEL;
        let admitted = meta
            .as_ref()
            .is_some_and(|meta| meta.size_raw <= PAYLOAD_BYTES && meta.size_stored <= PAYLOAD_BYTES);
        let (mut body, mut cut) = if !admitted {
            (
                format!(
                    "Payload omitted before verified decode: unavailable or exceeds the {PAYLOAD_BYTES}-byte raw/stored limit; inspect original event #{}",
                    event.seq
                ),
                true,
            )
        } else if attachment && meta.as_ref().is_none_or(|meta| meta.size_raw > BODY_BYTES as u64) {
            (
                format!(
                    "Attachment frame omitted before decoding; inspect session_recall at event #{}",
                    event.seq
                ),
                true,
            )
        } else {
            out.body_reads += 1;
            match crate::transcript::body(rook, &event, BODY_BYTES) {
                Ok(value) => value,
                Err(error) => {
                    out.issue(format!("Payload unavailable at #{}: {error}", event.seq));
                    (format!("Payload unavailable; inspect the original event #{}", event.seq), true)
                }
            }
        };
        if body.len() > BODY_BYTES {
            // Keep the end of a tool answer too: it commonly carries its
            // outcome. Reserve room for window_bytes' omission marker.
            let (window, _) = crate::context::window_bytes(body.as_bytes(), BODY_BYTES - 64);
            body = String::from_utf8_lossy(&window).into_owned();
            cut = true;
        }
        let mut label = event.record.label.clone();
        if label.len() > 256 {
            label = rook_llm::truncate(&label, 253);
            cut = true;
        }
        let image_note =
            if event.record.kind == EventKind::ToolResult && event.record.label == crate::browser::TOOL {
                if !admitted {
                    out.issue(format!(
                        "Image attribution not inspected at #{}: result payload was not admitted",
                        event.seq
                    ));
                    None
                } else {
                    out.companion_reads += 1;
                    match crate::tool_images::companion_bounded(rook, &event, METADATA_BYTES) {
                        Ok(note) => note.map(|note| note.seq),
                        Err(error) => {
                            out.issue(format!("Image attribution unavailable at #{}: {error}", event.seq));
                            None
                        }
                    }
                }
            } else {
                None
            };
        out.body_bytes += body.len();
        out.partial_payloads += usize::from(cut);
        // The checker renderer uses text and image attribution, not the UI's
        // measurement/change/detail companions. Do not load those companions.
        out.entries.push(TranscriptEntry {
            seq: event.seq,
            ts: event.record.ts,
            kind: event.record.kind.as_str().into(),
            label,
            object: event.record.body.to_hex(),
            bytes: meta.as_ref().map_or(0, |m| m.size_raw),
            stored_bytes: meta.as_ref().map_or(0, |m| m.size_stored),
            tokens_in: event.record.tokens_in,
            tokens_out: event.record.tokens_out,
            truncated: cut,
            body,
            doing: String::new(),
            tool_measurement: None,
            change_note: None,
            tool_details: None,
            image_note,
        });
    }
    out.unread_before = (before > floor).then_some(before);
    out.entries.reverse();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rook_store::{Kind, NewEvent, ObjectId, SessionMeta};

    fn engine(root: &std::path::Path) -> Rook {
        Rook::from_parts(
            rook_store::Store::open(root.join("store")).unwrap(),
            crate::Config::default(),
            rook_skills::Environment::bare("test", "test", "0.21.5"),
            rook_skills::SkillIndex::default(),
            root.into(),
        )
    }

    #[test]
    fn captured_stage_excludes_previous_work_and_later_appends() {
        let root = tempfile::tempdir().unwrap();
        let rook = engine(root.path());
        let session = rook.start_session("owned").unwrap();
        let old = rook.log(session, EventKind::AssistantMessage, "", "prior stage").unwrap();
        let first = rook.log(session, EventKind::UserMessage, "", "current instruction").unwrap();
        let last = rook.log(session, EventKind::AssistantMessage, "", "current answer").unwrap();
        rook.log(session, EventKind::AssistantMessage, "", "late append").unwrap();
        let excerpt = read_at(&rook, session, first, last + 1);
        assert!(old < first);
        assert_eq!(excerpt.entries.iter().map(|e| e.seq).collect::<Vec<_>>(), [first, last]);
        assert_eq!(excerpt.unread_before, None);
        assert!(excerpt.issues.is_empty());
        assert_eq!(excerpt.body_reads, 2);
    }

    #[test]
    fn excluded_note_flood_reaches_scan_bound_without_resolving_its_bodies() {
        let root = tempfile::tempdir().unwrap();
        let rook = engine(root.path());
        let session = rook.start_session("owned").unwrap();
        let first = rook.log(session, EventKind::AssistantMessage, "", "old proof remains saved").unwrap();
        for _ in 0..SCAN_EVENTS + 8 {
            // Invalid structured content would fail decoding if a display helper ran.
            rook.log(session, EventKind::Note, crate::branches::SUMMARY_LABEL, "not JSON").unwrap();
        }
        let last = rook.log(session, EventKind::AssistantMessage, "", "new proof").unwrap();
        assert!(last - first > SCAN_EVENTS as u64);
        let note = rook.store.events(session, first + 1, 1).unwrap().pop().unwrap();
        assert!(
            crate::transcript::body(&rook, &note, BODY_BYTES).is_err(),
            "the excluded fixture must actually fail the legacy display path"
        );
        let excerpt = read(&rook, session, first);
        assert_eq!(excerpt.metadata_steps, SCAN_EVENTS);
        assert_eq!(excerpt.scanned, SCAN_EVENTS);
        assert_eq!(excerpt.entries.len(), 1);
        assert_eq!(excerpt.entries[0].seq, last);
        assert_eq!(excerpt.body_reads, 1);
        assert!(excerpt.issues.is_empty());
        assert!(excerpt.unread_before.is_some());
        let notice = excerpt.notice(session, first);
        assert!(notice.contains("unread_before_exclusive"));
        assert!(
            notice.contains(&format!("\"around\":{first}}}")),
            "forward-only recall must start at the oldest omitted stage event: {notice}"
        );
        let saved = rook.transcript_entry(session, first, 0).unwrap();
        assert_eq!(saved.entry.body, "old proof remains saved");
    }

    #[test]
    fn visible_flood_bounds_retained_bodies_and_preserves_their_tail() {
        let root = tempfile::tempdir().unwrap();
        let rook = engine(root.path());
        let session = rook.start_session("owned").unwrap();
        let text = format!("{} tail proof", "x".repeat(BODY_BYTES * 3));
        assert!(text.len() > BODY_BYTES);
        for _ in 0..KEEP_ENTRIES + 8 {
            rook.log(session, EventKind::AssistantMessage, "", &text).unwrap();
        }
        let excerpt = read(&rook, session, 0);
        assert_eq!(excerpt.entries.len(), KEEP_ENTRIES);
        assert_eq!(excerpt.body_reads, KEEP_ENTRIES);
        assert_eq!(excerpt.partial_payloads, KEEP_ENTRIES);
        assert!(excerpt.body_bytes <= KEEP_ENTRIES * BODY_BYTES);
        assert!(excerpt.entries.iter().all(|e| e.body.len() <= BODY_BYTES && e.truncated));
        assert!(excerpt.entries.iter().all(|e| e.body.contains("tail proof")));
        assert!(excerpt.unread_before.is_some());
    }

    #[test]
    fn oversized_older_metadata_keeps_good_tail_and_cannot_cross_stage_floor() {
        let root = tempfile::tempdir().unwrap();
        let rook = engine(root.path());
        let session = rook.start_session("owned").unwrap();
        let label = "x".repeat(METADATA_BYTES * 2);
        let old = rook.log(session, EventKind::Note, &label, "owned").unwrap();
        let tail = rook.log(session, EventKind::AssistantMessage, "", "good tail").unwrap();
        assert!(label.len() > METADATA_BYTES);
        let partial = read(&rook, session, old);
        assert_eq!(partial.entries.len(), 1);
        assert_eq!(partial.entries[0].seq, tail);
        assert_eq!(partial.unread_before, Some(tail));
        assert_eq!(partial.issues.len(), 1);
        assert!(partial.notice(session, old).contains("metadata unavailable"));
        let current = read(&rook, session, tail);
        assert!(current.issues.is_empty(), "outside-stage metadata must not be decoded");
        assert_eq!(current.unread_before, None);
    }

    #[test]
    fn large_attachment_frames_are_omitted_before_decode_and_remain_readable() {
        let root = tempfile::tempdir().unwrap();
        let rook = engine(root.path());
        let session = rook.start_session("owned").unwrap();
        let small = crate::attachments::encode("small instruction", &[]).unwrap();
        let large_text = "l".repeat(BODY_BYTES * 2);
        let large = crate::attachments::encode(&large_text, &[]).unwrap();
        assert!(small.len() < BODY_BYTES && large.len() > BODY_BYTES);
        let first = rook.log(session, EventKind::UserMessage, crate::attachments::LABEL, &small).unwrap();
        let last = rook.log(session, EventKind::UserMessage, crate::attachments::LABEL, &large).unwrap();
        let excerpt = read(&rook, session, first);
        assert_eq!(excerpt.body_reads, 1, "large frame must be refused before decode");
        assert_eq!(excerpt.entries.len(), 2);
        assert!(excerpt.entries[0].body.contains("small instruction"));
        assert!(excerpt.entries[1].body.contains(&format!("event #{last}")));
        assert!(excerpt.entries[1].truncated);
        assert_eq!(excerpt.partial_payloads, 1);
        assert!(rook.transcript_entry(session, last, 0).unwrap().entry.body.contains(&large_text));
    }

    #[test]
    fn browser_attribution_checks_caption_without_decoding_pixel_payload() {
        let root = tempfile::tempdir().unwrap();
        let rook = engine(root.path());
        let session = rook.start_session("owned").unwrap();
        // Deliberately opaque bytes: attribution must not decode image content.
        let pixels = b"opaque owned companion";
        let note = rook
            .store
            .append_event(
                session,
                NewEvent::new(EventKind::Note, Kind::Message, pixels).label(crate::tool_images::LABEL),
            )
            .unwrap();
        let caption = format!("capture\n[1 tool image(s); content {}]", ObjectId::of(pixels).to_hex());
        let result = rook.log(session, EventKind::ToolResult, crate::browser::TOOL, &caption).unwrap();
        let excerpt = read(&rook, session, result);
        assert_eq!(excerpt.entries.len(), 1);
        assert_eq!(excerpt.entries[0].image_note, Some(note));
        assert_eq!(excerpt.companion_reads, 1);
        assert!(excerpt.issues.is_empty());
        rook.log(session, EventKind::ToolResult, crate::browser::TOOL, "unrelated answer").unwrap();
        assert_eq!(read(&rook, session, result + 1).entries[0].image_note, None);
    }

    #[test]
    fn unavailable_session_and_oversized_session_metadata_are_explicit() {
        let root = tempfile::tempdir().unwrap();
        let rook = engine(root.path());
        let missing = read(&rook, rook_store::new_session_id(), 0);
        assert!(missing.entries.is_empty());
        assert_eq!(missing.through, None);
        assert!(missing.notice(0, 0).contains("session metadata is unavailable"));
        let session = rook_store::new_session_id();
        let title = "x".repeat(SESSION_BYTES * 2);
        rook.store.create_session(&SessionMeta::new(session, title, "/owned", 0)).unwrap();
        let oversized = read(&rook, session, 0);
        assert_eq!(oversized.through, None);
        assert!(!oversized.issues.is_empty());
        assert!(oversized.notice(session, 0).len() < 4096);
    }

    #[test]
    fn empty_stage_and_invalid_floor_have_distinct_coverage() {
        let root = tempfile::tempdir().unwrap();
        let rook = engine(root.path());
        let session = rook.start_session("owned").unwrap();
        let empty = read_at(&rook, session, 10, 10);
        assert!(empty.entries.is_empty() && empty.issues.is_empty());
        assert_eq!(empty.through, Some(10));
        let invalid = read_at(&rook, session, 11, 10);
        assert!(!invalid.issues.is_empty());
        assert!(invalid.entries.is_empty());
    }

    #[test]
    fn multibyte_labels_and_bodies_stay_valid_inside_the_byte_limits() {
        let root = tempfile::tempdir().unwrap();
        let rook = engine(root.path());
        let session = rook.start_session("owned").unwrap();
        let label = "\u{1F4D6}".repeat(100);
        let text = format!("{} retained tail", "\u{1F4D6}".repeat(BODY_BYTES));
        rook.log(session, EventKind::AssistantMessage, &label, &text).unwrap();
        assert!(label.len() > 256 && text.len() > BODY_BYTES);
        let excerpt = read(&rook, session, 0);
        assert!(excerpt.entries[0].label.len() <= 256);
        assert!(excerpt.entries[0].body.len() <= BODY_BYTES);
        assert!(excerpt.entries[0].body.ends_with("retained tail"));
        assert!(!excerpt.entries[0].body.contains('\u{FFFD}'));
        assert!(excerpt.entries[0].truncated);
    }

    #[test]
    fn oversized_browser_companion_is_reported_without_dropping_its_result() {
        let root = tempfile::tempdir().unwrap();
        let rook = engine(root.path());
        let session = rook.start_session("owned").unwrap();
        let label = "x".repeat(METADATA_BYTES * 2);
        rook.log(session, EventKind::Note, &label, "owned companion").unwrap();
        let result =
            rook.log(session, EventKind::ToolResult, crate::browser::TOOL, "browser answer").unwrap();
        let excerpt = read(&rook, session, result);
        assert_eq!(excerpt.entries.len(), 1);
        assert_eq!(excerpt.entries[0].body, "browser answer");
        assert_eq!(excerpt.entries[0].image_note, None);
        assert_eq!(excerpt.companion_reads, 1);
        assert_eq!(excerpt.issues.len(), 1);
        assert!(excerpt.notice(session, result).contains("Image attribution unavailable"));
    }

    #[test]
    fn unreadable_payload_keeps_an_anchored_placeholder_and_explicit_issue() {
        let root = tempfile::tempdir().unwrap();
        let rook = engine(root.path());
        let session = rook.start_session("owned").unwrap();
        let mut random = 17_u64;
        let bytes: Vec<_> = (0..1024 * 1024 + 4096)
            .map(|_| {
                random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
                (random >> 56) as u8
            })
            .collect();
        let object = ObjectId::of(&bytes);
        let seq = rook
            .store
            .append_event(
                session,
                NewEvent::new(EventKind::ToolResult, Kind::ToolResult, &bytes).label("owned"),
            )
            .unwrap();
        assert!(
            rook.store.stat_object(&object).unwrap().unwrap().external,
            "the fixture must actually exercise an external payload read"
        );
        let hex = object.to_hex();
        let path = root.path().join("store/objects").join(&hex[..2]).join(&hex[2..4]).join(&hex);
        assert!(path.is_file());
        std::fs::write(path, b"owned corrupt fixture").unwrap();
        let excerpt = read(&rook, session, seq);
        assert_eq!(excerpt.entries.len(), 1);
        assert_eq!(excerpt.entries[0].seq, seq);
        assert!(excerpt.entries[0].truncated);
        assert!(excerpt.entries[0].body.contains(&format!("original event #{seq}")));
        assert!(excerpt.notice(session, seq).contains("Payload unavailable"));
        assert_eq!(excerpt.issues.len(), 1);
        assert_eq!(excerpt.body_reads, 1);
    }

    #[test]
    fn oversized_compressed_browser_result_is_not_decoded_for_text_or_caption() {
        let root = tempfile::tempdir().unwrap();
        let rook = engine(root.path());
        let session = rook.start_session("owned").unwrap();
        let pixels = b"opaque owned companion";
        rook.store
            .append_event(
                session,
                NewEvent::new(EventKind::Note, Kind::Message, pixels).label(crate::tool_images::LABEL),
            )
            .unwrap();
        let text = format!(
            "{}\n[1 tool image(s); content {}]",
            "x".repeat(PAYLOAD_BYTES as usize + 4096),
            ObjectId::of(pixels).to_hex()
        );
        let seq = rook.log(session, EventKind::ToolResult, crate::browser::TOOL, &text).unwrap();
        let meta = rook.store.stat_object(&ObjectId::of(text.as_bytes())).unwrap().unwrap();
        assert!(
            meta.size_raw > PAYLOAD_BYTES && meta.size_stored < PAYLOAD_BYTES,
            "the compressed fixture must exceed raw admission while fitting encoded admission"
        );
        let excerpt = read(&rook, session, seq);
        assert_eq!(excerpt.entries.len(), 1);
        assert_eq!(excerpt.body_reads, 0);
        assert_eq!(excerpt.companion_reads, 0, "caption lookup also verifies the entire result object");
        assert_eq!(excerpt.entries[0].image_note, None);
        assert!(excerpt.entries[0].body.contains("Payload omitted before verified decode"));
        assert!(excerpt.entries[0].truncated);
        assert_eq!(excerpt.partial_payloads, 1);
    }
}
