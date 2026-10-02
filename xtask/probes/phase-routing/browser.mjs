import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
const root = fs.readFileSync('target/phase-live-browser-root.txt','utf8').trim();
const address = fs.readFileSync(path.join(root,'home/rookd.addr'),'utf8').trim();
const portFile = path.join(root,'edge/DevToolsActivePort');
const deadline = Date.now()+30000;
while (!fs.existsSync(portFile)) { assert(Date.now()<deadline,'Edge did not start'); await new Promise(r=>setTimeout(r,100)); }
const port=fs.readFileSync(portFile,'utf8').split('\n')[0];
const tabs=await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
const socket=new WebSocket(tabs.find(t=>t.type==='page').webSocketDebuggerUrl);
await new Promise((resolve,reject)=>{socket.addEventListener('open',resolve,{once:true});socket.addEventListener('error',reject,{once:true});});
let next=0;const pending=new Map();
socket.addEventListener('message',e=>{const v=JSON.parse(e.data),p=pending.get(v.id);if(!p)return;pending.delete(v.id);clearTimeout(p.timer);v.error?p.reject(new Error(JSON.stringify(v.error))):p.resolve(v.result);});
function command(method,params={}){return new Promise((resolve,reject)=>{const id=++next,timer=setTimeout(()=>{pending.delete(id);reject(new Error(method+' timed out'));},30000);pending.set(id,{resolve,reject,timer});socket.send(JSON.stringify({id,method,params}));});}
async function evaluate(expression){const r=await command('Runtime.evaluate',{expression,awaitPromise:true,returnByValue:true});if(r.exceptionDetails)throw new Error(r.exceptionDetails.exception?.description||r.exceptionDetails.text);return r.result.value;}
async function wait(expression){const until=Date.now()+30000;while(Date.now()<until){if(await evaluate(expression))return;await new Promise(r=>setTimeout(r,100));}throw new Error('Condition timed out: '+expression);}
async function click(expression){const point=await evaluate(`(()=>{const e=${expression};if(!e)throw new Error('missing click target');e.scrollIntoView({block:'center'});const r=e.getBoundingClientRect();return{x:r.left+r.width/2,y:r.top+r.height/2};})()`);await command('Input.dispatchMouseEvent',{type:'mousePressed',button:'left',clickCount:1,...point});await command('Input.dispatchMouseEvent',{type:'mouseReleased',button:'left',clickCount:1,...point});}
async function capture(name){const text=await evaluate('document.querySelector("#view").textContent');fs.writeFileSync(path.join(root,name+'.txt'),text);const shot=await command('Page.captureScreenshot',{format:'png'});fs.writeFileSync(path.join(root,name+'.png'),Buffer.from(shot.data,'base64'));return text;}
const tab=name=>`document.querySelector('nav button[data-tab="${name}"]')`;
const button=name=>`[...document.querySelectorAll('#view button')].find(b=>b.textContent===${JSON.stringify(name)})`;
try {
 await command('Page.enable');
 await command('Emulation.setDeviceMetricsOverride',{width:1200,height:1300,deviceScaleFactor:1,mobile:false});
 await command('Page.navigate',{url:address});await wait('!!document.querySelector("#chat-input")');
 await evaluate(`(async()=>{window.phaseState=(await import('/lib.js')).state;})()`);
 await click('document.querySelector("#chat-input")');await command('Input.insertText',{text:'BROWSER_PHASE'});
 await click(button('Send'));await wait('window.phaseState.chat.busy && !!window.phaseState.chat.session');
 const id=await evaluate('window.phaseState.chat.session');fs.writeFileSync(path.join(root,'session-id'),id);
 const until=Date.now()+30000;
 while (!fs.existsSync(path.join(root,'started-BROWSER_PHASE-2'))) { assert(Date.now()<until,'implementation request did not start');await new Promise(r=>setTimeout(r,100)); }
 await wait('document.querySelector("#stream").textContent.includes("IMPLEMENTATION_BROWSER_PHASE")');
 await click(tab('context'));await wait('document.querySelector("#view").textContent.includes("Last response")');
 const live=await capture('browser-during-implementation');
 assert.match(live,/Selected: analysis · phase analysis/);
 assert.match(live,/Dispatched: analysis \/ analysis-model/);
 assert.match(live,/window 32768/);
 assert.match(live,/2 started · 1 completed · 0 failed · 0 incomplete · 0 interrupted · 1 pending/);
 fs.writeFileSync(path.join(root,'release-BROWSER_PHASE'),'1');
 await wait('!window.phaseState.chat.busy');
 await click(button('Refresh context'));await wait('document.querySelector("#view").textContent.includes("phase implementation")');
 const complete=await capture('browser-complete');
 assert.match(complete,/Selected: analysis · phase implementation/);
 assert.match(complete,/Dispatched: implementation \/ implementation-model/);
 assert.match(complete,/Reported model: server-implementation-model/);
 assert.match(complete,/Known subtotal: USD 0\.00007000/);
 assert.match(complete,/3 started · 3 completed/);
 assert.match(complete,/Receipt and attempt subtotals overlap; do not add them/);
 const costs=await (await fetch(`${address}/api/sessions/${id}/context`)).json();
 fs.writeFileSync(path.join(root,'browser-context.json'),JSON.stringify(costs));
 await command('Page.reload');await wait('document.readyState === "complete" && !!document.querySelector("nav button[data-tab=context]")');
 await click(tab('context'));await wait('document.querySelector("#view").textContent.includes("phase implementation")');
 const reopened=await capture('browser-reloaded');
 assert.match(reopened,/Known subtotal: USD 0\.00007000/);
 assert.match(reopened,/Dispatched: implementation \/ implementation-model/);
 const requests=fs.readdirSync(root).filter(n=>/^request-\d+\.json$/.test(n)).map(n=>JSON.parse(fs.readFileSync(path.join(root,n),'utf8')));
 assert.equal(requests.length,3,'inspection and reload must not generate another model call');
 assert.deepEqual(requests.filter(r=>r.stream).map(r=>r.model),['analysis-model','implementation-model']);
 assert.equal(fs.readFileSync(path.join(root,'workspace/routing.txt'),'utf8'),'BROWSER_PHASE\n');
 assert.equal(fs.readFileSync(path.join(root,'workspace/untouched.txt'),'utf8'),'untouched phase workspace');
 console.log(JSON.stringify({session:id,requests:requests.length,live:true,completed:true,reloaded:true}));
} finally { socket.close(); }
