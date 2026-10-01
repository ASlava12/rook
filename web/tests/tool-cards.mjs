// Run with `node --test web/tests/tool-cards.mjs`; no browser packages needed.
import test from 'node:test';
import assert from 'node:assert/strict';
import { historyPanel } from '../dist/history.js';

class Node {
  constructor(tag, text = '') {
    this.tag = tag; this.nodeType = tag === '#text' ? 3 : 1;
    this.text = text; this.children = []; this.listeners = {};
    this.isConnected = true; this.className = '';
  }
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) { this.children = children; }
  setAttribute(name, value) { this[name] = value; }
  removeAttribute(name) { delete this[name]; }
  addEventListener(name, handler) { this.listeners[name] = handler; }
  get textContent() { return this.text + this.children.map(child => child.textContent).join(''); }
  set textContent(value) { this.text = value; this.children = []; }
  find(predicate) {
    if (predicate(this)) return this;
    for (const child of this.children) { const found = child.find?.(predicate); if (found) return found; }
    return null;
  }
}
globalThis.document = {
  createElement: tag => new Node(tag),
  createTextNode: text => new Node('#text', text),
};
const tick = () => new Promise(resolve => setImmediate(resolve));

test('saved tool cards stay compact until opened and page large content on demand', async () => {
  const paths = [];
  globalThis.fetch = async path => {
    paths.push(path);
    let value;
    if (path.endsWith('/history')) value = {
      items: [
        { seq: 0, kind: 'tool-call', label: 'run_command', doing: 'run cargo test', bytes: 11, body: 'not shown' },
        { seq: 1, kind: 'tool-result', label: 'run_command', doing: '', bytes: 50000, body: 'not shown' },
      ], previous: null, next: null, through: 2,
    };
    else if (path.endsWith('/history/1?offset=0')) value = {
      entry: { seq: 1, body: '<script>first part</script>' }, offset: 0, next_offset: 27, previous_offset: null, total_bytes: 50000,
    };
    else if (path.endsWith('/history/1?offset=27')) value = {
      entry: { seq: 1, body: 'second part' }, offset: 27, next_offset: null, previous_offset: 0, total_bytes: 50000,
    };
    else throw new Error(`unexpected ${path}`);
    return { ok: true, json: async () => value };
  };
  const panel = historyPanel('session', () => {});
  await tick();
  const card = panel.find(node => node.tag === 'details' && node.className === 'entry tool-card' && node.textContent.includes('tool-result'));
  assert(card);
  assert.match(card.textContent, /run_command · 50000 stored bytes/);
  assert.doesNotMatch(card.textContent, /not shown|first part/);
  assert.deepEqual(paths, ['/api/sessions/session/history']);

  card.open = true;
  card.listeners.toggle();
  await tick();
  assert.deepEqual(paths, ['/api/sessions/session/history', '/api/sessions/session/history/1?offset=0']);
  assert.match(card.textContent, /<script>first part<\/script>/);
  assert.match(card.textContent, /current files and test results are not verified/);
  const next = card.find(node => node.tag === 'button' && node.textContent === 'Next part');
  assert(next);
  next.listeners.click();
  await tick();
  assert.equal(paths.at(-1), '/api/sessions/session/history/1?offset=27');
  assert.match(card.textContent, /second part/);
  assert.doesNotMatch(card.textContent, /first part/);
});
