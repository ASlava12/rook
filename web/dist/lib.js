// What every view needs: DOM construction, the API, formatting, shared state.
// No dependencies and no build step — the browser loads these modules as they
// are, independently of the native build (ADR-0016).

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

export const api = async (path, body, signal) => {
  const r = await fetch(path, body === undefined ? { signal } : {
    signal,
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
  const parsed = await r.json().catch(() => ({ error: r.statusText, kind: 'transport' }));
  if (!r.ok) throw parsed;
  return parsed;
};

// Admission for JSON values assembled by the composer. Count escaping and
// UTF-8 before stringify allocates the frame. Iterators keep nesting on the
// stack without copying a wide array of attachment/schema values.
export function jsonWithin(value, maximum) {
  let remaining = maximum;
  const spend = n => (remaining -= n) >= 0;
  const string = text => {
    if (!spend(2)) return false;
    for (const char of text) {
      const point = char.codePointAt(0);
      const size = point === 34 || point === 92 ? 2 :
        point < 32 ? (point === 8 || point === 9 || point === 10 || point === 12 || point === 13 ? 2 : 6) :
        point >= 0xd800 && point <= 0xdfff ? 6 :
        point < 0x80 ? 1 : point < 0x800 ? 2 : point < 0x10000 ? 3 : 4;
      if (!spend(size)) return false;
    }
    return true;
  };
  function* objectEntries(object) {
    for (const key in object) {
      if (Object.hasOwn(object, key) && object[key] !== undefined) yield [key, object[key]];
    }
  }
  const pending = [{ iterator: [value].values(), object: false }];
  while (pending.length) {
    const frame = pending.at(-1), next = frame.iterator.next();
    if (next.done) { pending.pop(); continue; }
    if (frame.seen && !spend(1)) return false;
    frame.seen = true;
    let item = next.value;
    if (frame.object) {
      if (!string(item[0]) || !spend(1)) return false;
      item = item[1];
    }
    if (item == null) { if (!spend(4)) return false; }
    else if (typeof item === 'string') { if (!string(item)) return false; }
    else if (typeof item === 'number') { if (!spend(Number.isFinite(item) ? String(item).length : 4)) return false; }
    else if (typeof item === 'boolean') { if (!spend(item ? 4 : 5)) return false; }
    else if (typeof item === 'object') {
      if (!spend(2)) return false;
      pending.push({ iterator: Array.isArray(item) ? item.values() : objectEntries(item), object: !Array.isArray(item) });
    } else return false;
  }
  return true;
}

// One object, shared by every view: a tab that re-renders reads what the
// others left, which is how "continue in chat" from the sessions tab works.
export const state = {
  tab: 'chat', session: null, skill: null, job: null, query: '', memoryAll: false, docsTopic: null,
  chat: { draft: '', session: null, workspace: null, connected: false, connection: 'connecting', busy: false, waiting: false, settings: null, spent: null, context: null, modelRequest: null, sessions: [] },
};

// Set by the app so views can switch tabs without importing the router.
export const nav = { go: () => {}, chatChanged: () => {}, disposeChat: () => {}, inspect: () => {} };

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

// Model text always becomes nodes. Links are protocol checked, raw HTML stays
// text, and a node budget keeps a short line repeated many times affordable.
export const MARKDOWN_NODES = 8192;
export function md(text) {
  const out = document.createDocumentFragment();
  let nodes = 0;
  const limit = Symbol('markdown node limit');
  const make = (...args) => { if (++nodes > MARKDOWN_NODES) throw limit; return el(...args); };
  const inline = value => {
    const result = [], pattern = /(`[^`\n]+`)|(\*\*[^*\n]+\*\*)|(\[[^\]\n]+\]\([^\s)]+\))|(\*[^*\n]+\*)/g;
    let at = 0, match;
    while ((match = pattern.exec(value))) {
      if (match.index > at) result.push(value.slice(at, match.index));
      if (match[1]) result.push(make('code', {}, match[1].slice(1,-1)));
      else if (match[2]) result.push(make('strong', {}, match[2].slice(2,-2)));
      else if (match[3]) {
        const split = match[3].indexOf(']('), label = match[3].slice(1,split), href = match[3].slice(split+2,-1);
        let safe = false;
        try { const url = new URL(href); safe = ['http:','https:','mailto:'].includes(url.protocol) && !url.username && !url.password; } catch { /* Render invalid links as ordinary text. */ }
        result.push(safe ? make('a', { href, target:'_blank', rel:'noopener noreferrer', referrerpolicy:'no-referrer' },label) : label);
      } else result.push(make('em', {}, match[4].slice(1,-1)));
      at = pattern.lastIndex;
    }
    if (at < value.length) result.push(value.slice(at));
    return result;
  };
  const cells = line => line.trim().replace(/^\|/,'').replace(/\|$/,'').split(/(?<!\\)\|/).slice(0,16).map(value=>value.trim().replace(/\\\|/g,'|'));
  const lines = String(text).replace(/\r\n/g,'\n').split('\n');
  try {
    for (let at = 0; at < lines.length;) {
      const line = lines[at];
      if (!line.trim()) { at++; continue; }
      const fence = /^\s*(```+|~~~+)([^\s]*)\s*$/.exec(line);
      if (fence) {
        const content = []; at++;
        while (at < lines.length && !lines[at].trim().startsWith(fence[1])) content.push(lines[at++]);
        if (at < lines.length) at++;
        const pre = make('pre',{class:'code'},make('code',fence[2]?{'data-lang':fence[2]}:{},content.join('\n').replace(/\n$/,'')));
        const copy = make('button',{class:'code-copy',type:'button','aria-label':'Copy code',onclick:async()=>{
          try { await navigator.clipboard.writeText(pre.textContent); copy.textContent='Copied'; }
          catch { const selection=window.getSelection?.(); if(selection&&document.createRange){const range=document.createRange();range.selectNodeContents(pre);selection.removeAllRanges();selection.addRange(range);} copy.textContent='Select and copy'; }
        }},'Copy');
        out.append(make('div',{class:'code-wrap'},make('span',{class:'code-language'},fence[2]||'code'),copy,pre));
        continue;
      }
      const heading = /^(#{1,6})\s+(.*)$/.exec(line);
      if (heading) { out.append(make(`h${heading[1].length}`,{},inline(heading[2]))); at++; continue; }
      if (/^\s*([-*_])(?:\s*\1){2,}\s*$/.test(line)) { out.append(make('hr')); at++; continue; }
      if (/^\s*>\s?/.test(line)) {
        const quoted = [];
        while (at < lines.length && /^\s*>\s?/.test(lines[at])) quoted.push(lines[at++].replace(/^\s*>\s?/,''));
        out.append(make('blockquote',{},inline(quoted.join('\n')))); continue;
      }
      if (line.includes('|') && at+1 < lines.length && /^\s*\|?\s*:?-{3,}:?\s*\|[\s|:\-]*$/.test(lines[at+1])) {
        const head = cells(line), body = []; at+=2;
        while (at < lines.length && lines[at].includes('|') && lines[at].trim()) body.push(make('tr',{},cells(lines[at++]).map(value=>make('td',{},inline(value)))));
        out.append(make('table',{},make('thead',{},make('tr',{},head.map(value=>make('th',{},inline(value))))),make('tbody',{},body)));
        continue;
      }
      if (/^\s*(?:[-*+]\s+|\d+[.)]\s+)/.test(line)) {
        const ordered = /^\s*\d/.test(line), items=[];
        const marker = ordered ? /^\s*\d+[.)]\s+/ : /^\s*[-*+]\s+/;
        while (at < lines.length && marker.test(lines[at])) items.push(make('li',{},inline(lines[at++].replace(marker,''))));
        out.append(make(ordered?'ol':'ul',{},items)); continue;
      }
      const paragraph = [line]; at++;
      while (at < lines.length && lines[at].trim() && !/^\s*(?:#{1,6}\s|```|~~~|>|[-*+]\s+|\d+[.)]\s+)/.test(lines[at]) && !(lines[at].includes('|')&&/^\s*\|?\s*:?-{3,}/.test(lines[at+1]||''))) paragraph.push(lines[at++]);
      out.append(make('p',{},inline(paragraph.join('\n'))));
    }
  } catch (error) {
    if (error !== limit) throw error;
    out.append(el('p',{class:'warn'},'Displayed formatting limit reached. Read the complete saved answer in Session history.'));
  }
  return out;
}

export function errorCard(e) {
  return el('div', { class: 'card' },
    el('h2', {}, 'error'),
    el('p', {}, e.error || String(e)),
    e.hint ? el('p', { class: 'sub' }, e.hint) : null);
}
