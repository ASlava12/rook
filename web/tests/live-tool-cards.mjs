import test from 'node:test';
import assert from 'node:assert/strict';

class Node {
  constructor(tag, text = '') {
    this.tag = tag; this.nodeType = tag === '#text' ? 3 : 1;
    this.text = text; this.children = []; this.className = ''; this.scrollHeight = 0;
  }
  append(...children) { this.children.push(...children); }
  remove() { this.isConnected = false; }
  get childElementCount() { return this.children.filter(child => child.nodeType === 1).length; }
  get firstElementChild() { return this.children.find(child => child.nodeType === 1); }
  get textContent() { return this.text + this.children.map(child => child.textContent).join(''); }
  set textContent(value) { this.text = value; this.children = []; }
  setAttribute(name, value) { this[name] = value; }
  addEventListener() {}
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
