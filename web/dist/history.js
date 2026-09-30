// One page at a time, including search and the body of a large event. The
// durable cursor belongs to the server; DOM positions never address history.
import { el, api } from './lib.js';

export function historyPanel(session, quote, rewind) {
  const base = `/api/sessions/${encodeURIComponent(session)}/history`;
  const notice = el('p', { class: 'sub', role: 'status', 'aria-live': 'polite' });
  const rows = el('div', { class: 'scroll', 'aria-label': 'History events' });
  const detail = el('div', { 'aria-label': 'Selected event' });
  const paging = el('div', { class: 'row' });
  const root = el('section', { 'aria-label': 'Session history' });
  let pending = false, query = '', page = null;
  const button = (label, action, disabled = false) =>
    el('button', { type: 'button', disabled, onclick: action }, label);
  async function read(path, apply) {
    if (pending) return;
    pending = true;
    root.setAttribute('aria-busy', 'true');
    notice.textContent = 'Reading history…';
    try {
      const result = await api(path);
      if (root.isConnected) { notice.textContent = ''; apply(result); }
    } catch (error) {
      if (root.isConnected) notice.textContent = error.error || String(error);
    } finally { pending = false; root.removeAttribute('aria-busy'); }
  }
  function insert(seq, offset) {
    read(`${base}/${seq}/quote?offset=${offset}`, result => {
      quote(result.text);
      notice.textContent = result.next_offset == null ? 'Quote added to draft.' : 'This part was added to the draft; more text remains in the event.';
    });
  }
  function open(seq, offset = 0) {
    read(`${base}/${seq}?offset=${offset}`, result => {
      const e = result.entry;
      detail.replaceChildren(
        el('h3', {}, `#${e.seq} · ${e.kind} ${e.label}`),
        el('p', { class: 'sub' }, `Bytes ${result.offset}–${result.next_offset ?? result.total_bytes} of ${result.total_bytes}`),
        el('pre', {}, e.body),
        el('div', { class: 'row' },
          button('Previous part', () => open(seq, result.previous_offset), result.previous_offset == null),
          button('Next part', () => open(seq, result.next_offset), result.next_offset == null),
          button('Quote into draft', () => insert(seq, result.offset)),
          rewind ? button(`Rewind to #${seq}`, () => rewind(seq)) : null));
    });
  }
  function row(e, snippet = e.body, offset = 0) {
    return el('article', { class: 'entry' },
      el('div', { class: 'hd' }, button(`#${e.seq} · ${e.kind} ${e.label}`, () => open(e.seq, offset))),
      el('pre', {}, snippet),
      button('Quote into draft', () => insert(e.seq, offset)));
  }
  function load(params = '') {
    read(base + params, result => {
      page = result;
      detail.replaceChildren();
      rows.replaceChildren(...result.items.map(e => row(e)));
      notice.textContent = result.items.length ? `Events #${result.items[0].seq}–#${result.items.at(-1).seq}` : 'No events on this page.';
      paging.replaceChildren(
        button('Older events', () => load(`?before=${result.previous}`), result.previous == null),
        button('Newer events', () => load(`?from=${result.next}`), result.next == null));
      rows.scrollTop = 0;
    });
  }
  function search(cursor = {}) {
    const params = new URLSearchParams({ q: query, ...cursor });
    read(`${base}/search?${params}`, result => {
      detail.replaceChildren();
      rows.replaceChildren(...result.hits.map(e => row(e, e.snippet, e.offset)));
      notice.textContent = `${result.hits.length} matching events on this scan; scanned ${result.scanned_events} events / ${result.scanned_bytes} bytes. ` +
        (result.next ? 'Continue searching for more results.' : 'Search complete.');
      paging.replaceChildren(button('Continue search', () => search(result.next), result.next == null),
        button('Back to history', () => load(page?.items.length ? `?from=${page.items[0].seq}` : '')));
      rows.scrollTop = 0;
    });
  }
  const needle = el('input', { placeholder: 'Find literal text', 'aria-label': 'Search history', maxlength: 256 });
  const number = el('input', { placeholder: 'Event number', 'aria-label': 'Jump to event', inputmode: 'numeric', pattern: '[0-9]+', maxlength: 20 });
  root.append(el('form', { class: 'row', onsubmit: event => {
    event.preventDefault();
    if (pending) return;
    if (!needle.value.trim() || new TextEncoder().encode(needle.value).length > 256) {
      notice.textContent = 'Search needs 1–256 bytes of text.'; return;
    }
    query = needle.value; search();
  } }, needle, el('button', { type: 'submit' }, 'Search'), button('Refresh latest', () => load())),
  el('form', { class: 'row', onsubmit: event => {
    event.preventDefault();
    if (/^[0-9]{1,20}$/.test(number.value)) open(number.value);
  } }, number, el('button', { type: 'submit' }, 'Jump')),
  notice, paging, rows, detail);
  // Let the caller attach the panel before even a cached request can finish.
  queueMicrotask(() => { if (root.isConnected) load(); });
  return root;
}
