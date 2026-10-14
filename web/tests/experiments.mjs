import test from 'node:test';
import assert from 'node:assert/strict';
import { experimentPanel } from '../dist/experiments.js';
class Node {
  constructor(tag, text = '') {
    this.tag = tag; this.nodeType = tag === '#text' ? 3 : 1;
    this.text = text; this.children = []; this.listeners = {}; this.isConnected = true; this.value = '';
  }
  append(...children) { this.children.push(...children); if (this.tag === 'select' && !this.value) this.value = this.children[0]?.value || ''; }
  replaceChildren(...children) { this.children = children; }
  setAttribute(name, value) { this[name] = value; }
  addEventListener(name, handler) { this.listeners[name] = handler; }
  get textContent() { return this.text + this.children.map(child => child.textContent).join(''); }
  find(predicate) { if (predicate(this)) return this; for (const child of this.children) { const found = child.find?.(predicate); if (found) return found; } return null; }
  button(label) { return this.find(node => node.tag === 'button' && node.textContent === label); }
}
globalThis.document = { createElement: tag => new Node(tag), createTextNode: text => new Node('#text', text) };
const flush = () => new Promise(resolve => setImmediate(resolve));

const response = data => ({ ok: true, json: async () => data });
const saved = { id: 'node-one', title: 'baseline', hypothesis: 'measured source', parameters: { score: '1' }, workspace: '/candidate', source: 'frozen-source', answered: false, state: 'ready', runs: [] };
test('experiment run keeps the owner, selected node and exact prepared token', async () => {
  const requests = [];
  globalThis.fetch = async (path, options) => {
    const action = JSON.parse(options.body); requests.push({ path, action });
    if (action.action === 'list') return response({ items: [saved], baseline: saved.id });
    if (action.action === 'prepare') return response({ node: saved.id, token: 'fixed-token', command: 'evaluate', parameters: saved.parameters });
    if (action.action === 'run') return response({ node: saved.id, run: 'actual-run', state: 'started' });
    return response(saved);
  };
  const panel = experimentPanel('original-owner');
  panel.button('Refresh tree').listeners.click(); await flush();
  panel.button('baseline · unanswered').listeners.click(); await flush();
  panel.button('Prepare run').listeners.click(); await flush();
  assert.equal(requests.some(r => r.action.action === 'run'), false);
  panel.button('Run saved experiment').listeners.click(); await flush();
  assert.equal(requests.at(-1).path, '/api/sessions/original-owner/experiments');
  assert.deepEqual(requests.at(-1).action, { action: 'run', node: saved.id, token: 'fixed-token', wait: false });
  assert.match(panel.textContent, /started/);
});
test('only the measured current round can offer a winner selection', async () => {
  const requests = [];
  globalThis.fetch = async (path, options) => {
    const action = JSON.parse(options.body); requests.push(action);
    if (action.action === 'list') return response({ items: [saved], baseline: saved.id });
    if (action.action === 'compare') return response({ baseline: saved.id, selection_baseline: saved.id, token: 'comparison-token', rows: [{ title: 'winner', node: 'child', promotable: true, deltas: { score: { delta: 1 } } }, { title: 'failed', node: 'failed-child', promotable: false, runs: [{ state: 'failed' }] }] });
    return response({ winner: 'child' });
  };
  const panel = experimentPanel('owner'); panel.button('Refresh tree').listeners.click(); await flush();
  panel.button('Compare current baseline').listeners.click(); await flush();
  assert.equal(requests.some(r => r.action === 'promote'), false);
  panel.button('Select measured winner').listeners.click(); await flush();
  assert.deepEqual(requests.at(-1), { action: 'promote', node: 'child', token: 'comparison-token' });
});
test('detached experiment replies cannot change the previous owner view', async () => {
  let deliver;
  globalThis.fetch = async () => new Promise(resolve => { deliver = resolve; });
  const panel = experimentPanel('old-owner'); panel.button('Refresh tree').listeners.click(); await flush();
  panel.isConnected = false; deliver(response({ items: [saved] })); await flush();
  assert.match(panel.textContent, /Reading experiment/); assert.equal(panel.button('baseline · unanswered'), null);
});
