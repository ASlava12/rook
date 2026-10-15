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
 querySelectorAll(selector){return this.all().filter(c=>selector==='input, button'?['input','button'].includes(c.tag):selector==='input'?c.tag==='input':selector==='input[type="password"]'?c.tag==='input'&&c.type==='password':selector==='.private-input-form'?c.className==='private-input-form':selector==='[data-input-key]'?!!c.dataset.inputKey:selector==='.approve, .ask-form'?['approve','ask-form'].includes(c.className):false);}
}
const stream=new Node('div');const reconnect=new Node('button');reconnect.hidden=true;
const widget=new Node('div');
globalThis.document={querySelector:s=>s==='#stream'?stream:s==='#reconnect-chat'?reconnect:s==='#extension-widget'?widget:null,
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
const secret=id=>({type:'secret_request',prompt:{id,session:'session',name:'deploy',reason:'Authenticate deployment'}});
const privateForm=()=>stream.all().find(n=>n.className==='private-input-form');
const posted=[];
globalThis.fetch=async(path,options)=>{posted.push({path,body:JSON.parse(options.body)});return {ok:true,json:async()=>({status:posted.at(-1).body.status})};};

test('private input discards its value on disconnect and never restores it as a draft',()=>{
 const socket=connect();socket.receive(snapshot([]));socket.receive(secret('private-one'));
 const original=privateForm(),input=original.all().find(n=>n.tag==='input');
 assert.equal(input.type,'password');input.value='PRIVATE_VALUE_NOT_A_DRAFT';
 socket.disconnect();assert.equal(input.value,'');assert.equal(privateForm(),undefined);
 const next=connect();next.receive(snapshot(['private-one']));next.receive(secret('private-one'));
 assert.notEqual(privateForm(),original);assert.equal(privateForm().all().find(n=>n.tag==='input').value,'');
 next.receive({type:'inputs',approvals:[],questions:[]});assert.equal(privateForm(),undefined);
 assert(!stream.textContent.includes('PRIVATE_VALUE_NOT_A_DRAFT'));
});

test('private save uses only the correlated HTTP endpoint and refusal carries no value',async()=>{
 const socket=connect();socket.receive(snapshot([]));socket.receive(secret('private-save'));
 let form=privateForm(),input=form.all().find(n=>n.tag==='input');input.value='PRIVATE_ONLY_28';
 form.listeners.submit({preventDefault(){}});await new Promise(resolve=>setImmediate(resolve));
 assert.equal(input.value,'');assert.equal(posted.at(-1).path,'/api/sessions/session/private-input/private-save');
 assert.deepEqual(posted.at(-1).body,{status:'saved',value:'PRIVATE_ONLY_28'});
 assert(!stream.textContent.includes('PRIVATE_ONLY_28'));
 socket.receive(secret('private-decline'));form=privateForm();input=form.all().find(n=>n.tag==='input');input.value='NEVER_SEND_ON_REFUSAL';
 form.all().find(n=>n.tag==='button'&&n.textContent==='Decline').listeners.click();
 await new Promise(resolve=>setImmediate(resolve));
 assert.equal(input.value,'');assert.deepEqual(posted.at(-1).body,{status:'declined'});
 assert(!stream.textContent.includes('NEVER_SEND_ON_REFUSAL'));
});

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
test('widget replacement ignores older or foreign prefixes and survives transcript replacement',()=>{
 const socket=connect();socket.receive(snapshot([]));
 const display=(through,text,session='session')=>({type:'agent',text:'legacy fallback',extension_ui:{session,through,state:{reports:[{source:{event:'prompt',ordinal:0,digest:'a'.repeat(64)},event_seq:through,item:{kind:'status',id:'build',text}}],omitted_updates:0,invalid_records:0}}});
 socket.receive(display(10,'newest'));assert(widget.textContent.includes('newest'));
 socket.receive(display(9,'older'));socket.receive(display(11,'foreign','other'));
 assert(widget.textContent.includes('newest'));assert(!widget.textContent.includes('older'));
 socket.receive(snapshot([]));assert(widget.textContent.includes('newest'));
 assert(!stream.textContent.includes('legacy fallback'));
 socket.receive({type:'agent',text:'',extension_ui:{session:'session',through:12,state:{reports:[],omitted_updates:0,invalid_records:0}}});
 assert.equal(widget.hidden,true);
 socket.receive(display(13,'current'));
 socket.receive({type:'started',session:'other'});assert.equal(widget.hidden,true);
});

test('server question links require an HTTPS user action and never interpret markup',()=>{
 const socket=connect();socket.receive(snapshot([]));
 socket.receive(question('url-flow','MCP server example\nhttps://example.org/authorize?state=one\n<script>ignore</script> javascript:alert(1) https://user:password@example.org/'));
 const links=form().all().filter(n=>n.tag==='a');
 assert.equal(links.length,1);assert.equal(links[0].href,'https://example.org/authorize?state=one');
 assert.equal(links[0].rel,'noopener noreferrer');assert.equal(links[0].referrerpolicy,'no-referrer');
 assert(!form().all().some(n=>n.tag==='script'));assert(form().textContent.includes('<script>ignore</script>'));
 socket.receive({type:'cancelled'});
});
