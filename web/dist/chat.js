// The chat: one socket for the tab's lifetime, a session that can be resumed,
// a turn that can be stopped, and the agent's questions answered in place.
import { $, el, api, ago, md, state, nav, notify, askToNotify } from './lib.js';

// Scrollback, not the record: the session holds every word of this and the
// sessions tab reads it back, so a tab left open for a day need not keep an
// afternoon of turns in the document to stay recoverable.
const MAX_SCROLLBACK_BLOCKS = 2000;

let socket = null;
// The assistant's current block, re-rendered from its whole text on every
// delta so a fence or a list that arrives in pieces still ends up drawn.
let current = null;

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

function saidByModel(text) {
  if (!current || !current.isConnected) {
    current = block('md', '');
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

function done() {
  state.chat.busy = false;
  state.chat.waiting = false;
  current = null;
  setTitle();
  const send = $('#send'), stop = $('#stop');
  if (send) send.textContent = 'Send';
  if (stop) stop.hidden = true;
}

export function connect() {
  if (socket && socket.readyState <= 1) return socket;
  socket = new WebSocket(`${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/api/chat`);
  socket.onmessage = (event) => {
    const e = JSON.parse(event.data);
    switch (e.type) {
      case 'started': state.chat.session = e.session; state.chat.spent = null; renderPicker(); break;
      case 'text': saidByModel(e.text); break;
      case 'reasoning': say('think', e.text); break;
      case 'tool': say('tool', `· ${e.name}`); break;
      case 'tool_done': {
        const last = chatOut() && chatOut().lastElementChild;
        if (last && last.className === 'tool') last.append(e.failed ? ' ✗' : ' ✓');
        current = null;
        break;
      }
      case 'settings': state.chat.settings = e; renderSettings(); break;
      case 'spent': state.chat.spent = e; renderSettings(); break;
      case 'remembered': say('stat', `remembered: ${e.text}`); break;
      case 'forgot': say('stat', `forgot: ${e.text}`); break;
      case 'error': say('err', e.message); done(); break;
      case 'cancelled': say('stat', '[stopped]'); done(); break;
      case 'interjected': say('you', `› ${e.text}`); say('stat', '(the turn will see this at its next step)'); break;
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
  socket.onclose = () => { say('err', 'disconnected'); done(); };
  return socket;
}

function send(message) {
  const s = connect();
  const deliver = () => s.send(JSON.stringify(message));
  if (s.readyState === 1) deliver(); else s.addEventListener('open', deliver, { once: true });
}

export function stop() {
  if (!state.chat.busy) return;
  send({ type: 'cancel' });
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

function askApproval(request) {
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
}

// One form for every question in the call: the agent asked them together
// because they are independent, and a chain of dialogs would undo that.
function askUser(request) {
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
    `${name} `,
    el('select', { onchange: (e) => send({ type: 'setting', name, value: e.target.value }) },
      values.map(v => el('option', { value: v, selected: v === selected }, v))));
  const spent = state.chat.spent;
  bar.replaceChildren(
    pick('stance', s.stances && s.stances.length ? s.stances : [s.mode], s.mode),
    pick('effort', s.efforts && s.efforts.length ? s.efforts : [s.effort], s.effort),
    // Beside the settings rather than in the transcript: it changes on every
    // step, and a running total that scrolled away would be no use.
    spent ? el('span', { class: 'sub' },
      `${spent.input_tokens} in / ${spent.output_tokens} out` +
      (spent.cached_tokens ? ` (${spent.cached_tokens} cached)` : '')) : null);
}

// Which session the next prompt goes to: a new one, or any of the recent
// ones, whose transcript is read back into the stream when chosen.
function renderPicker() {
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
    el('select', { onchange: (e) => resume(e.target.value || null) }, options)));
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
    const { items } = await api(`/api/sessions/${session}/transcript?limit=80&max_body=4000`);
    for (const e of items) {
      if (e.kind === 'user') say('you', `› ${e.body}`);
      else if (e.kind === 'assistant') { current = null; saidByModel(e.body); current = null; }
      else if (e.kind === 'tool-call') say('tool', `· ${e.label}`);
      else if (e.kind === 'tool-result') say('stat', e.body.split('\n').slice(0, 3).join('\n'));
      else if (e.kind === 'note') say('stat', `${e.label}: ${e.body}`);
    }
    say('stat', `— ${items.length} earlier entries; the next prompt continues this session —`);
  } catch (e) {
    say('err', e.error || String(e));
  }
}

export async function renderChat() {
  try { state.chat.sessions = (await api('/api/sessions')).items.slice(0, 30); } catch { state.chat.sessions = []; }
  const stream = el('div', { class: 'stream', id: 'stream' });
  const input = el('input', { placeholder: 'Ask the agent… (Esc stops a running turn)', autofocus: true });
  const sendButton = el('button', { id: 'send', type: 'submit' }, 'Send');
  const stopButton = el('button', { id: 'stop', type: 'button', hidden: true, onclick: stop }, 'Stop');

  const form = el('form', { class: 'ask', onsubmit: (event) => {
    event.preventDefault();
    const text = input.value.trim();
    if (!text) return;
    askToNotify();
    send({ type: 'prompt', session: state.chat.session, text });
    input.value = '';
    // While a turn runs this is something to say to it, and the server echoes
    // it back as `interjected` — so the transcript is written there, once, and
    // the working state is left alone.
    if (state.chat.busy) return;
    say('you', `› ${text}`);
    state.chat.busy = true;
    setTitle();
    sendButton.textContent = '…';
    stopButton.hidden = false;
  } }, input, sendButton, stopButton);
  input.addEventListener('keydown', (e) => { if (e.key === 'Escape') stop(); });

  $('#view').replaceChildren(el('div', { class: 'card' },
    el('div', { class: 'row', id: 'picker' }),
    el('div', { class: 'row', id: 'settings' }),
    stream, form));
  renderPicker();
  renderSettings();
  connect();
  if (state.chat.session && !state.chat.busy) await resume(state.chat.session);
  input.focus();
}

// From another tab: continue this session in the chat.
export function continueIn(session) {
  state.chat.session = session;
  nav.go('chat');
}
