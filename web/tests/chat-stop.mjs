import test from 'node:test';
import assert from 'node:assert/strict';

const values = new Map();
globalThis.sessionStorage = {
  getItem: key => values.get(key) ?? null,
  setItem: (key, value) => values.set(key, value),
  removeItem: key => values.delete(key),
};
globalThis.location = { protocol: 'http:', host: 'localhost:3000' };
globalThis.document = { querySelector: () => null };

class Socket {
  static current;
  constructor() { this.readyState = 1; this.sent = []; Socket.current = this; }
  addEventListener() {}
  send(value) { this.sent.push(JSON.parse(value)); }
  receive(value) { this.onmessage({ data: JSON.stringify(value) }); }
}
globalThis.WebSocket = Socket;

const { state } = await import('../dist/lib.js');
const chat = await import('../dist/chat.js');

test('browser Stop waits for goal identity, saves before sending, and retries through HTTP after reload', async () => {
  state.chat.session = 'session-one';
  state.chat.busy = true;
  const socket = chat.connect();
  chat.stop();
  assert.deepEqual(socket.sent, []);
  socket.receive({ type: 'goal', generation: 'generation-one' });
  chat.stop();
  const first = socket.sent[0];
  assert.equal(first.type, 'stop');
  assert.equal(first.generation, 'generation-one');
  assert.equal(JSON.parse(values.get('rook:pending-stop')).id, first.id);
  chat.stop();
  assert.deepEqual(socket.sent[1], first);
  socket.receive({ type: 'stop_applied', id: 'another-id', generation: first.generation, already_applied: false });
  assert.equal(JSON.parse(values.get('rook:pending-stop')).id, first.id);
  socket.receive({ type: 'stop_applied', id: first.id, generation: first.generation, already_applied: true });
  assert.equal(values.has('rook:pending-stop'), false);
  chat.stop();
  assert.notEqual(socket.sent[2].id, first.id);
  const saved = socket.sent[2];
  const requests = [];
  globalThis.fetch = async (path, options) => {
    requests.push({ path, options });
    return { ok: true, json: async () => options.method === 'POST'
      ? { id: saved.id, generation: saved.generation, already_applied: true, run: { status: 'queued' } }
      : { generation: saved.generation, status: 'queued' } };
  };
  await chat.retrySavedStop();
  assert.equal(requests.length, 2);
  assert.equal(requests[0].path, '/api/work/session-one');
  assert.equal(requests[1].path, '/api/work/session-one/control');
  assert.deepEqual(JSON.parse(requests[1].options.body),
    { id: saved.id, generation: saved.generation, action: 'pause' });
  assert.equal(values.has('rook:pending-stop'), false);
  chat.stop();
  globalThis.fetch = async () => ({ ok: true, json: async () => ({ generation: 'another-goal' }) });
  await chat.retrySavedStop();
  assert.equal(values.has('rook:pending-stop'), true, 'stale-generation retry must stay inspectable');
  socket.receive({ type: 'stop_applied', id: socket.sent.at(-1).id, generation: socket.sent.at(-1).generation,
    already_applied: true });
});

test('browser saves and retries the observed ordinary turn, then waits for a successor identity', async () => {
  state.chat.busy = true;
  state.chat.session = 'ordinary-session';
  const socket = chat.connect();
  socket.receive({ type: 'attached', session: 'ordinary-session', running: true });
  socket.receive({ type: 'goal', generation: null });
  const before = socket.sent.length;
  chat.stop();
  assert.equal(socket.sent.length, before);
  socket.receive({ type: 'turn', id: 'first-turn' });
  chat.stop();
  const first = socket.sent.at(-1);
  assert.equal(first.turn, 'first-turn');
  assert.equal(first.generation, null);
  assert.equal(JSON.parse(values.get('rook:pending-stop')).turn, 'first-turn');
  await chat.retrySavedStop();
  assert.deepEqual(socket.sent.at(-1), first);
  socket.receive({ type: 'stop_applied', id: first.id, generation: null, already_applied: true });
  assert.equal(values.has('rook:pending-stop'), false);
  socket.receive({ type: 'follow_up', id: 'next-message' });
  const sent = socket.sent.length;
  chat.stop();
  assert.equal(socket.sent.length, sent);
  socket.receive({ type: 'turn', id: 'second-turn' });
  chat.stop();
  assert.equal(socket.sent.at(-1).turn, 'second-turn');
});
