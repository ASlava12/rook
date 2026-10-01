// Run with `node --test web/tests/html-export.mjs`; no browser packages needed.
import test from 'node:test';
import assert from 'node:assert/strict';
import { exportHistoryHtml } from '../dist/html-export.js';
import { historyPanel } from '../dist/history.js';

function history(entries, bodies) {
  const paths = [];
  const read = async path => {
    paths.push(path);
    const url = new URL(path, 'http://localhost');
    const seq = url.pathname.match(/\/history\/(\d+)$/);
    if (seq) {
      const event = Number(seq[1]);
      return { entry: { ...entries[event], body: bodies[event] }, offset: 0, next_offset: null };
    }
    const from = Number(url.searchParams.get('from'));
    return { through: entries.length, items: entries.slice(from, from + 64) };
  };
  return { read, paths };
}

test('browser export scopes, escapes, shortens and attributes saved history', async () => {
  const entries = [
    { seq: 0, kind: 'user', label: '', doing: '' },
    { seq: 1, kind: 'tool-result', label: '<script>', doing: 'run & inspect', tool_measurement: { failed: true, duration_ms: 42, timing_seq: 3 } },
    { seq: 2, kind: 'assistant', label: '', doing: '' },
  ];
  const source = history(entries, ['outside-before', `<script>alert('bad')</script>${'&'.repeat(9000)}`, 'outside-after']);
  const result = await exportHistoryHtml('session', 1, 1, source.read);
  assert.equal(result.events, 1);
  assert.equal(result.shortened, 1);
  assert.match(result.html, /Session session · selected events #1–#1 · snapshot ended before #3/);
  assert.match(result.html, /&lt;script&gt;alert\(&#39;bad&#39;\)&lt;\/script&gt;/);
  assert.match(result.html, /run &amp; inspect/);
  assert.match(result.html, /saved failure · dispatch 42 ms · timing #3/);
  assert.match(result.html, /<details><summary>Show tool content<\/summary>/);
  assert.match(result.html, /Body shortened after 8192 bytes/);
  assert.doesNotMatch(result.html, /outside-before|outside-after|<script>/);
  assert.deepEqual(source.paths, [
    '/api/sessions/session/history?from=1&limit=64',
    '/api/sessions/session/history/1?offset=0',
  ]);
});

test('browser export refuses an over-limit selection before making a download', async () => {
  const entries = Array.from({ length: 513 }, (_, seq) => ({ seq, kind: 'assistant', label: '', doing: '' }));
  const source = history(entries, entries.map(() => 'x'));
  await assert.rejects(exportHistoryHtml('session', 0, 512, source.read), /exceeds 512 events/);
  assert.equal(source.paths.filter(path => path.includes('/history/')).length, 512);
  await assert.rejects(exportHistoryHtml('session', 2, 1, source.read), /Export end/);
});

test('history panel downloads the shown range on an explicit click', async () => {
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
  globalThis.fetch = async path => ({ ok: true, json: async () =>
    path.includes('/history/7') ? { entry: entries[0], offset: 0, next_offset: null } :
      { through: 8, items: entries, previous: null, next: null } });
  try {
    const root = historyPanel('session', () => {});
    await new Promise(resolve => setImmediate(resolve));
    const form = root.find('form', 'Download selected HTML');
    assert(form);
    await form.listeners.submit({ preventDefault() {} });
    assert.equal(clicked.download, 'rook-session-session-7-7.html');
    assert.equal(clicked.href, 'blob:test');
    assert.match(root.textContent, /Downloaded 1 event\(s\)/);
  } finally {
    URL.createObjectURL = originalCreate;
    URL.revokeObjectURL = originalRevoke;
  }
});
