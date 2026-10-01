// Saved history only: one bounded body part per action, shared by live and saved cards.
import { el, api } from './lib.js';

export function savedToolCard(session, e, open) {
  const base = `/api/sessions/${encodeURIComponent(session)}/history`;
  const notice = el('p', { class: 'sub', role: 'status' });
  const button = (label, action, disabled = false) =>
    el('button', { type: 'button', disabled, onclick: action }, label);
  let pending = false;
  async function read(path, apply) {
    if (pending) { notice.textContent = 'Reader is busy; try again after it finishes.'; return; }
    pending = true;
    notice.textContent = 'Reading saved history...';
    try {
      const result = await api(path);
      if (card.isConnected) { notice.textContent = ''; apply(result); }
    } catch (error) {
      if (card.isConnected) notice.textContent = error.error || String(error);
    } finally { pending = false; }
  }

  const card = el('details', { class: 'entry tool-card' });
  const title = `#${e.seq} · ${e.kind} · ${e.doing || e.label || 'tool'}${e.bytes == null ? '' : ` · ${e.bytes} stored bytes`}`;
  const summary = el('summary');
  function measured(measurement, details) {
    const status = measurement ? `saved ${measurement.failed ? 'failure' : 'completion'} · dispatch ${measurement.duration_ms} ms · timing #${measurement.timing_seq} (includes waits/hooks)` : 'saved status/duration unavailable';
    summary.textContent = title + (e.kind === 'tool-result' ? ` · ${status}${details ? ` · ${toolDetailsText(details)}` : ''}` : '');
  }
  measured(e.tool_measurement, e.tool_details);
  const body = el('pre', { class: 'body' });
  const parts = el('div', { class: 'row' });
  const savedChanges = el('div');
  let changeSource = null;
  function changes(seq) {
    if (seq == null || seq === changeSource) return;
    changeSource = seq;
    const preview = el('details', { class: 'saved-diff' });
    const text = el('pre', { class: 'body' });
    const controls = el('div', { class: 'row' });
    const source = el('p', { class: 'sub' }, 'Historical tool-reported preview; current files and tests are not verified.');
    let opened = false;
    function load(offset) {
      read(`${base}/${seq}?offset=${offset}`, result => {
        if (!preview.isConnected) return;
        opened = true;
        text.replaceChildren(...result.entry.body.split('\n').map(line =>
          el('span', { class: line.startsWith('+') ? 'diff-added' : line.startsWith('-') ? 'diff-removed' : line.startsWith('@@') ? 'diff-hunk' : null }, `${line}\n`)));
        source.textContent = `Source event #${seq} · bytes ${result.offset}–${result.next_offset ?? result.total_bytes} of ${result.total_bytes}. Historical tool-reported preview; current files and tests are not verified.`;
        controls.replaceChildren(...[
          button('Previous diff part', () => load(result.previous_offset), result.previous_offset == null),
          button('Next diff part', () => load(result.next_offset), result.next_offset == null),
          open ? button('Open change event', () => open(seq, result.offset)) : null].filter(Boolean));
      });
    }
    preview.addEventListener('toggle', () => { if (preview.open && !opened) load(0); });
    preview.append(el('summary', {}, `Saved file changes · source event #${seq}`), source, text, controls);
    savedChanges.replaceChildren(preview);
  }
  changes(e.change_note);
  const meta = el('p', { class: 'sub' }, 'Open to read one bounded part of the saved event.');
  let loaded = false;
  function part(offset) {
    read(`${base}/${e.seq}?offset=${offset}`, result => {
      if (!card.isConnected) return;
      loaded = true;
      measured(result.entry.tool_measurement, result.entry.tool_details);
      changes(result.entry.change_note);
      body.textContent = result.entry.body;
      meta.textContent = `Bytes ${result.offset}–${result.next_offset ?? result.total_bytes} of ${result.total_bytes}. Saved history; current files and test results are not verified here.`;
      parts.replaceChildren(...[
        button('Previous part', () => part(result.previous_offset), result.previous_offset == null),
        button('Next part', () => part(result.next_offset), result.next_offset == null),
        open ? button('Open event', () => open(e.seq, result.offset)) : null].filter(Boolean));
    });
  }
  card.addEventListener('toggle', () => { if (card.open && !loaded) part(0); });
  card.append(summary, meta, notice, body, parts, savedChanges);
  return card;
}

export function toolDetailsText(d) {
  let text;
  if (d.type === 'command') {
    text = d.timed_out ? 'command timed out; no completed exit status' : d.running ? 'background command started/running; no completed exit status' : d.exit_code == null ? 'command exit status unavailable' : `command exit ${d.exit_code}`;
  } else if (d.type === 'search') {
    text = `search ${d.matches} matching lines in ${d.files_scanned} scanned files${d.complete ? '' : ' (partial scan; more may exist)'}`;
  } else if (d.type === 'mcp') {
    text = `MCP ${d.server} / ${d.remote_tool}`;
    for (const [key, label] of [['text_blocks', 'text'], ['resource_blocks', 'resource'], ['unsupported_blocks', 'unsupported']]) {
      if (d[key] != null) text += ` · ${d[key]} ${label} block(s)`;
    }
    if (d.text_blocks == null) text += ' · content types unavailable';
    for (const image of d.images) text += ` · image ${image.mime_type} ${image.width}×${image.height} (pixels retained in history)`;
  } else return 'saved tool details unavailable';
  return `saved ${text} · details #${d.note_seq}`;
}
