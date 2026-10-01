// The chat: one socket for the tab's lifetime, a session that can be resumed,
// a turn that can be stopped, and the agent's questions answered in place.
import { $, el, api, ago, md, state, nav, notify, askToNotify, jsonWithin } from './lib.js';
import { historyPanel } from './history.js';
import { branchPanel } from './branches.js';
import { mcpPanel } from './mcp.js';
import { queuePanel, takeRestored } from './queue.js';
import { pendingSubmission, submissionError, submitSteering } from './submission.js';
import { promptRetry } from './prompt-retry.js';
import { stopRetry } from './stop-retry.js';

// Scrollback, not the record: the session holds every word of this and the
// sessions tab reads it back, so a tab left open for a day need not keep an
// afternoon of turns in the document to stay recoverable.
const MAX_SCROLLBACK_BLOCKS = 2000;

let socket = null;
// The assistant's current block, re-rendered from its whole text on every
// delta so a fence or a list that arrives in pieces still ends up drawn.
let current = null;
let retainedInputs = new Map();
const retryPrompt = promptRetry();
const retryStop = stopRetry();
let goalGeneration = null;
let goalObserved = false;
let turnId = null;

const chatOut = () => $('#stream');

function block(kind, ...kids) {
  const out = chatOut();
  if (!out) return null;
  const n = el('div', { class: kind }, ...kids);
  out.append(n);
  while (out.childElementCount > MAX_SCROLLBACK_BLOCKS) out.firstElementChild.remove();
  out.scrollTop = out.scrollHeight;
  return n;
}

function say(kind, text) {
  current = null;
  return block(kind, text);
}

// Receipt metadata is kept on bounded scrollback nodes, not in a second history.
function receiptNotice(receipt, text) {
  if (receipt.session !== state.chat.session) return;
  const out = chatOut();
  if (!out) return;
  let row = [...out.querySelectorAll('[data-receipt]')].find(node => node.dataset.receipt === receipt.reference);
  if (row) {
    const revision = BigInt(row.dataset.revision);
    const incoming = BigInt(receipt.revision);
    if (incoming < revision || (incoming === revision && row.dataset.status !== 'queued')) return;
  } else row = say('stat', '');
  row.dataset.receipt = receipt.reference;
  row.dataset.revision = String(receipt.revision);
  row.dataset.status = receipt.status;
  row.textContent = `[${receipt.reference} · r${receipt.revision} · ${receipt.status}] ${text}`;
  current = null;
}

function receiveQueueReceipt(session, entry, submittedText) {
  const input = $('#chat-input');
  if (submittedText !== undefined && session === state.chat.session &&
      (input?.value ?? state.chat.draft ?? '').trim() === submittedText) {
    if (input) input.value = '';
    state.chat.draft = '';
  }
  const r = entry.receipt;
  receiptNotice({ session, reference: entry.reference, revision: r.revision,
    status: r.applied_at !== null ? 'accepted' : r.withdrawn_at !== null ? 'withdrawn' : 'queued' }, r.text);
}

function saidByModel(text) {
  if (!current || !current.isConnected) {
    current = block('md', '');
    // A running turn keeps streaming while history is open on another tab.
    // Its durable events remain readable even when there is no chat viewport.
    if (!current) return;
    current.dataset.text = '';
  }
  current.dataset.text += text;
  current.replaceChildren(md(current.dataset.text));
  const out = chatOut();
  if (out) out.scrollTop = out.scrollHeight;
}

function setTitle() {
  document.title = state.chat.waiting ? '? rook' : state.chat.busy ? '● rook' : 'rook';
}

/// The page is watching a turn — whether it asked for one or joined one.
///
/// The mirror of `done`, and by the same route: the controls are found by id,
/// because a turn can now be joined from the socket's own handler, which is
/// nowhere near the composer that made them.
function working() {
  state.chat.busy = true;
  setTitle();
  const send = $('#send'), stop = $('#stop');
  if (send) send.textContent = '…';
  if (stop) stop.hidden = false;
  renderPromptRetry();
  renderStopRetry();
}

function renderPromptRetry() {
  const visible = !!retryPrompt.candidate() && !state.chat.busy;
  for (const id of ['#retry-prompt', '#discard-prompt']) {
    const button = $(id);
    if (button) button.hidden = !visible;
  }
}

function renderStopRetry() {
  const visible = !!retryStop.candidate() || !!retryStop.error();
  for (const id of ['#retry-stop', '#discard-stop']) {
    const button = $(id);
    if (button) button.hidden = !visible;
  }
}

// The one line a call in flight keeps, rewritten rather than repeated.
let callStatus = null;
function stillGoing(text) {
  if (callStatus && callStatus.isConnected) callStatus.textContent = text;
  else callStatus = say('stat', text);
}

function done() {
  retainedInputs.clear();
  state.chat.busy = false;
  state.chat.waiting = false;
  current = null;
  // Whatever the turn was waiting for goes with it. An `Allow once` button for
  // a turn that has ended is a control that does nothing, and nothing about it
  // says so — the terminal had the same fault, where it was worse, because an
  // approval there holds the keyboard.
  for (const open of document.querySelectorAll('.approve, .ask-form')) {
    open.replaceWith(el('div', { class: 'stat' }, 'the turn ended, so what it was waiting for is gone'));
  }
  setTitle();
  const send = $('#send'), stop = $('#stop');
  if (send) send.textContent = 'Send';
  if (stop) stop.hidden = true;
  renderPromptRetry();
  renderStopRetry();
}

