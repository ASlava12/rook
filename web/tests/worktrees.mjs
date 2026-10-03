import test from 'node:test';
import assert from 'node:assert/strict';
import { worktreePanel } from '../dist/worktrees.js';
class Node {
  constructor(tag, text = '') { this.tag = tag; this.nodeType = tag === '#text' ? 3 : 1; this.text = text; this.children = []; this.listeners = {}; }
  append(...children) { this.children.push(...children); }
  setAttribute(name, value) { this[name] = name === 'disabled' ? true : value; }
  addEventListener(name, handler) { this.listeners[name] = handler; }
  set textContent(value) { this.text = value; this.children = []; }
  get textContent() { return this.text + this.children.map(c => c.textContent).join(''); }
  find(predicate) { if (predicate(this)) return this; for (const c of this.children) { const found = c.find?.(predicate); if (found) return found; } }
}
globalThis.document = { createElement: tag => new Node(tag), createTextNode: text => new Node('#text', text) };
const id = '01M419GFMZ6MV2D7GWM94EYZS8';
const diagnosis = { child: id, state: 'missing_checkout', path: '/owned/checkout', index_sha256: 'a'.repeat(64), review_token: 'b'.repeat(64) };
const response = value => ({ ok: true, json: async () => value });

test('worktree restoration requires a current reviewed source and sends only its token', async () => {
  const calls = []; let confirmation = '';
  globalThis.fetch = async (path, options) => { calls.push({ path, options }); return response(options.method ? { ...diagnosis, review_token: null, state: 'restored' } : diagnosis); };
  globalThis.confirm = message => { confirmation = message; return true; };
  const panel = worktreePanel('parent', [{ id }], () => true);
  const diagnose = panel.find(n => n.tag === 'button' && n.textContent === 'Diagnose checkout');
  const restore = panel.find(n => n.tag === 'button' && n.textContent === 'Restore missing checkout');
  assert.equal(restore.disabled, true);
  await restore.listeners.click(); assert.equal(calls.length, 0);
  await diagnose.listeners.click(); assert.equal(restore.disabled, false);
  await restore.listeners.click(); assert.match(confirmation, /registered index aaaa/);
  assert.match(confirmation, /Existing entries are preserved/);
  assert.deepEqual(JSON.parse(calls[1].options.body), { review_token: diagnosis.review_token });
  assert.equal(restore.disabled, true); assert.match(panel.textContent, /restored/);
});

test('departed view and changed child discard late diagnoses and cannot restore another child', async () => {
  for (const change of ['view', 'child']) {
    let owned = true; let release; const calls = [];
    globalThis.fetch = (path, options) => { calls.push({ path, options }); return new Promise(resolve => { release = resolve; }); };
    const panel = worktreePanel('parent', [{ id }], () => owned);
    const diagnose = panel.find(n => n.tag === 'button' && n.textContent === 'Diagnose checkout');
    const restore = panel.find(n => n.tag === 'button' && n.textContent === 'Restore missing checkout');
    const request = diagnose.listeners.click();
    if (change === 'view') owned = false;
    else { const child = panel.find(n => n.tag === 'input'); child.value = '01M419GFMZ6MV2D7GWM94EYZS9'; child.listeners.input(); }
    release(response(diagnosis)); await request;
    assert.equal(restore.disabled, true); assert.doesNotMatch(panel.textContent, /missing_checkout/);
    await restore.listeners.click(); assert.equal(calls.length, 1);
  }
});

test('a lowercase child ID retains ownership of the canonical reviewed response', async () => {
  const calls = [];
  globalThis.fetch = async (path, options) => { calls.push({ path, options }); return response(options.method ? { ...diagnosis, review_token: null } : diagnosis); };
  globalThis.confirm = () => true;
  const panel = worktreePanel('parent', [{ id: id.toLowerCase() }], () => true);
  const diagnose = panel.find(n => n.tag === 'button' && n.textContent === 'Diagnose checkout');
  const restore = panel.find(n => n.tag === 'button' && n.textContent === 'Restore missing checkout');
  await diagnose.listeners.click();
  await restore.listeners.click();
  assert.equal(calls.length, 2, 'the reviewed canonical child must remain restorable');
  assert.equal(calls[1].path, `/api/sessions/parent/worktrees/${id}`);
  assert.deepEqual(JSON.parse(calls[1].options.body), { review_token: diagnosis.review_token });
});
