import test from 'node:test';
import assert from 'node:assert/strict';
import { MAX_MODEL_CHARACTERS } from '../dist/streamed-markdown.js';

let stream, parseCount = 0, next = 0;
const frames = new Map(), visibility = new Map();
globalThis.requestAnimationFrame = cb => { const id = ++next; frames.set(id, cb); return id; };
globalThis.cancelAnimationFrame = id => frames.delete(id);
class Node {
  constructor(tag, text = '') {
    this.tag = tag; this.text = text; this.nodeType = tag === '#text' ? 3 : 1;
    this.children = []; this.dataset = {}; this.className = ''; this.listeners = {}; this.value = ''; this.parent = null;
  }
  append(...nodes) { for (const node of nodes) { node.remove(); node.parent = this; this.children.push(node); } }
  remove() { if (this.parent) this.parent.children.splice(this.parent.children.indexOf(this), 1); this.parent = null; }
  replaceChildren(...nodes) {
    if (this.className === 'md') parseCount++;
    for (const node of this.children) node.parent = null;
    this.children = []; this.text = ''; this.append(...nodes);
  }
  replaceWith(node) { const parent = this.parent; if (!parent) return; const at = parent.children.indexOf(this); this.remove(); node.parent = parent; parent.children.splice(at, 0, node); }
  setAttribute(name, value) { this[name] = value; }
  addEventListener(name, handler) { this.listeners[name] = handler; }
  get textContent() { return this.text + this.children.map(c => c.textContent).join(''); }
  set textContent(value) { this.replaceChildren(); this.text = value; }
  get isConnected() { return this === stream || !!this.parent?.isConnected; }
  get childElementCount() { return this.children.filter(c => c.nodeType === 1).length; }
  get firstElementChild() { return this.children.find(c => c.nodeType === 1); }
  all() { return this.children.flatMap(c => [c, ...c.all()]); }
  querySelectorAll(selector) {
    return this.all().filter(c => selector === 'input, button' ? ['input', 'button'].includes(c.tag) :
      selector === '[data-input-key]' ? !!c.dataset.inputKey :
        selector === '[data-receipt]' ? !!c.dataset.receipt :
        selector === '.approve, .ask-form' ? ['approve', 'ask-form'].includes(c.className) : false);
  }
}
stream = new Node('div');
globalThis.document = {
  querySelector: selector => selector === '#stream' ? stream : null,
  querySelectorAll: selector => stream?.querySelectorAll(selector === '#stream [data-input-key]' ? '[data-input-key]' : selector) || [],
  addEventListener: (name, cb) => visibility.set(name, cb),
  createElement: tag => new Node(tag), createTextNode: text => new Node('#text', text), createDocumentFragment: () => new Node('#fragment'),
};
globalThis.window = {};
globalThis.location = { protocol: 'http:', host: 'localhost' };
const storage = new Map();
globalThis.sessionStorage = { getItem: k => storage.get(k) ?? null, setItem: (k, v) => storage.set(k, v), removeItem: k => storage.delete(k) };
class Socket {
  constructor() { this.readyState = 1; this.sent = []; }
  addEventListener() {}
  send(value) { this.sent.push(JSON.parse(value)); }
  receive(value) { this.onmessage({ data: JSON.stringify(value) }); }
  close() { this.readyState = 3; this.onclose(); }
}
globalThis.WebSocket = Socket;
const { state, nav } = await import('../dist/lib.js');
const chat = await import('../dist/chat.js');
nav.go = () => {};
const snapshot = (session = 'fixture', questions = []) => ({ type: 'snapshot', session, running: true, approvals: [], questions });
function start() {
  const socket = chat.connect(); socket.receive(snapshot()); socket.receive({ type: 'turn', id: 'turn' });
  parseCount = 0; return socket;
}
const text = (socket, value) => socket.receive({ type: 'text', text: value });
const frame = () => { for (const cb of [...frames.values()]) cb(); };
const answers = () => stream.children.filter(n => n.className === 'md');
const finish = (socket, reply) => socket.receive({ type: 'done', reply, steps: 1, input_tokens: 0, output_tokens: 0, delegated: [] });

