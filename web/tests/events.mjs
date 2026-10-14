import test from 'node:test';
import assert from 'node:assert/strict';
import { eventsPanel } from '../dist/events.js';
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
test('event inspection and source paging remain bound to the selected durable receipt',async()=>{
 const calls=[];globalThis.fetch=async(path,options)=>{const request=JSON.parse(options.body);calls.push({path,request});
  return reply(request.action==='list'?{receipts:[receipt],after:null}:request.action==='read'?{id:receipt.id,offset:request.offset,total_bytes:7,next_offset:request.offset===0?4:null,bytes_base64:request.offset===0?'ZGF0YQ==':'ZW5k'}:receipt);
 };
 const panel=eventsPanel();panel.button('List deliveries').listeners.click();await flush();panel.button('Inspect delivery').listeners.click();await flush();
 assert.equal(panel.button('Forget receipt'),null);panel.button('Read event source').listeners.click();await flush();assert.match(panel.textContent,/data/);
 panel.button('Read next part').listeners.click();await flush();assert.deepEqual(calls.at(-1),{path:'/api/events',request:{action:'read',id:'event-one',offset:4}});assert.match(panel.textContent,/end/);
});
test('departed event panels discard delayed responses and cannot expose stale controls',async()=>{
 let done;globalThis.fetch=async()=>new Promise(resolve=>done=resolve);const panel=eventsPanel();panel.button('List deliveries').listeners.click();await flush();panel.isConnected=false;done(reply({receipts:[receipt],after:null}));await flush();assert.equal(panel.button('Inspect delivery'),null);
});
test('forgetting a resolved receipt requires confirmation and does not send an unrelated task action',async()=>{
 const calls=[];globalThis.fetch=async(path,options)=>{const request=JSON.parse(options.body);calls.push(request);return reply(request.action==='list'?{receipts:[{...receipt,state:'cancelled'}],after:null}:{...receipt,state:'cancelled'});};
 const panel=eventsPanel();panel.button('List deliveries').listeners.click();await flush();panel.button('Inspect delivery').listeners.click();await flush();
 globalThis.confirm=()=>false;panel.button('Forget receipt').listeners.click();await flush();assert.equal(calls.some(r=>r.action==='forget'),false);
 globalThis.confirm=()=>true;panel.button('Forget receipt').listeners.click();await flush();assert.deepEqual(calls.at(-1),{action:'forget',id:'event-one'});
});
