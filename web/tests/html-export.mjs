// Run with `node --test web/tests/html-export.mjs`; no browser packages needed.
import test from 'node:test';
import assert from 'node:assert/strict';
import { exportHistoryHtml } from '../dist/html-export.js';
import { historyPanel } from '../dist/history.js';

test('browser sends one explicit profile and range to the shared export endpoint', async () => {
  const prepared = { html: '<!doctype html><p>Prepared by core</p>', events: 2, shortened: 1, omitted: { reasoning: 3 }, from: 1, through: 7, profile: 'share-tools' };
  const calls = [];
  const result = await exportHistoryHtml('session /', 1, 7, async (path, request) => { calls.push([path, request]); return prepared; }, 'share-tools');
  assert.strictEqual(result, prepared, 'browser downloads the exact reviewed core artifact without rebuilding hidden payloads');
  assert.deepEqual(calls, [['/api/sessions/session%20%2F/export', { from: 1, through: 7, profile: 'share-tools' }]]);
});

test('browser propagates backend admission failures and refuses malformed selections before a request', async () => {
  let called = 0;
  const read = async () => { called++; throw new Error('HTML export exceeds 512 events'); };
  await assert.rejects(exportHistoryHtml('session', 0, 512, read), /exceeds 512 events/);
  await assert.rejects(exportHistoryHtml('session', -1, 2, read), /nonnegative/);
  await assert.rejects(exportHistoryHtml('session', 0, 2, read, 'unknown'), /profile/);
  assert.equal(called, 1);
});

test('history panel requires a content preview before downloading the exact prepared range', async () => {
  class Node {
    constructor(tag, text = '') { this.tag = tag; this.nodeType = tag === '#text' ? 3 : 1; this.text = text; this.children = []; this.listeners = {}; this.value = ''; this.isConnected = true; }
    append(...nodes) { this.children.push(...nodes); }
    replaceChildren(...nodes) { this.children = nodes; }
    setAttribute(name, value) { this[name] = value; }
    removeAttribute(name) { delete this[name]; }
    addEventListener(name, handler) { this.listeners[name] = handler; }
    get textContent() { return this.text + this.children.map(node => node.textContent).join(''); }
    set textContent(value) { this.text = value; this.children = []; }
    find(tag, label) {
      if (this.tag === tag && (!label || this.textContent.includes(label))) return this;
      for (const child of this.children) { const found = child.find?.(tag, label); if (found) return found; }
      return null;
    }
    click() { this.listeners.click?.(); if (this.tag === 'a') clicked = this; }
    remove() {}
  }
  let clicked = null;
  globalThis.document = {
    body: new Node('body'),
    createElement: tag => new Node(tag),
    createTextNode: text => new Node('#text', text),
  };
  const originalCreate = URL.createObjectURL, originalRevoke = URL.revokeObjectURL;
  URL.createObjectURL = blob => { assert.equal(blob.type, 'text/html;charset=utf-8'); return 'blob:test'; };
  URL.revokeObjectURL = () => {};
  const entries = [{ seq: 7, kind: 'assistant', label: '', doing: '', body: 'saved reply' }];
  const prepared = { html: '<p>selected reply</p>', events: 1, shortened: 0, omitted: { reasoning: 2 }, from: 7, through: 7, profile: 'share' };
  globalThis.fetch = async (path, options) => {
    if (path.endsWith('/export')) {
      assert.equal(options.method, 'POST');
      assert.deepEqual(JSON.parse(options.body), { from: 7, through: 7, profile: 'share' });
      return { ok: true, json: async () => prepared };
    }
    return { ok: true, json: async () => ({ through: 8, items: entries, previous: null, next: null }) };
  };
  try {
    const root = historyPanel('session', () => {});
    await new Promise(resolve => setImmediate(resolve));
    root.find('select').value = 'share';
    const form = root.find('form', 'Preview selected HTML');
    assert(form);
    await form.listeners.submit({ preventDefault() {} });
    assert.equal(clicked, null, 'preview is not publication');
    const frame = root.find('iframe'); assert(frame);
    assert.equal(frame.sandbox, ''); assert.equal(frame.srcdoc, prepared.html);
    await root.find('button', 'Download previewed HTML').listeners.click();
    assert.equal(clicked.download, 'rook-session-session-7-7.html');
    assert.equal(clicked.href, 'blob:test');
    assert.match(root.textContent, /Preview: 1 event\(s\)/);
    assert.match(root.textContent, /reasoning: 2/);
  } finally {
    URL.createObjectURL = originalCreate;
    URL.revokeObjectURL = originalRevoke;
  }
});
