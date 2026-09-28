// Schedules own timing; each execution is an ordinary goal session.
import { $, el, api, state, nav } from './lib.js';
const label = (text, input) => el('label', {}, text, ' ', input);
const message = e => e.error || e.message || String(e);
const when = (at, zone) => at ? new Date(at * 1000).toLocaleString(undefined, { timeZone: zone }) : '—';

export async function renderTasks() {
  const root = el('section', { class: 'tasks' });
  $('#view').replaceChildren(root);
  const error = el('p', { role: 'status' });
  const list = el('div');
  const goal = el('textarea', { required: true, maxlength: 32768, rows: 3 });
  const timing = el('input', { required: true, value: 'weekdays 09:00', maxlength: 80 });
  const timezone = el('input', { required: true, value: Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC', maxlength: 80 });
  const workspace = el('input', { required: true, maxlength: 4096 });
  const stance = el('select', {}, ...['assist', 'readonly', 'autonomous'].map(value => el('option', { value }, value)));
  const seconds = el('input', { type: 'number', required: true, min: 1, max: 604800, value: 3600 });
  const tokens = el('input', { type: 'number', required: true, min: 1, max: 100000000, value: 100000 });
  const iterations = el('input', { type: 'number', required: true, min: 1, max: 10000, value: 100 });
  const submit = el('button', { type: 'submit' }, 'Create schedule');
  const form = el('form', { class: 'task-form card' }, el('h2', {}, 'New scheduled task'),
    label('Goal', goal), label('Schedule', timing),
    el('p', {}, 'once 2026-12-01 03:00 · every 30m · daily 09:00 · weekdays 09:00 · weekly fri 09:00'),
    label('Timezone', timezone), label('Workspace', workspace), label('Permissions', stance),
    label('Seconds per run', seconds), label('Tokens per run', tokens), label('Iterations per run', iterations), submit);
  let requestId = null, submitting = false;
  form.addEventListener('input', () => { requestId = null; });
  form.addEventListener('submit', async event => {
    event.preventDefault(); if (submitting) return;
    submitting = true; submit.disabled = true;
    // The server uses ULIDs; obtain one from the same timestamp/random shape
    // without asking it to allocate state before the form is submitted.
    requestId ||= ulid();
    try {
      await api('/api/tasks', { id: requestId, spec: { goal: goal.value, timing: timing.value,
        timezone: timezone.value, workspace: workspace.value, stance: stance.value,
        max_seconds: Number(seconds.value), max_tokens: Number(tokens.value), max_iterations: Number(iterations.value) } });
      goal.value = ''; requestId = null; error.textContent = 'Schedule saved'; await refresh();
    } catch (e) { error.textContent = message(e); }
    finally { submitting = false; submit.disabled = false; }
  });
  root.append(el('h2', {}, 'Scheduled tasks'), el('p', {}, 'Each run opens a separate session. The daemon must be running. Missed recurring occurrences are skipped; unfinished sessions prevent overlap. Disable affects future runs only.'), error, list, form);
  let signature = '', busy = false;
  async function control(id, action) {
    try { await api(`/api/tasks/${id}/control`, action); signature = ''; await refresh(); }
    catch (e) { error.textContent = message(e); }
  }
  async function refresh() {
    if (busy) return;
    busy = true;
    try {
      const tasks = await api('/api/tasks');
      if (!root.isConnected) return;
      const next = JSON.stringify(tasks); if (next === signature) return; signature = next;
      list.replaceChildren(...tasks.map(task => {
        const spec = task.spec;
        const history = el('div', {}, el('h3', {}, 'Run history'));
        for (const run of [...task.history].reverse()) history.append(el('p', {},
          `${when(run.at, spec.timezone)} · ${run.status} · ${run.reason} `,
          el('button', { disabled: task.pending === run.session, onclick: () => { state.session = run.session; nav.go('sessions'); } }, task.pending === run.session ? 'Waiting for worker' : 'Open session'),
          run.session === task.history.at(-1)?.session && !['Completed', 'Cancelled'].includes(run.status) ? el('button', { onclick: async () => {
            try { await api(`/api/tasks/${task.id}/control`, 'cancel_run'); signature = ''; await refresh(); }
            catch (e) { error.textContent = message(e); }
          } }, 'Cancel session') : null));
        const remove = el('button', { onclick: async () => {
          try {
            const response = await fetch(`/api/tasks/${task.id}`, { method: 'DELETE' });
            if (!response.ok) throw await response.json();
            signature = ''; await refresh();
          } catch (e) { error.textContent = message(e); }
        } }, 'Delete');
        return el('article', { class: 'card' }, el('h3', {}, spec.goal),
          el('p', {}, `${task.enabled ? 'Enabled' : 'Disabled'} · ${spec.timing} · ${spec.timezone}`),
          el('p', {}, `Next: ${task.enabled ? when(task.next_at, spec.timezone) : '—'} · ${task.note}`),
          el('p', {}, `${spec.workspace} · ${spec.stance} · ${spec.max_seconds}s / ${spec.max_tokens} tokens / ${spec.max_iterations} iterations`),
          el('div', { class: 'row' }, el('button', { onclick: () => control(task.id, 'run_now') }, 'Run now'),
            el('button', { onclick: () => control(task.id, task.enabled ? 'disable' : 'enable') }, task.enabled ? 'Disable' : 'Enable'), remove), history);
      }));
      if (!tasks.length) list.append(el('p', {}, 'No schedules yet.'));
    } catch (e) { error.textContent = `Connection unavailable: ${message(e)}`; }
    finally { busy = false; }
  }
  const health = await api('/api/health');
  if (!workspace.value) workspace.value = health.workspace;
  async function poll() {
    if (!root.isConnected) return;
    await refresh(); if (root.isConnected) setTimeout(poll, 1500);
  }
  await poll();
}
function ulid() {
  const alphabet = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';
  let value = BigInt(Date.now());
  for (const byte of crypto.getRandomValues(new Uint8Array(10))) value = (value << 8n) | BigInt(byte);
  let text = '';
  for (let i = 0; i < 26; i++) { text = alphabet[Number(value & 31n)] + text; value >>= 5n; }
  return text;
}
