import { el, api, jsonWithin } from './lib.js';
export function learningPanel(owner) {
  let pending = false, epoch = 0;
  const body = el('div');
  const panel = el('section', {}, el('h2', {}, 'Knowledge proposals'),
    el('p', { class: 'sub' }, 'Review what a completed task supports before saving a memory or reusable skill. A proposal is a draft; source quotations do not prove a general rule.'));
  const render = (...nodes) => body.replaceChildren(...nodes.filter(node => node != null));
  const button = (label, action) => el('button', { onclick: () => request(typeof action === 'function' ? action() : action) }, label);
  const request = async action => {
    if (pending) return;
    let generation;
    try {
      if (action.error) throw new Error(action.error);
      if (!jsonWithin(action, 65536)) throw new Error('Request exceeds 64 KiB. Shorten the draft or select fewer evidence anchors.');
      pending = true; generation = ++epoch; render(el('p', {}, action.action === 'suggest' ? 'Reviewing completed task…' : 'Reading knowledge proposal…'));
      const reply = await api(`/api/sessions/${encodeURIComponent(owner)}/learning`, action);
      if (generation !== epoch || !panel.isConnected) return;
      if (reply.items || reply.proposals) list(reply.items || reply.proposals, reply.attempts || []);
      else if (reply.duplicate) show(reply.proposal);
      else if (reply.token) preview(reply);
      else if (reply.id) show(reply);
      else render(el('pre', {}, JSON.stringify(reply, null, 2)));
    } catch (error) {
      if (panel.isConnected && (generation === undefined || generation === epoch)) render(el('p', { class: 'warn' }, String(error.error || error)),
        action.proposal ? button('Inspect saved proposal', { action: 'inspect', proposal: action.proposal }) : null);
    } finally { if (generation === undefined || generation === epoch) pending = false; }
  };
  const quote = source => el('details', {}, el('summary', {}, `Task result #${source.result} · ${source.turn}`),
    el('p', { class: 'sub' }, `Retained source: ${source.signature}`),
    ...(source.events || []).map(event => el('div', {}, el('p', {}, `#${event.seq} · ${event.kind}${event.truncated ? ' · unavailable within source budget' : ''}`), el('pre', {}, event.text))),
    source.omitted_events ? el('p', { class: 'warn' }, `${source.omitted_events} source events omitted.`) : null);
  const list = (items, attempts) => render(items.length ? el('div', {}, ...items.map(proposal =>
    button(`${proposal.id} · ${proposal.state} · ${proposal.draft.target.kind}`, { action: 'inspect', proposal: proposal.id }))) : el('p', {}, 'No proposals. Select a completed task to request a review, or enable automatic proposals in configuration.'),
    ...attempts.map(attempt => el('p', { class: 'sub' }, `Review of task result #${attempt.result}: ${attempt.state}${attempt.error ? ` · ${attempt.error}` : ''}`)));
  const preview = reply => {
    const reviewed = { proposal: reply.proposal, token: reply.token };
    render(quote(reply.source), el('pre', {}, JSON.stringify(reply.diff, null, 2)),
      button(reply.rollback ? 'Confirm rollback' : 'Apply reviewed change', { action: reply.rollback ? 'rollback' : 'apply', ...reviewed }),
      button('Back to proposal', { action: 'inspect', proposal: reply.proposal }));
  };
  const show = proposal => {
    const editor = el('textarea', { 'aria-label': 'Reviewed learning draft', maxlength: 65536 }, JSON.stringify(proposal.draft, null, 2));
    const note = el('textarea', { 'aria-label': 'Learning inspection note', maxlength: 4096 });
    const revise = () => {
      try { return { action: 'revise', proposal: proposal.id, draft: JSON.parse(editor.value) }; }
      catch { return { error: 'The reviewed draft must be valid JSON.' }; }
    };
    render(el('p', {}, `${proposal.id} · ${proposal.state}`), quote(proposal.source), el('p', {}, proposal.draft.reason),
      proposal.predecessor ? el('p', { class: 'sub' }, `Revises proposal ${proposal.predecessor}`) : null,
      proposal.error ? el('p', { class: 'warn' }, proposal.error) : null,
      proposal.state === 'proposed' ? el('div', {}, editor, button('Save reviewed revision', revise),
        button('Preview change', { action: 'prepare', proposal: proposal.id }),
        button('Reject proposal', { action: 'reject', proposal: proposal.id, note: 'Rejected during user review' })) : null,
      ['applied', 'partial', 'rollback_partial'].includes(proposal.state) ? button('Preview rollback', { action: 'prepare_rollback', proposal: proposal.id }) : null,
      ['applying', 'rolling_back', 'partial'].includes(proposal.state) ? el('div', {},
        el('p', {}, `Inspect change session ${proposal.session} and acknowledge unknown execution effects before recording recovery.`), note,
        button('Record inspected recovery', () => ({ action: 'recover', proposal: proposal.id, note: note.value }))) : null);
  };
  const tasks = async before => {
    if (pending) return;
    pending = true; const generation = ++epoch; render(el('p', {}, 'Finding completed tasks…'));
    try {
      const reply = await api(`/api/sessions/${encodeURIComponent(owner)}/turns${before == null ? '' : `?before=${encodeURIComponent(before)}`}`);
      if (generation !== epoch || !panel.isConnected) return;
      render(...(reply.items || []).map(entry => el('div', {}, el('p', {}, `${entry.summary.stopped} · ${entry.summary.reply}`),
        button('Review this completed task', { action: 'suggest', result: entry.result_seq }))),
        reply.before != null ? el('button', { onclick: () => tasks(reply.before) }, 'Find older tasks') : null);
    } catch (error) { if (generation === epoch && panel.isConnected) render(el('p', { class: 'warn' }, String(error.error || error))); }
    finally { if (generation === epoch) pending = false; }
  };
  panel.append(button('List knowledge proposals', { action: 'list' }), el('button', { onclick: () => tasks(null) }, 'Find completed tasks'), body);
  return panel;
}
