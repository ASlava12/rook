// A local review download assembled from bounded history pages. The browser
// never asks the daemon to write a file or publish a conversation.
import { api } from './lib.js';

const MAX_EVENTS = 512;
const MAX_BODY_BYTES = 8192;
const MAX_HTML_BYTES = 16 * 1024 * 1024;
const encoder = new TextEncoder();

const escapeHtml = value => String(value).replace(/[&<>"'\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/g,
  char => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[char] || '&#xfffd;');

function eventNumber(value, name) {
  if (!Number.isSafeInteger(value) || value < 0) throw new Error(`${name} must be a nonnegative event number.`);
  return value;
}

async function bodyPreview(base, seq, read) {
  let offset = 0, written = 0, body = '';
  while (true) {
    const page = await read(`${base}/${seq}?offset=${offset}`);
    if (page.entry.seq !== seq || page.offset < offset) throw new Error('History entry changed during export.');
    const text = page.entry.body;
    let part = '';
    for (const char of text) {
      const size = encoder.encode(char).length;
      if (written + size > MAX_BODY_BYTES) break;
      part += char;
      written += size;
    }
    body += escapeHtml(part);
    if (part.length < text.length || page.next_offset != null && written >= MAX_BODY_BYTES)
      return { body, shortened: true };
    if (page.next_offset == null) return { body, shortened: false };
    if (page.next_offset <= offset) throw new Error('History entry cursor did not advance.');
    offset = page.next_offset;
  }
}

export async function exportHistoryHtml(session, from = 0, through = null, read = api) {
  eventNumber(from, 'Start');
  if (through != null) eventNumber(through, 'End');
  const base = `/api/sessions/${encodeURIComponent(session)}/history`;
  let page = await read(`${base}?from=${from}&limit=64`);
  const end = eventNumber(page.through, 'History end');
  if (from >= end && !(end === 0 && from === 0)) throw new Error('Export starts after the saved history.');
  if (through == null) through = Math.max(0, end - 1);
  else if (through < from || through >= end) throw new Error('Export end must be a saved event at or after the start.');

  const parts = [];
  let htmlBytes = 0;
  const add = part => {
    const size = encoder.encode(part).length;
    if (size > MAX_HTML_BYTES - htmlBytes) throw new Error('HTML export exceeds 16 MiB; choose a narrower range.');
    htmlBytes += size;
    parts.push(part);
  };
  add(`<!doctype html><html lang="en"><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'"><title>Rook session export</title><style>body{font:16px/1.5 system-ui,sans-serif;max-width:80ch;margin:2rem auto;padding:0 1rem;color:#222}article{border-top:1px solid #bbb;padding:1rem 0}pre{white-space:pre-wrap;overflow-wrap:anywhere;background:#f5f5f5;padding:.8rem}summary{cursor:pointer}.meta,.note{color:#555}</style></head><body><h1>Rook session export</h1><p class="meta">Session ${escapeHtml(session)} · selected events #${from}–#${through} · snapshot ended before #${end}. Conversation history only; current workspace files and test results are not verified by this export.</p>`);
  let events = 0, shortened = 0, cursor = from;
  while (page.items.length) {
    let advanced = false;
    for (const entry of page.items) {
      const seq = eventNumber(entry.seq, 'Event');
      if (seq > through) break;
      if (seq < cursor) throw new Error('History page moved backwards during export.');
      if (events >= MAX_EVENTS) throw new Error(`HTML export exceeds ${MAX_EVENTS} events; choose a narrower range.`);
      const tool = entry.kind === 'tool-call' || entry.kind === 'tool-result';
      add(`<article id="event-${seq}"><h2><a href="#event-${seq}">#${seq}</a> · ${escapeHtml(entry.kind)}${entry.label ? ` · ${escapeHtml(entry.label)}` : ''}</h2>`);
      if (entry.doing) add(`<p class="meta">${escapeHtml(entry.doing)}</p>`);
      if (tool && entry.tool_measurement) {
        const m = entry.tool_measurement;
        add(`<p class="meta">saved ${m.failed ? 'failure' : 'completion'} · dispatch ${escapeHtml(m.duration_ms)} ms · timing #${escapeHtml(m.timing_seq)} (includes waits/hooks; current files/tests not verified)</p>`);
      }
      if (tool && entry.change_note != null) add(`<p class="meta">Saved file changes: source event #${escapeHtml(entry.change_note)}. Read that event in Rook for a bounded historical preview.</p>`);
      if (tool) add('<details><summary>Show tool content</summary>');
      const preview = await bodyPreview(base, seq, read);
      add(`<pre>${preview.body}</pre>`);
      if (preview.shortened) {
        shortened++;
        add(`<p class="note">Body shortened after ${MAX_BODY_BYTES} bytes; inspect event #${seq} in Rook for the rest.</p>`);
      }
      if (tool) add('</details>');
      add('</article>');
      events++;
      cursor = seq + 1;
      advanced = true;
    }
    if (!advanced || cursor > through) break;
    page = await read(`${base}?from=${cursor}&limit=64`);
    if (page.through < end) throw new Error('Saved history shrank during export.');
  }
  if (!events) add('<p>No events in this selected range.</p>');
  add(`<footer><p>${events} event(s) exported; ${shortened} body preview(s) shortened. Generated from saved conversation history.</p></footer></body></html>`);
  return { html: parts.join(''), events, shortened, from, through };
}

export async function downloadHistoryHtml(session, from, through) {
  const result = await exportHistoryHtml(session, from, through);
  const url = URL.createObjectURL(new Blob([result.html], { type: 'text/html;charset=utf-8' }));
  try {
    const link = document.createElement('a');
    link.href = url;
    link.download = `rook-session-${session}-${result.from}-${result.through}.html`;
    document.body.append(link);
    link.click();
    link.remove();
  } finally { setTimeout(() => URL.revokeObjectURL(url), 1000); }
  return result;
}
