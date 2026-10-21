import { el, api } from './lib.js';

// Read-only navigation never changes chat state or the current draft.
export function backgroundPanel(session) {
  const root = el('section', { 'aria-label': 'Session family background work' });
  const notice = el('p', { role: 'status', 'aria-live': 'polite' });
  const rows = el('div', { class: 'scroll' });
  const controls = el('div', { class: 'row' });
  let pending = false;
  const button = (label, action, disabled = false) => el('button', { type: 'button', disabled, onclick: action }, label);
  async function load(id, after = null) {
    if (pending) return;
    if (!id) { notice.textContent = 'Start or select a session to inspect background work.'; return; }
    pending = true; root.setAttribute('aria-busy', 'true'); notice.textContent = 'Reading background metadata…';
    try {
      const page = await api(`/api/sessions/${encodeURIComponent(id)}/background` + (after ? `?after=${encodeURIComponent(after)}` : ''));
      if (!root.isConnected) return;
      const tree = page.tree;
      const nodes = [...tree.ancestors, tree.selected, ...tree.children];
      rows.replaceChildren(...nodes.map((node, index) => {
        const entry = page.entries.find(e => e.session === node.id);
        const activity = entry?.activity;
        const phase = activity?.retry_at != null && activity.stage === 'queued' ? `waiting to retry at ${new Date(Number(activity.retry_at) * 1000).toLocaleString()}` : activity?.stage;
        const owner = entry?.owner_live == null ? 'Executor unverified · saved state'
          : entry.owner_live ? 'Live executor' : 'No observed executor';
        const summary = owner + (activity ? ` · ${phase} · idle ${activity.idle_secs}s${activity.stalled ? ' · stalled' : ''}` : '')
          + (entry?.managed_status ? ` · saved goal ${entry.managed_status}` : '');
        return el('article', { class: 'entry', style: `margin-left:${Math.min(index, tree.ancestors.length + 1, 8)}rem` },
          el('div', { class: 'hd' }, `${node.id === tree.selected.id ? 'Selected: ' : ''}${node.title || '(untitled)'}${node.title_truncated ? '…' : ''}${node.delegated ? ' · delegated task' : ''}`),
          el('p', { class: 'sub' }, node.id), el('p', {}, summary),
          ...page.jobs.filter(job => job.session === node.id).map(job => el('p', {},
            `${job.id} · ${job.exit_code == null ? 'running (owned registry)' : `driver exited ${job.exit_code}`}${job.quiet_for_secs == null ? '' : ` · quiet ${job.quiet_for_secs}s`}${job.exit_code != null && job.processes_stopped === false ? ' · process cleanup unverified' : ''}`)),
          button('Explore family here', () => load(node.id)));
      }));
      notice.textContent = `Examined ${tree.scanned_sessions} sessions; showing ancestors and direct children. Jobs registry ${page.jobs_observed ? 'observed' : 'unavailable'}; ${page.omitted_jobs} jobs omitted, ${page.unassigned_jobs} unassigned.`
        + (tree.next ? ' More sessions remain to scan.' : '') + (tree.missing_parent ? ` Missing parent: ${tree.missing_parent}.` : '');
      controls.replaceChildren(button('Refresh', () => load(tree.selected.id)),
        button('Scan more children', () => load(tree.selected.id, tree.next), !tree.next),
        button('Earlier ancestors', () => load(tree.earlier_ancestor), !tree.earlier_ancestor));
    } catch (error) { if (root.isConnected) notice.textContent = error.error || String(error); }
    finally { pending = false; root.removeAttribute('aria-busy'); }
  }
  root.append(el('p', {}, 'Explore a child to see deeper descendants. Saved scheduling state does not prove a live executor.'), notice, controls, rows);
  queueMicrotask(() => { if (root.isConnected) load(session); });
  return root;
}
