//! Bounded conversation artifacts with one export policy for every frontend.
use crate::{CoreError, Rook};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
macro_rules! ensure { ($condition:expr, $($message:tt)*) => { if !$condition { return Err(CoreError::Other(format!($($message)*)).into()); } }; }
macro_rules! bail { ($($message:tt)*) => { return Err(CoreError::Other(format!($($message)*)).into()) }; }

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Profile {
    #[default]
    Share,
    ShareTools,
    Archive,
}
impl Profile {
    pub fn parse(name: &str) -> std::result::Result<Self, String> {
        match name {
            "share" => Ok(Self::Share),
            "share-tools" => Ok(Self::ShareTools),
            "archive" => Ok(Self::Archive),
            _ => Err("export profile must be share, share-tools or archive".into()),
        }
    }
    fn includes(self, entry: &crate::TranscriptEntry) -> bool {
        match self {
            Self::Share => matches!(entry.kind.as_str(), "user" | "assistant"),
            Self::ShareTools => {
                matches!(entry.kind.as_str(), "user" | "assistant" | "tool-call" | "tool-result")
            }
            Self::Archive => !(entry.kind == "note" && entry.label == crate::provider_history::LABEL),
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Request {
    pub from: u64,
    pub through: Option<u64>,
    pub profile: Profile,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Preview {
    pub seq: u64,
    pub kind: String,
    pub body: String,
    pub shortened: bool,
}
/// Small, typed preview arguments; an empty request uses the share profile.
pub fn arguments(text: &str) -> crate::Result<Request> {
    if text.len() > 4096 {
        return Err(CoreError::Other("export preview arguments exceed 4096 bytes".into()));
    }
    Ok(serde_json::from_str(if text.trim().is_empty() { "{}" } else { text })?)
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Export {
    pub html: String,
    pub profile: Profile,
    pub events: usize,
    pub shortened: usize,
    pub from: u64,
    pub through: u64,
    pub omitted: BTreeMap<String, usize>,
    pub preview: Vec<Preview>,
}
impl Export {
    /// Escaped text preview only: never dumps the full HTML into a terminal.
    pub fn preview_json(&self) -> crate::Result<String> {
        Ok(serde_json::to_string_pretty(
            &serde_json::json!({"profile":self.profile,"events":self.events,"shortened":self.shortened,"from":self.from,"through":self.through,"omitted":self.omitted,"preview":self.preview,"notice":"Retained conversation text may contain private information; this is not an anonymity guarantee."}),
        )?)
    }
}
struct Limited(Vec<u8>);
impl Write for Limited {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > (16 * 1024 * 1024usize).saturating_sub(self.0.len()) {
            return Err(std::io::Error::other("HTML export exceeds 16 MiB; choose a narrower range"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Rook {
    /// Prepare an immutable local artifact and omission manifest; never publishes it.
    pub fn conversation_export(&self, session: u128, request: &Request) -> crate::Result<Export> {
        let mut out = Limited(Vec::new());
        let report = write_html(self, session, request.from, request.through, request.profile, &mut out)
            .map_err(|error| CoreError::Other(error.to_string()))?;
        let html = String::from_utf8(out.0).map_err(|error| CoreError::Other(error.to_string()))?;
        Ok(Export {
            html,
            profile: request.profile,
            events: report.events,
            shortened: report.shortened,
            from: report.from,
            through: report.through,
            omitted: report.omitted,
            preview: report.preview,
        })
    }
}
trait HistoryRead {
    fn page(&self, session: u128, from: u64) -> Result<crate::transcript::Page>;
    fn entry(&self, session: u128, seq: u64, offset: u64) -> Result<crate::transcript::EntryPage>;
}
impl HistoryRead for Rook {
    fn page(&self, session: u128, from: u64) -> Result<crate::transcript::Page> {
        Ok(self.transcript_page(
            session,
            &crate::transcript::PageRequest { from: Some(from), before: None, limit: Some(PAGE) },
        )?)
    }
    fn entry(&self, session: u128, seq: u64, offset: u64) -> Result<crate::transcript::EntryPage> {
        Ok(self.transcript_entry(session, seq, offset)?)
    }
}

const PAGE: usize = 64;
const MAX_EVENTS: usize = 512;
const MAX_BODY_BYTES: usize = 8192;

struct Report {
    pub events: usize,
    pub shortened: usize,
    pub from: u64,
    pub through: u64,
    omitted: BTreeMap<String, usize>,
    preview: Vec<Preview>,
}

fn escaped(out: &mut impl Write, text: &str) -> Result<()> {
    for c in text.chars() {
        match c {
            '&' => out.write_all(b"&amp;")?,
            '<' => out.write_all(b"&lt;")?,
            '>' => out.write_all(b"&gt;")?,
            '"' => out.write_all(b"&quot;")?,
            '\'' => out.write_all(b"&#39;")?,
            '\n' | '\r' | '\t' => write!(out, "{c}")?,
            c if c.is_control() => out.write_all(b"&#xfffd;")?,
            _ => write!(out, "{c}")?,
        }
    }
    Ok(())
}

fn body(source: &impl HistoryRead, session: u128, seq: u64, out: &mut impl Write) -> Result<bool> {
    let mut offset = 0u64;
    let mut written = 0usize;
    loop {
        let page = source.entry(session, seq, offset)?;
        ensure!(page.entry.seq == seq && page.offset >= offset, "history entry changed during export");
        let text = page.entry.body;
        let remaining = MAX_BODY_BYTES - written;
        let mut end = text.len().min(remaining);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        escaped(out, &text[..end])?;
        written += end;
        if end < text.len() || page.next_offset.is_some() && written >= MAX_BODY_BYTES {
            return Ok(true);
        }
        let Some(next) = page.next_offset else { return Ok(false) };
        ensure!(next > offset, "history entry cursor did not advance");
        offset = next;
    }
}

fn write_html(
    source: &impl HistoryRead,
    session: u128,
    from: u64,
    through: Option<u64>,
    profile: Profile,
    out: &mut impl Write,
) -> Result<Report> {
    let mut page = source.page(session, from)?;
    let end = page.through;
    ensure!(from < end || end == 0 && from == 0, "export starts after the saved history");
    let through = match through {
        Some(seq) => {
            ensure!(seq < end && seq >= from, "export end must be a saved event at or after --from");
            seq
        }
        None => end.saturating_sub(1),
    };
    write!(
        out,
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'\"><title>Rook session export</title><style>body{{font:16px/1.5 system-ui,sans-serif;max-width:80ch;margin:2rem auto;padding:0 1rem;color:#222}}article{{border-top:1px solid #bbb;padding:1rem 0}}pre{{white-space:pre-wrap;overflow-wrap:anywhere;background:#f5f5f5;padding:.8rem}}summary{{cursor:pointer}}.meta,.note{{color:#555}}</style></head><body><h1>Rook session export</h1><p class=\"meta\">Session {} · selected events #{}–#{} · snapshot ended before #{}. Conversation history only; current workspace files and test results are not verified by this export.</p>",
        rook_store::format_session_id(session),
        from,
        through,
        end
    )?;
    let mut report =
        Report { events: 0, shortened: 0, from, through, omitted: BTreeMap::new(), preview: Vec::new() };
    write!(
        out,
        "<p>Profile: {:?}. Retained conversation text may contain private information; this artifact is not guaranteed anonymous.</p>",
        profile
    )?;
    let mut scanned = 0;
    let mut cursor = from;
    loop {
        if page.items.is_empty() {
            break;
        }
        let mut advanced = false;
        for entry in &page.items {
            if entry.seq > through {
                break;
            }
            ensure!(entry.seq >= cursor, "history page moved backwards during export");
            if scanned >= MAX_EVENTS {
                bail!("HTML export exceeds {MAX_EVENTS} events; choose a narrower --from/--through range");
            }
            scanned += 1;
            cursor = entry.seq.saturating_add(1);
            advanced = true;
            if !profile.includes(entry) {
                *report.omitted.entry(entry.kind.clone()).or_default() += 1;
                continue;
            }
            if report.preview.len() < 8 {
                let mut end = entry.body.len().min(512);
                while !entry.body.is_char_boundary(end) {
                    end -= 1;
                }
                report.preview.push(Preview {
                    seq: entry.seq,
                    kind: entry.kind.clone(),
                    body: entry.body[..end].into(),
                    shortened: entry.truncated || end < entry.body.len(),
                });
            }
            let tool = matches!(entry.kind.as_str(), "tool-call" | "tool-result");
            write!(
                out,
                "<article id=\"event-{}\"><h2><a href=\"#event-{}\">#{}</a> · ",
                entry.seq, entry.seq, entry.seq
            )?;
            escaped(out, &entry.kind)?;
            if profile == Profile::Archive && !entry.label.is_empty() {
                out.write_all(" · ".as_bytes())?;
                escaped(out, &entry.label)?;
            }
            out.write_all(b"</h2>")?;
            if profile != Profile::Share && !entry.doing.is_empty() {
                out.write_all(b"<p class=\"meta\">")?;
                escaped(out, &entry.doing)?;
                out.write_all(b"</p>")?;
            }
            if tool {
                if let Some(note) = entry.change_note {
                    write!(
                        out,
                        "<p class=\"meta\">Saved file changes: source event #{note}. Read that event in Rook for a bounded historical preview.</p>"
                    )?;
                }
                if let Some(measurement) = entry.tool_measurement {
                    out.write_all(b"<p class=\"meta\">")?;
                    escaped(out, &measurement.text())?;
                    out.write_all(b"</p>")?;
                }
                if let Some(details) = &entry.tool_details {
                    out.write_all(b"<p class=\"meta\">")?;
                    escaped(out, &details.text())?;
                    out.write_all(b"</p>")?;
                }
                out.write_all(b"<details><summary>Show tool content</summary>")?;
            }
            out.write_all(b"<pre>")?;
            let shortened = body(source, session, entry.seq, out)?;
            out.write_all(b"</pre>")?;
            if shortened {
                report.shortened += 1;
                write!(
                    out,
                    "<p class=\"note\">Body shortened after {MAX_BODY_BYTES} bytes; inspect event #{} in Rook for the rest.</p>",
                    entry.seq
                )?;
            }
            if tool {
                out.write_all(b"</details>")?;
            }
            out.write_all(b"</article>")?;
            report.events += 1;
            cursor = entry.seq.saturating_add(1);
            advanced = true;
        }
        if !advanced || cursor > through {
            break;
        }
        page = source.page(session, cursor)?;
        ensure!(page.through >= end, "saved history shrank during export");
    }
    if report.events == 0 {
        out.write_all(b"<p>No events in this selected range.</p>")?;
    }
    out.write_all(b"<aside><h2>Omission manifest</h2><ul>")?;
    for (kind, count) in &report.omitted {
        out.write_all(b"<li>")?;
        escaped(out, kind)?;
        write!(out, ": {count} omitted</li>")?;
    }
    out.write_all(b"</ul></aside>")?;
    write!(
        out,
        "<footer><p>{} event(s) exported; {} body preview(s) shortened. Generated from saved conversation history.</p></footer></body></html>",
        report.events, report.shortened
    )?;
    Ok(report)
}
