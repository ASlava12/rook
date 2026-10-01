// Run with `node --test web/tests/branch-offer.mjs`; no browser packages needed.
import test from 'node:test';
import assert from 'node:assert/strict';
import { branchPanel } from '../dist/branches.js';

class Node {
  constructor(tag, value = '') {
    this.tag = tag;
    this.nodeType = tag === '#text' ? 3 : 1;
    this.children = [];
    this.value = value;
    this.isConnected = true;
    this.listeners = {};
  }
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) { this.children = children; }
  setAttribute() {}
  removeAttribute() {}
  addEventListener(name, callback) { this.listeners[name] = callback; }
  focus() {}
  click() { this.listeners.click?.(); }
  get textContent() { return this.tag === '#text' ? this.value : this.children.map(child => child.textContent).join(''); }
  set textContent(value) { this.children = [new Node('#text', value)]; }
  findButton(label) {
    if (this.tag === 'button' && this.textContent === label) return this;
    for (const child of this.children) {
      const found = child.findButton?.(label);
      if (found) return found;
    }
    return null;
  }
  findTag(tag) {
    if (this.tag === tag) return this;
    for (const child of this.children) {
      const found = child.findTag?.(tag);
      if (found) return found;
    }
    return null;
  }
}

globalThis.document = {
  createElement: tag => new Node(tag),
  createTextNode: value => new Node('#text', value),
};

const source = { id: 'source', title: 'Old branch', workspace: 'work', next_seq: 2 };
const target = { id: 'target', title: 'New branch', workspace: 'work', next_seq: 1 };

async function panel() {
  const requests = [], continued = [];
  globalThis.fetch = async (path, options) => {
    requests.push({ path, options });
    assert.equal(path, '/api/sessions/source/tree');
    return { ok: true, json: async () => ({ selected: source, ancestors: [], children: [target], scanned_sessions: 2 }) };
  };
  const root = branchPanel('source', id => continued.push(id));
  await new Promise(resolve => setImmediate(resolve));
  assert(root.findButton('Continue in chat'));
  return { root, requests, continued };
}

function buttons(root, label) {
  const found = [];
  function visit(node) {
    if (node.tag === 'button' && node.textContent === label) found.push(node);
    for (const child of node.children) visit(child);
  }
  visit(root);
  return found;
}

test('leaving a branch offers an optional review without saving or switching', async () => {
  const { root, requests, continued } = await panel();
  buttons(root, 'Continue in chat')[1].click();
  assert.deepEqual(continued, []);
  assert.equal(requests.length, 1);
  assert(root.findButton('Review summary'));
  root.findButton('Review summary').click();
  assert(root.findTag('textarea'));
  assert(root.findButton('Load recorded excerpts'));
  assert.equal(requests.length, 1);
});

test('the offer can be skipped and never writes a summary', async () => {
  const { root, requests, continued } = await panel();
  buttons(root, 'Continue in chat')[1].click();
  root.findButton('Continue without summary').click();
  assert.deepEqual(continued, ['target']);
  assert.equal(requests.length, 1);
});

test('the current branch continues directly and cancelling another offer stays put', async () => {
  const { root, requests, continued } = await panel();
  buttons(root, 'Continue in chat')[1].click();
  root.findButton('Cancel').click();
  assert.deepEqual(continued, []);
  buttons(root, 'Continue in chat')[0].click();
  assert.deepEqual(continued, ['source']);
  assert.equal(requests.length, 1);
});