export function connect() {
  if (socket && socket.readyState <= 1) return socket;
  socket = new WebSocket(`${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/api/chat?live_snapshots=true`);
  const connection = socket;
  socket.onmessage = (event) => {
    if (socket !== connection) return;
    const e = JSON.parse(event.data);
    switch (e.type) {
      case 'inputs': {
        const active = new Set([
          ...e.approvals.map(id => `approval:${id}`),
          ...e.questions.map(id => `question:${id}`),
        ]);
        for (const node of document.querySelectorAll('#stream [data-input-key]')) {
          if (!active.has(node.dataset.inputKey)) node.replaceWith(el('div', { class: 'stat' }, 'request resolved'));
        }
        retainedInputs = new Map([...retainedInputs].filter(([key]) => active.has(key)));
        state.chat.waiting = active.size > 0;
        setTitle();
        break;
      }
      case 'snapshot': {
        goalGeneration = null;
        goalObserved = false;
        turnId = null;
        const active = new Set([
          ...e.approvals.map(id => `approval:${id}`),
          ...e.questions.map(id => `question:${id}`),
        ]);
        const out = chatOut();
        const visible = out ? [...out.querySelectorAll('[data-input-key]')] : [];
        const candidates = [...retainedInputs, ...visible.map(node => [node.dataset.inputKey, node])];
        retainedInputs = new Map(candidates.filter(([key]) => active.has(key)));
        if (out) out.replaceChildren();
        current = null; callStatus = null;
        state.chat.session = e.session;
        state.chat.spent = null; state.chat.context = null; state.chat.modelRequest = null;
        state.chat.waiting = false;
        if (e.running) working(); else done();
        say('stat', e.running ? '[joined a turn already running here]' : '[nothing is running in this session]');
        if (e.truncated) say('stat', '[live view refreshed; saved conversation is in history]');
        renderSettings(); renderPicker();
        break;
      }
      case 'started': goalGeneration = null; goalObserved = false; turnId = null; retryPrompt.started(e.session); state.chat.session = e.session; state.chat.spent = null; state.chat.context = null; state.chat.modelRequest = null; renderSettings(); renderPicker(); break;
      // Joined a turn this page did not start. Said out loud either way: a
      // page that quietly starts streaming looks like it is answering
      // something you did not ask, and one that says nothing after asking
      // cannot be told from a daemon that did not hear.
      case 'attached':
        goalGeneration = null;
        goalObserved = false;
        turnId = null;
        if (state.chat.session !== e.session) {
          state.chat.context = null; state.chat.modelRequest = null;
          state.chat.spent = null;
          renderSettings();
        }
        state.chat.session = e.session;
        renderPicker();
        if (e.running) { say('stat', '[joined a turn already running here]'); working(); }
        break;
      case 'goal': goalGeneration = e.generation; goalObserved = true; break;
      case 'turn': turnId = e.id; break;
      case 'stop_applied':
        retryStop.settled(e.id);
        renderStopRetry();
        break;
      case 'follow_up':
        turnId = null;
        say('stat', `Starting follow-up ${e.id}`); callStatus = null;
        state.chat.spent = null; state.chat.context = null; state.chat.modelRequest = null;
        working(); renderSettings(); break;
      case 'text': saidByModel(e.text); break;
      case 'reasoning': say('think', e.text); break;
      // A sub-agent working, which is not the model thinking.
      case 'agent': if (e.receipt) receiptNotice(e.receipt, e.text); else say('agent', e.text); break;
      // `doing` says which file, which command; a daemon older than the
      // field sends nothing and the name is what it always said.
      case 'tool': callStatus = null; say('tool', `· ${e.doing || e.name}`); break;
      // A call taking a while, saying whether anything is happening in it. On
      // the same line each time, because it is a state and not a log: a call
      // that runs for ten minutes would otherwise leave a hundred and twenty
      // lines saying the same thing in different numbers.
      case 'tool_working': stillGoing(`  ${e.name}: ${e.said}`); break;
      case 'tool_done': {
        callStatus = null;
        const last = chatOut() && chatOut().lastElementChild;
        if (last && last.className === 'tool') last.append(e.failed ? ' ✗' : ' ✓');
        current = null;
        break;
      }
      case 'model_request': state.chat.modelRequest = e; renderSettings(); break;
      case 'settings': state.chat.settings = e; renderSettings(); break;
      case 'spent': state.chat.spent = e; renderSettings(); break;
      case 'context': state.chat.context = e; renderSettings(); break;
      case 'remembered': say('stat', `remembered: ${e.text}`); break;
      case 'forgot': say('stat', `forgot: ${e.text}`); break;
      case 'failed': retryPrompt.disconnected(); say('err', e.message); done(); break;
      case 'error': say('err', e.message); break;
      case 'cancelled': retryPrompt.disconnected(); say('stat', '[stopped]'); done(); break;
      case 'interjected':
        if (e.receipt) receiptNotice(e.receipt, e.text);
        else { say('you', `› ${e.text}`); say('stat', '(the turn will see this at its next step)'); }
        break;
      case 'approval': askApproval(e); break;
      case 'ask': askUser(e); break;
      // Which step of the budget it is on, where the button already says it
      // is busy: `…` says something is happening and not how much room is
      // left, and a turn at 190 of 200 is about to stop mid-task.
      case 'step': {
        const send = $('#send');
        if (send && state.chat.busy) send.textContent = `${e.at}/${e.of}`;
        break;
      }
      case 'done': {
        if (e.stopped === 'already_admitted') { retryPrompt.settled(); done(); break; }
        retryPrompt.completed(state.chat.session);
        if (typeof e.reply === 'string' && current?.dataset.text !== e.reply) {
          current = null;
          saidByModel(e.reply);
        }
        // A turn that ran out of steps and one that finished read the same
        // without this, and they are not the same thing to whoever asked.
        if (e.stopped && e.stopped !== 'end_turn' && e.stopped !== 'stop') {
          say('stat', e.stopped === 'max_steps'
            ? 'stopped at the step limit — raise [agent] max_steps or narrow the task'
            : `the turn ended as "${e.stopped}" rather than finishing`);
        }
        // What it wrote, before what it cost: a turn that says it did the work
        // and one that did it read the same from the outside.
        const wrote = e.files_changed || [];
        if (wrote.length) {
          say('stat', wrote.length === 1 ? `wrote ${wrote[0]}`
            : `wrote ${wrote.length} files: ${wrote.slice(0, 5).join(', ')}` +
              (wrote.length > 5 ? `, and ${wrote.length - 5} more` : ''));
        }
        say('stat', `[${e.steps} steps · ${e.input_tokens} in / ${e.output_tokens} out` +
          (e.compactions ? ` · ${e.compactions} compactions` : '') +
          (e.delegated.length ? ` · ${e.delegated.length} sub-agent(s)` : '') + ']');
        // Sorted by what they are: a decision is settled, an open question is
        // waiting for whoever reads this — and is worth a notification.
        for (const d of e.decisions || []) block('decided', el('span', { class: 'tag' }, 'decided'), ' ', d);
        for (const q of e.open_questions || []) block('open', el('span', { class: 'tag' }, 'open question'), ' ', q);
        if ((e.open_questions || []).length) notify('rook: an open question', e.open_questions[0]);
        done();
        break;
      }
      default: break;
    }
  };
  // A turn belongs to the daemon, so one may be running in this session that
  // this page has never seen — after a reload, or after the tab was closed and
  // opened again. Asking is how it finds out; before the turn outlived the
  // socket there was nothing to ask about.
  socket.addEventListener('open', () => {
    if (socket === connection && state.chat.session) connection.send(JSON.stringify({ type: 'attach', session: state.chat.session }));
  }, { once: true });
  socket.onclose = () => {
    if (socket === connection) {
      retryPrompt.disconnected();
      say('err', retryPrompt.candidate()
        ? 'disconnected; use Retry saved prompt if its delivery is uncertain' : 'disconnected');
      if (retryStop.candidate()) say('stat', 'Stop delivery is uncertain; reconnect, inspect the session, then use Retry saved Stop');
      done();
    }
  };
  return socket;
}

