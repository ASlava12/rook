// Opaque references and revisions keep a stale view from changing newer work.
import { el, api } from './lib.js';

// One reserved handoff, including an in-flight withdrawal. Changing tabs or
// sessions cannot append this message to an unrelated conversation's draft.
let restored = null;
export function takeRestored(session) {
  if (!restored || restored.session !== session || restored.text === null) return null;
  const text = restored.text;
  restored = null;
  return text;
}

export function queuePanel(session, receiveDraft, receiveReceipt) {
  const root = el('section', { class: 'queue-controls', 'aria-label': 'Message queue' });
  if (!session) { root.append(el('p', {}, 'Start or open a session first.')); return root; }
  const base = `/api/sessions/${encodeURIComponent(session)}/queue`;
  const notice = el('p', { role: 'status', 'aria-live': 'polite' });
  const rows = el('div');
  const detail = el('div');
  let pending = false, editing = false, next = null, maxBytes = 0;
  const all = el('input', { type: 'checkbox', 'aria-label': 'Include finished messages' });
  const refresh = el('button', { type: 'button', onclick: () => load() }, 'Refresh queue');
  const more = el('button', { type: 'button', disabled: true, onclick: () => load(next) }, 'Next page');
  function busy(value) {
    pending = value;
    root.setAttribute('aria-busy', String(value));
    refresh.disabled = all.disabled = value || editing;
    more.disabled = value || editing || !next;
    for (const button of rows.querySelectorAll('button')) button.disabled = value || editing;
    for (const button of detail.querySelectorAll('button')) button.disabled = value;
    for (const input of detail.querySelectorAll('textarea')) input.readOnly = value;
  }
  const queued = receipt => receipt.applied_at === null && receipt.withdrawn_at === null;
  const status = receipt => receipt.withdrawn_at !== null ? 'withdrawn' : receipt.applied_at !== null ? 'accepted' : 'queued';
  function display(page) {
    next = page.next;
    maxBytes = page.max_message_bytes;
    detail.replaceChildren();
    rows.replaceChildren(...page.items.map(entry => el('article', { class: 'entry' },
      el('strong', {}, `${status(entry.receipt)} · revision ${entry.receipt.revision}`),
      el('p', { class: 'sub' }, entry.reference),
      el('pre', {}, entry.receipt.text),
      entry.truncated ? el('p', {}, 'Preview shortened; open the message for full text.') : null,
      el('button', { type: 'button', onclick: () => open(entry.reference, false) }, 'Open message'),
      queued(entry.receipt) ? el('button', { type: 'button', onclick: () => open(entry.reference, true) }, 'Edit message') : null,
      queued(entry.receipt) ? el('button', { type: 'button', onclick: () => withdraw(entry, false) }, 'Withdraw message') : null,
      queued(entry.receipt) ? el('button', { type: 'button', onclick: () => withdraw(entry, true) }, 'Withdraw to draft') : null)));
    if (!page.items.length) rows.append(el('p', {}, 'No messages on this page.'));
  }
  async function load(after) {
    if (pending || editing) return;
    busy(true);
    try {
      const query = new URLSearchParams({ include_finished: String(all.checked) });
      if (after) query.set('after', after);
      const page = await api(`${base}?${query}`);
      if (root.isConnected) { display(page); notice.textContent = `${page.total} matching receipts. Acceptance means included in context, not executed.`; }
    } catch (error) { notice.textContent = error.error || String(error); }
    finally { busy(false); }
  }
  async function open(reference, edit) {
    if (pending || editing) return;
    busy(true);
    try {
      const entry = await api(`${base}/${encodeURIComponent(reference)}`);
      if (!root.isConnected) return;
      if (!edit || !queued(entry.receipt)) {
        detail.replaceChildren(el('p', {}, `${status(entry.receipt)} · revision ${entry.receipt.revision}`), el('pre', {}, entry.receipt.text));
        return;
      }
      editing = true;
      const input = el('textarea', { 'aria-label': 'Edit queued message', rows: 8, maxlength: maxBytes });
      input.value = entry.receipt.text;
      const form = el('form', { onsubmit: async event => {
        event.preventDefault();
        if (pending) return;
        if (!input.value.trim() || new TextEncoder().encode(input.value).length > maxBytes) {
          notice.textContent = `Enter text within work.max_message_bytes (${maxBytes}).`; return;
        }
        busy(true);
        try {
          const saved = await api(base, { action: 'edit', reference, revision: entry.receipt.revision, text: input.value });
          receiveReceipt?.(session, saved);
          editing = false;
          detail.replaceChildren();
          notice.textContent = 'Message updated.';
        } catch (error) { notice.textContent = `${error.error || String(error)} Your edit is retained; cancel editing to refresh.`; }
        finally { busy(false); }
        if (!editing && root.isConnected) load();
      } }, input, el('button', { type: 'submit' }, 'Save message'), el('button', { type: 'button', onclick: () => {
        editing = false; detail.replaceChildren(); busy(false); load();
      } }, 'Cancel editing'));
      detail.replaceChildren(form);
      input.focus();
    } catch (error) { notice.textContent = error.error || String(error); }
    finally { busy(false); }
  }
  async function withdraw(entry, toDraft) {
    if (pending || editing) return;
    if (toDraft && restored) { notice.textContent = `Open session ${restored.session} to receive its previous restored message first.`; return; }
    if (toDraft) restored = { session, text: null };
    busy(true);
    try {
      const result = await api(base, { action: 'withdraw', reference: entry.reference, revision: entry.receipt.revision });
      receiveReceipt?.(session, result);
      if (toDraft) {
        restored.text = result.receipt.text;
        receiveDraft();
      }
      notice.textContent = toDraft ? 'Message withdrawn; returned to its session draft without sending.' : 'Message withdrawn.';
    } catch (error) {
      if (toDraft) restored = null;
      notice.textContent = `${error.error || String(error)} Refresh before trying again.`;
      return;
    } finally { busy(false); }
    if (root.isConnected) load();
  }
  all.addEventListener('change', () => load());
  root.append(el('p', {}, 'Pending messages can be changed until the agent accepts them. Editing does not start a turn.'),
    el('div', { class: 'row' }, refresh, more, el('label', {}, all, ' Include accepted and withdrawn')), notice, rows, detail);
  setTimeout(() => { if (root.isConnected) load(); }, 0);
  return root;
}
