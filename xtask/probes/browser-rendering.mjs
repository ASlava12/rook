// Controlled arrivals in actual Edge against an owned scratch daemon. No model
// speed claim: the measured work is this browser's Markdown parsing and DOM.
// node xtask/probes/browser-rendering.mjs baseline|batched
import fs from 'node:fs';
import path from 'node:path';
import { spawn, execFileSync } from 'node:child_process';
import assert from 'node:assert/strict';

const phase = process.argv[2];
assert(['baseline', 'batched'].includes(phase));
// Intercept just this historical module to repeat the baseline without changing
// the checkout or rebuilding/restarting an operator's installed daemon.
const baselineCommit = 'd4e98e0';
const baselineModule = phase === 'baseline'
  ? execFileSync('git', ['show', `${baselineCommit}:web/dist/chat.js`]) : null;
const artifacts = path.resolve('target/reference-browser-rendering');
fs.mkdirSync(artifacts, { recursive: true });
const root = fs.mkdtempSync(path.join(artifacts, `${phase}-`));
for (const name of ['home', 'workspace', 'bin']) fs.mkdirSync(path.join(root, name));
fs.writeFileSync(path.join(root, 'home/config.toml'), `[agent]
model = "fixture"
install_servers = false
[models.fixture]
api = "openai"
url = "http://127.0.0.1:1/v1"
model = "fixture"
`);
const executable = path.join(root, 'bin', process.platform === 'win32' ? 'rookd.exe' : 'rookd');
fs.copyFileSync(path.resolve('target/debug', path.basename(executable)), executable);
const daemon = spawn(executable, ['--port', '0', '--workspace', path.join(root, 'workspace')], {
  windowsHide: true, env: { ...process.env, ROOK_HOME: path.join(root, 'home') },
  stdio: ['ignore', fs.openSync(path.join(root, 'daemon.out'), 'w'), fs.openSync(path.join(root, 'daemon.err'), 'w')],
});
let edge, socket, next = 0;
let startupFailure = null;
daemon.on('error', error => { startupFailure = error; });
const pending = new Map();
const sleep = ms => new Promise(r => setTimeout(r, ms));
async function waitFor(check, name) {
  const until = Date.now() + 30000;
  while (Date.now() < until) {
    if (startupFailure) throw startupFailure;
    if (await check()) return;
    await sleep(100);
  }
  throw new Error(`${name} did not become ready; artifacts: ${root}`);
}
function command(method, params = {}) {
  return new Promise((resolve, reject) => {
    const id = ++next, timer = setTimeout(() => { pending.delete(id); reject(new Error(`${method} timed out`)); }, 30000);
    pending.set(id, { resolve, reject, timer }); socket.send(JSON.stringify({ id, method, params }));
  });
}
async function evaluate(expression) {
  const value = await command('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
  if (value.exceptionDetails) throw new Error(value.exceptionDetails.exception?.description || value.exceptionDetails.text);
  return value.result.value;
}
async function stopOwned(child) {
  if (!child || !child.pid || child.exitCode !== null || child.signalCode !== null) return;
  const ended = new Promise(resolve => child.once('exit', resolve));
  child.kill();
  await Promise.race([ended, sleep(5000)]);
  assert(child.exitCode !== null || child.signalCode !== null, `Owned process ${child.pid} did not stop`);
}
try {
  const addrFile = path.join(root, 'home/rookd.addr');
  await waitFor(() => fs.existsSync(addrFile), 'scratch daemon');
  const address = fs.readFileSync(addrFile, 'utf8').trim();
  const edgePath = process.env.ROOK_PROBE_BROWSER || (process.platform === 'win32'
    ? 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe' : 'chromium');
  edge = spawn(edgePath, ['--headless=new', '--remote-debugging-port=0', `--user-data-dir=${path.join(root, 'edge')}`,
    '--no-first-run', '--no-default-browser-check', 'about:blank'], { windowsHide: true, stdio: 'ignore' });
  edge.on('error', error => { startupFailure = error; });
  const portFile = path.join(root, 'edge/DevToolsActivePort');
  await waitFor(() => fs.existsSync(portFile), 'owned Edge');
  const port = fs.readFileSync(portFile, 'utf8').split('\n')[0];
  const tabs = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
  socket = new WebSocket(tabs.find(t => t.type === 'page').webSocketDebuggerUrl);
  await new Promise((resolve, reject) => {
    socket.addEventListener('open', resolve, { once: true }); socket.addEventListener('error', reject, { once: true });
  });
  socket.addEventListener('message', e => {
    const value = JSON.parse(e.data);
    if (value.method === 'Fetch.requestPaused') {
      command('Fetch.fulfillRequest', { requestId: value.params.requestId, responseCode: 200,
        responseHeaders: [{name:'Content-Type', value:'text/javascript'}], body: baselineModule.toString('base64') })
        .catch(error => { console.error(error); });
      return;
    }
    const p = pending.get(value.id); if (!p) return;
    pending.delete(value.id); clearTimeout(p.timer);
    value.error ? p.reject(new Error(JSON.stringify(value.error))) : p.resolve(value.result);
  });
  await command('Page.enable');
  await command('Network.enable'); await command('Network.setCacheDisabled', { cacheDisabled: true });
  if (baselineModule) await command('Fetch.enable', { patterns: [{ urlPattern: '*/chat.js' }] });
  await command('Page.addScriptToEvaluateOnNewDocument', { source: `
    window.renderMeasure = { parses: 0, parseMs: 0 };
    const fragments = new WeakMap(), fragment = Document.prototype.createDocumentFragment;
    Document.prototype.createDocumentFragment = function() {
      const node = fragment.call(this); fragments.set(node, performance.now()); return node;
    };
    const replace = Element.prototype.replaceChildren;
    Element.prototype.replaceChildren = function(...nodes) {
      if (this.classList.contains('md') && fragments.has(nodes[0])) {
        window.renderMeasure.parses++; window.renderMeasure.parseMs += performance.now() - fragments.get(nodes[0]);
      }
      return replace.apply(this, nodes);
    };
    // Feed the production socket handler deterministic cohorts. This deliberately
    // measures UI arrivals, not provider, network or server stream throughput.
    window.WebSocket = class {
      constructor() { this.readyState = 1; this.sent = []; window.probeSocket = this; }
      addEventListener() {}
      send(value) { this.sent.push(JSON.parse(value)); }
      close() { this.readyState = 3; this.onclose?.(); }
    };
  ` });
  await command('Page.navigate', { url: address });
  await waitFor(() => evaluate('!!document.querySelector("#chat-input") && !!window.probeSocket'), 'chat viewport');
  const result = await evaluate(`(async () => {
    const { state } = await import('/lib.js');
    const emit = value => window.probeSocket.onmessage({data: JSON.stringify(value)});
    emit({type:'started', session:'render-fixture'});
    emit({type:'turn', id:'render-turn'});
    const text = 'A bounded rendering fixture.\\n\\n\x60\x60\x60sql\\n' +
      Array.from({length:2200}, (_,i) => 'SELECT ' + i + ' AS value, \\'строка 🦉\\';').join('\\n') + '\\n\x60\x60\x60\\n\\nComplete final paragraph.';
    const chunks = []; for(let i=0; i<text.length; i+=67) chunks.push(text.slice(i,i+67));
    const start = performance.now(); let deliveryMs = 0;
    for(let i=0; i<chunks.length; i+=32) {
      const began = performance.now();
      for(const chunk of chunks.slice(i,i+32)) emit({type:'text', text:chunk});
      deliveryMs += performance.now()-began;
      await new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)));
    }
    emit({type:'done', reply:text, steps:1, input_tokens:0, output_tokens:0, delegated:[]});
    const node = document.querySelector('#stream .md');
    const code = node?.querySelector('pre code')?.textContent;
    const expected = text.split('\x60\x60\x60')[1].split('\\n').slice(1).join('\\n').replace(/\\n$/, '');
    if (code !== expected || !node.textContent.includes('Complete final paragraph.')) throw new Error('Final text or split fence was lost');
    if (document.querySelectorAll('#stream .md').length !== 1) throw new Error('Duplicate final answer');
    return {...window.renderMeasure, phase:${JSON.stringify(phase)}, characters:text.length,
      chunks:chunks.length, cohorts:Math.ceil(chunks.length/32), deliveryMs, wallMs:performance.now()-start,
      datasetCharacters:node.dataset.text?.length || 0, finalCodeCharacters:code.length, finalVerified:true};
  })()`);
  if (phase === 'batched') {
    result.boundaries = await evaluate(`(async () => {
      const { state } = await import('/lib.js'), chat = await import('/chat.js');
      const check = (value, why) => { if (!value) throw new Error(why); };
      const emit = value => window.probeSocket.onmessage({data:JSON.stringify(value)});
      const snapshot = questions => emit({type:'snapshot', session:'boundary-fixture', running:true, approvals:[], questions});
      snapshot([]); emit({type:'turn', id:'boundary-turn'});
      emit({type:'text', text:'partial before error'}); emit({type:'error', message:'controlled failure'});
      check(document.querySelector('#stream').textContent.includes('partial before error'), 'pending error text lost');
      snapshot([]); emit({type:'turn', id:'boundary-turn'}); emit({type:'goal', generation:null});
      emit({type:'text', text:'partial before Stop'}); chat.stop();
      check(document.querySelector('#stream').textContent.includes('partial before Stop'), 'Stop failed to flush');
      const stop = window.probeSocket.sent.at(-1);
      check(stop?.type === 'stop' && stop.turn === 'boundary-turn', 'Stop control identity lost');
      emit({type:'stop_applied', id:stop.id}); emit({type:'cancelled'});
      snapshot([]);
      const question = {type:'ask', id:'question', questions:[{question:'Choose next', choices:[], multi:false}]};
      emit(question); const form = document.querySelector('.ask-form');
      form.querySelector('input').value = 'private answer';
      document.querySelector('#chat-input').value = 'composer draft';
      emit({type:'text', text:'old pending snapshot'}); snapshot(['question']); emit(question);
      emit({type:'text', text:'new snapshot text'});
      await new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)));
      check(document.querySelector('.ask-form') === form && form.querySelector('input').value === 'private answer', 'question draft lost');
      check(!document.querySelector('#stream').textContent.includes('old pending snapshot'), 'old snapshot frame rendered');
      const receipt = {session:'boundary-fixture', reference:'queued-correction', revision:0, status:'queued'};
      emit({type:'agent', text:'queued next', receipt}); emit({type:'text', text:'before queue acceptance'});
      emit({type:'interjected', text:'accepted next', receipt:{...receipt, revision:1, status:'accepted'}});
      emit({type:'agent', text:'stale queued', receipt});
      const receipts = document.querySelectorAll('#stream [data-receipt]');
      check(receipts.length===1 && receipts[0].dataset.status==='accepted' && receipts[0].textContent.includes('accepted next'), 'queue receipt ownership lost');
      check(document.querySelector('#stream').textContent.includes('before queue acceptance'), 'queue acceptance failed to flush');
      const old = window.probeSocket;
      emit({type:'text', text:'old pending branch'}); chat.continueIn('another-branch');
      const until = performance.now()+15000;
      while (window.probeSocket === old || !document.querySelector('#chat-input')) {
        check(performance.now()<until, 'new branch viewport did not become ready');
        await new Promise(r => setTimeout(r, 20));
      }
      emit({type:'snapshot', session:'another-branch', running:true, approvals:[], questions:[]});
      old.onmessage({data:JSON.stringify({type:'text', text:'late old socket'})});
      emit({type:'text', text:'new selected branch'});
      await new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)));
      const out = document.querySelector('#stream').textContent;
      check(out.includes('new selected branch') && !out.includes('late old socket') && !out.includes('old pending branch'), 'branch ownership lost: '+out);
      check(document.querySelector('#chat-input').value === 'composer draft', 'composer draft lost on branch selection');
      emit({type:'text', text:'partial before disconnect'}); window.probeSocket.close();
      check(document.querySelector('#stream').textContent.includes('partial before disconnect'), 'disconnect failed to flush');
      return {error:true, stop:true, questionDraft:true, composerDraft:true, queueReceipt:true, snapshot:true, branch:true, disconnect:true};
    })()`);
  }
  fs.writeFileSync(path.join(artifacts, `${phase}.json`), JSON.stringify({ ...result, root, baselineCommit }, null, 2));
  if (phase === 'batched') {
    const baseline = JSON.parse(fs.readFileSync(path.join(artifacts, 'baseline.json'), 'utf8'));
    assert.equal(result.characters, baseline.characters); assert.equal(result.chunks, baseline.chunks);
    assert(result.parses <= result.cohorts + 1, JSON.stringify(result));
    assert(result.parses < baseline.parses / 4, 'actual Markdown parsing must be reduced');
    assert.equal(result.datasetCharacters, 0, 'the DOM must not retain a second source buffer');
  }
  console.log(JSON.stringify(result));
} finally {
  if (socket?.readyState === 1) { await command('Browser.close').catch(() => {}); socket.close(); }
  await Promise.all([stopOwned(edge), stopOwned(daemon)]);
  for (const p of pending.values()) { clearTimeout(p.timer); p.reject(new Error('Probe closed')); }
}
