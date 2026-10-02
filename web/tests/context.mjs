// Run with `node --test web/tests/context.mjs`; no browser packages needed.
import test from 'node:test';
import assert from 'node:assert/strict';
import { contextPanel, renderContext } from '../dist/context.js';
import { state } from '../dist/lib.js';

class Node {
  constructor(tag, text = '') {
    this.tag = tag; this.nodeType = tag === '#text' ? 3 : 1;
    this.text = text; this.children = []; this.listeners = {};
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
}
const view = new Node('main');
globalThis.document = {
  createElement: tag => new Node(tag),
  createTextNode: text => new Node('#text', text),
  querySelector: selector => selector === '#view' ? view : null,
};

const usage = {
  window: 16000, usable: 14000, compact_at: 10000, compactions: 1,
  live_tokens: 5000, logged_tokens: 12000, replay_from: 13, needs_compaction: false,
  by_kind: [['tool-result', { events: 2, tokens: 800 }]],
  last_request: { event_seq: 22, catalog: {
    provider_id: 'scripted/test', delivery: 'native', detail: 'stub', used_tokens: 4900,
    tool_count: 1, omitted_tools: 0, tools: [{ name: 'mcp_tools', estimated_tokens: 20 }],
    mcp: { discovered: 2, advertised: 1, deferred: 1, deferred_names: ['camera__shot'], omitted_deferred: 0 },
    sources: { discovered_skills: 1, applicable_skills: 1, advertised_skills: 1, loaded_skill_events: 1,
      sources: [{ kind: 'skill', name: 'camera', inclusion: 'loaded', origin: '<img src=x onerror=bad()>', estimated_tokens: 40, complete: true }], omitted_sources: 0 },
  } },
};

test('browser context separates the live estimate from recorded request provenance', () => {
  const panel = contextPanel(usage);
  assert.match(panel.textContent, /Current live estimate:|Current live estimate/);
  assert.match(panel.textContent, /Last request attempt · event #22/);
  assert.match(panel.textContent, /Historical snapshot/);
  assert.match(panel.textContent, /camera__shot/);
  assert.match(panel.textContent, /1 loaded events/);
  assert.match(panel.textContent, /<img src=x onerror=bad\(\)>/);
  assert(!panel.find(node => node.tag === 'img'), 'source metadata stays DOM text');
  const old = contextPanel({ ...usage, last_request: { event_seq: 23, catalog: {
    provider_id: 'old', delivery: 'prompt', detail: 'full', used_tokens: 2,
    tool_count: 0, omitted_tools: 0, tools: []
  } } });
  assert.match(old.textContent, /event #23/);
  assert.doesNotMatch(old.textContent, /camera__shot/);
});

test('browser context reads the selected session and its workspace, then refreshes', async () => {
  state.tab = 'context';
  state.session = 'first';
  const paths = [];
  globalThis.fetch = async path => {
    paths.push(path);
    const value = path === '/api/sessions' ? { items: [
      { id: 'first', title: 'First', workspace: 'C:/work one' },
      { id: 'second', title: 'Second', workspace: 'C:/work two' }
    ] } : usage;
    return { ok: true, json: async () => value };
  };
  await renderContext();
  assert.deepEqual(paths, ['/api/sessions', '/api/sessions/first/context?workspace=C%3A%2Fwork%20one']);
  assert.match(view.textContent, /camera__shot/);
  view.find(node => node.tag === 'button' && node.textContent === 'Refresh context').listeners.click();
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(paths.at(-1), '/api/sessions/first/context?workspace=C%3A%2Fwork%20one');
  view.find(node => node.tag === 'li' && node.textContent.includes('Second')).listeners.click();
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(paths.at(-1), '/api/sessions/second/context?workspace=C%3A%2Fwork%20two');
});

test('response receipt keeps selected, dispatched and adapter-returned models distinct without inventing money', () => {
  const panel = contextPanel({ ...usage, last_response: { event_seq: 24, receipt: {
    selected: 'analysis', phase: 'implementation', dispatch: { provider: 'fallback', model: 'physical-fast' },
    reported_model: '<script>server-echo</script>', usage: { input_tokens: 120, output_tokens: 7, cache_read_tokens: 80, cache_write_tokens: 10 }
  } } });
  assert.match(panel.textContent, /Selected: analysis · phase implementation/);
  assert.match(panel.textContent, /Dispatched: fallback \/ physical-fast/);
  assert.match(panel.textContent, /Reported model: <script>server-echo<\/script>/);
  assert.match(panel.textContent, /120 input · 7 output · 80 cache read · 10 cache write/);
  assert.match(panel.textContent, /Monetary cost: unknown/);
  assert.match(panel.textContent, /input\/output counters reported: false/);
  assert.match(panel.textContent, /configured fallback if omitted by server/);
  assert.match(panel.textContent, /not total session spend/);
  assert.equal(panel.find(node => node.tag === 'script'), null);
  const priced = contextPanel({ ...usage, last_response: { event_seq: 25, receipt: {
    selected: 'analysis', phase: 'implementation', dispatch: { provider: 'target', model: 'physical-fast' },
    reported_model: 'echo', complete: true, usage_reported: true, elapsed_ms: 120, usage: { input_tokens: 1, output_tokens: 1 },
    cost: { estimated_usd: 0.000008 }
  } } });
  assert.match(priced.textContent, /USD 0.00000800 estimate from recorded configured rates; not an invoice/);
  assert.match(priced.textContent, /120 ms · completion confirmed: true/);
  assert.match(priced.textContent, /input\/output counters reported: true/);
});

test('cost coverage renders a known subset without presenting uncovered work as free or inherited charges as new', () => {
  const panel = contextPanel({ ...usage, cost_coverage: {
    main_receipts: 1, auxiliary_receipts: 2, priced_receipts: 2, unpriced_receipts: 1,
    usage_events_without_receipt: 3, known_subtotal_usd: 0.0000946, complete_accounting: false
  } });
  assert.match(panel.textContent, /Known subtotal: USD 0.00009460 configured-rate estimate/);
  assert.match(panel.textContent, /Priced receipts: 2 · unpriced receipts: 1 · usage events without receipt: 3/);
  assert.match(panel.textContent, /Total cost is unknown: retry\/failure, delegated and branch-summary costs/);
  assert.match(panel.textContent, /Inherited receipts are historical, not new charges/);
  const unknown = contextPanel({ ...usage, cost_coverage: { known_subtotal_usd: null, unpriced_receipts: 2 } });
  assert.match(unknown.textContent, /unknown \(no priced receipts\)/);
  assert.doesNotMatch(unknown.textContent, /USD 0\.00000000/);
});
