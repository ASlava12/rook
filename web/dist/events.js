import { el, api, state, nav } from './lib.js';
export function eventsPanel() {
  const content = el('div');
  const panel = el('section', {}, el('h2', {}, 'Inbound events'),
    el('p', {}, 'Authenticated deliveries start tasks defined in configuration. Each delivery keeps its source session and task receipt.'));
  const render = (...nodes) => content.replaceChildren(...nodes.filter(n => n != null));
  let busy = false, epoch = 0, current = null;
  const button = (text, action) => el('button', {onclick: () => request(action)}, text);
  const open = session => el('button', {onclick: () => { state.session = session; nav.go('sessions'); }}, 'Open source session');
  const request = async action => {
    if (busy) return;
    if ((action.action === 'forget' || action.action === 'cancel') &&
      !confirm(action.action === 'forget' ? 'Forget this resolved receipt? A redelivery with the same ID may then create a new task. The source session remains.' : 'Cancel this pending delivery before it starts a task?')) return;
    busy = true; const generation = ++epoch;
    render(el('p', {}, 'Reading event receipts…'));
    try {
      const reply = await api('/api/events', action);
      if (generation !== epoch || !panel.isConnected) return;
      if (reply.receipts) {
        current = null;
        render(...reply.receipts.map(item => el('article', {},
          el('p', {}, `${item.route} · ${item.delivery} · ${item.state}`),
          item.error ? el('p', {class: 'warn'}, item.error) : null,
          button('Inspect delivery', {action: 'inspect', id: item.id}), open(item.session))),
          reply.after ? button('More deliveries', {action: 'list', after: reply.after}) : el('p', {}, reply.receipts.length ? 'End of page' : 'No retained events.'));
      } else if (reply.bytes_base64 != null) {
        const data = Uint8Array.from(atob(reply.bytes_base64), c => c.charCodeAt(0));
        render(el('p', {}, `Source bytes ${reply.offset}–${reply.next_offset ?? reply.total_bytes}`),
          el('pre', {}, new TextDecoder().decode(data)),
          reply.next_offset != null ? button('Read next part', {action: 'read', id: reply.id, offset: reply.next_offset}) : null,
          button('Inspect delivery', {action: 'inspect', id: reply.id}));
      } else if (reply.route) {
        current = reply.id;
        const details = el('div', {}, el('p', {}, `${reply.route} · ${reply.delivery} · ${reply.state}`),
          el('p', {}, reply.workspace), el('pre', {}, reply.task), open(reply.session),
          el('p', {}, `Body: ${reply.body_bytes} bytes · ${reply.body_hash}`),
          button('Read event source', {action: 'read', id: current, offset: 0}));
        if (reply.error || reply.work_error) details.append(el('p', {class: 'warn'}, reply.error || reply.work_error));
        if (reply.work) details.append(el('p', {}, `Task ${reply.work.id} · ${reply.work.status} · ${reply.work.reason}`),
          el('pre', {}, reply.work.reply || 'No retained task reply yet.'),
          ...(reply.work.session ? [el('button', {onclick: () => {state.session = reply.work.session; nav.go('sessions');}}, 'Open task session')] : []));
        if (reply.state === 'pending') details.append(button('Cancel pending delivery', {action: 'cancel', id: current}));
        if ((!reply.run && reply.state !== 'pending') || (reply.work && ['completed', 'cancelled'].includes(reply.work.status))) details.append(button('Forget receipt', {action: 'forget', id: current}));
        render(details);
      } else {
        current = null;
        render(el('p', {}, `Receipt ${reply.id} · ${reply.state}`), button('List deliveries', {action: 'list', after: null}));
      }
    } catch (error) {
      if (generation === epoch && panel.isConnected) render(el('p', {class: 'warn'}, error.error || String(error)), button('List deliveries', {action: 'list', after: null}));
    } finally { if (generation === epoch) busy = false; }
  };
  panel.append(button('List deliveries', {action: 'list', after: null}), content);
  return panel;
}