function send(message) {
  const s = connect();
  const deliver = () => s.send(JSON.stringify(message));
  if (s.readyState === 1) deliver(); else s.addEventListener('open', deliver, { once: true });
}

export function stop() {
  if (!state.chat.busy) return;
  if (!goalObserved) {
    say('err', 'Wait for the current goal identity before stopping this turn');
    return;
  }
  if (goalGeneration === null) {
    if (!turnId) { say('err', 'Wait for the current turn identity before stopping it'); return; }
    try {
      const { frame, persisted } = retryStop.remember(state.chat.session, null, turnId);
      say('stat', `Stop ID ${frame.id} for ordinary turn ${turnId}${persisted ? '' : '; browser storage unavailable, keep this ID'}`);
      renderStopRetry();
      send(frame);
    } catch (error) { say('err', String(error)); }
    return;
  }
  try {
    const { frame, persisted } = retryStop.remember(state.chat.session, goalGeneration);
    const note = goalGeneration
      ? `Stop ID ${frame.id}; retry with rook task pause ${state.chat.session} --control-id ${frame.id} --generation ${goalGeneration}`
      : `Stop ID ${frame.id} for session ${state.chat.session}`;
    say('stat', persisted ? note : `${note}; browser storage unavailable, keep this ID`);
    renderStopRetry();
    send(frame);
  } catch (error) { say('err', String(error)); }
}

export async function retrySavedStop() {
  const saved = retryStop.candidate();
  if (!saved) { say('err', retryStop.error() || 'No saved Stop to retry'); return; }
  if (state.chat.session !== saved.session) {
    say('err', `Open session ${saved.session} before retrying its Stop`);
    return;
  }
  if (saved.turn) {
    try {
      send(retryStop.retry());
      say('stat', `Retried Stop ${saved.id} for ordinary turn ${saved.turn}; awaiting daemon acknowledgement`);
    } catch (error) { say('err', `Saved Stop ${saved.id} still needs inspection: ${error}`); }
    return;
  }
  const path = `/api/work/${encodeURIComponent(saved.session)}`;
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 30000);
  try {
    const current = await api(path, undefined, controller.signal);
    if (current.generation !== saved.generation) throw new Error('The saved Stop belongs to an earlier goal generation');
    const outcome = await api(`${path}/control`,
      { id: saved.id, generation: saved.generation, action: 'pause' }, controller.signal);
    if (outcome.id !== saved.id || outcome.generation !== saved.generation ||
        typeof outcome.already_applied !== 'boolean' || typeof outcome.run?.status !== 'string') {
      throw new Error('Stop control acknowledgement is incomplete');
    }
    retryStop.settled(saved.id);
    renderStopRetry();
    say('stat', outcome.already_applied
      ? `Stop ${saved.id} was already applied; current goal: ${outcome.run.status}`
      : `Stop ${saved.id} applied; current goal: ${outcome.run.status}`);
  } catch (error) { say('err', `Saved Stop ${saved.id} still needs inspection: ${error?.error || error?.message || error}`); }
  finally { clearTimeout(timeout); }
}

