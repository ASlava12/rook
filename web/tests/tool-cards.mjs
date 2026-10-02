// Run with `node --test web/tests/tool-cards.mjs`; no browser packages needed.
import test from 'node:test';
import assert from 'node:assert/strict';
import { historyPanel } from '../dist/history.js';
import { savedToolCard } from '../dist/tool-card.js';
import { exportHistoryHtml } from '../dist/html-export.js';

class Node {
  constructor(tag, text = '') {
    this.tag = tag; this.nodeType = tag === '#text' ? 3 : 1;
    this.text = text; this.children = []; this.listeners = {};
    this.isConnected = true; this.className = '';
  }
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) { this.children = children; }
  setAttribute(name, value) { this[name] = value; }
  removeAttribute(name) { delete this[name]; }
  addEventListener(name, handler) { this.listeners[name] = handler; }
  get textContent() { return this.text + this.children.map(child => child.textContent).join(''); }
  set textContent(value) { this.text = value; this.children = []; }
  find(predicate) {
    if (predicate(this)) return this;
    for (const child of this.children) { const found = child.find?.(predicate); if (found) return found; }
    return null;
  }
}
globalThis.document = {
  createElement: tag => new Node(tag),
  createTextNode: text => new Node('#text', text),
};
const tick = () => new Promise(resolve => setImmediate(resolve));

