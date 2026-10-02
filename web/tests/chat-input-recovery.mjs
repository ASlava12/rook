import test from 'node:test';
import assert from 'node:assert/strict';
class Node {
 constructor(tag,text=''){this.tag=tag;this.text=text;this.nodeType=tag==='#text'?3:1;this.children=[];this.dataset={};this.className='';this.listeners={};this.value='';this.disabled=false;this.parent=null;}
 append(...children){for(const c of children){c.parent=this;this.children.push(c);}}
 replaceChildren(...children){for(const c of this.children)c.parent=null;this.children=[];this.append(...children);}
 replaceWith(node){const p=this.parent;if(!p)return;const at=p.children.indexOf(this);p.children.splice(at,1,node);node.parent=p;this.parent=null;}
 setAttribute(name,value){this[name]=value;}
 addEventListener(name,handler){this.listeners[name]=handler;}
 get textContent(){return this.text+this.children.map(c=>c.textContent).join('');}
 get isConnected(){return this===stream||!!this.parent?.isConnected;}
 get childElementCount(){return this.children.length;}
 all(){return this.children.flatMap(c=>[c,...c.all()]);}
 querySelectorAll(selector){return this.all().filter(c=>selector==='input, button'?['input','button'].includes(c.tag):selector==='[data-input-key]'?!!c.dataset.inputKey:selector==='.approve, .ask-form'?['approve','ask-form'].includes(c.className):false);}
}
const stream=new Node('div');const reconnect=new Node('button');reconnect.hidden=true;
globalThis.document={querySelector:s=>s==='#stream'?stream:s==='#reconnect-chat'?reconnect:null,
 querySelectorAll:s=>stream.querySelectorAll(s==='#stream [data-input-key]'?'[data-input-key]':s),createElement:tag=>new Node(tag),createTextNode:text=>new Node('#text',text),createDocumentFragment:()=>new Node('#fragment')};
globalThis.window={};
globalThis.location={protocol:'http:',host:'localhost'};
class Socket {
 constructor(){this.readyState=1;}
 addEventListener(){}
 receive(value){this.onmessage({data:JSON.stringify(value)});}
 disconnect(){this.readyState=3;this.onclose();}
}
globalThis.WebSocket=Socket;
const {connect}=await import('../dist/chat.js');
const question=(id,text='Extension form · request one')=>({type:'ask',id,questions:[{question:text,choices:[],multi:false}]});
const form=()=>stream.all().find(n=>n.className==='ask-form');
const snapshot=ids=>({type:'snapshot',session:'session',running:true,truncated:false,approvals:[],questions:ids});

test('disconnect disables and retains draft until the same request is authoritatively recovered',()=>{
 const socket=connect();socket.receive(question('same'));
 const original=form(),input=original.all().find(n=>n.tag==='input');input.value='private draft';
 socket.disconnect();assert.equal(form(),original);assert.equal(input.disabled,true);assert.equal(reconnect.hidden,false);
 const next=connect();next.receive(snapshot(['same']));next.receive(question('same'));
 assert.equal(form(),original);assert.equal(input.value,'private draft');assert.equal(input.disabled,false);
 next.receive({type:'inputs',approvals:[],questions:[]});assert.equal(form(),undefined);
});
test('a changed question never inherits text even if an older server reuses its id',()=>{
 const socket=connect();socket.receive(question('reused'));
 const original=form();original.all().find(n=>n.tag==='input').value='old private draft';
 socket.receive(snapshot(['reused']));socket.receive(question('reused','A different source or field'));
 assert.notEqual(form(),original);assert.equal(form().all().find(n=>n.tag==='input').value,'');
 socket.receive({type:'inputs',approvals:[],questions:[]});
});
test('an ended turn clears pending controls instead of retaining a disconnected draft',()=>{
 const socket=connect();socket.receive(question('ending'));
 socket.receive({type:'cancelled'});assert.equal(form(),undefined);
});
