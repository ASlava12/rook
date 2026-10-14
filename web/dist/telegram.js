import { el, api, state, nav } from './lib.js';
export function telegramPanel() {
  const content = el('div');
  const panel = el('section', {}, el('h2', {}, 'Telegram'),
    el('p', {}, 'Chats and topics keep ordinary sessions. A lost send response requires a reviewed retry; retrying can send a duplicate.'));
  let busy = false, epoch = 0;
  const button = (label, action) => el('button', {onclick: () => request(action)}, label);
  const open = session => el('button', {onclick: () => {state.session = session; nav.go('sessions');}}, 'Open session');
  async function request(action) {
    if (busy) return;
    if (['retry', 'discard'].includes(action.action)) {
      const note = prompt(action.action === 'retry' ? 'Explain why this inspected message should be retried. It may already have been delivered.' : 'Explain why this inspected delivery should be discarded.');
      if (!note?.trim() || new TextEncoder().encode(note).length > 512) return;
      action = {...action, note};
    }
    busy = true; const generation = ++epoch;
    content.replaceChildren(el('p', {}, 'Reading Telegram receipts…'));
    try {
      const reply = await api('/api/telegram', action);
      if (generation !== epoch || !panel.isConnected) return;
      if (reply.routes && Array.isArray(reply.routes)) {
        content.replaceChildren(...reply.routes.map(r => el('article', {}, el('p', {}, `${r.chat}/${r.topic} · ${r.session}`), open(r.session))),
          ...(reply.after ? [button('More chats', {action:'routes', after:reply.after})] : []));
      } else if (reply.outbox && Array.isArray(reply.outbox)) {
        content.replaceChildren(...reply.outbox.map(i => el('article', {}, el('p', {}, `${i.id} · ${i.state} · attempt ${i.attempts}`),
          button('Inspect message', {action:'inspect', id:i.id}), open(i.session))),
          ...(reply.after ? [button('More messages', {action:'outbox', after:reply.after})] : []));
      } else if (reply.inputs && Array.isArray(reply.inputs)) {
        content.replaceChildren(...reply.inputs.map(i => el('article', {}, el('p', {}, `${i.update} · ${i.state}`), button('Inspect input', {action:'input',update:i.update}), open(i.session))), ...(reply.after != null ? [button('More inputs', {action:'inputs',after:reply.after})] : []));
      } else if (reply.update != null && reply.text != null) {
        content.replaceChildren(el('p', {}, `Input ${reply.update} · ${reply.state} · ${reply.mode || 'pending dispatch'}`), el('pre', {}, reply.text), el('p', {}, reply.error || ''), open(reply.session));
      } else if (reply.text != null) {
        const controls = [], observed = {id:reply.id, attempts:reply.attempts};
        if (['failed','uncertain'].includes(reply.state)) controls.push(button('Retry inspected message', {action:'retry',...observed}));
        if (['failed','uncertain','pending'].includes(reply.state)) controls.push(button('Discard inspected message', {action:'discard',...observed}));
        if (['sent','discarded'].includes(reply.state)) controls.push(button('Forget resolved receipt', {action:'forget',...observed}));
        content.replaceChildren(el('p', {}, `${reply.id} · ${reply.state} · attempt ${reply.attempts}`), el('pre', {}, reply.text),
          el('p', {}, reply.error || reply.note || ''), open(reply.session), ...controls);
      } else content.replaceChildren(el('pre', {}, JSON.stringify(reply, null, 2)));
    } catch (error) {
      if (generation === epoch && panel.isConnected) content.replaceChildren(el('p', {class:'warn'}, error.error || String(error)));
    } finally { if (generation === epoch) busy = false; }
  }
  panel.append(button('Bot status', {action:'status'}), button('Chats and topics', {action:'routes',after:null}),
    button('Outbound messages', {action:'outbox',after:null}), button('Input receipts', {action:'inputs',after:null}), content);
  return panel;
}
