// Saved history only: one bounded body part per action, shared by live and saved cards.
import { el, api } from './lib.js';

const imageResponseBytes = Math.ceil(2 * 1024 * 1024 / 3) * 4 + 1024;
let visiblePixels = null;
let pixelRequest = null;
async function readImage(path, signal) {
  const response = await fetch(path, { signal });
  const maximum = response.ok ? imageResponseBytes : 4096;
  if (Number(response.headers.get('content-length')) > maximum) {
    await response.body?.cancel();
    throw new Error('Saved image response exceeds its byte limit.');
  }
  const reader = response.body.getReader();
  const bytes = new Uint8Array(maximum);
  let length = 0;
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      if (value.length > maximum - length) throw new Error('Saved image response exceeds its byte limit.');
      bytes.set(value, length); length += value.length;
    }
  } catch (error) { await reader.cancel().catch(() => {}); throw error; }
  finally { reader.releaseLock(); }
  const result = JSON.parse(new TextDecoder().decode(bytes.subarray(0, length)));
  if (!response.ok) throw result;
  return result;
}

export function savedToolCard(session, e, open) {
  const base = `/api/sessions/${encodeURIComponent(session)}/history`;
  const notice = el('p', { class: 'sub', role: 'status' });
  const button = (label, action, disabled = false) =>
    el('button', { type: 'button', disabled, onclick: action }, label);
  let pending = false;
  async function read(path, apply, reader = api) {
    if (pending) { notice.textContent = 'Reader is busy; try again after it finishes.'; return; }
    pending = true;
    notice.textContent = 'Reading saved history...';
    try {
      const result = await reader(path);
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
  const savedImages = el('div', { class: 'saved-images' });
  const pixels = el('div', { class: 'saved-pixels' });
  let imageNote = null;
  let ownRequest = null;
  function hidePixels() {
    pixels.replaceChildren();
    if (visiblePixels?.deref() === pixels) visiblePixels = null;
    ownRequest?.abort();
  }
  function imageControls(note, index = 0, count = null) {
    if (note == null) { hidePixels(); savedImages.replaceChildren(); imageNote = null; return; }
    imageNote = note;
    savedImages.replaceChildren(...[
      button(`Show saved image ${index + 1}`, () => showImage(index)),
      count > 1 ? button('Previous image', () => showImage(index - 1), index === 0) : null,
      count > 1 ? button('Next image', () => showImage(index + 1), index + 1 >= count) : null,
      button('Hide image', hidePixels),
    ].filter(Boolean));
  }
  async function showImage(index) {
    if (pending) { notice.textContent = 'Reader is busy; try again after it finishes.'; return; }
    // One decoded picture across the page. Weak ownership cannot keep removed
    // history cards (and their former parents) alive after a snapshot replaces them.
    pixelRequest?.abort(); visiblePixels?.deref()?.replaceChildren(); visiblePixels = null;
    const controller = new AbortController();
    pixelRequest = controller; ownRequest = controller;
    const deadline = setTimeout(() => controller.abort(), 60000);
    try {
      await read(`${base}/${e.seq}/images/${index}`, result => {
        const image = result.image;
        if (!Number.isSafeInteger(result.note_seq) || result.note_seq < 0 || result.note_seq !== imageNote || result.index !== index || !Number.isInteger(result.count) || result.count < 1 || result.count > 4 || index >= result.count ||
            !image || !['image/png', 'image/jpeg', 'image/webp', 'image/gif'].includes(image.mime_type) ||
            typeof image.data !== 'string' || image.data.length > imageResponseBytes - 1024 || !/^[A-Za-z0-9+/]*={0,2}$/.test(image.data) ||
            !Number.isInteger(image.width) || image.width < 1 || image.width > 4096 || !Number.isInteger(image.height) || image.height < 1 || image.height > 4096) {
          throw new Error('Saved image source or payload is invalid.');
        }
        if (controller.signal.aborted) return;
        pixels.replaceChildren(
          el('p', { class: 'sub' }, `Session ${session} · result #${e.seq} · image source #${result.note_seq} · image ${index + 1} of ${result.count}. Historical tool output; current files and tests are not verified.`),
          el('img', { src: `data:${image.mime_type};base64,${image.data}`, alt: `Saved tool image ${index + 1}`, style: 'max-width:100%;max-height:50vh;object-fit:contain' }),
        );
        visiblePixels = new WeakRef(pixels);
        imageControls(imageNote, index, result.count);
      }, path => readImage(path, controller.signal));
    } finally {
      clearTimeout(deadline);
      if (pixelRequest === controller) pixelRequest = null;
      if (ownRequest === controller) ownRequest = null;
    }
  }
  imageControls(e.image_note);
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
      if (result.entry.image_note !== imageNote) imageControls(result.entry.image_note);
      body.textContent = result.entry.body;
      meta.textContent = `Bytes ${result.offset}–${result.next_offset ?? result.total_bytes} of ${result.total_bytes}. Saved history; current files and test results are not verified here.`;
      parts.replaceChildren(...[
        button('Previous part', () => part(result.previous_offset), result.previous_offset == null),
        button('Next part', () => part(result.next_offset), result.next_offset == null),
        open ? button('Open event', () => open(e.seq, result.offset)) : null].filter(Boolean));
    });
  }
  card.addEventListener('toggle', () => { if (card.open && !loaded) part(0); if (!card.open) hidePixels(); });
  card.append(summary, meta, notice, body, parts, savedChanges, savedImages, pixels);
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
