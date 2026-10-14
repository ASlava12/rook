import { el, api, jsonWithin } from './lib.js';

export function shellUndoPanel(owner) {
  let epoch = 0;
  let pending = false;
  const content = el('div');
  const panel = el('section', {}, el('h2', {}, 'Shell changes and undo'),
    el('p', { class: 'sub' }, 'Select files to restore to their saved state before a command. Newer edits cause a conflict. Excluded directories are outside the saved scope.'));
  const render = (...nodes) => content.replaceChildren(...nodes.filter(node => node != null));
  const request = async action => {
    if (pending) return;
    if (!jsonWithin(action, 65536)) {
      render(el('p', {}, 'Request exceeds 64 KiB; select fewer files or shorten the recovery note.'));
      return;
    }
    pending = true;
    const generation = ++epoch;
    render(el('p', {}, 'Reading shell receipt…'));
    try {
      const reply = await api(`/api/sessions/${encodeURIComponent(owner)}/shell-undo`, action);
      if (generation !== epoch || !panel.isConnected) return;
      if (reply.items) {
        render(reply.items.length ? el('div', {}, ...reply.items.map(receipt =>
          el('button', { onclick: () => request({ action: 'inspect', receipt: receipt.id }) }, `${receipt.id} · ${receipt.state}`))) :
          el('p', {}, 'No shell receipts. Enable shell_undo in configuration before running commands.'));
      } else if (reply.token) {
        const prepared = { receipt: reply.receipt, paths: [...reply.paths], token: reply.token };
        render(el('p', {}, 'Restore these selected files? Existing files return to their saved contents and permissions; files created by the command are removed.'),
          el('pre', {}, prepared.paths.join('\n')),
          el('button', { onclick: () => request({ action: 'restore', ...prepared }) }, 'Restore selected files'),
          el('button', { onclick: () => request({ action: 'inspect', receipt: prepared.receipt }) }, 'Back to receipt'));
      } else if (reply.restored) {
        render(el('p', {}, 'Restored files:'), el('pre', {}, reply.restored.join('\n')),
          el('button', { onclick: () => request({ action: 'inspect', receipt: reply.receipt }) }, 'Inspect restore receipt'));
      } else {
        show(reply);
      }
    } catch (error) {
      if (generation === epoch && panel.isConnected) render(el('p', { class: 'warn' }, String(error.error || error)),
        action.receipt ? el('button', { onclick: () => request({ action: 'inspect', receipt: action.receipt }) }, 'Inspect receipt after failure') : null);
    } finally {
      if (generation === epoch) pending = false;
    }
  };
  const show = receipt => {
    const restored = new Set(receipt.restored_paths || []);
    const changed = (receipt.changed_paths || []).filter(path => !restored.has(path));
    const paths = el('textarea', { maxlength: 65536, 'aria-label': 'Shell restore file paths', placeholder: 'Exact file paths, one per line' });
    const note = el('textarea', { maxlength: 4096, 'aria-label': 'Shell recovery inspection note', placeholder: 'What you inspected and how remaining processes were stopped' });
    const attempts = receipt.restore_attempts || [];
    render(el('p', {}, `${receipt.id} · ${receipt.state}`),
      receipt.error ? el('p', { class: 'warn' }, receipt.error) : null,
      el('p', { class: 'sub' }, `Excluded entry names: ${(receipt.excluded_entry_names || []).join(', ') || 'none'}`),
      el('p', {}, `Changed files still available for selection (${changed.length}):`),
      el('pre', {}, changed.slice(0, 128).join('\n')),
      changed.length > 128 ? el('p', {}, 'Showing the first 128 paths. The CLI inspect action lists all changed paths.') : null,
      ...attempts.map(attempt => el('p', { class: 'sub' }, `${attempt.state}: restored ${(attempt.completed || []).join(', ') || 'none'}${attempt.pending ? `; pending result: ${attempt.pending}` : ''}${attempt.error ? `; ${attempt.error}` : ''}`)),
      paths, el('button', { onclick: () => request({ action: 'prepare', receipt: receipt.id,
        paths: paths.value.split('\n').filter(path => path.length > 0) }) }, 'Preview selected restore'),
      !receipt.after_source || attempts.some(attempt => attempt.state === 'started' || attempt.state === 'partial') ? el('div', {},
        el('p', {}, 'Stop remaining processes and inspect execution recovery first. Recording recovery observes current files; it does not mark the interrupted command successful.'), note,
        el('button', { onclick: () => request({ action: 'recover', receipt: receipt.id, note: note.value }) }, 'Record inspected recovery')) : null);
  };
  panel.append(el('button', { onclick: () => request({ action: 'list' }) }, 'List shell receipts'), content);
  return panel;
}
