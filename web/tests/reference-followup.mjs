import test from 'node:test';
import assert from 'node:assert/strict';
import {patchRows,splitRows} from '../dist/diff-view.js';
import {presetPanel} from '../dist/presets.js';
import {state} from '../dist/lib.js';
class Node {
 constructor(tag,text=''){this.tag=tag;this.text=text;this.nodeType=tag==='#text'?3:1;this.children=[];this.listeners={};this.isConnected=true;this.value='';this.checked=false;}
 append(...nodes){this.children.push(...nodes);}
 replaceChildren(...nodes){this.children=nodes;}
 setAttribute(key,value){this[key]=value;}
 addEventListener(name,handler){this.listeners[name]=handler;}
 dispatchEvent(event){this.listeners[event.type]?.(event);}
 get textContent(){return this.text+this.children.map(node=>node.textContent).join('');}
 set textContent(value){this.text=value;this.children=[];}
 find(predicate){if(predicate(this))return this;return this.children.map(node=>node.find?.(predicate)).find(Boolean);}
 button(label){return this.find(node=>node.tag==='button'&&node.textContent===label);}
}
globalThis.document={createElement:tag=>new Node(tag),createTextNode:text=>new Node('#text',text)};
const flush=()=>new Promise(resolve=>setImmediate(resolve));
const response=body=>({ok:true,json:async()=>body});
test('split diffs align replacements and retain exact before/after line anchors as literal text',()=>{
 const patch='diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -3,3 +3,4 @@\n same\n-old\n+new <script>unsafe()</script>\n+extra\n end\n';
 const rows=patchRows(patch),pairs=splitRows(rows);
 const changed=pairs.find(pair=>pair.old?.text==='old');
 assert.equal(changed.old.before,4);assert.equal(changed.new.after,4);
 assert.equal(changed.new.text,'new <script>unsafe()</script>');
 assert(pairs.some(pair=>pair.old===null&&pair.new.text==='extra'));
 assert.equal(rows.find(row=>row.text==='end').before,5);assert.equal(rows.find(row=>row.text==='end').after,6);
 assert(patchRows('partial page\n+unanchored').every(row=>row.before===undefined&&row.after===undefined),'a page beginning mid-hunk cannot invent line anchors');
});
test('presets save without clearing drafts, load only into drafts, and restore prior text',async()=>{
 state.chat.settings={model:'fixture',effort:'low'};state.chat.busy=false;
 const input=new Node('textarea');input.value='saved prompt';const settings=[];const requests=[];
 const item={name:'review',revision:'reviewed-revision',preset:{prompt:'saved prompt',model:null,effort:null}};
 globalThis.fetch=async(_,options)=>{const request=JSON.parse(options.body);requests.push(request);return response(request.action==='list'?{items:[item]}:request.action==='get'?item:{saved:true});};
 const panel=presetPanel(input,value=>settings.push(value));panel.open=true;await panel.listeners.toggle();await flush();
 panel.find(node=>node['aria-label']==='Preset name').value='new';await panel.button('Save current draft').listeners.click();await flush();
 assert.equal(input.value,'saved prompt');assert(requests.some(request=>request.action==='save'&&request.expected===null));
 input.value='original draft';await panel.button('Load draft').listeners.click();await flush();assert.equal(input.value,'saved prompt');
 panel.button('Restore draft').listeners.click();assert.equal(input.value,'original draft');assert.deepEqual(settings,[]);
 assert(requests.every(request=>['list','get','save'].includes(request.action)),'loading never submits a prompt');
 globalThis.confirm=()=>false;const before=requests.length;await panel.button('Replace').listeners.click();assert.equal(requests.length,before);
 globalThis.confirm=()=>true;await panel.button('Replace').listeners.click();await flush();assert(requests.some(request=>request.action==='save'&&request.expected==='reviewed-revision'));
});
test('active work or dismissed settings confirmation leaves draft and session settings intact',async()=>{
 const input=new Node('textarea');input.value='keep';const applied=[];
 const item={name:'settings',revision:'x',preset:{prompt:'other',model:'fixture',effort:'high'}};
 globalThis.fetch=async(_,options)=>response(JSON.parse(options.body).action==='list'?{items:[item]}:item);
 const panel=presetPanel(input,p=>applied.push(p));panel.open=true;await panel.listeners.toggle();await flush();
 state.chat.busy=true;await panel.button('Load draft').listeners.click();assert.equal(input.value,'keep');assert.deepEqual(applied,[]);
 state.chat.busy=false;globalThis.confirm=()=>false;await panel.button('Load draft').listeners.click();assert.equal(input.value,'keep');assert.deepEqual(applied,[]);
});
test('a preset response from the previous conversation cannot replace the new conversation draft',async()=>{
 const input=new Node('textarea');input.value='new conversation draft';const applied=[];
 const item={name:'saved',revision:'x',preset:{prompt:'old response',model:'fixture',effort:'low'}};
 let resolveGet;
 globalThis.fetch=async(_,options)=>JSON.parse(options.body).action==='list'?response({items:[item]}):new Promise(resolve=>{resolveGet=()=>resolve(response(item));});
 state.chat.session='source';state.chat.busy=false;
 const panel=presetPanel(input,p=>applied.push(p));panel.open=true;await panel.listeners.toggle();await flush();
 const loading=panel.button('Load draft').listeners.click();await flush();state.chat.session='destination';resolveGet();await loading;
 assert.equal(input.value,'new conversation draft');assert.deepEqual(applied,[]);
});