function waitingOn(what) {
  state.chat.waiting = true;
  setTitle();
  notify(`rook needs you: ${what}`, 'the turn is waiting for an answer');
}
function answered() {
  state.chat.waiting = false;
  setTitle();
}

// A recovered request with the same id is the same form. Keep partially typed
// answers and selected choices while replacing the surrounding live transcript.
function reuseInput(kind, id) {
  const key = `${kind}:${id}`, out = chatOut();
  if (!out) return false;
  const existing = [...out.querySelectorAll('[data-input-key]')]
    .find(node => node.dataset.inputKey === key);
  const kept = existing || retainedInputs.get(key);
  if (!kept) return false;
  retainedInputs.delete(key);
  if (!existing) out.append(kept);
  state.chat.waiting = true;
  setTitle();
  return true;
}

function askApproval(request) {
  if (reuseInput('approval', request.id)) return;
  waitingOn(`${request.tool} wants to ${request.action}`);
  const decide = (decision) => {
    send({ type: 'approval', id: request.id, decision });
    box.replaceWith(el('div', { class: 'stat' }, `approval: ${decision}`));
    answered();
  };
  // The family is named on the button rather than called "this kind": what it
  // allows is the difference between answering once and answering all
  // afternoon, and nobody presses a button for a category with no visible edge.
  const kinds = (request.kind || []).map((k) => `\`${k}\``).join(', ');
  const box = block('approve',
    el('span', {}, `${request.tool} wants to ${request.action}`),
    ...(request.preview ? [el('pre', { class: 'preview' }, request.preview)] : []),
    el('button', { onclick: () => decide('once') }, 'Allow once'),
    el('button', { onclick: () => decide('for_run') }, 'Always this one'),
    ...(kinds ? [el('button', { onclick: () => decide('kind_for_run') }, `Every ${kinds}`)] : []),
    el('button', { onclick: () => decide('deny') }, 'Deny'));
  if (box) box.dataset.inputKey = `approval:${request.id}`;
}

// One form for every question in the call: the agent asked them together
// because they are independent, and a chain of dialogs would undo that.
function askUser(request) {
  if (reuseInput('question', request.id)) return;
  waitingOn(request.questions[0] ? request.questions[0].question : 'a question');
  const fields = request.questions.map((q, i) => {
    const name = `q${request.id}_${i}`;
    const rows = q.choices.map((choice) => el('label', {},
      el('input', { type: q.multi ? 'checkbox' : 'radio', name, value: choice }), ` ${choice}`));
    // Always a free-text row: the answer worth having is often not on the list.
    const other = el('input', { type: 'text', name: `${name}_other`,
      placeholder: q.choices.length ? 'or type your own answer' : 'your answer' });
    return el('fieldset', {}, el('p', {}, q.question), ...rows, other);
  });
  const submit = (answers) => {
    send({ type: 'answers', id: request.id, answers });
    form.replaceWith(el('div', { class: 'stat' },
      answers.map((a) => `answered: ${a.join(', ') || '(skipped)'}`).join('\n')));
    answered();
  };
  const typed = (q, i) => {
    const name = `q${request.id}_${i}`;
    // A typed answer wins: someone who wrote past the options meant to.
    const own = form.querySelector(`[name="${name}_other"]`).value.trim();
    return own ? [own] : [...form.querySelectorAll(`[name="${name}"]:checked`)].map((n) => n.value);
  };
  const form = el('form', { class: 'ask-form', onsubmit: (e) => {
      e.preventDefault();
      submit(request.questions.map(typed));
    } },
    ...fields,
    el('button', { type: 'submit' }, 'Answer'),
    el('button', { type: 'button', onclick: () => submit(request.questions.map(() => [])) }, 'Skip'));
  form.dataset.inputKey = `question:${request.id}`;
  const out = chatOut();
  if (out) { out.append(form); out.scrollTop = out.scrollHeight; }
}

