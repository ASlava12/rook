// Recall uses the same bounded pages as the model, CLI and TUI. A replaced
// panel cannot receive an old reply or silently move the active conversation.
import { el, api, state, nav } from './lib.js';

export function recallPanel(initialWorkspace = '') {
  let epoch = 0;
  let pending = null;
  const workspace = el('input', { value: initialWorkspace, 'aria-label': 'Conversation workspace' });
  const query = el('input', { value: state.query, maxlength: 256, placeholder: 'Literal text from an earlier conversation…' });
  const results = el('div', { class: 'scroll' });
  const panel = el('div', { class: 'card' }, el('h2', {}, 'Find past conversations'),
    el('p', { class: 'sub' }, 'Search this workspace, then read a saved event. Historical claims do not verify current files.'),
    el('label', {}, 'Workspace ', workspace));

  const read = async request => {
    const token = ++epoch;
    pending?.abort();
    pending = new AbortController();
    const path = `/api/history/recall?workspace=${encodeURIComponent(workspace.value)}`;
    results.replaceChildren(el('p', { class: 'empty' }, 'reading saved conversations…'));
    try {
      const response = await api(path, request, pending.signal);
      if (token !== epoch || !panel.isConnected) return;
      const children = [];
      for (const hit of response.hits || []) {
        children.push(el('div', { class: 'entry' },
          el('div', { class: 'hd' }, hit.title || '(untitled)', ` · ${hit.session} · #${hit.seq} (${hit.kind})`),
          el('pre', {}, hit.snippet),
          el('button', { onclick: () => read({ session: hit.session, around: hit.seq }) }, 'Read around event'),
          el('button', { onclick: () => { ++epoch; pending?.abort(); state.session = hit.session; nav.go('sessions'); } }, 'Open session')));
      }
      if (response.page) {
        children.push(el('p', { class: 'sub' }, `Saved session ${response.session} through #${response.page.through}`));
        for (const entry of response.page.items) {
          children.push(el('div', { class: 'entry' },
            el('div', { class: 'hd' }, `#${entry.seq} ${entry.kind} ${entry.label}`),
            el('pre', {}, entry.body),
            entry.truncated ? el('p', { class: 'sub' }, 'Body shortened; open the session history to read more.') : null));
        }
        children.push(el('button', { onclick: () => { ++epoch; pending?.abort(); state.session = response.session; nav.go('sessions'); } }, 'Open session'));
      } else {
        children.push(el('p', { class: 'sub' }, `${response.hits.length} hit(s) · ${response.scanned_sessions} sessions · ${response.scanned_events} events · ${response.scanned_bytes} bytes scanned · ${response.inherited_prefixes_skipped} inherited prefixes skipped`));
      }
      if (response.next) {
        children.push(el('button', { onclick: () => read({ ...request, cursor: response.next }) }, 'Continue bounded scan'));
      }
      results.replaceChildren(...children);
    } catch (error) {
      if (token === epoch && panel.isConnected && error.name !== 'AbortError') {
        results.replaceChildren(el('p', { class: 'err' }, error.error || String(error)));
      }
    }
  };
  panel.append(el('form', { class: 'ask', onsubmit: event => {
    event.preventDefault(); state.query = query.value.trim(); read({ query: state.query });
  } }, query, el('button', {}, 'Find conversations')), results);
  return panel;
}
