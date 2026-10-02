// The current context estimate beside the saved, historical request attempt.
// All values arrive through the existing bounded session context API.
import { $, el, api, state } from './lib.js';

let generation = 0;

const limited = (value, size = 192) => String(value ?? '').slice(0, size);
const dollars = value => Number(value) > 0 && Number(value) < 1e-8 ? Number(value).toExponential(3) : Number(value).toFixed(8);
const rows = (headers, values) => el('table', {},
  el('thead', {}, el('tr', {}, headers.map(name => el('th', {}, name)))),
  el('tbody', {}, values.map(value => el('tr', {}, value.map(cell => el('td', {}, cell))))));

export function contextPanel(usage) {
  const current = el('section', { class: 'card' },
    el('h2', {}, 'Current live estimate'),
    el('p', {}, `~${usage.live_tokens} of ${usage.usable} usable tokens (${Math.round(100 * usage.live_tokens / Math.max(1, usage.usable))}%)`),
    el('p', { class: 'sub' }, `window ${usage.window} · compact at ${usage.compact_at} · ${usage.compactions} compactions · ~${usage.logged_tokens} logged tokens`),
    usage.needs_compaction ? el('p', { class: 'warn' }, 'The next turn will compact this context.') : null,
    usage.replay_from ? el('p', { class: 'sub' }, `Live replay starts at event #${usage.replay_from} after the last summary.`) : null,
    rows(['kind', 'events', 'tokens'], (usage.by_kind || []).slice(0, 32).map(([kind, value]) =>
      [limited(kind, 64), String(value.events), `~${value.tokens}`])));
  const saved = usage.last_request;
  const response = usage.last_response;
  if (response) {
    const r = response.receipt;
    const u = r.usage || {};
    current.append(el('section', {},
      el('h3', {}, `Last response · event #${response.event_seq} · historical receipt`),
      el('p', {}, `Selected: ${limited(r.selected || 'unknown', 256)} · phase ${limited(r.phase, 32)}`),
      el('p', {}, `Dispatched: ${r.dispatch ? `${limited(r.dispatch.provider, 256)} / ${limited(r.dispatch.model, 256)}` : 'unknown (legacy/custom provider)'}`),
      el('p', {}, `Reported model: ${limited(r.reported_model || 'unknown', 256)} (adapter value; configured fallback if omitted by server)`),
      el('p', {}, `Provider counters: ${u.input_tokens || 0} input · ${u.output_tokens || 0} output · ${u.cache_read_tokens || 0} cache read · ${u.cache_write_tokens || 0} cache write`),
      el('p', {}, `Monetary cost: ${r.cost ? `USD ${dollars(r.cost.estimated_usd)} estimate from recorded configured rates; not an invoice` : 'unknown (missing pricing, identity or complete usage)'}`),
      el('p', {}, `Elapsed: ${r.elapsed_ms || 0} ms · completion confirmed: ${Boolean(r.complete)} · input/output counters reported: ${Boolean(r.usage_reported)}`),
      el('p', { class: 'sub' }, 'This is one response, not total session spend. Zero counters may mean omitted server usage.')));
  }
  if (usage.cost_coverage) {
    const c = usage.cost_coverage;
    current.append(el('section', {},
      el('h3', {}, 'Cost coverage · saved branch history'),
      el('p', {}, `Known subtotal: ${c.known_subtotal_usd == null ? 'unknown (no priced receipts)' : `USD ${dollars(c.known_subtotal_usd)} configured-rate estimate`}`),
      el('p', {}, `Priced receipts: ${c.priced_receipts || 0} · unpriced receipts: ${c.unpriced_receipts || 0} · usage events without receipt: ${c.usage_events_without_receipt || 0}`),
      el('p', {}, `Recorded physical attempts: ${c.attempts_started || 0} started · ${c.attempts_completed || 0} completed · ${c.attempts_failed || 0} failed · ${c.attempts_incomplete || 0} incomplete · ${c.attempts_interrupted || 0} interrupted · ${c.attempts_pending || 0} pending`),
      el('p', { class: 'sub' }, 'Total cost is unknown: retry/failure attempts may lack complete usage; delegated and branch-summary costs are not fully covered. Inherited receipts are historical, not new charges.')));
  }
  if (!saved) return el('div', { class: 'context-view' }, current,
    el('section', { class: 'card' }, el('h2', {}, 'Last request attempt'),
      el('p', { class: 'empty' }, 'No recorded request attempt in this session.')));

  const request = saved.catalog;
  const sources = request.sources || {};
  const mcp = request.mcp || {};
  const tools = (request.tools || []).slice(0, 32);
  const included = (sources.sources || []).slice(0, 32);
  const historical = el('section', { class: 'card' },
    el('h2', {}, `Last request attempt · event #${saved.event_seq}`),
    el('p', { class: 'sub' }, 'Historical snapshot of an attempted request. Recorded sources and tools may differ from the current workspace or server.'),
    el('p', {}, `${limited(request.provider_id, 96)} · ${limited(request.delivery, 16)} tools · ${limited(request.detail, 16)} schemas · ~${request.used_tokens} tokens used`),
    el('h3', {}, `Offered tools (${request.tool_count})`),
    rows(['name', 'schema tokens'], tools.map(tool => [limited(tool.name, 64), `~${tool.estimated_tokens}`])),
    request.omitted_tools ? el('p', { class: 'sub' }, `${request.omitted_tools} offered names omitted from this view.`) : null);
  if (mcp.discovered > 0) {
    historical.append(...[el('h3', {}, 'MCP catalog'),
      el('p', {}, `${mcp.discovered} discovered · ${mcp.advertised} directly offered · ${mcp.deferred} deferred via mcp_tools / mcp_call`),
      el('ul', {}, (mcp.deferred_names || []).slice(0, 16).map(name => el('li', {}, limited(name, 64)))),
      mcp.omitted_deferred ? el('p', { class: 'sub' }, `${mcp.omitted_deferred} deferred names omitted from this view.`) : null].filter(Boolean));
  }
  historical.append(...[el('h3', {}, 'Included sources'),
    el('p', { class: 'sub' }, `${sources.discovered_skills || 0} skills discovered · ${sources.applicable_skills || 0} applicable · ${sources.advertised_skills || 0} advertised · ${sources.loaded_skill_events || 0} loaded events`),
    rows(['kind', 'name', 'included as', 'origin', 'tokens'], included.map(source => [
      limited(source.kind, 32), limited(source.name, 64), limited(source.inclusion, 16),
      limited(source.origin, 192), `~${source.estimated_tokens}${source.complete === false ? ' partial' : ''}`
    ])),
    sources.omitted_sources ? el('p', { class: 'sub' }, `${sources.omitted_sources} sources omitted from this view.`) : null].filter(Boolean));
  return el('div', { class: 'context-view' }, current, historical);
}

