// What every view needs: DOM construction, the API, formatting, shared state.
// No dependencies and no build step — the browser loads these modules as they
// are, and `cargo build` is the whole story on every platform (ADR-0012).

export const $ = (s, r = document) => r.querySelector(s);

export const el = (tag, attrs = {}, ...kids) => {
  const n = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === 'class') n.className = v;
    else if (k.startsWith('on')) n.addEventListener(k.slice(2), v);
    else if (v !== null && v !== undefined && v !== false) n.setAttribute(k, v === true ? '' : v);
  }
  for (const kid of kids.flat()) if (kid !== null && kid !== undefined)
    n.append(kid.nodeType ? kid : document.createTextNode(kid));
  return n;
};

export const bytes = n => {
  const u = ['B', 'KiB', 'MiB', 'GiB', 'TiB']; let i = 0, v = Number(n) || 0;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return i === 0 ? `${v} B` : `${v.toFixed(1)} ${u[i]}`;
};

export const ago = ts => {
  const d = Math.floor(Date.now() / 1000) - ts;
  if (d < 60) return `${d}s ago`;
  if (d < 3600) return `${Math.floor(d / 60)}m ago`;
  if (d < 86400) return `${Math.floor(d / 3600)}h ago`;
  return `${Math.floor(d / 86400)}d ago`;
};

export const api = async (path, body) => {
  const r = await fetch(path, body === undefined ? undefined : {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
  const parsed = await r.json().catch(() => ({ error: r.statusText, kind: 'transport' }));
  if (!r.ok) throw parsed;
  return parsed;
};

// One object, shared by every view: a tab that re-renders reads what the
// others left, which is how "continue in chat" from the sessions tab works.
export const state = {
  tab: 'chat', session: null, skill: null, job: null, query: '', memoryAll: false,
  chat: { session: null, busy: false, waiting: false, settings: null, spent: null, sessions: [] },
};

// Set by the app so views can switch tabs without importing the router.
export const nav = { go: () => {} };

// A permission asked for once, on a click, which is the one time a browser
// grants it. Off until then: an autonomous turn that needs a person is the
// case for it, and a tab left in the background is where that person is.
export async function notify(title, body) {
  if (!('Notification' in window) || Notification.permission !== 'granted' || !document.hidden) return;
  try { new Notification(title, { body, tag: 'rook' }); } catch { /* denied or unsupported */ }
}
export function askToNotify() {
  if ('Notification' in window && Notification.permission === 'default') Notification.requestPermission();
}

// The little of Markdown a model's answer actually uses — fenced code,
// paragraphs, bullet lists, inline code and bold — built as nodes rather than
// pasted as HTML, because the text is the model's and so is anything in it.
export function md(text) {
  const out = document.createDocumentFragment();
  const parts = String(text).split(/```/);
  parts.forEach((part, i) => {
    if (i % 2 === 1) {
      // Inside a fence: the first line may be a language name.
      const nl = part.indexOf('\n');
      const lang = nl >= 0 ? part.slice(0, nl).trim() : '';
      const code = nl >= 0 ? part.slice(nl + 1) : part;
      const pre = el('pre', { class: 'code' }, el('code', lang ? { 'data-lang': lang } : {}, code.replace(/\n$/, '')));
      out.append(pre);
      return;
    }
    for (const block of part.split(/\n\s*\n/)) {
      const lines = block.split('\n').filter(l => l.trim() !== '');
      if (!lines.length) continue;
      if (lines.every(l => /^\s*[-*]\s+/.test(l))) {
        out.append(el('ul', {}, lines.map(l => el('li', {}, inline(l.replace(/^\s*[-*]\s+/, ''))))));
      } else if (/^#{1,6}\s+/.test(lines[0]) && lines.length === 1) {
        out.append(el('p', { class: 'h' }, inline(lines[0].replace(/^#{1,6}\s+/, ''))));
      } else {
        out.append(el('p', {}, inline(lines.join('\n'))));
      }
    }
  });
  return out;
}

function inline(text) {
  const nodes = [];
  // `code` first, then **bold**; a `*` inside code is code.
  const re = /(`[^`]+`)|(\*\*[^*]+\*\*)/g;
  let last = 0, m;
  while ((m = re.exec(text))) {
    if (m.index > last) nodes.push(text.slice(last, m.index));
    if (m[1]) nodes.push(el('code', {}, m[1].slice(1, -1)));
    else nodes.push(el('strong', {}, m[2].slice(2, -2)));
    last = m.index + m[0].length;
  }
  if (last < text.length) nodes.push(text.slice(last));
  return nodes;
}

export function errorCard(e) {
  return el('div', { class: 'card' },
    el('h2', {}, 'error'),
    el('p', {}, e.error || String(e)),
    e.hint ? el('p', { class: 'sub' }, e.hint) : null);
}
