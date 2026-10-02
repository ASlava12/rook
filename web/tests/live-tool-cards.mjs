import test from 'node:test';
import assert from 'node:assert/strict';

class Node {
  constructor(tag, text = '') {
    this.tag = tag; this.nodeType = tag === '#text' ? 3 : 1;
    this.text = text; this.children = []; this.className = ''; this.dataset = {}; this.scrollHeight = 0; this.isConnected = true; this.listeners = {};
  }
  append(...children) { this.children.push(...children); }
  remove() { this.isConnected = false; }
  get childElementCount() { return this.children.filter(child => child.nodeType === 1).length; }
  get firstElementChild() { return this.children.find(child => child.nodeType === 1); }
  get textContent() { return this.text + this.children.map(child => child.textContent).join(''); }
  set textContent(value) { this.text = value; this.children = []; }
  setAttribute(name, value) { this[name] = value; }
  addEventListener(name, handler) { this.listeners[name] = handler; }
  replaceChildren(...children) { this.children = children; }
  find(predicate) {
    if (predicate(this)) return this;
    for (const child of this.children) { const found = child.find?.(predicate); if (found) return found; }
    return null;
  }
}

const stream = new Node('div');
globalThis.document = {
  querySelector: selector => selector === '#stream' ? stream : null,
  querySelectorAll: () => [],
  createElement: tag => new Node(tag),
  createTextNode: text => new Node('#text', text),
  createDocumentFragment: () => new Node('#fragment'),
};
globalThis.location = { protocol: 'http:', host: 'localhost:3000' };
class Socket {
  constructor() { this.readyState = 1; }
  addEventListener() {}
  receive(value) { this.onmessage({ data: JSON.stringify(value) }); }
}
globalThis.WebSocket = Socket;
const { connect } = await import('../dist/chat.js');

test('live tool cards match same-name completions in order and show observed status', () => {
  const socket = connect();
  socket.receive({ type: 'tool', name: 'read_file', doing: 'read first.txt' });
  socket.receive({ type: 'tool', name: 'read_file', doing: 'read second.txt' });
  socket.receive({ type: 'tool_working', name: 'read_file', said: 'still reading' });
  const cards = stream.children.filter(node => node.className === 'tool');
  assert.equal(cards.length, 2);
  assert.match(cards[0].textContent, /read first\.txt.*Running/);
  assert.match(cards[1].textContent, /read second\.txt.*Running/);

  socket.receive({ type: 'tool_done', name: 'read_file', failed: true });
  assert.match(cards[0].textContent, /Failed · observed \d+ ms in this tab/);
  assert.match(cards[1].textContent, /Running/);
  socket.receive({ type: 'tool_done', name: 'read_file', failed: false });
  assert.match(cards[1].textContent, /Finished · observed \d+ ms in this tab/);
  assert.match(cards[1].textContent, /saved result in history/);
});

const { state } = await import('../dist/lib.js');
const tick = () => new Promise(resolve => setImmediate(resolve));

test('live completions open their exact saved result and diff in bounded parts without eager reads', async () => {
  state.chat.session = 'original';
  const paths = [];
  globalThis.fetch = async path => {
    paths.push(path);
    let value;
    if (path.endsWith('/history/0?offset=0')) value = {
      entry: { body: '<script>result</script>', change_note: 4, tool_measurement: { failed: false, duration_ms: 73, timing_seq: 1 } },
      offset: 0, next_offset: 24, total_bytes: 50000,
    };
    else if (path.endsWith('/history/0?offset=24')) value = {
      entry: { body: 'next result part', change_note: 4 }, offset: 24, previous_offset: 0, total_bytes: 50000,
    };
    else if (path.endsWith('/history/4?offset=0')) value = {
      entry: {body: '-old\n+<script>diff</script>\n'}, offset: 0, total_bytes: 27,
    };
    else throw new Error('Unexpected '+path);
    return { ok: true, json: async () => value };
  };
  const socket = connect();
  socket.receive({type:'tool', name:'write_file', doing:'write sample'});
  const wrapper = stream.children.at(-1);
  const running = wrapper.find(n => n.className === 'live-tool-card');
  running.open = true;
  socket.receive({type:'tool_done',name:'write_file',failed:false,result_seq:0});
  const saved = wrapper.find(n => n.className === 'entry tool-card');
  assert(saved && saved.open, 'opening during execution remains open after completion');
  assert.deepEqual(paths, []);
  state.chat.session = 'another';
  saved.listeners.toggle(); await tick();
  assert.deepEqual(paths, ['/api/sessions/original/history/0?offset=0']);
  assert.match(saved.textContent, /dispatch 73 ms.*timing #1/);
  assert.match(saved.textContent, /<script>result<\/script>/);
  assert.equal(saved.find(n => n.tag === 'script'), null);
  const diff = saved.find(n => n.className === 'saved-diff');
  diff.open = true; diff.listeners.toggle(); await tick();
  assert.equal(paths.at(-1), '/api/sessions/original/history/4?offset=0');
  assert(diff.find(n => n.className === 'diff-added'));
  const next = saved.find(n => n.tag === 'button' && n.textContent === 'Next part');
  next.listeners.click(); await tick();
  assert.match(saved.textContent, /next result part/);
  assert.doesNotMatch(saved.textContent, /<script>result/);
  assert.equal(paths.at(-1), '/api/sessions/original/history/0?offset=24');
});

test('resumed chat keeps human notes while auxiliary receipts and compaction usage stay in the inspector', async () => {
  state.chat.busy = false;
  const requests = [];
  globalThis.fetch = async path => {
    requests.push(path);
    return { ok: true, json: async () => ({ items: [
      { kind: 'user', body: 'visible prompt' },
      { kind: 'assistant', body: 'visible answer' },
      { kind: 'note', label: 'btw', body: 'visible aside' },
      { kind: 'note', label: 'rook:model-aux:v1', body: '{"purpose":"aside","receipt":"internal data"}' },
      { kind: 'note', label: 'compaction usage', body: 'compaction provider usage' },
    ] }) };
  };
  const { resume } = await import('../dist/chat.js');
  await resume('recorded-costs');
  assert.deepEqual(requests, ['/api/sessions/recorded-costs/history']);
  assert.match(stream.textContent, /visible prompt.*visible answer.*visible aside/);
  assert.doesNotMatch(stream.textContent, /rook:model-aux|internal data|compaction provider usage/);
});
