// Sessions: what each one changed on disk, its transcript as it grows, and
// the two things a person does with one — continue it, or rewind it.
import { $, el, api, ago, state, nav } from './lib.js';
import { continueIn, quoteIntoDraft, branchFromEvent } from './chat.js';
import { historyPanel } from './history.js';
import { branchPanel } from './branches.js';
import { worktreePanel } from './worktrees.js';

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
  const { items } = await api('/api/sessions');
  if (!state.session && items.length) state.session = items[0].id;
  const list = el('ul', { class: 'list' }, items.map(s => el('li', {
      'aria-current': String(String(s.id) === String(state.session)),
      onclick: () => { state.session = s.id; renderSessions(); }
    },
    el('div', { class: 'name' }, s.title || '(untitled)'),
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
    // A command declares no paths, so nothing holds what these were before:
    // they can be named and neither diffed nor put back.
    const byCommand = changed.written_by_commands || [];
    if (byCommand.length) {
      right.append(el('p', { class: 'sub' }, 'written by commands — nothing kept to diff or restore:'));
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
    right.append(historyPanel(session, text => quoteIntoDraft(session, text), rewindTo, seq => branchFromEvent(session, seq)));
  } else {
    right.append(el('p', { class: 'empty' }, 'no sessions yet — run `rook run "…"`'));
  }
  if (token !== follow || state.tab !== 'sessions') return;
  $('#view').replaceChildren(el('div', { class: 'grid' },
    el('div', { class: 'card' }, el('h2', {}, `sessions (${items.length})`), list), right));
}