// The selects are built from the lists the server sent, so a stance added to
// the engine appears here without the page knowing its name.
function renderSettings() {
  const bar = $('#settings');
  const s = state.chat.settings;
  if (!bar || !s) return;
  const pick = (name, values, selected) => el('label', {},
    `${name === 'effort' ? 'requested effort' : name} `,
    el('select', { onchange: (e) => send({ type: 'setting', name, value: e.target.value }) },
      values.map(v => el('option', { value: v, selected: v === selected }, v))));
  const spent = state.chat.spent;
  const controls = [
    pick('stance', s.stances && s.stances.length ? s.stances : [s.mode], s.mode),
    pick('effort', s.efforts && s.efforts.length ? s.efforts : [s.effort], s.effort),
    // Only where there is a choice. A daemon with nothing under `[models]`
    // sends an empty list, and so does one older than the field — a select with
    // one option in it is a control that does nothing.
    //
    // The one in use goes in front where it is not among them, which is the
    // ordinary case for a configuration still written as `provider/model`: a
    // select that does not contain its own current value shows the wrong answer
    // and changes it on the first click.
    (() => {
      const named = s.models || [];
      const all = named.includes(s.model) ? named : [s.model, ...named];
      return all.length > 1 ? pick('model', all, s.model) : null;
    })(),
    // Beside the settings rather than in the transcript: it changes on every
    // step, and a running total that scrolled away would be no use.
    state.chat.modelRequest ? el('span', { class: 'sub', 'data-testid': 'model-request' },
      `last request (${state.chat.modelRequest.model}): requested ${state.chat.modelRequest.requested_effort}; ${state.chat.modelRequest.effort}`) : null,
    state.chat.context ? el('span', { class: 'sub' },
      `context ${state.chat.context.used} / ${state.chat.context.size} tokens`) : null,
    spent ? el('span', { class: 'sub' },
      `${spent.input_tokens} in / ${spent.output_tokens} out` +
      (spent.cached_tokens ? ` (${spent.cached_tokens} cached)` : '')) : null];
  bar.replaceChildren(...controls.filter(node => node !== null));
}

// Which session the next prompt goes to: a new one, or any of the recent
// ones, whose transcript is read back into the stream when chosen.
function receiveQueueDraft() {
  const text = takeRestored(state.chat.session);
  if (text === null) return;
  const input = $('#chat-input');
  const draft = input?.value ?? state.chat.draft ?? '';
  state.chat.draft = draft + (draft ? '\n\n' : '') + text;
  if (input) { input.value = state.chat.draft; input.focus(); }
}

function rememberRenamedBranch(updated) {
  const saved = state.chat.sessions.find(session => session.id === updated.id);
  if (saved) saved.title = updated.title;
  else state.chat.sessions.unshift({ id: updated.id, title: updated.title, updated_at: Math.floor(Date.now() / 1000) });
  renderPicker();
}

function renderPicker() {
  receiveQueueDraft();
  const branches = $('#branch-controls');
  if (branches && branches.dataset.session !== (state.chat.session || '')) {
    branches.dataset.session = state.chat.session || '';
    branches.querySelector('section')?.remove();
    if (branches.open) branches.append(branchPanel(state.chat.session, continueIn, quoteIntoDraft, branchFromEvent, rememberRenamedBranch));
  }
  const queue = $('#queue-controls');
  if (queue && queue.dataset.session !== (state.chat.session || '')) {
    queue.dataset.session = state.chat.session || '';
    queue.querySelector('section')?.remove();
    if (queue.open) queue.append(queuePanel(state.chat.session, receiveQueueDraft, receiveQueueReceipt));
  }
  const mcp = $('#mcp-controls');
  if (mcp && mcp.dataset.session !== (state.chat.session || '')) {
    mcp.dataset.session = state.chat.session || '';
    mcp.querySelector('section')?.remove();
    if (mcp.open) mcp.append(mcpPanel(state.chat.session));
  }
  const box = $('#picker');
  if (!box) return;
  const current = state.chat.session;
  const options = [el('option', { value: '', selected: !current }, 'new session')];
  for (const s of state.chat.sessions) {
    options.push(el('option', { value: s.id, selected: String(s.id) === String(current) },
      `${s.title || '(untitled)'} · ${ago(s.updated_at)}`));
  }
  if (current && !state.chat.sessions.some(s => String(s.id) === String(current))) {
    options.push(el('option', { value: current, selected: true }, `session ${current}`));
  }
  box.replaceChildren(el('label', {}, 'session ',
    el('select', { onchange: (e) => continueIn(e.target.value || null) }, options)));
}

// Read back what the session already holds, so a resumed conversation is
// seen and not only continued.
export async function resume(session) {
  if (state.chat.busy) return;
  state.chat.session = session;
  const out = chatOut();
  if (out) out.replaceChildren();
  current = null;
  renderPicker();
  if (!session) return;
  try {
    const { items } = await api(`/api/sessions/${session}/history`);
    if (state.chat.session !== session || state.chat.busy) return;
    for (const e of items) {
      if (e.kind === 'user') say('you', `› ${e.body}`);
      else if (e.kind === 'assistant') { current = null; saidByModel(e.body); current = null; }
      else if (e.kind === 'tool-call') say('tool', `· ${e.doing || e.label}`);
      else if (e.kind === 'tool-result') say('stat', e.body.split('\n').slice(0, 3).join('\n'));
      // `wrote` is JSON for `changes` to read, not prose for anybody. The same
      // question is `note_is_for_a_person` in rook-core, which the window asks.
      else if (e.kind === 'note' && e.label !== 'wrote') say('stat', `${e.label}: ${e.body}`);
    }
    say('stat', `— ${items.length} earlier entries; the next prompt continues this session —`);
  } catch (e) {
    say('err', e.error || String(e));
  }
}

// What follows the last `@` of the word the caret is in, or null when no file
// is being named.
//
// The word rather than the whole line, because a prompt is a sentence and `@`
// belongs to one file in it; behind the caret rather than the whole value, so
// going back to fix an earlier mention offers that one. A second `@` in the
// word means an address, which is not a mention.
function naming(input) {
  const before = input.value.slice(0, input.selectionStart ?? input.value.length);
  const word = before.split(/\s/).pop();
  if (!word.startsWith('@')) return null;
  const fragment = word.slice(1);
  return fragment.includes('@') ? null : fragment;
}

