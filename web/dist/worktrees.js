// Explicit checkout recovery remains attached to the selected parent/view.
import { el, api } from './lib.js';

export function worktreePanel(parent, children, owned) {
  let generation = 0;
  let pending = false;
  let reviewed = null;
  const child = el('input', { placeholder: 'Delegated child session ID', maxlength: 26,
    'aria-label': 'Worktree child session ID', value: children[0]?.id || '' });
  const selected = () => child.value.trim().toUpperCase();
  const report = el('pre', {});
  const restore = el('button', { disabled: true, onclick: async () => {
    if (!owned() || pending || !reviewed || reviewed.child !== selected()) return;
    const source = reviewed;
    if (!confirm(`Restore missing files in ${source.path} from registered index ${source.index_sha256}?\n\nExisting entries are preserved. Deleted unstaged/untracked files cannot be recovered. This does not complete the child task or verify parent tests.`)) return;
    await run(source.child, source.review_token);
  } }, 'Restore missing checkout');
  const diagnose = el('button', { onclick: async () => {
    const id = selected();
    if (!/^[0-9A-HJKMNP-TV-Z]{26}$/i.test(id)) { report.textContent = 'Enter a complete child session ID.'; return; }
    await run(id, null);
  } }, 'Diagnose checkout');
  child.addEventListener('input', () => { generation++; reviewed = null; restore.disabled = true; });
  async function run(id, token) {
    if (!owned() || pending) return;
    const request = ++generation;
    pending = true; diagnose.disabled = true; restore.disabled = true; reviewed = null;
    try {
      const result = await api(`/api/sessions/${parent}/worktrees/${id}`, token ? { review_token: token } : undefined);
      if (!owned() || request !== generation || selected() !== id) return;
      report.textContent = JSON.stringify(result, null, 2);
      reviewed = result;
      restore.disabled = !result.review_token;
    } catch (error) {
      if (owned() && request === generation) report.textContent = String(error.error || error);
    } finally {
      pending = false;
      if (owned()) diagnose.disabled = false;
    }
  }
  return el('details', {}, el('summary', {}, 'Delegated worktree recovery'),
    el('p', { class: 'sub' }, 'Read-only diagnosis first. Restoration uses the registered child Git index and preserves existing files; child results are historical, not current parent file/test evidence.'),
    el('div', { class: 'row' }, child, diagnose, restore), report);
}
