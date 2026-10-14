import test from 'node:test';
import assert from 'node:assert/strict';
import { reviewPanel } from '../dist/reviews.js';
import { state } from '../dist/lib.js';

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
const saved = { id: 'review-one', state: 'ready', source: 'source-one', files: [{ path: 'sum.txt' }], findings: [], comments: [] };
const response = data => ({ ok: true, json: async () => data });
const reset = () => { state.chat.session = 'owner'; state.chat.busy = false; state.chat.draft = ''; };

test('review capture, source pages, comments and feedback retain their owner and exact source', async () => {
  reset(); const requests = []; const drafts = [];
  globalThis.fetch = async (path, options) => {
    const action = JSON.parse(options.body); requests.push({ path, action });
    if (action.action === 'source') return response({ source: saved.source, path: 'sum.txt', side: 'after', first_line: 1, offset: 0, text: '<script>saved text</script>', next: 12 });
    if (action.action === 'feedback') return response({ prompt: 'source-bound corrections' });
    return response(saved);
  };
  const panel = reviewPanel('owner', (...args) => drafts.push(args));
  panel.find(node => node['aria-label'] === 'Selected paths').value = 'sum.txt\nexact file.txt';
  panel.button('Capture selected changes').listeners.click(); await flush();
  assert.equal(requests[0].path, '/api/sessions/owner/reviews');
  assert.deepEqual(requests[0].action.selection, { target: 'working', paths: ['sum.txt', 'exact file.txt'] });
  panel.button('Read saved source').listeners.click(); await flush();
  assert.equal(panel.find(node => node.tag === 'script'), null);
  assert.match(panel.textContent, /saved text/);
  panel.button('Next source page').listeners.click(); await flush();
  assert.deepEqual(requests.at(-1).action, { action: 'source', review: saved.id, path: 'sum.txt', side: 'after', offset: 12 });
  panel.button('Back to review').listeners.click(); await flush();
  panel.find(node => node.tag === 'textarea' && node.placeholder.startsWith('Comment')).value = 'Restore addition';
  panel.find(node => node['aria-label'] === 'Source line').value = '2';
  // Navigation/global session changes cannot redirect the retained comment.
  state.chat.session = 'other';
  panel.button('Save line comment').listeners.click(); await flush();
  assert.equal(requests.at(-1).path, '/api/sessions/owner/reviews');
  assert.deepEqual(requests.at(-1).action.comment, { source: saved.source, path: 'sum.txt', side: 'after', line: 2, text: 'Restore addition' });
  state.chat.busy = true;
  panel.button('Put comments into draft').listeners.click(); await flush();
  assert.deepEqual(drafts, []); assert.match(panel.textContent, /Another session/);
  state.chat.busy = false; state.chat.session = 'owner';
  panel.button('Open review').listeners.click(); await flush();
  panel.button('Put comments into draft').listeners.click(); await flush();
  assert.deepEqual(drafts, [['owner', 'source-bound corrections']]);
});

test('late review replies and detached panels cannot replace a newer source', async () => {
  reset(); const pending = [];
  globalThis.fetch = () => new Promise(resolve => pending.push(resolve));
  const panel = reviewPanel('owner', () => assert.fail('no draft expected'));
  const open = panel.button('Open review');
  open.listeners.click(); open.listeners.click();
  pending[1](response({ ...saved, source: 'newer-source' })); await flush();
  pending[0](response({ ...saved, source: 'stale-source' })); await flush();
  assert.match(panel.textContent, /newer-source/); assert.doesNotMatch(panel.textContent, /stale-source/);
  open.listeners.click(); panel.isConnected = false;
  pending[2](response(saved)); await flush();
  assert.match(panel.textContent, /Reading review/); assert.doesNotMatch(panel.textContent, /source-one/);
});
