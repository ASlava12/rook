// Sessions: what each one changed on disk, its transcript as it grows, and
// the two things a person does with one — continue it, or rewind it.
import { $, el, api, ago, bytes, state } from './lib.js';
import { continueIn } from './chat.js';

// A transcript being written elsewhere — a `rook run` in a terminal, a TUI —
// grows under the reader. Polled while this tab is showing and the page is
// visible, from the last entry seen; stopped by the next render.
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

function entry(e) {
  return el('div', { class: 'entry' },
    el('div', { class: 'hd' },
      el('span', {}, `#${e.seq}`),
      el('span', { class: 'tag' }, e.kind),
      e.label ? el('span', {}, e.label) : null,
      el('span', {}, `${bytes(e.bytes)} → ${bytes(e.stored_bytes)}`),
      e.truncated ? el('span', { class: 'warn' }, 'elided') : null),
    el('pre', {}, e.body),
    // Rewinding forks rather than truncating, so nothing is lost — but it
    // does put files back, which is why it asks first.
    el('button', { class: 'quiet', onclick: () => rewindTo(e.seq) }, `rewind to #${e.seq}`));
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
      el('label', {}, 'goal '),
      el('input', { id: 'goal', placeholder: 'what this session is for', value: chosen.goal || '' }),
      el('button', { onclick: async () => {
        await api(`/api/sessions/${state.session}/goal`, { goal: $('#goal').value });
        renderSessions();
      } }, 'Set')));

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
    const { items: entries } = await api(`/api/sessions/${state.session}/transcript?limit=200`);
    const box = el('div', { class: 'scroll' });
    if (!entries.length) box.append(el('p', { class: 'empty' }, 'no events yet'));
    for (const e of entries) box.append(entry(e));
    right.append(box);

    let last = entries.length ? entries[entries.length - 1].seq : 0;
    const session = state.session;
    const tick = async () => {
      if (token !== follow || state.tab !== 'sessions' || String(state.session) !== String(session)) return;
      if (!document.hidden) {
        try {
          const more = await api(`/api/sessions/${session}/transcript?from=${last + 1}&limit=200`);
          for (const e of more.items) { box.append(entry(e)); last = Math.max(last, e.seq); }
          if (more.items.length) box.scrollTop = box.scrollHeight;
        } catch { /* the next tick tries again */ }
      }
      if (token === follow) setTimeout(tick, 3000);
    };
    setTimeout(tick, 3000);
  } else {
    right.append(el('p', { class: 'empty' }, 'no sessions yet — run `rook run "…"`'));
  }
  $('#view').replaceChildren(el('div', { class: 'grid' },
    el('div', { class: 'card' }, el('h2', {}, `sessions (${items.length})`), list), right));
}
