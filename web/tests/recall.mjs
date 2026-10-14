import test from 'node:test';
import assert from 'node:assert/strict';
import { recallPanel } from '../dist/recall.js';
import { nav, state } from '../dist/lib.js';

class Node {
  constructor(tag, text = '') {
    this.tag = tag; this.nodeType = tag === '#text' ? 3 : 1;
    this.text = text; this.children = []; this.listeners = {}; this.isConnected = true;
  }
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) { this.children = children; }
  setAttribute(name, value) { this[name] = value; }
  addEventListener(name, handler) { this.listeners[name] = handler; }
  get textContent() { return this.text + this.children.map(child => child.textContent).join(''); }
  find(predicate) {
    if (predicate(this)) return this;
    for (const child of this.children) { const found = child.find?.(predicate); if (found) return found; }
    return null;
  }
  button(label) { return this.find(node => node.tag === 'button' && node.textContent === label); }
}
globalThis.document = { createElement: tag => new Node(tag), createTextNode: text => new Node('#text', text) };
const flush = () => new Promise(resolve => setImmediate(resolve));
const response = { hits: [{ session: 'saved', seq: 9, kind: 'user', title: '<script>title</script>', snippet: 'LPIC <img src=x>' }], next: 'cursor', scanned_sessions: 2, scanned_events: 10, scanned_bytes: 256, inherited_prefixes_skipped: 1 };

test('recall searches the selected workspace, follows cursors and reads exact historical events as text', async () => {
  const requests = [];
  state.query = 'LPIC';
  globalThis.fetch = async (path, options) => {
    const request = JSON.parse(options.body); requests.push({ path, request });
    const data = request.session ? { ...response, hits: [], session: 'saved', next: null, page: { through: 12, items: [{ seq: 9, kind: 'user', label: '', body: '<script>old message</script>', truncated: true }] } } : response;
    return { ok: true, json: async () => data };
  };
  const panel = recallPanel('/work one');
  panel.find(node => node.tag === 'form').listeners.submit({ preventDefault() {} });
  await flush();
  assert.equal(requests[0].path, '/api/history/recall?workspace=%2Fwork%20one');
  assert.deepEqual(requests[0].request, { query: 'LPIC' });
  assert.match(panel.textContent, /saved · #9/);
  assert.equal(panel.find(node => node.tag === 'script' || node.tag === 'img'), null);
  await panel.button('Continue bounded scan').listeners.click();
  assert.deepEqual(requests[1].request, { query: 'LPIC', cursor: 'cursor' });
  await panel.button('Read around event').listeners.click();
  assert.deepEqual(requests[2].request, { session: 'saved', around: 9 });
  assert.match(panel.textContent, /Saved session saved through #12/);
  assert.match(panel.textContent, /Body shortened/);
  const navigated = []; nav.go = tab => navigated.push(tab);
  panel.button('Open session').listeners.click();
  assert.equal(state.session, 'saved');
  assert.deepEqual(navigated, ['sessions']);
});

test('a delayed reply cannot overwrite a newer recall or a detached panel', async () => {
  const pending = [];
  globalThis.fetch = (_path, options) => new Promise(resolve => pending.push({ resolve, signal: options.signal }));
  state.query = 'first';
  const panel = recallPanel('/work');
  const form = panel.find(node => node.tag === 'form');
  const submit = () => form.listeners.submit({ preventDefault() {} });
  submit();
  const query = form.find(node => node.tag === 'input'); query.value = 'second'; submit();
  assert.equal(pending[0].signal.aborted, true);
  pending[1].resolve({ ok: true, json: async () => ({ ...response, hits: [{ ...response.hits[0], title: 'SECOND_RESULT' }] }) });
  await flush();
  pending[0].resolve({ ok: true, json: async () => ({ ...response, hits: [{ ...response.hits[0], title: 'STALE_RESULT' }] }) });
  await flush();
  assert.match(panel.textContent, /SECOND_RESULT/);
  assert.doesNotMatch(panel.textContent, /STALE_RESULT/);
  submit(); panel.isConnected = false;
  pending[2].resolve({ ok: true, json: async () => response });
  await flush();
  assert.match(panel.textContent, /reading saved conversations/);
});