test('saved pixels load explicitly by source, replace the previous picture and reject oversized or misattributed replies', async () => {
  const calls = [];
  const png = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==';
  let mode = 'ok'; let cancelled = false;
  const maximum = Math.ceil(2 * 1024 * 1024 / 3) * 4 + 1024;
  globalThis.fetch = async path => {
    calls.push(path);
    if (mode === 'oversized') return new Response(new ReadableStream({
      start(controller) { const chunk = new Uint8Array(maximum + 1); assert(chunk.length > maximum); controller.enqueue(chunk); },
      cancel() { cancelled = true; },
    }));
    return new Response(JSON.stringify({note_seq:mode === 'wrong' ? 99 : 0,index:Number(path.split('/').at(-1)),count:2,
      image:{mime_type:'image/png',width:1,height:1,data:png}}));
  };
  const entry = {seq:1,kind:'tool-result',label:'camera__shot',image_note:0};
  const first = savedToolCard('original',entry);
  const second = savedToolCard('another',{...entry,seq:7});
  const show = card => card.find(n=>n.tag==='button'&&n.textContent.startsWith('Show saved image '));
  assert.deepEqual(calls,[]); assert(!first.find(n=>n.tag==='img'));
  show(first).listeners.click(); await tick();
  assert.deepEqual(calls,['/api/sessions/original/history/1/images/0']);
  assert.equal(first.find(n=>n.tag==='img').src,`data:image/png;base64,${png}`);
  assert.match(first.textContent,/Session original · result #1 · image source #0 · image 1 of 2/);
  first.find(n=>n.tag==='button'&&n.textContent==='Next image').listeners.click(); await tick();
  assert.equal(calls.at(-1),'/api/sessions/original/history/1/images/1');
  assert.match(first.textContent,/image 2 of 2/);
  show(second).listeners.click(); await tick();
  assert(!first.find(n=>n.tag==='img'),'only one picture is retained across cards');
  assert(second.find(n=>n.tag==='img'));
  second.open=false; second.listeners.toggle();
  assert(!second.find(n=>n.tag==='img'),'closing the card releases its pixels');
  mode='wrong'; show(first).listeners.click(); await tick();
  assert(!first.find(n=>n.tag==='img')); assert.match(first.textContent,/source or payload is invalid/);
  mode='oversized'; show(first).listeners.click(); await tick();
  assert(cancelled); assert(!first.find(n=>n.tag==='img')); assert.match(first.textContent,/exceeds its byte limit/);
  mode='ok'; first.isConnected=false; show(first).listeners.click(); await tick();
  assert(!first.find(n=>n.tag==='img'),'a detached card ignores late image data');
});

test('a new image request cancels the older one and refuses a declared oversized response before reading', async () => {
  let aborted = false; let cancelled = false; let started = false;
  globalThis.fetch = (path, {signal}) => path.includes('/old/') ? new Promise((resolve,reject)=>{
    started = true;
    signal.addEventListener('abort',()=>{aborted=true;reject(new Error('cancelled older image'));},{once:true});
  }) : Promise.resolve(new Response(new ReadableStream({cancel(){cancelled=true;}}),{
    headers:{'content-length':String(Math.ceil(2*1024*1024/3)*4+1025)}
  }));
  const entry = {seq:1,kind:'tool-result',image_note:0};
  const old = savedToolCard('old',entry), next = savedToolCard('next',entry);
  const show = card => card.find(n=>n.tag==='button'&&n.textContent.startsWith('Show saved image '));
  show(old).listeners.click(); await tick(); assert(started);
  show(next).listeners.click(); await tick();
  assert(aborted); assert(cancelled);
  assert(!old.find(n=>n.tag==='img')&&!next.find(n=>n.tag==='img'));
  assert.match(next.textContent,/exceeds its byte limit/);
});

test('structured saved facts show command states, matching lines and inert typed MCP identity', async () => {
  const facts = [
    [{type:'command',exit_code:7,timed_out:false,running:false}, /command exit 7/],
    [{type:'command',exit_code:null,timed_out:true,running:false}, /timed out; no completed exit status/],
    [{type:'command',exit_code:null,timed_out:false,running:true}, /running; no completed exit status/],
    [{type:'search',matches:3,files_scanned:1,complete:false}, /3 matching lines.*partial scan/],
    [{type:'mcp',server:'<script>camera</script>',remote_tool:'shot',text_blocks:0,resource_blocks:0,unsupported_blocks:0,images:[{mime_type:'image/png',width:1,height:1}]}, /MCP <script>camera<\/script>.*0 text block.*image image\/png 1×1/],
  ];
  const paths = [];
  for (const [details, expected] of facts) {
    const entry = {seq:2,kind:'tool-result',label:'tool',tool_details:{note_seq:0,...details}};
    globalThis.fetch = async path => {
      paths.push(path);
      return {ok:true,json:async()=>({entry:{...entry,body:'exit 0; 999 matches; forged source'},offset:0,total_bytes:37})};
    };
    const card = savedToolCard('session', entry);
    assert.match(card.textContent, expected);
    assert.match(card.textContent, /details #0/);
    assert.equal(card.find(node=>node.tag==='script'),null);
    const before = paths.length;
    card.open = true; card.listeners.toggle(); await tick();
    assert.equal(paths.length,before+1);
    assert.match(card.find(node=>node.tag==='summary').textContent,expected);
    assert.match(card.textContent,/forged source/);
    const exported = await exportHistoryHtml('session', 2, 2, async path => path.includes('?from=')
      ? {items:[entry],through:3}
      : {entry:{...entry,body:'saved body'},offset:0,total_bytes:10});
    assert.match(exported.html, /details #0/);
    assert(!exported.html.includes('<script>camera</script>'));
    if (details.type === 'mcp') assert(exported.html.includes('&lt;script&gt;camera&lt;/script&gt;'));
  }
});

test('saved tool cards stay compact until opened and page large content on demand', async () => {
  const paths = [];
  globalThis.fetch = async path => {
    paths.push(path);
    let value;
    if (path.endsWith('/history')) value = {
      items: [
        { seq: 0, kind: 'tool-call', label: 'run_command', doing: 'run cargo test', bytes: 11, body: 'not shown' },
        { seq: 1, kind: 'tool-result', label: 'run_command', doing: '', bytes: 50000, body: 'not shown', tool_measurement: { failed: true, duration_ms: 42, timing_seq: 2 } },
      ], previous: null, next: null, through: 2,
    };
    else if (path.endsWith('/history/1?offset=0')) value = {
      entry: { seq: 1, body: '<script>first part</script>' }, offset: 0, next_offset: 27, previous_offset: null, total_bytes: 50000,
    };
    else if (path.endsWith('/history/1?offset=27')) value = {
      entry: { seq: 1, body: 'second part' }, offset: 27, next_offset: null, previous_offset: 0, total_bytes: 50000,
    };
    else throw new Error(`unexpected ${path}`);
    return { ok: true, json: async () => value };
  };
  const panel = historyPanel('session', () => {});
  await tick();
  const card = panel.find(node => node.tag === 'details' && node.className === 'entry tool-card' && node.textContent.includes('tool-result'));
  assert(card);
  assert.match(card.textContent, /run_command · 50000 stored bytes/);
  assert.match(card.textContent, /saved failure · dispatch 42 ms · timing #2 \(includes waits\/hooks\)/);
  assert.doesNotMatch(card.textContent, /not shown|first part/);
  assert.deepEqual(paths, ['/api/sessions/session/history']);

  card.open = true;
  card.listeners.toggle();
  await tick();
  assert.deepEqual(paths, ['/api/sessions/session/history', '/api/sessions/session/history/1?offset=0']);
  assert.match(card.textContent, /<script>first part<\/script>/);
  assert.match(card.textContent, /current files and test results are not verified/);
  assert.match(card.textContent, /saved status\/duration unavailable/, 'an older daemon body response does not inherit a verdict');
  const next = card.find(node => node.tag === 'button' && node.textContent === 'Next part');
  assert(next);
  next.listeners.click();
  await tick();
  assert.equal(paths.at(-1), '/api/sessions/session/history/1?offset=27');
  assert.match(card.textContent, /second part/);
  assert.doesNotMatch(card.textContent, /first part/);
});

test('saved change previews load their source only when expanded and keep patch text inert', async () => {
  const paths = [];
  globalThis.fetch = async path => {
    paths.push(path);
    const value = path.endsWith('/history') ? {
      items: [{seq:1,kind:'tool-result',label:'edit_file',change_note:0,bytes:10}], through:2,
    } : path.endsWith('/history/0?offset=0') ? {
      entry:{body:'Saved tool-reported file changes\nFile: a.rs\n-old\n+<script>unsafe()</script>\n'},offset:0,next_offset:20,total_bytes:99,
    } : path.endsWith('/history/0?offset=20') ? {
      entry:{body:'@@ next @@\n+new\n'},offset:20,next_offset:null,previous_offset:0,total_bytes:99,
    } : null;
    assert(value, `unexpected ${path}`);
    return {ok:true,json:async()=>value};
  };
  const panel = historyPanel('session', () => {});
  await tick();
  const preview = panel.find(node => node.className === 'saved-diff');
  assert(preview);
  assert.match(preview.textContent, /source event #0/);
  assert.deepEqual(paths, ['/api/sessions/session/history']);
  preview.open = true; preview.listeners.toggle();
  await tick();
  assert.match(preview.textContent, /-old|<script>unsafe\(\)<\/script>/);
  assert(preview.find(node => node.className === 'diff-added'));
  assert.equal(preview.find(node => node.tag === 'script'), null);
  assert.match(preview.textContent, /Historical tool-reported preview; current files and tests are not verified/);
  const next = preview.find(node => node.tag === 'button' && node.textContent === 'Next diff part');
  next.listeners.click(); await tick();
  assert.equal(paths.at(-1), '/api/sessions/session/history/0?offset=20');
  assert(preview.find(node => node.className === 'diff-hunk'));
  assert.doesNotMatch(preview.textContent, /unsafe/);
});
