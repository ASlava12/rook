import test from 'node:test';
import assert from 'node:assert/strict';
import { environmentPanel } from '../dist/environments.js';
class Node {
  constructor(tag, text = '') {
    this.tag = tag; this.nodeType = tag === '#text' ? 3 : 1;
    this.text = text; this.children = []; this.listeners = {}; this.isConnected = true; this.value = '';
  }
  append(...children) { this.children.push(...children); if (this.tag === 'select' && !this.value) this.value = this.children[0]?.value || ''; }
  replaceChildren(...children) { this.children = children; }
  setAttribute(name, value) { this[name] = value; }
  addEventListener(name, handler) { this.listeners[name] = handler; }
  get textContent() { return this.text + this.children.map(child => child.textContent).join(''); }
  find(predicate) { if (predicate(this)) return this; for (const child of this.children) { const found = child.find?.(predicate); if (found) return found; } return null; }
  button(label) { return this.find(node => node.tag === 'button' && node.textContent === label); }
}
globalThis.document = { createElement: tag => new Node(tag), createTextNode: text => new Node('#text', text) };
const flush = () => new Promise(resolve => setImmediate(resolve));

const response = data => ({ ok: true, json: async () => data });

const saved={profile:'box',kind:'container',saved:{status:'interrupted_stopped'},observed:{id:'owned-id',running:false},busy:false,workspace:'/workspace'};
test('resource release requires a preview and keeps the original owner and reviewed token',async()=>{
 const calls=[];globalThis.fetch=async(path,options)=>{const action=JSON.parse(options.body);calls.push({path,action});return response(action.action==='prepare_release'?{...saved,token:'exact-source-token'}:action.action==='release'?{profile:'box',kind:'container',state:'released',session:'release-session'}:saved);};
 const panel=environmentPanel('owner/one');panel.button('Inspect environment').listeners.click();await flush();panel.button('Preview resource release').listeners.click();await flush();
 assert.equal(calls.some(call=>call.action.action==='release'),false);
 panel.find(node=>node['aria-label']==='Environment inspection note').value='Observed the stopped owned container';panel.button('Release inspected resource').listeners.click();await flush();
 assert.deepEqual(calls.at(-1),{path:'/api/sessions/owner%2Fone/environment',action:{action:'release',token:'exact-source-token',note:'Observed the stopped owned container'}});assert.match(panel.textContent,/release-session/);
});
test('busy resources offer inspection and cannot be released from the panel',async()=>{
 globalThis.fetch=async()=>response({...saved,busy:true});const panel=environmentPanel('owner');panel.button('Inspect environment').listeners.click();await flush();assert.equal(panel.button('Preview resource release'),null);assert.match(panel.textContent,/busy/);
});
test('a departed resource panel discards delayed inspection responses',async()=>{
 let deliver;globalThis.fetch=async()=>new Promise(resolve=>{deliver=resolve;});const panel=environmentPanel('old');panel.button('Inspect environment').listeners.click();await flush();panel.isConnected=false;deliver(response(saved));await flush();assert.equal(panel.button('Preview resource release'),null);
});
test('SSH file recovery stays bound to its inspected owner/token and source paging uses the retained call',async()=>{
 const calls=[];globalThis.fetch=async(path,options)=>{const action=JSON.parse(options.body);calls.push({path,action});if(action.action==='prepare_recover')return response({...saved,kind:'ssh',token:'recover-token',action:'recover',saved:{status:'interrupted_stopped',call:'call-one'},observed:{state:{command_started:true}}});if(action.action==='changes')return response({call:'call-one',changes:[{path:'same',before:true,after:true,directory:false}],after:null});if(action.action==='read')return response({call:'call-one',which:action.which,path:'same',offset:0,total_bytes:6,next_offset:null,bytes_base64:'cmVtb3Rl'});return response({...saved,kind:'ssh',saved:{status:'interrupted_stopped',call:'call-one'},observed:{state:{command_started:true}}});};
 const panel=environmentPanel('ssh-owner');panel.button('Inspect environment').listeners.click();await flush();panel.button('Preview file recovery').listeners.click();await flush();assert.equal(calls.some(call=>call.action.action==='recover'),false);panel.find(n=>n['aria-label']==='Environment inspection note').value='Reviewed stopped command and before/after files';panel.button('Publish reviewed recovery').listeners.click();await flush();assert.deepEqual(calls.at(-1),{path:'/api/sessions/ssh-owner/environment',action:{action:'recover',token:'recover-token',note:'Reviewed stopped command and before/after files'}});
 panel.button('Review retained changes').listeners.click();await flush();panel.button('Read after').listeners.click();await flush();assert.deepEqual(calls.at(-1).action,{action:'read',call:'call-one',which:'after',path:'same',offset:0});assert.match(panel.textContent,/remote/);
});
test('released SSH sources remain inspectable and file images survive directory replacements', async () => {
  const calls = [];
  globalThis.fetch = async (path, options) => {
    const action = JSON.parse(options.body); calls.push(action);
    if (action.action === 'changes') return response({call: 'retained', changes: [
      {path: 'file-to-directory', before: true, after: false, directory: true},
      {path: 'directory-to-file', before: false, after: true, directory: true},
    ], after: null});
    return response({kind: 'ssh', profile: 'lan', saved: {status: 'released', call: 'retained'}});
  };
  const panel = environmentPanel('owner');
  panel.button('Inspect environment').listeners.click(); await flush();
  panel.button('Review retained changes').listeners.click(); await flush();
  assert.ok(panel.button('Read before'));
  assert.ok(panel.button('Read after'));
  panel.button('Read after').listeners.click(); await flush();
  assert.deepEqual(calls.at(-1), {action: 'read', call: 'retained', which: 'after', path: 'directory-to-file', offset: 0});
});
test('native checkpoints keep the owner and require source inspection before explicit undo', async () => {
  const calls = [], entry = {call: 'owned-call', kind: 'edit', path: '<remote-path>', status: 'applied', before_sha256: 'before', after_sha256: 'after'};
  globalThis.fetch = async (path, options) => {
    const action = JSON.parse(options.body); calls.push({path, action});
    if (action.action === 'native_inspect' && !action.id) return response({owner: 'native-owner', receipts: [entry]});
    if (action.action === 'native_inspect') return response({receipt: entry, workspace: {host: 'trusted', root: '/project'}, observation: {}});
    if (action.action === 'native_undo') return response({receipt: {...entry, status: 'undone'}, session: 'undo-operation'});
    return response({kind: 'ssh_native', profile: 'native', host: 'trusted', workspace: '/project', state: 'configured'});
  };
  const panel = environmentPanel('native/owner');
  panel.button('Inspect environment').listeners.click(); await flush();
  panel.button('List owned checkpoints').listeners.click(); await flush();
  assert.equal(panel.button('Undo inspected checkpoint'), null);
  panel.button('Inspect checkpoint').listeners.click(); await flush();
  assert.match(panel.textContent, /before.*after/s);
  assert.equal(calls.some(c => c.action.action === 'native_undo'), false);
  panel.button('Undo inspected checkpoint').listeners.click(); await flush();
  assert.deepEqual(calls.at(-1), {path: '/api/sessions/native%2Fowner/environment', action: {action: 'native_undo', id: 'owned-call'}});
  assert.match(panel.textContent, /undone.*undo-operation|undo-operation.*undone/s);
  assert.equal(panel.button('Undo inspected checkpoint'), null);
});
test('a native command still running offers observation instead of undo', async () => {
  globalThis.fetch = async () => response({receipt: {call:'running', kind:'run', path:'.', status:'unknown'}, observation:{execution:{quiet:false}}});
  const panel = environmentPanel('owner'); panel.button('Inspect environment').listeners.click(); await flush();
  assert.equal(panel.button('Undo inspected checkpoint'), null);
  assert.ok(panel.button('Inspect checkpoint'));
});
