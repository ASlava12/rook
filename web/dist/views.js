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

// Named snapshots of the workspace: what somebody takes before a risky change
// and puts back after one. Both were a terminal away — `rook checkpoint
// create`, then `rook checkpoint restore <object> --to <dir>` with the id read
// off a third command.
export async function renderCheckpoints() {
  const [{ items }, health] = await Promise.all([api('/api/checkpoints'), api('/api/health')]);
  // The reference is `checkpoint/<name>/<id>`; the name is what was typed.
  const named = (ref) => (ref.startsWith('checkpoint/') ? ref.slice(11).replace(/\/[^/]*$/, '') : ref);

  const take = async () => {
    const name = $('#snapshot').value.trim();
    if (!name) return;
    try {
      await api('/api/checkpoints', { name });
      renderCheckpoints();
    } catch (e) {
      alert(e.error || e);
    }
  };

  const restore = async (row) => {
    if (!confirm(`Restore "${named(row.ref)}" over ${health.workspace}? Files there are written over.`)) return;
    try {
      const { restored } = await api('/api/checkpoints/restore', { object: row.object, to: health.workspace });
      alert(`${restored} file(s) written`);
    } catch (e) {
      alert(e.error || e);
    }
  };

  const rows = items.map(row => el('tr', {},
    el('td', {}, named(row.ref)),
    el('td', { class: 't' }, row.object.slice(0, 12)),
    el('td', {}, el('button', { onclick: () => restore(row) }, 'Restore'))));

  $('#view').replaceChildren(el('div', { class: 'card' },
    el('h2', {}, `checkpoints (${items.length})`),
    el('p', { class: 'sub' }, `of ${health.workspace}`),
    el('form', { class: 'ask', onsubmit: (e) => { e.preventDefault(); take(); } },
      el('input', { id: 'snapshot', placeholder: 'name this snapshot…' }),
      el('button', {}, 'Take one')),
    items.length
      ? el('table', {}, el('tr', {}, ['name', 'object', ''].map(h => el('th', {}, h))), rows)
      : el('p', { class: 'empty' }, 'nothing snapshotted yet — the agent takes its own before every write, and those are what a rewind puts back; these are yours')));
}

// Secrets, managed and never shown. The page can set one and drop one; there is
// no call anywhere that returns a value, so there is nothing here to leak — the
// list is names, where each comes from, and whether it answers.
export async function renderSecrets() {
  const { items } = await api('/api/secrets');

  const set = async () => {
    const name = $('#secret-name').value.trim();
    const value = $('#secret-value').value;
    const from = $('#secret-source').value.trim();
    if (!name) return;
    if (!value && !from) { alert('a secret needs a value or a source'); return; }
    try {
      // One of the two, and the value never comes back: the field is cleared
      // here rather than waiting for a re-render, so it is not sitting in the
      // page while the request is in flight.
      const body = from ? { name, source: from } : { name, value };
      $('#secret-value').value = '';
      await api('/api/secrets', body);
      renderSecrets();
    } catch (e) {
      alert(e.error || e);
    }
  };

  const drop = async (row) => {
    if (!confirm(`Drop the secret "${row.name}"? Anything using it stops working.`)) return;
    try {
      await api('/api/secrets/forget', { name: row.name });
      renderSecrets();
    } catch (e) {
      alert(e.error || e);
    }
  };

  const rows = items.map(row => el('tr', {},
    el('td', {}, row.name),
    el('td', { class: 't' }, row.source),
    el('td', { class: row.resolves ? 'ok' : 'warn' }, row.resolves ? 'answers' : 'does not answer'),
    el('td', {}, el('button', { onclick: () => drop(row) }, 'Drop'))));

  $('#view').replaceChildren(el('div', { class: 'card' },
    el('h2', {}, `secrets (${items.length})`),
    el('p', { class: 'sub' }, 'The agent uses these by name — run_command {"secrets": ["name"]} — and never sees a value. Nothing here, and no other page, can read one back.'),
    el('form', { class: 'ask', onsubmit: (e) => { e.preventDefault(); set(); } },
      el('input', { id: 'secret-name', placeholder: 'name, e.g. ssh_prod' }),
      el('input', { id: 'secret-value', type: 'password', autocomplete: 'new-password', placeholder: 'value…' }),
      el('input', { id: 'secret-source', placeholder: 'or where it lives: env:NAME, cmd:…' }),
      el('button', {}, 'Keep')),
    items.length
      ? el('table', {}, el('tr', {}, ['name', 'from', '', ''].map(h => el('th', {}, h))), rows)
      : el('p', { class: 'empty' }, 'nothing set — a value typed here is kept on this machine in ~/.rook/secrets.toml, 0600; a source names where it already lives and keeps it there')));
}

