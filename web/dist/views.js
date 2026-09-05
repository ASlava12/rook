// The read-mostly tabs: search, memory, skills, jobs and the store.
import { $, el, api, ago, bytes, state } from './lib.js';

export async function renderSearch() {
  const input = el('input', { placeholder: 'Search everything said, read and run…', value: state.query });
  const results = el('div', { class: 'scroll' });

  const look = async () => {
    state.query = input.value.trim();
    if (!state.query) { results.replaceChildren(); return; }
    results.replaceChildren(el('p', { class: 'empty' }, 'searching…'));
    const found = await api(`/api/search?q=${encodeURIComponent(state.query)}`);
    if (!found.hits.length) {
      results.replaceChildren(el('p', { class: 'empty' }, `nothing in ${found.objects_scanned} objects`));
      return;
    }
    results.replaceChildren(...found.hits.map(h => el('div', { class: 'entry' },
      el('div', { class: 'hd' },
        el('span', {}, h.title || '(untitled)'),
        el('span', { class: 'tag' }, h.kind),
        // A file hit belongs to a path, not to a place in a transcript.
        el('span', {}, h.file ? h.file : `#${h.seq}`),
        el('span', {}, ago(h.when))),
      el('pre', {}, h.snippet))),
      el('p', { class: 'sub' },
        `${found.hits.length} hit(s) across ${found.objects_scanned} object(s)` +
        (found.truncated ? ' — the scan hit its budget' : '')));
  };

  const form = el('form', { class: 'ask', onsubmit: (e) => { e.preventDefault(); look(); } },
    input, el('button', {}, 'Find'));

  $('#view').replaceChildren(el('div', { class: 'card' }, el('h2', {}, 'search'), form, results));
  input.focus();
  if (state.query) look();
}

// What the agent believes about you, and a way to correct it. Forgetting is the
// point: a fact nobody can remove is one that quietly steers every later turn.
export async function renderMemory() {
  const all = state.memoryAll;
  const { items } = await api(`/api/memory?all=${all}`);

  const forget = async (fact) => {
    if (!confirm(`Forget "${fact.text}"?`)) return;
    try {
      await api('/api/memory', { id: fact.id });
      renderMemory();
    } catch (e) {
      alert(e.error || e);
    }
  };

  const typed = el('input', { placeholder: 'Something the agent should know…' });
  const everywhere = el('input', { type: 'checkbox' });
  const add = async () => {
    const text = typed.value.trim();
    if (!text) return;
    try {
      await api('/api/memory/add', { text, global: everywhere.checked });
      renderMemory();
    } catch (e) {
      alert(e.error || e);
    }
  };

  const rows = items.map(f => el('tr', {},
    el('td', { class: 't' }, f.id),
    el('td', {}, f.pinned ? 'pinned' : ''),
    el('td', { class: 't' }, f.scope.kind === 'global' ? 'global' : (f.scope.path || '').split('/').pop()),
    el('td', {}, f.text),
    el('td', { class: 't' }, (f.tags || []).join(' ')),
    el('td', {}, el('button', { onclick: () => forget(f) }, 'Forget'))));

  $('#view').replaceChildren(el('div', { class: 'card' },
    el('h2', {}, `memory (${items.length})`),
    el('div', { class: 'row' },
      el('label', {},
        el('input', { type: 'checkbox', checked: all,
          onchange: (e) => { state.memoryAll = e.target.checked; renderMemory(); } }),
        ' every workspace')),
    // Adding, not only forgetting: what the agent believes is corrected where
    // it is read, and a fact only the command line could write was one more
    // window to go and find.
    el('form', { class: 'ask', onsubmit: (e) => { e.preventDefault(); add(); } },
      typed,
      el('label', {}, everywhere, ' everywhere'),
      el('button', {}, 'Remember')),
    items.length
      ? el('table', {}, el('tr', {}, ['id', '', 'scope', 'fact', 'tags', ''].map(h => el('th', {}, h))), rows)
      : el('p', { class: 'empty' }, 'nothing remembered yet')));
}

