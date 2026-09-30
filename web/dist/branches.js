// Lazy tree pages never infer that switching history restored workspace files.
import { el, api } from './lib.js';
import { historyPanel } from './history.js';

export function branchPanel(session, continueBranch, quote, forkEvent) {
  const root = el('section', { 'aria-label': 'Conversation branches', style: 'overflow-wrap:anywhere' });
  const notice = el('p', { role: 'status', 'aria-live': 'polite', class: 'sub' });
  const rows = el('div', { class: 'scroll', 'aria-label': 'Branch nodes' });
  const controls = el('div', { class: 'row' });
  const history = el('div', { 'aria-label': 'Branch history' });
  let pending = false;
  const button = (text, action, disabled = false) => el('button', { type: 'button', disabled, onclick: action }, text);
  function row(node, depth, selected) {
    const title = (node.title || '(untitled)') + (node.title_truncated ? '…' : '');
    const position = node.forked_at == null ? (node.parent && !node.delegated ? ' · boundary unknown' : '') : node.delegated ? ` · delegated at #${node.forked_at}` : ` · fork before #${node.forked_at}`;
    return el('article', { class: 'entry', style: `margin-left:${Math.min(depth, 8)}rem`, 'aria-label': `Branch ${title}` },
      el('div', { class: 'hd' }, `${selected ? 'Selected: ' : ''}${title}${node.delegated ? ' · delegated task' : ''}${position}`),
      el('p', { class: 'sub' }, `${node.id} · ${node.workspace}${node.workspace_truncated ? '…' : ''}`),
      el('div', { class: 'row' }, button('Explore branch', () => load(node.id)),
      button('Read history', () => {
        if (!pending) history.replaceChildren(historyPanel(node.id, text => quote(node.id, text), undefined,
          forkEvent ? seq => forkEvent(node.id, seq) : undefined));
      }),
      button('Continue in chat', () => { if (!pending) continueBranch(node.id); })));
  }
  async function load(id, after = null) {
    if (pending) return;
    pending = true; root.setAttribute('aria-busy', 'true'); notice.textContent = 'Reading branch links…';
    try {
      const page = await api(`/api/sessions/${encodeURIComponent(id)}/tree` + (after == null ? '' : `?after=${encodeURIComponent(after)}`));
      if (!root.isConnected) return;
      session = page.selected.id;
      history.replaceChildren();
      rows.replaceChildren(...page.ancestors.map((n, depth) => row(n, depth, false)),
        row(page.selected, page.ancestors.length, true),
        ...page.children.map(n => row(n, page.ancestors.length + 1, false)));
      notice.textContent = `${page.children.length} direct children on this scan page; examined ${page.scanned_sessions} sessions.` +
        (page.next ? ' More sessions remain to scan.' : '') +
        (page.missing_parent ? ` Parent no longer available: ${page.missing_parent}.` : '');
      controls.replaceChildren(button('Scan more children', () => load(session, page.next), page.next == null),
        button('Refresh branch', () => load(session)),
        button('Earlier ancestors', () => load(page.earlier_ancestor), page.earlier_ancestor == null));
      rows.scrollTop = 0;
    } catch (error) { if (root.isConnected) notice.textContent = error.error || String(error); }
    finally { pending = false; root.removeAttribute('aria-busy'); }
  }
  root.append(el('p', {}, 'Explore ancestors and direct branches. Browsing and switching leave workspace files as they are; file recovery is a separate action.'), notice, controls, rows, history);
  queueMicrotask(() => {
    if (!root.isConnected) return;
    if (session) load(session); else notice.textContent = 'Start or open a session first.';
  });
  return root;
}
