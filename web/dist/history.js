// One page at a time, including search and the body of a large event. The
// durable cursor belongs to the server; DOM positions never address history.
import { el, api } from './lib.js';

export function historyPanel(session, quote, rewind, branch) {
  const base = `/api/sessions/${encodeURIComponent(session)}/history`;
  const notice = el('p', { class: 'sub', role: 'status', 'aria-live': 'polite' });
  const totals = el('p', { class: 'sub', 'aria-label': 'Recorded turn totals' });
  const rows = el('div', { class: 'scroll', 'aria-label': 'History events' });
  const detail = el('div', { 'aria-label': 'Selected event' });
  const bookmarkRows = el('div', { class: 'scroll', 'aria-label': 'Saved bookmarks' });
  const bookmarkPanel = el('details', {}, el('summary', {}, 'Bookmarks'), bookmarkRows);
  const bookmarkEditor = el('div', { class: 'row', 'aria-label': 'Bookmark editor' });
  const paging = el('div', { class: 'row' });
  const root = el('section', { 'aria-label': 'Session history' });
  let pending = false, query = '', page = null;
  let bookmarkItems = [];
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
  async function branchAt(seq) {
    if (pending) return;
    pending = true;
    notice.textContent = 'Creating a branch; workspace files stay unchanged…';
    try { await branch(seq); }
    catch (error) { if (root.isConnected) notice.textContent = error.error || String(error); }
    finally { pending = false; }
  }
  function drawBookmarks() {
    bookmarkRows.replaceChildren(...(bookmarkItems.length ? bookmarkItems.map(mark =>
      el('div', { class: 'row' },
        button(`#${mark.seq} · ${mark.label}`, () => open(mark.seq), !mark.available),
        !mark.available ? el('span', { class: 'sub' }, 'Event unavailable') : null,
        button('Edit label', () => editBookmark(mark.seq)),
        button('Remove', () => saveBookmark(mark.seq, '')))) :
      [el('p', { class: 'sub' }, 'No bookmarks yet. Mark an event in the history below.')]));
  }
  async function refreshBookmarks() {
    try {
      const page = await api(`/api/sessions/${encodeURIComponent(session)}/bookmarks`);
      bookmarkItems = page.items;
      if (root.isConnected) drawBookmarks();
    } catch (error) { if (root.isConnected) notice.textContent = error.error || String(error); }
  }
  async function saveBookmark(seq, label) {
    if (pending) return;
    pending = true;
    bookmarkEditor.setAttribute('aria-busy', 'true');
    try {
      const page = await api(`/api/sessions/${encodeURIComponent(session)}/bookmarks`, { event: seq, label });
      bookmarkItems = page.items;
      if (root.isConnected) {
        drawBookmarks();
        bookmarkEditor.replaceChildren();
        notice.textContent = label.trim() ? `Bookmarked event #${seq}.` : `Removed bookmark for event #${seq}.`;
      }
    } catch (error) { if (root.isConnected) notice.textContent = error.error || String(error); }
    finally { pending = false; bookmarkEditor.removeAttribute('aria-busy'); }
  }
  function editBookmark(seq) {
    const input = el('input', { type: 'text', maxlength: 128, 'aria-label': `Bookmark label for event #${seq}` });
    input.value = bookmarkItems.find(mark => mark.seq === seq)?.label || '';
    bookmarkEditor.replaceChildren(el('form', { class: 'row', onsubmit: event => {
      event.preventDefault(); saveBookmark(seq, input.value);
    } }, input, el('button', { type: 'submit' }, 'Save bookmark'),
    button('Cancel', () => bookmarkEditor.replaceChildren())));
    input.focus();
  }
  bookmarkPanel.addEventListener('toggle', () => { if (bookmarkPanel.open) refreshBookmarks(); });
  function open(seq, offset = 0) {
    read(`${base}/${seq}?offset=${offset}`, result => {
      const e = result.entry;
      detail.replaceChildren(
        el('h3', {}, `#${e.seq} · ${e.kind} ${e.label}`),
        el('p', { class: 'sub' }, `Bytes ${result.offset}–${result.next_offset ?? result.total_bytes} of ${result.total_bytes}`),
        el('pre', { class: 'body' }, e.body),
        el('div', { class: 'row' },
          button('Previous part', () => open(seq, result.previous_offset), result.previous_offset == null),
          button('Next part', () => open(seq, result.next_offset), result.next_offset == null),
          button('Quote into draft', () => insert(seq, result.offset)),
          button('Mark event', () => editBookmark(seq)),
          branch ? button(e.kind === 'user' ? 'Edit in new branch' : 'Continue after event in new branch', () => branchAt(seq)) : null,
          rewind ? button(`Rewind to #${seq}`, () => rewind(seq)) : null));
    });
  }
  function row(e, snippet = e.body, offset = 0) {
    return el('article', { class: 'entry' },
      el('div', { class: 'hd' }, button(`#${e.seq} · ${e.kind} ${e.label}`, () => open(e.seq, offset))),
      el('pre', {}, snippet),
      button('Quote into draft', () => insert(e.seq, offset)),
      button('Mark event', () => editBookmark(e.seq)),
      branch ? button(e.kind === 'user' ? 'Edit in new branch' : 'Continue after event in new branch', () => branchAt(e.seq)) : null);
  }
  function load(params = '') {
    read(base + params, result => {
      page = result;
      totals.textContent = '';
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
      totals.textContent = '';
      detail.replaceChildren();
      rows.replaceChildren(...result.hits.map(e => row(e, e.snippet, e.offset)));
      notice.textContent = `${result.hits.length} matching events on this scan; scanned ${result.scanned_events} events / ${result.scanned_bytes} bytes. ` +
        (result.next ? 'Continue searching for more results.' : 'Search complete.');
      paging.replaceChildren(button('Continue search', () => search(result.next), result.next == null),
        button('Back to history', () => load(page?.items.length ? `?from=${page.items[0].seq}` : '')));
      rows.scrollTop = 0;
    });
  }
  function turns(before = null) {
    const path = `/api/sessions/${encodeURIComponent(session)}/turns` +
      (before == null ? '' : `?before=${before}`);
    read(path, result => {
      const t = result.totals;
      detail.replaceChildren();
      totals.textContent = `Recorded turns: ${t.turns} (${t.completed} completed). Tokens in/out/cache: ${t.input_tokens}/${t.output_tokens}/${t.cached_tokens}. ` +
        `${t.steps} steps, ${t.elapsed_seconds}s.${t.saturated ? ' Counters saturated.' : ''} ` +
        `Coverage starts at ${result.coverage_from == null ? 'no recorded outcome' : `event #${result.coverage_from}`}; inherited branch turns and attempts without a saved outcome are excluded.`;
      rows.replaceChildren(...result.items.map(entry => {
        const s = entry.summary;
        const body = `${s.turn} · ${s.stopped}\n${s.steps} steps · tokens in/out/cache: ${s.input_tokens}/${s.output_tokens}/${s.cached_tokens} · ${s.files_changed} files\n` +
          `Prompt #${s.prompt_seq ?? 'unknown'} · result #${entry.result_seq}` +
          (s.follow_up ? ` · follow-up ${s.follow_up}` : '') +
          (s.continuation ? ` · continues ${s.continuation}` : '') + `\n\n${s.reply}` +
          (s.reply_truncated ? '\n[preview; open result for full outcome]' : '');
        return row({ seq: entry.result_seq, kind: 'turn', label: s.stopped, body });
      }));
      if (!result.items.length) rows.append(el('p', {}, 'No recorded outcomes on this scan page. Scan older events when available.'));
      paging.replaceChildren(button('Older results', () => turns(result.before), result.before == null),
        button('Latest results', () => turns()), button('Back to history', () => load()));
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
  } }, needle, el('button', { type: 'submit' }, 'Search'), button('Refresh latest', () => load()),
    button('Turn results', () => turns())),
  el('form', { class: 'row', onsubmit: event => {
    event.preventDefault();
    if (/^[0-9]{1,20}$/.test(number.value)) open(number.value);
  } }, number, el('button', { type: 'submit' }, 'Jump')),
  bookmarkPanel, notice, totals, paging, rows, detail, bookmarkEditor);
  // Let the caller attach the panel before even a cached request can finish.
  queueMicrotask(() => { if (root.isConnected) load(); });
  return root;
}
