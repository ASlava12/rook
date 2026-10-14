import test from 'node:test';
import assert from 'node:assert/strict';
import { recipesPanel } from '../dist/recipes.js';
class Node {
 constructor(tag,text=''){this.tag=tag;this.nodeType=tag==='#text'?3:1;this.text=text;this.children=[];this.listeners={};this.isConnected=true;}
 append(...kids){this.children.push(...kids);}
 replaceChildren(...kids){this.children=kids;}
 setAttribute(k,v){this[k]=v;}
 addEventListener(k,v){this.listeners[k]=v;}
 get textContent(){return this.text+this.children.map(c=>c.textContent).join('');}
 find(f){if(f(this))return this;for(const c of this.children){const x=c.find?.(f);if(x)return x;}return null;}
 button(label){return this.find(n=>n.tag==='button'&&n.textContent===label);}
}
globalThis.document={createElement:tag=>new Node(tag),createTextNode:text=>new Node('#text',text)};
globalThis.confirm=()=>true;
const flush=()=>new Promise(resolve=>setImmediate(resolve));
const reply=x=>({ok:true,json:async()=>x});
const receipt={id:'event-one',route:'alpha',delivery:'delivery-one',session:'source-one',state:'pending',task:'Configured task',workspace:'/workspace',body_bytes:4,body_hash:'hash'};


test('stage source and child navigation stay bound to the selected composition',async()=>{
 const calls=[];globalThis.fetch=async(path,options)=>{const action=JSON.parse(options.body);calls.push({path,action});return reply(action.action==='list'?{calls:[{id:'call-one',state:'completed',mode:'sequential'}]}:action.action==='read'?{id:'call-one',bytes_base64:'T1VUX0E=',next_offset:null}:{id:'call-one',state:'completed',mode:'sequential',stages:[{id:'one',recipe:'a',state:'completed',child:'child-one',output:7}],definitions:[2]});};
 const panel=recipesPanel('parent-one');panel.button('List compositions').listeners.click();await flush();panel.button('Inspect composition').listeners.click();await flush();assert.ok(panel.button('Open stage session'));panel.button('Read stage output').listeners.click();await flush();assert.match(panel.textContent,/OUT_A/);assert.deepEqual(calls.at(-1),{path:'/api/sessions/parent-one/recipes',action:{action:'read',id:'call-one',seq:7,offset:0}});
});
test('interrupted receipts require a review note and cannot be forgotten before review',async()=>{
 const calls=[];globalThis.prompt=()=>"Reviewed original child recovery";globalThis.fetch=async(path,options)=>{const action=JSON.parse(options.body);calls.push(action);return reply(action.action==='list'?{calls:[{id:'one',state:'interrupted',mode:'parallel'}]}:{id:'one',state:'interrupted',mode:'parallel',stages:[{id:'a',recipe:'a',state:'interrupted',child:'child'}],definitions:[]});};
 const panel=recipesPanel('parent');panel.button('List compositions').listeners.click();await flush();panel.button('Inspect composition').listeners.click();await flush();assert.equal(panel.button('Forget resolved composition'),null);panel.button('Resolve reviewed interruption').listeners.click();await flush();assert.deepEqual(calls.at(-1),{action:'resolve',id:'one',note:'Reviewed original child recovery'});
});
test('departed composition panels cannot expose a late source or recovery control',async()=>{
 let done;globalThis.fetch=async()=>new Promise(resolve=>done=resolve);const panel=recipesPanel('old-parent');panel.button('List compositions').listeners.click();await flush();panel.isConnected=false;done(reply({calls:[{id:'stale'}]}));await flush();assert.equal(panel.button('Inspect composition'),null);
});
