import test from 'node:test';
import assert from 'node:assert/strict';
import { shellUndoPanel } from '../dist/shell-undo.js';
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
const saved = { id: 'command-one', state: 'finished', after_source: 'after', changed_paths: ['exact file.txt'], restored_paths: [], restore_attempts: [], excluded_entry_names: ['.git'] };
test('selected shell restore keeps the receipt owner, exact paths and preview token through navigation', async () => {
  const requests = [];
  globalThis.fetch = async (path, options) => {
    const action = JSON.parse(options.body); requests.push({ path, action });
    if (action.action === 'list') return response({ items: [saved] });
    if (action.action === 'prepare') return response({ receipt: saved.id, paths: action.paths, token: 'frozen-token' });
    if (action.action === 'restore') return response({ receipt: saved.id, restored: action.paths });
    return response(saved);
  };
  const panel = shellUndoPanel('original-owner');
  panel.button('List shell receipts').listeners.click(); await flush();
  panel.button('command-one · finished').listeners.click(); await flush();
  assert.match(panel.textContent, /Excluded entry names: .git/);
  panel.find(node => node['aria-label'] === 'Shell restore file paths').value = 'exact file.txt';
  panel.button('Preview selected restore').listeners.click(); await flush();
  assert.equal(requests.filter(request => request.action.action === 'restore').length, 0, 'preview never mutates files');
  panel.button('Restore selected files').listeners.click(); await flush();
  assert.equal(requests.at(-1).path, '/api/sessions/original-owner/shell-undo');
  assert.deepEqual(requests.at(-1).action, { action: 'restore', receipt: 'command-one', paths: ['exact file.txt'], token: 'frozen-token' });
  assert.match(panel.textContent, /Restored files/);
});
test('interrupted shell recovery requires an explicit note and detached replies cannot change the view', async () => {
  const requests = []; let deliver;
  globalThis.fetch = async (path, options) => {
    const action = JSON.parse(options.body); requests.push(action);
    if (action.action === 'list') return response({ items: [{ ...saved, state: 'interrupted' }] });
    if (action.action === 'recover') return new Promise(resolve => { deliver = resolve; });
    return response({ ...saved, state: 'interrupted', after_source: null, changed_paths: [] });
  };
  const panel = shellUndoPanel('owner');
  panel.button('List shell receipts').listeners.click(); await flush();
  panel.button('command-one · interrupted').listeners.click(); await flush();
  assert.equal(requests.some(action => action.action === 'recover'), false);
  panel.find(node => node['aria-label'] === 'Shell recovery inspection note').value = 'Inspected files and verified the process group stopped';
  panel.button('Record inspected recovery').listeners.click(); await flush();
  panel.button('List shell receipts').listeners.click();
  assert.equal(requests.length, 3, 'an in-flight recovery cannot be duplicated or superseded');
  assert.equal(requests.at(-1).note, 'Inspected files and verified the process group stopped');
  panel.isConnected = false; deliver(response({ ...saved, state: 'recovered' })); await flush();
  assert.match(panel.textContent, /Reading shell receipt/); assert.doesNotMatch(panel.textContent, /recovered/);
});
