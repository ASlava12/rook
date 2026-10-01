//! A local, bounded review artifact assembled from the same paged history in
//! direct and daemon modes. It never publishes the conversation.

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};

use crate::source::Source;

pub(crate) trait HistoryRead {
    fn page(&self, session: u128, from: u64) -> Result<rook_core::transcript::Page>;
    fn entry(&self, session: u128, seq: u64, offset: u64) -> Result<rook_core::transcript::EntryPage>;
}
impl HistoryRead for Source {
    fn page(&self, session: u128, from: u64) -> Result<rook_core::transcript::Page> {
        self.transcript_page(
            session,
            &rook_core::transcript::PageRequest { from: Some(from), before: None, limit: Some(PAGE) },
        )
    }
    fn entry(&self, session: u128, seq: u64, offset: u64) -> Result<rook_core::transcript::EntryPage> {
        self.transcript_entry(session, seq, offset)
    }
}
impl HistoryRead for rook_core::Rook {
    fn page(&self, session: u128, from: u64) -> Result<rook_core::transcript::Page> {
        Ok(self.transcript_page(
            session,
            &rook_core::transcript::PageRequest { from: Some(from), before: None, limit: Some(PAGE) },
        )?)
    }
    fn entry(&self, session: u128, seq: u64, offset: u64) -> Result<rook_core::transcript::EntryPage> {
        Ok(self.transcript_entry(session, seq, offset)?)
    }
}

const PAGE: usize = 64;
const MAX_EVENTS: usize = 512;
const MAX_BODY_BYTES: usize = 8192;

pub(crate) struct Report {
    pub events: usize,
    pub shortened: usize,
    pub from: u64,
    pub through: u64,
}

pub(crate) fn save(
    source: &impl HistoryRead,
    session: u128,
    from: u64,
    through: Option<u64>,
    output: &Path,
) -> Result<Report> {
    let parent = output.parent().filter(|path| !path.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("cannot create an export beside {}", output.display()))?;
    let report = {
        let mut writer = BufWriter::new(temporary.as_file_mut());
        let report = write_html(source, session, from, through, &mut writer)?;
        writer.flush()?;
        report
    };
    temporary.as_file().sync_all()?;
    temporary.persist_noclobber(output).map_err(|error| {
        anyhow::anyhow!(
            "cannot save {} without replacing an existing file: {}",
            output.display(),
            error.error
        )
    })?;
    Ok(report)
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
    let mut report = Report { events: 0, shortened: 0, from, through };
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
            if report.events >= MAX_EVENTS {
                bail!("HTML export exceeds {MAX_EVENTS} events; choose a narrower --from/--through range");
            }
            let tool = matches!(entry.kind.as_str(), "tool-call" | "tool-result");
            write!(
                out,
                "<article id=\"event-{}\"><h2><a href=\"#event-{}\">#{}</a> · ",
                entry.seq, entry.seq, entry.seq
            )?;
            escaped(out, &entry.kind)?;
            if !entry.label.is_empty() {
                out.write_all(" · ".as_bytes())?;
                escaped(out, &entry.label)?;
            }
            out.write_all(b"</h2>")?;
            if !entry.doing.is_empty() {
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
    write!(
        out,
        "<footer><p>{} event(s) exported; {} body preview(s) shortened. Generated from saved conversation history.</p></footer></body></html>",
        report.events, report.shortened
    )?;
    Ok(report)
}

/// `NEW_FILE` may contain spaces. An optional `FROM..THROUGH` prefix selects
/// an inclusive range; all other text is the destination path.
pub(crate) fn slash_arguments(rest: &str) -> Result<(u64, Option<u64>, PathBuf)> {
    let rest = rest.trim();
    ensure!(!rest.is_empty(), "use /export-html [FROM..THROUGH] NEW_FILE");
    let (first, path) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    if let Some((from, through)) = first.split_once("..")
        && !from.is_empty()
        && !through.is_empty()
        && from.bytes().all(|byte| byte.is_ascii_digit())
        && through.bytes().all(|byte| byte.is_ascii_digit())
    {
        ensure!(!path.trim().is_empty(), "use /export-html [FROM..THROUGH] NEW_FILE");
        return Ok((from.parse()?, Some(through.parse()?), PathBuf::from(path.trim())));
    }
    Ok((0, None, PathBuf::from(rest)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_escapes_markup_and_controls() {
        let mut out = Vec::new();
        escaped(&mut out, "<script x=\"&\">'\0\n").unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "&lt;script x=&quot;&amp;&quot;&gt;&#39;&#xfffd;\n");
    }

    #[test]
    fn slash_range_keeps_the_rest_of_a_path() {
        let (from, through, path) = slash_arguments("2..8 review with spaces.html").unwrap();
        assert_eq!((from, through), (2, Some(8)));
        assert_eq!(path, PathBuf::from("review with spaces.html"));
        assert_eq!(slash_arguments("my review.html").unwrap().2, PathBuf::from("my review.html"));
        assert!(slash_arguments("2..8 ").is_err());
    }
}