export async function renderContext() {
  const turn = ++generation;
  const { items } = await api('/api/sessions');
  if (turn !== generation || state.tab !== 'context') return;
  let selected = items.find(item => String(item.id) === String(state.session));
  if (!selected && items.length) {
    selected = items[0];
    state.session = selected.id;
  }
  const shown = items.slice(0, 100);
  if (selected && !shown.includes(selected)) shown.unshift(selected);
  const list = el('ul', { class: 'list' }, shown.map(item => el('li', {
    'aria-current': String(String(item.id) === String(state.session)),
    onclick: () => { state.session = item.id; renderContext(); }
  }, el('div', { class: 'name' }, limited(item.title || '(untitled)', 128)),
  el('div', { class: 'sub' }, limited(item.id, 32)))));
  const detail = el('div', { class: 'context-view' });
  let request = 0;
  const load = async () => {
    if (!selected) {
      detail.replaceChildren(el('p', { class: 'empty' }, 'No session selected.'));
      return;
    }
    const read = ++request;
    detail.replaceChildren(el('p', { class: 'empty' }, 'reading context…'));
    try {
      const usage = await api(`/api/sessions/${encodeURIComponent(selected.id)}/context?workspace=${encodeURIComponent(selected.workspace)}`);
      if (turn === generation && state.tab === 'context' && read === request)
        detail.replaceChildren(contextPanel(usage));
    } catch (error) {
      if (turn === generation && state.tab === 'context' && read === request)
        detail.replaceChildren(el('p', { class: 'bad' }, error.error || String(error)));
    }
  };
  $('#view').replaceChildren(el('div', { class: 'grid' },
    el('div', { class: 'card' }, el('h2', {}, `sessions (${items.length})`), list,
      items.length > shown.length ? el('p', { class: 'sub' }, `${items.length - shown.length} older sessions omitted from this list.`) : null),
    el('div', {}, el('div', { class: 'row' }, el('button', { onclick: load }, 'Refresh context')), detail)));
  await load();
}
