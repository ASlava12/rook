// Sessions: what each one changed on disk, its transcript as it grows, and
// the two things a person does with one — continue it, or rewind it.
import { $, el, api, ago, state, nav } from './lib.js';
import { continueIn, quoteIntoDraft, branchFromEvent } from './chat.js';
import { historyPanel } from './history.js';
import { activityPhase } from './activity.js';
import { branchPanel } from './branches.js';
import { backgroundPanel } from './background.js';
import { worktreePanel } from './worktrees.js';
import { reviewPanel } from './reviews.js';
import { shellUndoPanel } from './shell-undo.js';
import { experimentPanel } from './experiments.js';
import { learningPanel } from './learning.js';
import { environmentPanel } from './environments.js';
import { recipesPanel } from './recipes.js';

let follow = 0;

async function rewindTo(seq) {
  const files = confirm(`Rewind to #${seq}. Put the workspace files back too?\n\n` +
    'Cancel forks the conversation alone, which changes nothing on disk.');
  try {
    const done = await api(`/api/sessions/${state.session}/rewind`, { to_seq: seq, restore_files: files });
    state.session = done.session;
    renderSessions();
  } catch (e) {
    alert(e.error || e);
  }
}

export async function renderSessions() {
  const token = ++follow;
  const { items } = await api('/api/sessions?tree=true');
  if (!state.session && items.length) state.session = items[0].id;
  const attention = el('section', { class: 'card' }, el('h2', {}, 'Waiting for you'));
  const attentionRows = el('div');
  const attentionNext = el('button', { hidden: true }, 'Scan next sessions');
  let attentionCursor = null, attentionPending = false;
  async function loadAttention(after = null) {
    if (attentionPending) return;
    attentionPending = true;
    try {
      const page = await api(`/api/attention?limit=256${after ? `&after=${encodeURIComponent(after)}` : ''}`);
      if (token !== follow || state.tab !== 'sessions') return;
      const nodes = page.items.map(item => el('div', {},
        el('span', { class: 'session-tree-prefix', 'aria-hidden': true }, item.prefix || ''),
        el('button', { onclick: () => continueIn(item.session) }, item.title || item.session),
        el('span', {}, item.live ? ` · ${item.live.questions} questions · ${item.live.approvals} permissions · ${item.live.secrets} private inputs` : ' · saved observation; owner unknown'),
        item.parent ? el('button', { onclick: () => continueIn(item.parent) }, `Parent: ${item.parent}`) : null));
      attentionRows.replaceChildren(...nodes, ...(nodes.length ? [] : [el('p', {}, 'No requests in this scanned page.')]));
      attentionCursor = page.next;
      attentionNext.hidden = !page.next;
    } catch (error) { attentionRows.textContent = error.error || String(error); }
    finally { attentionPending = false; }
  }
  attentionNext.onclick = () => loadAttention(attentionCursor);
  attention.append(el('p', {}, 'Open a session to answer its current request. You can skip or decline; secret values are never listed here.'),
    el('button', { onclick: () => loadAttention() }, 'Refresh waiting sessions'), attentionRows, attentionNext);
  loadAttention();
  const list = el('ul', { class: 'list' }, items.map(s => el('li', {
      'aria-current': String(String(s.id) === String(state.session)),
      onclick: () => { state.session = s.id; renderSessions(); }
    },
    el('div', { class: 'name' },
      el('span', { class: 'session-tree-prefix', 'aria-hidden': true }, s.tree?.prefix || ''),
      s.title || '(untitled)'),
    el('div', { class: 'sub' }, `${ago(s.updated_at)} · ${s.event_count} events · ${s.model || '—'}`),
    s.forked_at != null ? el('div', { class: 'sub' }, `forked at event ${s.forked_at}`) : null,
    s.goal ? el('div', { class: 'sub' }, `goal: ${s.goal}`) : null)));

  const right = el('div', { class: 'card' }, el('h2', {}, 'transcript'));
  if (state.session) {
    const chosen = items.find(s => String(s.id) === String(state.session)) || {};
    right.append(el('div', { class: 'row' },
      el('button', { onclick: () => continueIn(state.session) }, 'Continue in chat'),
      el('button', { onclick: () => nav.go('context') }, 'Inspect context'),
      el('label', {}, 'goal '),
      el('input', { id: 'goal', placeholder: 'what this session is for', value: chosen.goal || '' }),
      el('button', { onclick: async () => {
        await api(`/api/sessions/${state.session}/goal`, { goal: $('#goal').value });
        renderSessions();
      } }, 'Set')));

    const activity = el('pre', { 'aria-live': 'polite' });
    right.append(el('div', { class: 'row' }, el('button', { onclick: async () => {
      const session = state.session;
      try {
        const report = await api(`/api/sessions/${session}/diagnostics?logs=false`);
        if (!activity.isConnected || state.session !== session) return;
        const a = report.activity;
        const phase = activityPhase(a);
        activity.textContent = a ? `${a.session}: ${a.owner_live === null ? 'saved observation; owner unknown' : a.owner_live ? 'running' : 'owner stopped'}; ${phase}${a.operation ? ` · ${a.operation}` : ''}; no observed progress for ${a.idle_secs}s; last progress ${new Date(a.last_progress_at * 1000).toISOString()}${a.stalled ? '; inactivity threshold reached' : ''}` : 'No activity observation has been recorded for this session.';
        if (report.runtime) activity.textContent += `\nDaemon timings: ${report.runtime.aggregates.map(row => `${row.operation}/${row.metric}: max ${row.stats.max_ms} ms, slow ${row.stats.slow}`).join('; ')}. Observations may include waits; they do not establish a cause.`;
      } catch (error) { if (activity.isConnected) activity.textContent = String(error.error || error); }
    } }, 'Check activity')), activity);

    const includeLogs = el('input', { type: 'checkbox', 'aria-label': 'Include redacted log tails' });
    right.append(el('div', { class: 'row' },
      el('label', {}, includeLogs, ' Include redacted logs (may contain private text)'),
      el('button', { onclick: async () => {
        const session = state.session;
        try {
          const report = await api(`/api/sessions/${session}/diagnostics?logs=${includeLogs.checked}`);
          const url = URL.createObjectURL(new Blob([JSON.stringify(report, null, 2)], { type: 'application/json' }));
          const link = el('a', { href: url, download: `rook-diagnostics-${session}.json` });
          document.body.append(link);
          link.click();
          link.remove();
          setTimeout(() => URL.revokeObjectURL(url), 1000);
        } catch (error) { alert(error.error || error); }
      } }, 'Download diagnostics')));

    const receipts = await api(`/api/sessions/${state.session}/recovery`);
    if (receipts.length) {
      const recovery = el('details', { open: receipts.some(r => r.unknown.length > 0) },
        el('summary', {}, 'Execution and recovery'));
      for (const receipt of receipts) {
        recovery.append(el('p', {}, `${receipt.session}: ${receipt.status}; ${receipt.completed_operations} recorded operations; owner ${receipt.owner} (pid ${receipt.pid})`));
        recovery.append(el('pre', {}, receipt.task));
        for (const op of [receipt.pending, ...receipt.background].filter(Boolean)) {
          recovery.append(el('p', {}, `Last observed in progress: ${op.tool} (${op.id})`), el('pre', {}, op.arguments));
        }
        for (const op of receipt.unknown) {
          const note = el('textarea', { 'aria-label': 'Inspection note', placeholder: 'What you inspected and what happened', maxlength: 4096 });
          const acknowledge = el('button', { onclick: async () => {
            try {
              await api(`/api/sessions/${receipt.session}/recovery`, { operation: op.id, note: note.value });
              await renderSessions();
            } catch (error) { alert(error.error || error); }
          } }, 'Record inspection');
          recovery.append(el('p', {}, `Unknown result: ${op.tool} (${op.id}), source ${op.session} event #${op.call_seq}`),
            el('pre', {}, op.arguments),
            el('p', { class: 'sub' }, 'Recording inspection allows changes to resume. It does not repeat this operation or mark it successful.'),
            el('div', { class: 'row' }, note, acknowledge));
        }
      }
      right.append(recovery);
    }

    // What it changed on disk, before what it said: that is the question a
    // transcript is usually being read to answer.
    const changed = await api(`/api/sessions/${state.session}/changes`);
    if (changed.files.length) {
      right.append(el('table', {},
        el('tr', {}, ['file', '', '+', '−'].map(h => el('th', {}, h))),
        changed.files.map(f => el('tr', {},
          el('td', { class: 't' }, f.path),
          el('td', { class: 'tag' }, String(f.change).toLowerCase()),
          el('td', {}, `+${f.lines_added}`),
          el('td', {}, `−${f.lines_removed}`)))));
    }
    // Legacy shell observations only name paths; optional retained shell
    // receipts below provide their own source and restoration contract.
    const byCommand = changed.written_by_commands || [];
    if (byCommand.length) {
      right.append(el('p', { class: 'sub' }, 'written by commands — check shell receipts below for retained preimages:'));
      right.append(el('ul', {}, byCommand.map(p => el('li', { class: 'sub' }, p))));
    }
    if (changed.watched === false) {
      right.append(el('p', { class: 'warn' }, 'the workspace was too large to walk, so more may have been written'));
    }
    const session = state.session;
    right.append(worktreePanel(session, items.filter(s => String(s.parent) === String(session)),
      () => token === follow && state.tab === 'sessions' && String(state.session) === String(session) && right.isConnected));
    const branches = el('details', {}, el('summary', {}, 'Conversation branches'));
    branches.addEventListener('toggle', () => {
      branches.querySelector('section')?.remove();
      if (branches.open) branches.append(branchPanel(session, continueIn, quoteIntoDraft, branchFromEvent, () => renderSessions()));
    });
    right.append(branches);
    const background = el('details', {}, el('summary', {}, 'Background work · session family'));
    background.addEventListener('toggle', () => {
      background.querySelector('section')?.remove();
      if (background.open) background.append(backgroundPanel(session));
    });
    right.append(background);
    right.append(historyPanel(session, text => quoteIntoDraft(session, text), rewindTo, seq => branchFromEvent(session, seq)));
    const reviews=el('details',{},el('summary',{},'Change reviews'));
    reviews.ontoggle=()=>{if(reviews.open && reviews.children.length===1) reviews.append(reviewPanel(session,quoteIntoDraft));};
    right.append(reviews);
    const shell=el('details',{},el('summary',{},'Shell changes and undo'));
    shell.ontoggle=()=>{if(shell.open && shell.children.length===1) shell.append(shellUndoPanel(session));};
    right.append(shell);
    const experiments=el('details',{},el('summary',{},'Experiments'));
    experiments.ontoggle=()=>{if(experiments.open && experiments.children.length===1) experiments.append(experimentPanel(session));};
    right.append(experiments);
    const environment=el('details',{},el('summary',{},'Execution environment'));
    environment.ontoggle=()=>{if(environment.open && environment.children.length===1) environment.append(environmentPanel(session));};
    right.append(environment);
    const learning=el('details',{},el('summary',{},'Knowledge proposals'));
    learning.ontoggle=()=>{if(learning.open && learning.children.length===1) learning.append(learningPanel(session));};
    right.append(learning);
    const recipes=el('details',{},el('summary',{},'Composite recipes'));
    recipes.ontoggle=()=>{if(recipes.open&&recipes.children.length===1)recipes.append(recipesPanel(session));};
    right.append(recipes);
  } else {
    right.append(el('p', { class: 'empty' }, 'no sessions yet — run `rook run "…"`'));
  }
  if (token !== follow || state.tab !== 'sessions') return;
  $('#view').replaceChildren(attention, el('div', { class: 'grid' },
    el('div', { class: 'card' }, el('h2', {}, `sessions (${items.length})`), list), right));
}
