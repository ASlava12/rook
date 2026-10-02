import { el, jsonWithin } from './lib.js';

const events = new Set(['session_start', 'prompt', 'pre_tool', 'post_tool', 'turn_end']);
const safeInt = n => Number.isSafeInteger(n) && n >= 0;
const text = (v, max, multiline = false) => typeof v === 'string' &&
 new TextEncoder().encode(v).length <= max &&
 ![...v].some(c => /[\u0000-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/u.test(c) && !(multiline && c === '\n'));
const id = v => typeof v === 'string' && /^[A-Za-z0-9._-]{1,64}$/.test(v);

export function extensionPanel(state) {
 if (!state || !jsonWithin(state, 1048576) || !Array.isArray(state.reports) || state.reports.length > 128 ||
     !safeInt(state.omitted_updates) || !safeInt(state.invalid_records)) throw new Error('Invalid extension display limits');
 for (const r of state.reports) {
  const s = r.source, i = r.item;
  if (!s || !i || !events.has(s.event) || !safeInt(s.ordinal) || !/^[a-fA-F0-9]{64}$/.test(s.digest) || !safeInt(r.event_seq) || !id(i.id)) throw new Error('Invalid extension report source');
  const valid = i.kind === 'status' ? text(i.text, 1024) :
   i.kind === 'progress' ? text(i.label, 256) && safeInt(i.done) && safeInt(i.total) && i.total > 0 && i.done <= i.total :
   i.kind === 'result' ? text(i.title, 256) && text(i.body, 2048, true) : false;
  if (!valid) throw new Error('Invalid extension report fields');
 }
 const panel = el('section', { class: 'extension-reports' },
  el('h3', {}, 'Extension reports · saved branch history'),
  el('p', { class: 'sub' }, 'Reported by extensions; current files and tests are not verified.'));
 for (const r of state.reports) {
  const s = r.source, i = r.item;
  panel.append(el('section', {},
   el('p', { class: 'sub' }, `hook ${s.event} #${s.ordinal + 1} · source ${s.digest.slice(0, 8)} · event #${r.event_seq}`),
   el('p', {}, `${i.id}: ${i.text ?? i.label ?? i.title}${i.kind === 'progress' ? ` · ${i.done}/${i.total}` : ''}`),
   i.kind === 'result' ? el('pre', {}, i.body) : null));
 }
 if (state.omitted_updates || state.invalid_records) panel.append(el('p', { class: 'warn' },
  `${state.omitted_updates} omitted updates · ${state.invalid_records} invalid records; displayed reports may be older.`));
 panel.hidden = !state.reports.length && !state.omitted_updates && !state.invalid_records;
 return panel;
}

export async function readExtension(session) {
 const controller = new AbortController(), timer = setTimeout(() => controller.abort(), 30000);
 let reader;
 try {
  const response = await fetch(`/api/sessions/${encodeURIComponent(session)}/extension-ui`, {signal:controller.signal});
  if (!response.ok) throw new Error(`Extension display HTTP ${response.status}`);
  const cap = 2 * 1024 * 1024;
  if (Number(response.headers.get('content-length')) > cap) throw new Error('Extension response exceeds limit');
  reader = response.body.getReader();
  const buffer = new Uint8Array(cap); let used = 0;
  for (;;) {
   const {value, done} = await reader.read(); if (done) break;
   if (value.length > cap - used) throw new Error('Extension response exceeds limit');
   buffer.set(value, used); used += value.length;
  }
  return JSON.parse(new TextDecoder().decode(buffer.subarray(0, used)));
 } finally { await reader?.cancel().catch(() => {}); clearTimeout(timer); }
}