test('Done before a pending frame preserves a complete split fence without a duplicate or source attribute', () => {
  const socket = start(), reply = 'Paragraph\n\n```sql\nSELECT "🦉";\n```\n\nFinal';
  for (const part of reply.split('')) text(socket, part);
  assert.equal(parseCount, 0); assert.equal(frames.size, 1);
  const pending = [...frames.values()][0]; finish(socket, reply); pending();
  assert.equal(parseCount, 1); assert.equal(answers().length, 1);
  assert.equal(answers()[0].all().find(n => n.tag === 'code').textContent, 'SELECT "🦉";');
  assert(answers()[0].textContent.includes('Final')); assert.equal(answers()[0].dataset.text, undefined);
  assert.equal(frames.size, 0);
});

test('an authoritative final reply replaces a partial current answer instead of duplicating it', () => {
  const socket = start(); text(socket, 'Incomplete'); frame(); finish(socket, 'Complete final');
  assert.equal(answers().length, 1); assert.equal(answers()[0].textContent, 'Complete final');
});

test('error failure cancellation and disconnect synchronously flush pending text', () => {
  for (const event of [{ type: 'error', message: 'bad' }, { type: 'failed', message: 'bad' }, { type: 'cancelled' }, null]) {
    const socket = start(); text(socket, 'visible partial'); const pending = [...frames.values()][0];
    if (event) socket.receive(event); else socket.close();
    assert.equal(answers()[0].textContent, 'visible partial'); assert.equal(parseCount, 1);
    pending(); assert.equal(parseCount, 1); assert.equal(frames.size, 0);
  }
});

test('Stop flushes its pending frame before sending the admitted turn control', () => {
  const socket = start(); socket.receive({ type: 'goal', generation: null });
  text(socket, 'partial before Stop'); chat.stop();
  assert.equal(answers()[0].textContent, 'partial before Stop'); assert.equal(parseCount, 1);
  const sent = socket.sent.at(-1); assert.equal(sent.type, 'stop'); assert.equal(sent.turn, 'turn');
  assert.equal(JSON.parse(storage.get('rook:pending-stop')).id, sent.id);
  socket.receive({ type: 'stop_applied', id: sent.id }); socket.receive({ type: 'cancelled' });
});

test('snapshot discards old frames while recovering the same private question and composer draft', () => {
  const socket = start(), question = { type: 'ask', id: 'same', questions: [{ question: 'Choose next', choices: [], multi: false }] };
  state.chat.draft = 'composer draft'; socket.receive(question);
  const form = stream.all().find(n => n.className === 'ask-form'), input = form.all().find(n => n.tag === 'input');
  input.value = 'private answer'; text(socket, 'old streamed branch'); const obsolete = [...frames.values()][0];
  socket.receive(snapshot('fixture', ['same'])); socket.receive(question);
  text(socket, 'new owned branch'); obsolete(); assert.equal(parseCount, 0);
  frame(); assert.equal(answers().length, 1); assert.equal(answers()[0].textContent, 'new owned branch');
  assert.equal(stream.all().find(n => n.className === 'ask-form'), form); assert.equal(input.value, 'private answer');
  assert.equal(state.chat.draft, 'composer draft');
  socket.receive({ type: 'cancelled' });
});

test('branch selection rejects a late old socket and callback without losing the current draft', () => {
  const old = start(); state.chat.draft = 'saved draft'; text(old, 'old branch');
  const obsolete = [...frames.values()][0]; chat.continueIn('new-branch');
  const fresh = chat.connect(); fresh.receive(snapshot('new-branch')); text(fresh, 'new branch');
  old.receive({ type: 'text', text: 'late old connection' }); obsolete();
  assert.equal(parseCount, 0); frame(); assert.equal(answers()[0].textContent, 'new branch');
  assert.equal(state.chat.draft, 'saved draft'); assert.equal(state.chat.session, 'new-branch');
  fresh.receive({ type: 'cancelled' });
});

test('queued corrections flush model text on acceptance and keep one receipt with monotonic ownership', () => {
  const socket = start(), receipt = { session: 'fixture', reference: 'queued-correction', revision: 0, status: 'queued' };
  state.chat.draft = 'independent composer';
  socket.receive({ type: 'agent', text: 'queued next', receipt });
  text(socket, 'current answer');
  socket.receive({ type: 'interjected', text: 'accepted next', receipt: { ...receipt, revision: 1, status: 'accepted' } });
  assert.equal(answers()[0].textContent, 'current answer'); assert.equal(parseCount, 1);
  text(socket, 'after acceptance');
  socket.receive({ type: 'agent', text: 'stale queued', receipt });
  socket.receive({ type: 'agent', text: 'foreign receipt', receipt: { ...receipt, session: 'another-session', revision: 2 } });
  assert.equal(parseCount, 1, 'stale/foreign updates cannot flush a new owned model burst');
  const rows = stream.querySelectorAll('[data-receipt]'); assert.equal(rows.length, 1);
  assert.equal(rows[0].dataset.status, 'accepted'); assert.match(rows[0].textContent, /accepted next/);
  assert.doesNotMatch(rows[0].textContent, /stale|foreign/);
  assert.equal(state.chat.draft, 'independent composer');
  frame(); assert.equal(answers()[1].textContent, 'after acceptance'); socket.receive({ type: 'cancelled' });
});