export async function renderSkills() {
  const { items } = await api('/api/skills');
  if (!state.skill && items.length) state.skill = items[0].name;
  const list = el('ul', { class: 'list' }, items.map(c => el('li', {
      'aria-current': String(c.name === state.skill),
      onclick: () => { state.skill = c.name; renderSkills(); }
    },
    el('div', { class: 'name' },
      el('span', { class: c.applicable ? 'ok' : 'sub' }, c.applicable ? '✓ ' : '· '), c.name),
    el('div', { class: 'sub' }, `${c.version} · ${c.source} · ~${c.body_tokens} tok`))));

  const right = el('div', { class: 'card' }, el('h2', {}, 'skill'));
  if (state.skill) {
    const card = items.find(c => c.name === state.skill);
    if (card) {
      right.append(el('p', {}, card.description));
      if (!card.applicable) {
        right.append(el('p', { class: 'warn' }, 'blocked in this environment:'));
        right.append(el('ul', {}, card.mismatches.map(m => el('li', { class: 'sub' }, m))));
      }
    }
    let history = [];
    try { history = (await api(`/api/skills/${encodeURIComponent(state.skill)}/history`)).items; } catch { /* none */ }
    right.append(el('h2', { style: 'margin-top:1rem' }, `versions (${history.length})`));
    if (!history.length) {
      right.append(el('p', { class: 'sub' }, `no captures yet — \`rook skills capture ${state.skill}\``));
    } else {
      right.append(el('table', {},
        el('tr', {}, ['object', 'version', 'captured', 'files', 'size', 'note'].map(h => el('th', {}, h))),
        history.map(h => el('tr', {},
          el('td', {}, h.object.slice(0, 12)),
          el('td', {}, h.version),
          el('td', {}, new Date(h.captured_at * 1000).toISOString().slice(0, 16).replace('T', ' ')),
          el('td', {}, String(h.files)),
          el('td', {}, bytes(h.bytes)),
          el('td', { class: 't' }, h.note || '')))));
    }
    try {
      const full = await api(`/api/skills/${encodeURIComponent(state.skill)}`);
      right.append(el('h2', { style: 'margin-top:1rem' }, full.variant ? `body — variant ${full.variant}` : 'body'));
      right.append(el('pre', { class: 'body' }, full.body));
    } catch { /* the card is still worth showing */ }
  }
  $('#view').replaceChildren(el('div', { class: 'grid' },
    el('div', { class: 'card' }, el('h2', {}, `skills (${items.length})`), list), right));
}

// Commands the agent left running. The CLI and the TUI have had `/jobs` since
// they had jobs; a browser could start a dev server and then not see it.
export async function renderJobs() {
  const { items } = await api('/api/jobs');
  if (state.job && !items.some(j => j.id === state.job)) state.job = null;

  const stateOf = (j) => j.exit_code === null || j.exit_code === undefined
    ? `running ${Math.max(0, Math.round(Date.now() / 1000) - j.started_at)}s`
    : `exit ${j.exit_code}`;

  const stopJob = async (id) => {
    try { await api(`/api/jobs/${id}/stop`, {}); renderJobs(); } catch (e) { alert(e.error || e); }
  };

  const list = el('ul', { class: 'list' }, items.map(j => el('li', {
      'aria-current': String(j.id === state.job),
      onclick: () => { state.job = j.id; renderJobs(); }
    },
    el('div', { class: 'name' }, `${j.id} · ${stateOf(j)}`),
    el('div', { class: 'sub' }, j.command))));

  const right = el('div', { class: 'card' }, el('h2', {}, 'output'));
  if (state.job) {
    const full = await api(`/api/jobs/${state.job}`);
    right.append(el('div', { class: 'row' },
      el('span', { class: 'sub' }, `${full.id} · ${stateOf(full)}`),
      full.exit_code === null ? el('button', { onclick: () => stopJob(full.id) }, 'Stop') : null));
    right.append(el('pre', { class: 'body' }, full.output || '(nothing printed yet)'));
  }
  $('#view').replaceChildren(el('div', { class: 'grid' },
    el('div', { class: 'card' }, el('h2', {}, `jobs (${items.length})`),
      items.length ? list : el('p', { class: 'empty' }, 'nothing running in the background')), right));
}

