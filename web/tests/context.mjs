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

test('extension reports retain host source, plain text and historical attribution without a request', () => {
  const extension_ui = { reports: [
    { source: { event: 'prompt', ordinal: 0, digest: 'a'.repeat(64) }, event_seq: 7,
      item: { kind: 'status', id: 'build', text: '<script>fake success</script>' } },
    { source: { event: 'post_tool', ordinal: 1, digest: 'b'.repeat(64) }, event_seq: 9,
      item: { kind: 'progress', id: 'scan', label: 'checking', done: 1, total: 3 } },
    { source: { event: 'turn_end', ordinal: 2, digest: 'c'.repeat(64) }, event_seq: 11,
      item: { kind: 'result', id: 'tests', title: 'Reported result', body: '<img src=x onerror=bad()>' } },
  ], omitted_updates: 2, invalid_records: 1 };
  const panel = contextPanel({ ...usage, last_request: null, extension_ui });
  assert.match(panel.textContent, /saved branch history/);
  assert.match(panel.textContent, /current files and tests are not verified/);
  assert.match(panel.textContent, /source aaaaaaaa · event #7/);
  assert.match(panel.textContent, /checking · 1\/3/);
  assert.match(panel.textContent, /<script>fake success<\/script>/);
  assert.match(panel.textContent, /<img src=x onerror=bad\(\)>/);
  assert.match(panel.textContent, /2 omitted updates · 1 invalid records/);
  assert.equal(panel.find(node => node.tag === 'script' || node.tag === 'img'), null);
  assert.doesNotMatch(contextPanel({ ...usage, extension_ui: {} }).textContent, /Extension reports/);
});

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
    usage_events_without_receipt: 3, known_subtotal_usd: 0.0000946, complete_accounting: false,
    attempts_started: 6, attempts_completed: 3, attempts_failed: 1, attempts_incomplete: 0,
    attempts_interrupted: 1, attempts_pending: 1,
    priced_attempts: 3, unpriced_attempts: 2, attempt_known_subtotal_usd: 0.0000946
  } });
  assert.match(panel.textContent, /Known subtotal: USD 0.00009460 configured-rate estimate/);
  assert.match(panel.textContent, /Priced receipts: 2 · unpriced receipts: 1 · usage events without receipt: 3/);
  assert.match(panel.textContent, /Recorded physical attempts: 6 started · 3 completed · 1 failed · 0 incomplete · 1 interrupted · 1 pending/);
  assert.match(panel.textContent, /Attempt subtotal: USD 0.00009460 configured-rate estimate · 3 priced · 2 unpriced endings/);
  assert.match(panel.textContent, /Receipt and attempt subtotals overlap; do not add them/);
  assert.match(panel.textContent, /Total cost is unknown: retry\/failure attempts may lack complete usage/);
  assert.match(panel.textContent, /Inherited receipts are historical, not new charges/);
  const unknown = contextPanel({ ...usage, cost_coverage: { known_subtotal_usd: null, unpriced_receipts: 2 } });
  assert.match(unknown.textContent, /unknown \(no priced receipts\)/);
  assert.match(unknown.textContent, /unknown \(no priced attempts\)/);
  assert.doesNotMatch(unknown.textContent, /USD 0\.00000000/);
});

test('delegated estimates combine only captured history and keep absent bills unknown', () => {
  const coverage = { known_subtotal_usd: 0.00001, attempt_known_subtotal_usd: 0.00002,
    delegated: { started: 3, completed: 1, failed: 1, interrupted: 0, pending: 1,
      captured_sessions: 4, missing_snapshots: 1, unfinished_descendants: 2, attempts_pending: 2,
      priced_receipts: 3, unpriced_receipts: 1, usage_events_without_receipt: 2,
      known_receipt_subtotal_usd: 0.00003, known_attempt_subtotal_usd: 0.00006 } };
  const panel = contextPanel({ ...usage, cost_coverage: coverage });
  assert.match(panel.textContent, /3 started · 1 completed · 1 failed · 0 interrupted · 1 pending/);
  assert.match(panel.textContent, /4 sessions · 1 missing snapshots · 2 unfinished descendants · 2 pending attempts/);
  assert.match(panel.textContent, /Child receipts: 3 priced · 1 unpriced · 2 usage events without receipt/);
  assert.match(panel.textContent, /response subtotal: USD 0\.00004000/);
  assert.match(panel.textContent, /attempt subtotal: USD 0\.00008000/);
  assert.match(panel.textContent, /later child activity is excluded/);
  assert.match(panel.textContent, /Response and attempt subtotals overlap; do not add them/);
  const unknown = contextPanel({ ...usage, cost_coverage: { delegated: { started: 1, pending: 1 } } });
  assert.match(unknown.textContent, /response subtotal: unknown/);
  assert.match(unknown.textContent, /attempt subtotal: unknown/);
  assert.doesNotMatch(unknown.textContent, /USD 0\.00000000/);
});
