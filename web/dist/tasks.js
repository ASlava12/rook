// Durable work lives in rookd. Polling only observes it; closing this tab stops no task.
import { $, el, api, state, md } from './lib.js';

const drafts = new Map();
const label = (text, input) => el('label', {}, text, ' ', input);
const when = seconds => new Date(seconds * 1000).toLocaleString();
const message = error => error.error || error.message || String(error);

export async function renderTasks() {
  const root = el('section', { class: 'tasks' });
  $('#view').replaceChildren(root);
  const error = el('p', { role: 'status', 'aria-live': 'polite' });
  const list = el('div', { class: 'row' });
  const details = el('div', { class: 'card' });
  const goal = el('textarea', { rows: 3, required: true, maxlength: 32768, 'aria-label': 'Task goal' });
  const workspace = el('input', { 'aria-label': 'Task workspace', placeholder: 'Daemon workspace' });
  const autonomous = el('input', { type: 'checkbox' });
  const days = el('input', { type: 'number', min: 0, step: 'any', value: 7, required: true });
  const tokens = el('input', { type: 'number', min: 0, step: 1, value: 0, required: true });
  const start = el('button', { type: 'submit' }, 'Start task');
  const form = el('form', { class: 'task-form card' }, el('h2', {}, 'New background task'), label('Goal', goal),
    label('Workspace', workspace), label('Days (0 = no time limit)', days),
    label('Tokens (0 = no total limit)', tokens),
    label('Approve operations allowed by the deny list', autonomous), start);
  form.addEventListener('submit', async event => {
    event.preventDefault(); start.disabled = true;
    try {
      const run = await api('/api/work', { goal: goal.value, workspace: workspace.value || null,
        autonomous: autonomous.checked, max_seconds: Math.round(Number(days.value) * 86400),
        max_tokens: Number(tokens.value) });
      state.task = run.id; goal.value = ''; selected = null; await refresh();
    } catch (e) { error.textContent = message(e); }
    finally { start.disabled = false; }
  });
  root.append(el('h2', {}, 'Background tasks'),
    el('p', {}, 'Tasks continue after you close the page. After a daemon restart, runnable tasks resume from saved state. Pause and cancel stop at a safe boundary.'),
    error, list, form, details);

  let selected = null, status, history, receipts, correction, submit, actions, retryKey = null;
  let generation = 0, submitting = false, lastSent = null;
  let previousList = null, previousRun = null;
  function select(id) {
    if (selected && correction) drafts.set(selected, { text: correction.value, key: retryKey });
    selected = id; previousRun = null;
    const draft = drafts.get(id) || { text: '', key: null };
    retryKey = draft.key;
    status = el('div'); history = el('div'); receipts = el('div'); actions = el('div', { class: 'row' });
    correction = el('textarea', { rows: 3, maxlength: 8192, required: true, 'aria-label': 'Correction for this task' });
    correction.value = draft.text;
    correction.addEventListener('input', () => {
      // Editing creates a new instruction. An unchanged retry keeps the old id.
      retryKey = null;
      drafts.set(id, { text: correction.value, key: null });
    });
    submit = el('button', { type: 'submit' }, 'Send correction');
    const steering = el('form', { class: 'task-form' }, label('Correction', correction), submit);
    steering.addEventListener('submit', async event => {
      event.preventDefault(); submitting = true; submit.disabled = true;
      retryKey ||= crypto.randomUUID();
      const text = correction.value, key = retryKey;
      drafts.set(id, { text, key });
      try {
        await api(`/api/work/${id}/steer`, { id: key, text });
        if (selected === id && correction.value === text) { correction.value = ''; retryKey = null; }
        drafts.delete(id); lastSent = { task: id, message: key };
        error.textContent = 'Correction saved. Waiting for the agent to take it into context.';
        await refresh();
      } catch (e) { error.textContent = `Instruction ${key}: ${message(e)}. Retry unchanged to avoid duplicates.`; }
      finally { submitting = false; if (selected === id) submit.disabled = correction.disabled; }
    });
    details.replaceChildren(status, actions, steering,
      el('p', {}, '“In context” acknowledges delivery, not completion. Pending messages survive a disconnect or restart. For a paused or blocked task, send guidance and choose Resume.'),
      receipts, history);
  }

  async function refresh() {
    const request = ++generation;
    const runs = await api('/api/work');
    if (!root.isConnected || request !== generation) return;
    for (const key of drafts.keys()) if (!runs.some(run => run.id === key)) drafts.delete(key);
    const listSignature = JSON.stringify([runs.map(run => [run.id, run.status, run.goal]), state.task]);
    if (listSignature !== previousList) {
    previousList = listSignature;
    list.replaceChildren(...runs.map(run => el('button', {
      onclick: () => { state.task = run.id; refresh().catch(e => { error.textContent = message(e); }); },
      'aria-pressed': state.task === run.id,
    }, `${run.status} · ${run.goal.slice(0, 100)}`)));
    if (!runs.length) list.append(el('p', {}, 'No tasks yet.'));
    }
    const id = runs.some(run => run.id === state.task) ? state.task : runs[0]?.id;
    if (!id) return;
    state.task = id;
    if (selected !== id) select(id);
    const run = await api(`/api/work/${id}`);
    if (!root.isConnected || selected !== id || request !== generation) return;
    if (error.textContent.startsWith('Connection unavailable:')) error.textContent = '';
    const acknowledged = lastSent?.task === id && run.instructions.find(item => item.id === lastSent.message && item.applied_at);
    if (acknowledged) { error.textContent = 'Correction taken into the agent’s context. Execution is not yet confirmed.'; lastSent = null; }
    const runSignature = JSON.stringify(run);
    if (runSignature === previousRun) return;
    previousRun = runSignature;
    status.replaceChildren(el('h2', {}, run.goal), el('p', {}, `${run.status}: ${run.reason}`),
      el('p', {}, `${run.iterations} iterations · ${run.tokens} tokens · ${run.workspace}`),
      el('p', {}, `Task ${run.id}${run.session ? ` · Session ${run.session}` : ''}`));
    if (run.next_attempt_at) status.append(el('p', {}, `Next retry: ${when(run.next_attempt_at)}`));
    actions.replaceChildren();
    const terminal = ['completed', 'cancelled'].includes(run.status);
    for (const action of terminal ? [] : ['pause', 'resume', 'cancel']) {
      actions.append(el('button', { onclick: async event => {
        event.target.disabled = true;
        try { await api(`/api/work/${id}/control`, action); await refresh(); }
        catch (e) { error.textContent = message(e); event.target.disabled = false; }
      } }, action));
    }
    if (terminal) actions.append(el('button', { onclick: async event => {
      event.target.disabled = true;
      try {
        const response = await fetch(`/api/work/${id}`, { method: 'DELETE' });
        if (!response.ok) throw await response.json();
        state.task = null; selected = null; drafts.delete(id); details.replaceChildren(); await refresh();
      } catch (e) { error.textContent = message(e); event.target.disabled = false; }
    } }, 'Forget task record'));
    correction.disabled = terminal; submit.disabled = terminal || submitting;
    receipts.replaceChildren(el('h3', {}, 'Corrections'), ...run.instructions.map(item =>
      el('p', {}, `${item.applied_at ? `✓ In context since ${when(item.applied_at)}` : 'Pending'} · ${item.id}\n${item.text}`)));
    history.replaceChildren(el('h3', {}, 'Latest answer'), md(run.reply || 'No finished iteration yet.'),
      el('h3', {}, 'Independent verification'), md(run.verification || 'Not yet verified.'));
  }

  let refreshing = false;
  // A serial loop prevents overlapping responses from overwriting newer state.
  const poll = async () => {
    if (!root.isConnected) return;
    if (!refreshing) {
      refreshing = true;
      try { await refresh(); }
      catch (e) { error.textContent = `Connection unavailable: ${message(e)}. Reconnecting…`; }
      finally { refreshing = false; }
    }
    if (root.isConnected) setTimeout(poll, 1500);
  };
  await poll();
}