// Deletion is not undoable, so the dry run is offered first and reported in the
// same place as the real one.
async function runMaintenance(dryRun) {
  const out = $('#maintenance-report');
  out.textContent = dryRun ? 'checking…' : 'running…';
  try {
    const r = await api('/api/maintenance', { dry_run: dryRun });
    out.textContent = [
      `${dryRun ? 'would delete' : 'deleted'} ${r.prune.sessions_deleted} session(s), ` +
        `${r.prune.events_deleted} event(s), ${r.prune.protected} protected`,
      `${dryRun ? 'would collect' : 'collected'} ${r.gc.collected} object(s), ${bytes(r.gc.bytes_freed)} freed`,
      r.dictionaries_trained.length
        ? `trained ${r.dictionaries_trained.map(d => `${d[0]} from ${d[1]}`).join(', ')}` : null,
      r.over_budget_by ? `still ${bytes(r.over_budget_by)} over the size budget` : null,
    ].filter(Boolean).join('\n');
    if (!dryRun) renderStore();
  } catch (e) {
    out.textContent = `failed: ${e.error || e}`;
  }
}

export async function renderStore() {
  const s = await api('/api/store/stats');
  const { items } = await api('/api/store/objects?limit=200');
  const max = Math.max(1, ...s.per_kind.map(k => k.bytes_stored));
  const ratio = s.bytes_stored ? (s.bytes_raw / s.bytes_stored) : 1;

  $('#view').replaceChildren(el('div', { class: 'grid', style: 'grid-template-columns:1fr 1fr' },
    el('div', { class: 'card' }, el('h2', {}, 'footprint'),
      el('dl', { class: 'kv' },
        el('dt', {}, 'logical'), el('dd', {}, bytes(s.bytes_raw)),
        el('dt', {}, 'stored'), el('dd', {}, `${bytes(s.bytes_stored)} (${ratio.toFixed(1)}× compression)`),
        el('dt', {}, 'saved by dedup'), el('dd', {}, bytes(s.dedup_saved_hint)),
        el('dt', {}, 'on disk'), el('dd', {}, `${bytes(s.index_bytes + s.external_bytes)} (index ${bytes(s.index_bytes)})`),
        el('dt', {}, 'objects'), el('dd', {}, String(s.objects)),
        el('dt', {}, 'events'), el('dd', {}, String(s.events)),
        el('dt', {}, 'sessions'), el('dd', {}, String(s.sessions)),
        el('dt', {}, 'dictionaries'), el('dd', {},
          s.dictionaries.length ? s.dictionaries.map(d => `${d[0]} ${bytes(d[1])}`).join(', ')
                                : 'none yet — `rook store train`')),
      el('h2', { style: 'margin-top:1.2rem' }, 'by kind'),
      el('table', {},
        el('tr', {}, ['kind', 'objects', 'logical', 'stored', 'ratio', ''].map(h => el('th', {}, h))),
        s.per_kind.map(k => el('tr', {},
          el('td', { class: 't' }, k.kind),
          el('td', {}, String(k.objects)),
          el('td', {}, bytes(k.bytes_raw)),
          el('td', {}, bytes(k.bytes_stored)),
          el('td', {}, (k.bytes_raw / Math.max(1, k.bytes_stored)).toFixed(1) + '×'),
          el('td', {}, el('span', { class: 'bar', style: `width:${(k.bytes_stored / max * 100).toFixed(0)}%` })))))),

    el('div', { class: 'card' }, el('h2', {}, 'maintenance'),
      el('p', { class: 'muted' },
        'Prune to the retention policy, collect what that frees, and enforce the size budget.'),
      el('div', { class: 'row' },
        el('button', { onclick: () => runMaintenance(true) }, 'Dry run'),
        el('button', { onclick: () => runMaintenance(false) }, 'Run')),
      el('pre', { id: 'maintenance-report', class: 'muted' }, '')),

    el('div', { class: 'card' }, el('h2', {}, `objects (newest ${items.length})`),
      el('div', { class: 'scroll' }, el('table', {},
        el('tr', {}, ['id', 'kind', 'logical', 'stored', 'where'].map(h => el('th', {}, h))),
        items.map(o => el('tr', {},
          el('td', {}, o.short),
          el('td', { class: 't' }, o.kind),
          el('td', {}, bytes(o.size_raw)),
          el('td', {}, bytes(o.size_stored)),
          el('td', {}, o.external ? 'file' : 'inline'))))))));
}
