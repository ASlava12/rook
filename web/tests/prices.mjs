import test from 'node:test';
import assert from 'node:assert/strict';
import { renderPrices } from '../dist/prices.js';
import { state } from '../dist/lib.js';

class Node {
  constructor(tag, text = '') { this.tag = tag; this.text = text; this.nodeType = tag === '#text' ? 3 : 1; this.children = []; this.listeners = {}; }
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) { this.children = children; }
  get firstChild() { return this.children[0]; }
  get textContent() { return this.text + this.children.map(n => n.textContent).join(''); }
  set textContent(text) { this.text = text; this.children = []; }
  setAttribute(key, value) { this[key] = value; }
  addEventListener(key, handler) { this.listeners[key] = handler; }
  find(predicate) { if (predicate(this)) return this; for (const child of this.children) { const found = child.find?.(predicate); if (found) return found; } return null; }
}
const view = new Node('main');
globalThis.document = { createElement: tag => new Node(tag), createTextNode: text => new Node('#text', text), querySelector: () => view };
const listing = () => ({ source_url: 'https://models.dev/api.json', observed_at: 100, age_secs: 10, stale: false, notices: ['Manual values retain priority.'], models: [{ source: 'cloud', model: '<script>physical</script>', provider: 'openai', configured: { input: 9 }, reference: { input: 2, output: 6, cache_read: 0.2 }, apply_fields: ['output_usd_per_million', 'cache_read_usd_per_million'], review_token: 'private-review-token', reason: 'Only missing rates', last_application: '', reference_context: 1234 }] });
const tick = () => new Promise(resolve => setImmediate(resolve));
function setup() {
  state.tab = 'prices'; view.replaceChildren();
  const requests = [];
  globalThis.fetch = (path, options) => new Promise(resolve => requests.push({ path, options, resolve: (value, ok = true) => resolve({ ok, json: async () => value }) }));
  globalThis.confirm = () => true;
  return requests;
}

test('inspection is offline, untrusted names stay text, and applying sends the reviewed source/token only', async () => {
  const requests = setup(), drawing = renderPrices();
  assert.deepEqual(requests.map(r => r.path), ['/api/models/prices']);
  requests[0].resolve(listing()); await drawing;
  assert.match(view.textContent, /<script>physical<\/script>/);
  assert.match(view.textContent, /display only/);
  assert.doesNotMatch(view.textContent, /private-review-token/);
  const button = view.find(n => n.tag === 'button' && n.textContent === 'Apply missing rates');
  let question = ''; globalThis.confirm = text => { question = text; return true; };
  button.listeners.click();
  assert.match(question, /cache_read_usd_per_million/);
  assert.deepEqual(JSON.parse(requests[1].options.body), { source: 'cloud', review_token: 'private-review-token' });
  button.listeners.click(); assert.equal(requests.length, 2, 'busy form admits only one application');
  requests[1].resolve({}); await tick();
  const applied = listing(); applied.models[0].review_token = null;
  requests[2].resolve(applied); await tick();
  button.listeners.click(); assert.equal(requests.length, 3, 'detached previous review cannot submit');
});

test('late initial responses cannot replace another tab or a newer price form', async () => {
  const requests = setup(), old = renderPrices();
  const fresh = renderPrices();
  requests[1].resolve(listing()); await fresh;
  const current = view.firstChild;
  requests[0].resolve(listing()); await old;
  assert.equal(view.firstChild, current);
  state.tab = 'chat'; view.replaceChildren(new Node('p', 'current chat'));
  current.find(n => n.tag === 'button' && n.textContent === 'Apply missing rates').listeners.click();
  assert.equal(requests.length, 2);
  assert.equal(view.textContent, 'current chat');
});

test('refresh is explicit and a stale-review error removes all old application buttons', async () => {
  const requests = setup(), drawing = renderPrices(); requests[0].resolve(listing()); await drawing;
  const old = view.find(n => n.tag === 'button' && n.textContent === 'Apply missing rates');
  old.listeners.click(); requests[1].resolve({ error: 'account changed; inspect again' }, false); await tick();
  assert.match(view.textContent, /account changed/);
  old.listeners.click(); assert.equal(requests.length, 2);
  const refresh = view.find(n => n.tag === 'button');
  refresh.listeners.click(); assert.equal(requests[2].path, '/api/models/prices');
  requests[2].resolve(listing()); await tick();
  refresh.listeners.click(); assert.equal(requests[3].path, '/api/models/prices/refresh');
  state.tab = 'chat'; view.replaceChildren(new Node('p', 'chat wins'));
  requests[3].resolve({}); await tick(); requests[4].resolve(listing()); await tick();
  assert.equal(view.textContent, 'chat wins');
});

test('declining application keeps the reviewed rates without a write', async () => {
  const requests = setup(), drawing = renderPrices(); requests[0].resolve(listing()); await drawing;
  globalThis.confirm = () => false;
  view.find(n => n.tag === 'button' && n.textContent === 'Apply missing rates').listeners.click();
  assert.equal(requests.length, 1);
});