// The documentation the agent gathered, with both addresses on every page: the
// local copy an answer was made of, and the source anybody else can check.
// Gathering from here as well as from a terminal, because a page that can only
// show what somebody else collected is a report, not a tool.
export async function renderDocs() {
  const { items } = await api('/api/docs');
  if (!state.docsTopic && items.length) state.docsTopic = `${items[0].topic}/${items[0].version}`;

  const gather = async () => {
    const asked = $('#topic').value.trim();
    if (!asked) return;
    // The last word is a version only when it looks like one: "redis
    // persistence" is two words of topic, and reading the second as a version
    // gathers the wrong thing under a name nobody will find again.
    const words = asked.split(/\s+/);
    const last = words[words.length - 1];
    const versioned = words.length > 1 && (last === 'latest' || /^v?\d/.test(last));
    const topic = versioned ? words.slice(0, -1).join(' ') : asked;
    const version = versioned ? last : undefined;
    const button = $('#gather');
    // It goes to the web and reads several pages, which is seconds rather than
    // milliseconds; a button that looks idle while it works gets pressed twice.
    button.disabled = true;
    button.textContent = 'reading…';
    try {
      const said = await api('/api/docs', { topic, version });
      state.docsTopic = said.ref.replace(/^docs\//, '');
      renderDocs();
    } catch (e) {
      alert(e.error || e);
      button.disabled = false;
      button.textContent = 'Gather';
    }
  };

  const drop = async (row) => {
    if (!confirm(`Drop the ${row.topic} ${row.version} documentation? The agent gathers it again when it is asked.`)) return;
    try {
      await api('/api/docs/forget', { topic: row.topic, version: row.version });
      state.docsTopic = null;
      renderDocs();
    } catch (e) {
      alert(e.error || e);
    }
  };

  const list = el('ul', { class: 'list' }, items.map(row => el('li', {
      'aria-current': String(`${row.topic}/${row.version}` === state.docsTopic),
      onclick: () => { state.docsTopic = `${row.topic}/${row.version}`; renderDocs(); }
    },
    el('div', { class: 'name' }, `${row.topic} ${row.version}`),
    el('div', { class: 'sub' }, `${row.pages} page(s) · ${bytes(row.bytes)} · read ${ago(row.fetched_at)}`))));

  const left = el('div', { class: 'card' },
    el('h2', {}, `docs (${items.length})`),
    el('form', { class: 'ask', onsubmit: (e) => { e.preventDefault(); gather(); } },
      el('input', { id: 'topic', placeholder: 'redis, or redis 6.2…' }),
      el('button', { id: 'gather' }, 'Gather')),
    items.length ? list : el('p', { class: 'empty' }, 'nothing gathered yet — a topic here is read from the web once and answered from afterwards, so the agent cites a page instead of its training'));

  const right = el('div', { class: 'card' }, el('h2', {}, 'sources'));
  if (state.docsTopic) {
    const [topic, version] = state.docsTopic.split('/');
    try {
      const set = await api(`/api/docs/${encodeURIComponent(topic)}?version=${encodeURIComponent(version)}`);
      const row = items.find(r => r.topic === topic && r.version === version);
      right.append(el('p', { class: 'sub' }, `kept as docs/${topic}/${version} · read ${ago(set.fetched_at)}`));
      right.append(el('ul', { class: 'list' }, set.pages.map(page => el('li', {},
        el('div', { class: 'name' }, page.title),
        el('div', { class: 'sub' }, el('a', { href: page.url, target: '_blank', rel: 'noreferrer' }, page.url)),
        el('p', {}, page.text.slice(0, 400) + (page.text.length > 400 ? '…' : ''))))));
      if (row) right.append(el('button', { onclick: () => drop(row) }, 'Drop this set'));
    } catch (e) {
      right.append(el('p', { class: 'warn' }, e.error || String(e)));
    }
  }

  $('#view').replaceChildren(el('div', { class: 'split' }, left, right));
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
    // Taking a version and going back to one were `rook skills capture` and
    // `rook skills rollback <name> <object>` in a terminal, with the id read
    // off this very table. The page that shows the table can do both.
    const act = async (path, body, whether) => {
      if (whether && !confirm(whether)) return;
      try {
        await api(path, body);
        renderSkills();
      } catch (e) {
        alert(e.error || e);
      }
    };
    const capture = el('button', {
      onclick: () => act(`/api/skills/${encodeURIComponent(state.skill)}/capture`,
        { message: 'captured from the browser' }),
    }, 'Capture a version');

    right.append(el('h2', { style: 'margin-top:1rem' },
      `versions (${history.length})`, ' ', capture));
    if (!history.length) {
      right.append(el('p', { class: 'sub' }, 'no captures yet — Capture takes one, and each row can be restored'));
    } else {
      right.append(el('table', {},
        el('tr', {}, ['object', 'version', 'captured', 'files', 'size', 'note', ''].map(h => el('th', {}, h))),
        history.map(h => el('tr', {},
          el('td', {}, h.object.slice(0, 12)),
          el('td', {}, h.version),
          el('td', {}, new Date(h.captured_at * 1000).toISOString().slice(0, 16).replace('T', ' ')),
          el('td', {}, String(h.files)),
          el('td', {}, bytes(h.bytes)),
          el('td', { class: 't' }, h.note || ''),
          // What is there now is captured first, so this is itself undoable —
          // and the asking is because it writes over a directory.
          el('td', {}, el('button', {
            onclick: () => act(`/api/skills/${encodeURIComponent(state.skill)}/rollback`,
              { object: h.object },
              `Roll ${state.skill} back to ${h.object.slice(0, 12)}? What is there now is captured first.`),
          }, 'Roll back'))))));
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
