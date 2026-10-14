import test from 'node:test';
import assert from 'node:assert/strict';
import { telegramPanel } from '../dist/telegram.js';
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

test('reviewed retry retains the selected message and observed attempt count',async()=>{
 const calls=[];globalThis.prompt=()=>"checked the chat; no message received";
 globalThis.fetch=async(path,options)=>{const request=JSON.parse(options.body);calls.push({path,request});return reply(request.action==='outbox'?{outbox:[{id:'one',session:'session-one',state:'uncertain',attempts:2}]}:request.action==='inspect'?{id:'one',session:'session-one',state:'uncertain',attempts:2,text:'retained response'}:{id:'one',state:'pending'});};
 const panel=telegramPanel();panel.button('Outbound messages').listeners.click();await flush();panel.button('Inspect message').listeners.click();await flush();
 assert.equal(panel.button('Forget resolved receipt'),null);panel.button('Retry inspected message').listeners.click();await flush();
 assert.deepEqual(calls.at(-1),{path:'/api/telegram',request:{action:'retry',id:'one',attempts:2,note:'checked the chat; no message received'}});
});
test('cancelled retry and a delayed departed panel expose no stale action',async()=>{
 let done;globalThis.fetch=async()=>new Promise(resolve=>done=resolve);const panel=telegramPanel();panel.button('Outbound messages').listeners.click();await flush();panel.isConnected=false;done(reply({outbox:[{id:'stale',session:'s',state:'uncertain',attempts:1}]}));await flush();assert.equal(panel.button('Inspect message'),null);
});
test('sent messages offer retention cleanup without a delivery retry',async()=>{
 globalThis.fetch=async(path,options)=>reply(JSON.parse(options.body).action==='outbox'?{outbox:[{id:'one',session:'s',state:'sent',attempts:1}]}:{id:'one',session:'s',state:'sent',attempts:1,text:'done'});
 const panel=telegramPanel();panel.button('Outbound messages').listeners.click();await flush();panel.button('Inspect message').listeners.click();await flush();assert.equal(panel.button('Retry inspected message'),null);assert.ok(panel.button('Forget resolved receipt'));
});
