// Lazy tree pages never infer that switching history restored workspace files.
import { el, api } from './lib.js';
import { historyPanel } from './history.js';

export function branchPanel(session, continueBranch, quote, forkEvent, renamed) {
  const departed = session;
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
    const edit = el('div', { class: 'row' });
    const heading = el('div', { class: 'hd' }, `${selected ? 'Selected: ' : ''}${title}${node.delegated ? ' · delegated task' : ''}${position}`);
    const article = el('article', { class: 'entry', style: `margin-left:${Math.min(depth, 8)}rem`, 'aria-label': `Branch ${title}` },
      heading,
      el('p', { class: 'sub' }, `${node.id} · ${node.workspace}${node.workspace_truncated ? '…' : ''}`),
      el('div', { class: 'row' }, button('Explore branch', () => load(node.id)),
      button('Read history', () => {
        if (!pending) history.replaceChildren(historyPanel(node.id, text => quote(node.id, text), undefined,
          forkEvent ? seq => forkEvent(node.id, seq) : undefined));
      }),
      button('Continue in chat', () => { if (!pending) continueBranch(node.id); }),
      button('Carry reviewed summary', () => {
        if (pending || !departed || node.id === departed) return;
        const input = el('textarea', { rows: 5, 'aria-label': `Summary of departed branch ${departed}` });
        edit.replaceChildren(el('p', { class: 'sub' },
          `Summarize saved conversation ${departed} for ${node.id}. Historical file and test claims must be checked in the current workspace.`),
        input, button('Save summary and continue', async () => {
          if (pending) return;
          const text = input.value.trim();
          if (!text || new TextEncoder().encode(text).length > 16384) {
            notice.textContent = 'Summary must be 1–16384 UTF-8 bytes.';
            return;
          }
          pending = true; root.setAttribute('aria-busy', 'true');
          try {
            const saved = await api(`/api/sessions/${encodeURIComponent(node.id)}/summary`, { source: departed, text });
            if (root.isConnected) {
              notice.textContent = `Saved source-attributed summary at event #${saved.event}.`;
              continueBranch(node.id);
            }
          } catch (error) {
            if (root.isConnected) notice.textContent = `${error.error || String(error)} Check target history before retrying an uncertain save.`;
          } finally { pending = false; root.removeAttribute('aria-busy'); }
        }), button('Cancel', () => edit.replaceChildren()));
        input.focus();
      }, !departed || node.id === departed),
      button('Rename branch', () => {
        if (pending) return;
        const input = el('input', { type: 'text', value: node.title, maxlength: 4096, 'aria-label': `New name for branch ${node.id}` });
        edit.replaceChildren(input, button('Save name', async () => {
          if (pending) return;
          pending = true;
          root.setAttribute('aria-busy', 'true');
          try {
            const updated = await api(`/api/sessions/${encodeURIComponent(node.id)}/rename`, { title: input.value });
            node.title = updated.title;
            const shown = updated.title + (updated.title_truncated ? '…' : '');
            heading.textContent = `${selected ? 'Selected: ' : ''}${shown}${node.delegated ? ' · delegated task' : ''}${position}`;
            article.setAttribute('aria-label', `Branch ${shown}`);
            edit.replaceChildren();
            if (root.isConnected) notice.textContent = `Renamed branch ${node.id}.`;
            renamed?.(updated);
          } catch (error) { if (root.isConnected) notice.textContent = error.error || String(error); }
          finally { pending = false; root.removeAttribute('aria-busy'); }
        }), button('Cancel', () => edit.replaceChildren()));
        input.focus();
      })), edit);
    return article;
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
