import test from 'node:test';
import assert from 'node:assert/strict';
class Node {
 constructor(tag,text=''){this.tag=tag;this.text=text;this.nodeType=tag==='#text'?3:1;this.children=[];this.hidden=false;}
 append(...nodes){this.children.push(...nodes);}
 setAttribute(name,value){this[name]=value;}
 get textContent(){return this.text+this.children.map(n=>n.textContent).join('');}
 all(){return this.children.flatMap(n=>[n,...n.all()]);}
}
globalThis.document={createElement:tag=>new Node(tag),createTextNode:text=>new Node('#text',text)};
const {extensionPanel,readExtension}=await import('../dist/extension-ui.js');
const source={event:'prompt',ordinal:0,digest:'a'.repeat(64)};
const state=reports=>({reports,omitted_updates:0,invalid_records:0});
const report=item=>({source,event_seq:3,item});
test('status progress and results show source and historical attribution as literal text',()=>{
 const panel=extensionPanel(state([
  report({kind:'status',id:'build',text:'<script>unsafe()</script>'}),
  report({kind:'progress',id:'scan',label:'Scanning',done:2,total:3}),
  report({kind:'result',id:'result',title:'Checks',body:'echo <unsafe>\nsecond line'}),
 ]));
 assert.match(panel.textContent,/hook prompt #1 · source aaaaaaaa · event #3/);
 assert.match(panel.textContent,/current files and tests are not verified/);
 assert.match(panel.textContent,/Scanning · 2\/3/);
 assert(panel.textContent.includes('<script>unsafe()</script>'));
 assert(!panel.all().some(n=>n.tag==='script'));
 assert(panel.textContent.includes('echo <unsafe>\nsecond line'));
 assert.equal(extensionPanel(state([])).hidden,true);
});
test('display rejects excess entries forged sources controls in text and imprecise progress',()=>{
 for(const value of [
  state(Array(129).fill(null)),
  state([{source:{...source,digest:'wrong'},event_seq:3,item:{kind:'status',id:'x',text:'x'}}]),
  state([report({kind:'status',id:'x',text:'escape\u001b[2J'})]),
  state([report({kind:'progress',id:'x',label:'x',done:1,total:9007199254740992})]),
  {...state([]),extra:'x'.repeat(1048577)},
 ]) assert.throws(()=>extensionPanel(value));
});
test('HTTP display admission refuses the oversized arriving chunk and cancels its reader',async()=>{
 const previous=globalThis.fetch;let cancelled=false;
 const cap=2*1024*1024,chunk=new Uint8Array(cap+1);
 assert(chunk.length>cap);
 globalThis.fetch=async()=>new Response(new ReadableStream({start(c){c.enqueue(chunk)},cancel(){cancelled=true}}));
 try {await assert.rejects(readExtension('session'),/exceeds limit/);assert.equal(cancelled,true);}
 finally {globalThis.fetch=previous;}
});
