import { el, api, jsonWithin } from './lib.js';
export function environmentPanel(owner) {
  let pending = false, epoch = 0;
  const body = el('div'), panel = el('section', {}, el('h2', {}, 'Execution environment'),
    el('p', { class: 'sub' }, 'Inspect the environment selected in trusted configuration. Release and recovery never rerun its command.'));
  const render = (...nodes) => body.replaceChildren(...nodes.filter(node => node != null));
  const button = (label, action) => el('button', { onclick: () => request(typeof action === 'function' ? action() : action) }, label);
  const request = async action => {
    if (pending) return;
    if (!jsonWithin(action, 8192)) { render(el('p', { class: 'warn' }, 'Request exceeds 8 KiB. Shorten the inspection note.')); return; }
    pending = true; const generation = ++epoch;
    render(el('p', {}, 'Inspecting execution environment…'));
    try {
      const reply = await api(`/api/sessions/${encodeURIComponent(owner)}/environment`, action);
      if (generation !== epoch || !panel.isConnected) return;
      if (reply.changes) {
        render(el('p', {}, `Retained changes from call ${reply.call}`),
          ...reply.changes.map(change => el('div', {}, el('p', {}, `${change.directory ? 'Directory' : 'File'}: ${change.path}`),
            change.before ? button('Read before', { action: 'read', call: reply.call, which: 'before', path: change.path, offset: 0 }) : null,
            change.after ? button('Read after', { action: 'read', call: reply.call, which: 'after', path: change.path, offset: 0 }) : null)),
          reply.after ? button('More changed paths', { action: 'changes', call: reply.call, after: reply.after }) : null,
          button('Inspect environment', { action: 'inspect' }));
        return;
      }
      if (reply.bytes_base64 != null) {
        const bytes = Uint8Array.from(atob(reply.bytes_base64), char => char.charCodeAt(0));
        render(el('p', {}, `${reply.which} · ${reply.path} · bytes ${reply.offset}–${reply.next_offset ?? reply.total_bytes}`),
          el('pre', {}, new TextDecoder().decode(bytes)),
          reply.next_offset != null ? button('Read next part', { action: 'read', call: reply.call, which: reply.which, path: reply.path, offset: reply.next_offset }) : null,
          button('Back to changed paths', { action: 'changes', call: reply.call, after: null }));
        return;
      }
      if (reply.receipts || reply.receipt) {
        const entries = reply.receipts || [reply.receipt];
        const controls = el('div', {}, el('p', {}, `SSH-native checkpoints · owner ${reply.owner || owner}`),
          reply.workspace?.host ? el('p', {}, `${reply.workspace.host}:${reply.workspace.root}`) : null,
          reply.session ? el('p', {}, `Operation receipt: ${reply.session}`) : null);
        for (const entry of entries) {
          const row = el('div', {}, el('p', {}, `${entry.call} · ${entry.kind} · ${entry.path} · ${entry.status}`),
            button('Inspect checkpoint', { action: 'native_inspect', id: entry.call }));
          if (reply.receipt) {
            row.append(el('pre', {}, JSON.stringify({ before_sha256: entry.before_sha256, after_sha256: entry.after_sha256, execution: reply.observation?.execution }, null, 2)));
            if (['prepared', 'applied', 'unknown'].includes(entry.status) && reply.observation?.execution?.quiet !== false) {
              row.append(el('p', { class: 'sub' }, 'Undo restores only the recorded source. External changes or an active command refuse restoration.'),
                button('Undo inspected checkpoint', { action: 'native_undo', id: entry.call }));
            }
          }
          controls.append(row);
        }
        controls.append(button('List owned checkpoints', { action: 'native_inspect' }));
        render(controls);
        return;
      }
      const state = reply.saved?.status || reply.state || 'not started';
      const details = el('div', {}, el('p', {}, `${reply.profile} · ${reply.kind} · ${state}${reply.busy ? ' · busy' : ''}`),
        reply.workspace ? el('p', {}, `Workspace: ${reply.workspace}`) : null,
        reply.observed ? el('p', {}, `Resource: ${reply.observed.id || 'unresolved'} · ${reply.observed.running === true ? 'running' : reply.observed.running === false ? 'stopped' : 'inspect saved state'}`) : null,
        reply.inspection_error ? el('p', { class: 'warn' }, `Remote observation unavailable: ${reply.inspection_error}`) : null,
        reply.saved?.error ? el('p', { class: 'warn' }, reply.saved.error) : null,
        reply.saved?.pending ? el('p', { class: 'warn' }, `Pending write: ${reply.saved.pending}`) : null,
        reply.session ? el('p', {}, `Operation receipt: ${reply.session}`) : null);
      if (reply.kind === 'ssh_native') {
        details.append(el('p', {}, `Host: ${reply.host}`),
          button('List owned checkpoints', { action: 'native_inspect' }));
      }
      if (reply.token) {
        const token = reply.token, recovering = reply.action === 'recover', stopping = reply.action === 'stop', note = el('textarea', { 'aria-label': 'Environment inspection note', maxlength: 4096 });
        details.append(el('p', {}, recovering ? 'Review retained file versions and acknowledge unknown execution before publishing observed changes.' : stopping ? 'Confirm stopping this inspected command. Describe what you checked.' : 'Confirm release of this inspected resource. Describe what you checked.'), note,
          button(recovering ? 'Publish reviewed recovery' : stopping ? 'Stop inspected command' : 'Release inspected resource', () => ({ action: recovering ? 'recover' : stopping ? 'stop' : 'release', token, note: note.value })));
      } else if (reply.saved && state !== 'released' && !reply.busy && !reply.foreign_owner_active && !reply.inspection_error) {
        details.append(button('Preview resource release', { action: 'prepare_release' }));
        if (reply.kind === 'ssh' && reply.observed?.running) details.append(button('Preview retained command stop', { action: 'prepare_stop' }));
        if (reply.kind === 'ssh' && state !== 'ready' && reply.observed?.state?.command_started) details.append(button('Preview file recovery', { action: 'prepare_recover' }));
      }
      if (reply.kind === 'ssh' && reply.saved?.call && (['writeback_unknown', 'interrupted_stopped', 'ready', 'released'].includes(state) || reply.action === 'recover')) details.append(button('Review retained changes', { action: 'changes', call: reply.saved.call, after: null }));
      render(details);
    } catch (error) {
      if (generation === epoch && panel.isConnected) render(el('p', { class: 'warn' }, String(error.error || error)),
        button('Inspect saved state', { action: 'inspect' }));
    } finally { if (generation === epoch) pending = false; }
  };
  panel.append(button('Inspect environment', { action: 'inspect' }), body);
  return panel;
}