// Put `path` where the mention being typed is, and a space after it: the name
// is finished and the sentence goes on.
function name(input, path) {
  const at = input.selectionStart ?? input.value.length;
  const fragment = naming(input);
  if (fragment === null) return;
  const start = at - fragment.length - 1;
  // A space after it, so the sentence goes on — unless there is already one,
  // which is what completing a mention in the middle of a line runs into.
  const rest = input.value.slice(at);
  const gap = rest.startsWith(' ') ? '' : ' ';
  input.value = `${input.value.slice(0, start)}@${path}${gap}${rest}`;
  const caret = start + path.length + 1 + gap.length;
  state.chat.draft = input.value;
  input.setSelectionRange(caret, caret);
  input.focus();
}

// Ranked by the daemon, because a browser cannot walk a filesystem and a second
// ranking written here is a second answer to one question. A newer keystroke
// wins: the answer to `@ser` is worthless once `@serv` has been asked.
let asked = 0;
async function offer(input, row) {
  const fragment = naming(input);
  if (fragment === null) {
    row.replaceChildren();
    return;
  }
  const mine = ++asked;
  let found = [];
  try {
    found = await api(`/api/files?fragment=${encodeURIComponent(fragment)}`);
  } catch {
    found = [];
  }
  if (mine !== asked) return;
  row.replaceChildren(...found.map((path, at) => el('button', {
    type: 'button',
    class: at === 0 ? 'chip picked' : 'chip',
    onclick: () => { name(input, path); row.replaceChildren(); },
  }, path)));
}