test('follow-up flushes the previous turn and identity changes discard obsolete scheduled work', () => {
  const socket = start(); text(socket, 'previous turn'); socket.receive({ type: 'follow_up', id: 'next' });
  assert.equal(answers()[0].textContent, 'previous turn');
  socket.receive({ type: 'turn', id: 'next-turn' }); text(socket, 'obsolete pending'); const obsolete = [...frames.values()][0];
  socket.receive({ type: 'turn', id: 'last-turn' }); text(socket, 'last turn'); obsolete();
  assert.equal(parseCount, 1); socket.receive({ type: 'turn', id: 'last-turn' }); frame();
  assert.equal(answers().at(-1).textContent, 'last turn'); socket.receive({ type: 'cancelled' });
});

test('view replacement and visibility change cannot deliver text to an unrelated viewport', () => {
  const socket = start(); text(socket, 'old viewport'); const old = stream; stream = null; frame();
  stream = new Node('div'); text(socket, 'visible now'); visibility.get('visibilitychange')();
  assert.equal(old.children.find(n => n.className === 'md').textContent, '');
  assert.equal(answers()[0].textContent, 'visible now');
  socket.receive({ type: 'cancelled' });
});

test('recovered history renders every assistant synchronously', async () => {
  const socket = start(); socket.receive({ type: 'cancelled' });
  globalThis.fetch = async url => ({ ok: true, json: async () => url.endsWith('/history')
    ? { items: [{ kind: 'assistant', body: 'First saved answer' }, { kind: 'assistant', body: 'Second saved answer' }] }
    : {} });
  await chat.resume('saved'); assert.deepEqual(answers().map(n => n.textContent), ['First saved answer', 'Second saved answer']);
  assert.equal(frames.size, 0);
});

test('late successful and failed history loads cannot overwrite a newer view even for the same session', async () => {
  for (const fail of [false, true]) {
    const socket = start(); socket.receive({ type: 'cancelled' });
    let finishFetch, started;
    const pending = new Promise(resolve => { started = resolve; });
    globalThis.fetch = async url => {
      if (!url.endsWith('/history')) return { ok: true, json: async () => ({}) };
      started();
      await new Promise((resolve, reject) => { finishFetch = fail ? () => reject(new Error('obsolete failure')) : resolve; });
      return { ok: true, json: async () => ({ items: [{ kind: 'assistant', body: 'obsolete success' }] }) };
    };
    const old = chat.resume('same-session'); await pending;
    globalThis.fetch = async url => ({ ok: true, json: async () => url.endsWith('/history')
      ? { items: [{ kind: 'assistant', body: 'new saved view' }] } : {} });
    await chat.resume('same-session'); finishFetch(); await old;
    assert.deepEqual(answers().map(n => n.textContent), ['new saved view']);
    assert.doesNotMatch(stream.textContent, /obsolete/);
  }
});

test('large single replies actually reach the live cap and total retained model text retires old blocks', () => {
  const socket = start(); const tooLarge = 'x'.repeat(MAX_MODEL_CHARACTERS + 1024);
  assert(tooLarge.length > MAX_MODEL_CHARACTERS);
  text(socket, tooLarge); frame();
  assert.equal(Number(answers()[0].dataset.modelCharacters), MAX_MODEL_CHARACTERS);
  assert.match(answers()[0].textContent, /Displayed answer shortened.*fixture/);
  socket.receive({ type: 'reasoning', text: 'boundary' });
  for (let i = 0; i < 4; i++) { text(socket, tooLarge); socket.receive({ type: 'reasoning', text: 'boundary' }); }
  assert.equal(answers().length, 4, 'five capped answers must exceed the total 4 Mi-unit admission budget');
  assert.equal(answers().reduce((n, answer) => n + Number(answer.dataset.modelCharacters), 0), 4 * MAX_MODEL_CHARACTERS);
  socket.receive({ type: 'cancelled' });
});