export async function renderChat() {
  try { state.chat.sessions = (await api('/api/sessions')).items.slice(0, 30); } catch { state.chat.sessions = []; }
  const stream = el('div', { class: 'stream', id: 'stream' });
  const input = el('textarea', { id: 'chat-input', rows: 3, 'aria-label': 'Prompt', placeholder: 'Ask the agent… (Enter sends, Shift+Enter adds a line, Esc stops)', autofocus: true }, state.chat.draft || '');
  const sendButton = el('button', { id: 'send', type: 'submit' }, 'Send');
  const followupButton = el('button', { type: 'submit', value: 'follow_up', title: 'Start a separate turn after this turn or goal finishes' }, 'Queue follow-up');
  const retryButton = el('button', { id: 'retry-prompt', type: 'button', hidden: true,
    title: 'Resend the exact previous prompt and caller ID after uncertain delivery',
    onclick: () => {
      if (state.chat.busy) return;
      const frame = retryPrompt.retry();
      if (!frame) return;
      send(frame);
      say('stat', 'Retrying the saved prompt with its original ID and options');
      working();
    } }, 'Retry saved prompt');
  const discardButton = el('button', { id: 'discard-prompt', type: 'button', hidden: true,
    onclick: () => { retryPrompt.discard(); renderPromptRetry(); } }, 'Discard saved prompt');
  const retryStopButton = el('button', { id: 'retry-stop', type: 'button', hidden: true,
    title: 'Retry the saved Stop with its original caller ID and goal generation',
    onclick: retrySavedStop }, 'Retry saved Stop');
  const discardStopButton = el('button', { id: 'discard-stop', type: 'button', hidden: true,
    onclick: () => { retryStop.discard(); renderStopRetry(); } }, 'Discard saved Stop');
  const stopButton = el('button', { id: 'stop', type: 'button', hidden: true, onclick: stop }, 'Stop');

  let loadingAttachments = false;
  const attachmentsInput = el('input', { id: 'attachments', type: 'file', multiple: true, 'aria-label': 'Images or UTF-8 context files' });
  const historicalAttachments = el('div', { class: 'row', 'aria-label': 'Historical attachments' });
  const showHistoricalAttachments = () => {
    const kept = state.chat.historicalAttachments || [];
    historicalAttachments.replaceChildren(...(kept.length ? [
      el('span', {}, `Historical attachments: ${kept.length} (${kept.map(a => a.name.slice(0, 120)).join(', ')})`),
      el('button', { type: 'button', onclick: () => { state.chat.historicalAttachments = []; showHistoricalAttachments(); } }, 'Clear historical attachments'),
    ] : []));
  };
  showHistoricalAttachments();
  const recipePath = el('input', { id: 'recipe-name', 'aria-label': 'Run recipe', placeholder: 'Recipe name or workspace-relative .toml file (optional)' });
  const recipeParameters = el('textarea', { id: 'recipe-parameters', 'aria-label': 'Recipe parameters', placeholder: 'Parameters as JSON, for example {"scope":"src"}', rows: 2 });
  const outputPath = el('input', { id: 'output-file', 'aria-label': 'Final answer file', placeholder: 'Save final answer: workspace-relative path (optional)' });
  const outputSchema = el('textarea', { id: 'output-schema', 'aria-label': 'Output JSON Schema', placeholder: 'JSON Schema (optional)', rows: 3 });
  const repairs = el('input', { id: 'output-repairs', type: 'number', min: 0, max: 3, value: 2, title: 'Format repair attempts' });
  const outputSettings = el('details', {}, el('summary', {}, 'Attachments, recipe and output'),
    el('div', { class: 'row' }, el('label', { for: 'attachments' }, 'Images or text files (up to 4)'), attachmentsInput),
    el('div', { class: 'row' }, recipePath), el('div', { class: 'row' }, recipeParameters),
    el('div', { class: 'row' }, outputPath), el('div', { class: 'row' }, outputSchema),
    el('div', { class: 'row' }, el('label', { for: 'output-repairs' }, 'Repair attempts'), repairs));
  const form = el('form', { class: 'ask', onsubmit: async (event) => {
    event.preventDefault();
    if (loadingAttachments) return;
    const files = Array.from(attachmentsInput.files || []);
    const text = input.value.trim() || (recipePath.value.trim() ? `Run recipe ${recipePath.value.trim()}` : files.length ? 'Analyse the attachments' : '');
    if (!text) return;
    askToNotify();
    const submittingSession = state.chat.session;
    const wasBusy = state.chat.busy;
    const followUp = event.submitter?.value === 'follow_up';
    let options;
    try {
      const schema = outputSchema.value.trim();
      if (new TextEncoder().encode(schema).length > 65536) throw new Error('Output schema exceeds 64 KiB');
      const retries = Number(repairs.value);
      if (!Number.isInteger(retries) || retries < 0 || retries > 3) throw new Error('Repair attempts must be 0..3');
      let recipe = null;
      if (recipePath.value.trim()) {
        if (new TextEncoder().encode(recipeParameters.value).length > 65536) throw new Error('Recipe parameters exceed 64 KiB');
        const parameters = JSON.parse(recipeParameters.value.trim() || '{}');
        if (!parameters || Array.isArray(parameters) || typeof parameters !== 'object' ||
            Object.values(parameters).some(value => typeof value !== 'string')) throw new Error('Recipe parameters must be a JSON object of strings');
        recipe = { path: recipePath.value.trim(), parameters };
      }
      const kept = state.chat.historicalAttachments || [];
      if (files.length + kept.length > 4) throw new Error('At most 4 attachments per turn');
      if (state.chat.busy && (files.length || kept.length)) throw new Error('Wait for the running turn to finish before attaching files');
      let textBytes = kept.filter(a => a.type === 'text').reduce((n, a) => n + new TextEncoder().encode(a.text).length, 0);
      for (const file of files) {
        const isImage = /^image\//.test(file.type) || /\.(png|jpe?g|webp|gif)$/i.test(file.name);
        if (isImage && file.size > 2 * 1024 * 1024) throw new Error('An image exceeds 2 MiB; resize it first');
        if (!isImage) textBytes += file.size;
      }
      if (textBytes > 256 * 1024) throw new Error('Embedded text exceeds 256 KiB');
      loadingAttachments = true;
      const attachments = [...kept];
      for (const file of files) {
        const isImage = /^image\//.test(file.type) || /\.(png|jpe?g|webp|gif)$/i.test(file.name);
        const bytes = await file.arrayBuffer();
        if (isImage) {
          const uri = await new Promise((resolve, reject) => {
            const reader = new FileReader();
            reader.onload = () => resolve(reader.result);
            reader.onerror = () => reject(reader.error);
            reader.readAsDataURL(new Blob([bytes]));
          });
          const extension = file.name.split('.').pop().toLowerCase();
          const mime = file.type.startsWith('image/') ? file.type : `image/${extension === 'jpg' ? 'jpeg' : extension}`;
          attachments.push({ type: 'image', name: file.name, mime_type: mime, data: uri.slice(uri.indexOf(',') + 1) });
        } else {
          attachments.push({ type: 'text', name: file.name, text: new TextDecoder('utf-8', { fatal: true }).decode(bytes) });
        }
      }
      options = { recipe, attachments, output: outputPath.value.trim() || null,
        output_schema: schema ? JSON.parse(schema) : null, schema_retries: retries };
    } catch (error) { say('err', String(error)); return; }
    finally { loadingAttachments = false; }
    if (followUp || pendingSubmission() || submissionError() || (wasBusy && !text.startsWith('/goal '))) {
      try {
        if (options.attachments.length) throw new Error('Queued corrections cannot include attachments');
        if (followUp && (options.recipe || options.output || options.output_schema)) throw new Error('Follow-ups currently accept text only; clear recipe and output settings first');
        const submission = submitSteering(submittingSession, text, followUp ? 'follow_up' : 'submit');
        $('#queue-controls section')?.refreshSubmission?.();
        const result = await submission;
        receiveQueueReceipt(result.session, result.entry);
        if (state.chat.session === submittingSession && input.isConnected && input.value.trim() === text) {
          input.value = ''; state.chat.draft = '';
        }
      } catch (error) {
        say('err', `${error.error || String(error)} Open Message queue to retry the saved submission.`);
      } finally {
        $('#queue-controls section')?.refreshSubmission?.();
      }
      return;
    }
    if (retryPrompt.candidate()) {
      say('err', 'Resolve the saved prompt first: use Retry saved prompt or Discard saved prompt. Your draft is retained.');
      return;
    }
    const message = { type: 'prompt', session: state.chat.session, text,
      id: crypto.randomUUID(), options };
    if (!jsonWithin(message, 16 * 1024 * 1024)) {
      say('err', 'The prompt and attachments exceed the 16 MiB message limit; the draft was retained.'); return;
    }
    if (!retryPrompt.remember(message)) {
      say('stat', 'Browser storage could not retain this prompt across reloads; keep this tab open to retry it.');
    }
    send(message);
    input.value = '';
    state.chat.draft = '';
    attachmentsInput.value = '';
    state.chat.historicalAttachments = [];
    showHistoricalAttachments();
    state.chat.branchNotice = '';
    $('#branch-draft-note')?.replaceChildren();
    // While a turn runs this is something to say to it, and the server echoes
    // it back as `interjected` — so the transcript is written there, once, and
    // the working state is left alone.
    if (state.chat.busy) return;
    say('you', `› ${text}`);
    working();
  } }, input, sendButton, followupButton, retryButton, discardButton,
  retryStopButton, discardStopButton, stopButton);
  // Naming a file meant knowing the path and typing it, which in a browser
  // means leaving the page to go and look. The ranking is the daemon's, so the
  // page offers the same list the terminal does.
  const naming = el('div', { class: 'row', id: 'naming' });
  input.addEventListener('input', () => { state.chat.draft = input.value; offer(input, naming); });
  input.addEventListener('keydown', (e) => {
    const offered = naming.firstElementChild;
    if (e.key === 'Enter' && !e.shiftKey && !e.altKey && !e.isComposing) {
      e.preventDefault();
      form.requestSubmit();
      return;
    }
    if (e.key === 'Tab' && offered) {
      // Tab moves focus by default, which here means leaving the box you are
      // still typing in.
      e.preventDefault();
      name(input, offered.textContent);
      state.chat.draft = input.value;
      naming.replaceChildren();
      return;
    }
    // Escape closes the list first and stops the turn only when there is no
    // list: one key, and the nearer thing goes first.
    if (e.key === 'Escape') {
      if (offered) naming.replaceChildren();
      else stop();
    }
  });

  const history = el('details', {}, el('summary', {}, 'Search, jump and quote history'));
  history.addEventListener('toggle', () => {
    if (!history.open) { history.querySelector('section')?.remove(); return; }
    const session = state.chat.session;
    if (session && !history.querySelector('section')) history.append(historyPanel(session, text => quoteIntoDraft(session, text), undefined, seq => branchFromEvent(session, seq)));
  });
  const mcp = el('details', { id: 'mcp-controls' }, el('summary', {}, 'MCP connections'));
  const branches = el('details', { id: 'branch-controls' }, el('summary', {}, 'Conversation branches'));
  branches.addEventListener('toggle', () => {
    branches.querySelector('section')?.remove();
    if (branches.open) branches.append(branchPanel(state.chat.session, continueIn, quoteIntoDraft, branchFromEvent, rememberRenamedBranch));
  });
  mcp.addEventListener('toggle', () => {
    mcp.querySelector('section')?.remove();
    if (mcp.open) mcp.append(mcpPanel(state.chat.session));
  });
  const queue = el('details', { id: 'queue-controls' }, el('summary', {}, 'Message queue'));
  queue.addEventListener('toggle', () => {
    queue.querySelector('section')?.remove();
    if (queue.open) queue.append(queuePanel(state.chat.session, receiveQueueDraft, receiveQueueReceipt));
  });
  $('#view').replaceChildren(el('div', { class: 'card' },
    el('div', { class: 'row', id: 'picker' }),
    el('div', { class: 'row', id: 'settings' }),
    stream, history, branches, mcp, queue, outputSettings, historicalAttachments,
    el('p', { id: 'branch-draft-note', class: 'sub', role: 'status' }, state.chat.branchNotice || ''), form, naming));
  renderPicker();
  renderSettings();
  connect();
  if (state.chat.busy) working();
  else { renderPromptRetry(); renderStopRetry(); }
  if (state.chat.session && !state.chat.busy) await resume(state.chat.session);
  input.focus();
}

// From another tab: continue this session in the chat.
export function continueIn(session, prepared = null) {
  // Detach this observer before choosing another conversation. A queued frame
  // from the old socket must not change the selected session or its controls.
  const previous = socket;
  socket = null;
  previous?.close();
  state.chat.draft = prepared ? prepared.text : ($('#chat-input')?.value ?? state.chat.draft ?? '');
  if (prepared) state.chat.historicalAttachments = prepared.attachments;
  done();
  callStatus = null;
  state.chat.spent = null; state.chat.context = null; state.chat.modelRequest = null;
  state.chat.session = session;
  nav.go('chat');
}

export async function branchFromEvent(session, event) {
  const canLoad = () => !($('#chat-input')?.value ?? state.chat.draft ?? '') &&
    !($('#attachments')?.files.length) && !(state.chat.historicalAttachments?.length);
  if (!canLoad()) throw new Error('Save or clear the current draft and attachments before branching.');
  const forked = await api(`/api/sessions/${encodeURIComponent(session)}/branch`, { event });
  if (!canLoad()) throw new Error(`Created branch ${forked.node.id}; current draft retained because it changed while the branch was being created.`);
  state.chat.branchNotice = forked.draft?.notice || 'New branch created. Edit the draft and send when ready; workspace files are unchanged.';
  continueIn(forked.node.id, forked.draft || { text: '', attachments: [] });
}

// A quote is just draft text. It cannot send a prompt or switch a running turn.
export function quoteIntoDraft(session, text) {
  const input = $('#chat-input');
  const draft = input?.value ?? state.chat.draft ?? '';
  state.chat.draft = draft + (draft ? '  ' : '') + text;
  if (state.tab === 'chat' && input) {
    input.value = state.chat.draft;
    input.focus();
  } else {
    if (!state.chat.busy) state.chat.session = session;
    nav.go('chat');
  }
}
