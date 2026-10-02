import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
const root=fs.readFileSync('target/extension-live-browser-root.txt','utf8').trim();
const address=fs.readFileSync(path.join(root,'home/rookd.addr'),'utf8').trim();
const portFile=path.join(root,'edge/DevToolsActivePort');
const deadline=Date.now()+30000;
while(!fs.existsSync(portFile)){assert(Date.now()<deadline);await new Promise(r=>setTimeout(r,100));}
const port=fs.readFileSync(portFile,'utf8').split('\n')[0];
const tabs=await(await fetch(`http://127.0.0.1:${port}/json/list`)).json();
const socket=new WebSocket(tabs.find(t=>t.type==='page' && (t.url==='about:blank' || t.url.startsWith(address))).webSocketDebuggerUrl);
await new Promise((resolve,reject)=>{socket.addEventListener('open',resolve,{once:true});socket.addEventListener('error',reject,{once:true});});
let next=0;const pending=new Map();
socket.addEventListener('message',e=>{const v=JSON.parse(e.data),p=pending.get(v.id);if(!p)return;pending.delete(v.id);clearTimeout(p.timer);v.error?p.reject(new Error(JSON.stringify(v.error))):p.resolve(v.result);});
function command(method,params={}){return new Promise((resolve,reject)=>{const id=++next,timer=setTimeout(()=>{pending.delete(id);reject(new Error(method+' timed out'));},30000);pending.set(id,{resolve,reject,timer});socket.send(JSON.stringify({id,method,params}));});}
async function evaluate(expression){const r=await command('Runtime.evaluate',{expression,awaitPromise:true,returnByValue:true});if(r.exceptionDetails)throw new Error(r.exceptionDetails.exception?.description||r.exceptionDetails.text);return r.result.value;}
async function wait(expression){const until=Date.now()+30000;while(Date.now()<until){if(await evaluate(expression))return;await new Promise(r=>setTimeout(r,100));}throw new Error('Condition timed out: '+expression);}
async function click(expression){const point=await evaluate(`(()=>{const e=${expression};if(!e)throw new Error('missing click target');e.scrollIntoView({block:'center'});const r=e.getBoundingClientRect();return{x:r.left+r.width/2,y:r.top+r.height/2};})()`);await command('Input.dispatchMouseEvent',{type:'mousePressed',button:'left',clickCount:1,...point});await command('Input.dispatchMouseEvent',{type:'mouseReleased',button:'left',clickCount:1,...point});}
async function capture(name){fs.writeFileSync(path.join(root,name+'.txt'),await evaluate('document.querySelector("#view").textContent'));const shot=await command('Page.captureScreenshot',{format:'png'});fs.writeFileSync(path.join(root,name+'.png'),Buffer.from(shot.data,'base64'));}
const field=i=>`document.querySelectorAll('.ask-form fieldset')[${i}]`;
try{
 await command('Page.enable');
 await command('Page.addScriptToEvaluateOnNewDocument',{source:'const ProbeOriginalWebSocket=WebSocket;window.probeSockets=[];window.WebSocket=class extends ProbeOriginalWebSocket{constructor(...args){super(...args);window.probeSockets.push(this)}};'});
 await command('Emulation.setDeviceMetricsOverride',{width:1200,height:1300,deviceScaleFactor:1,mobile:false});
 await command('Page.navigate',{url:address});await wait('!!document.querySelector("#chat-input")');
 await evaluate(`(async()=>{window.extensionState=(await import('/lib.js')).state;})()`);
 await click('document.querySelector("#chat-input")');await command('Input.insertText',{text:'BROWSER_EXTENSION'});
 await click('document.querySelector("#send")');
 await wait('document.querySelectorAll(".ask-form fieldset").length===4');
 await wait('document.querySelector("#extension-widget").textContent.includes("waiting for an answer")');
 assert.match(await evaluate('document.querySelector("#extension-widget").textContent'),/hook prompt #1.*source/s);
 assert.match(await evaluate('document.querySelector("#extension-widget").textContent'),/current files and tests are not verified/);
 const id=await evaluate('window.extensionState.chat.session');fs.writeFileSync(path.join(root,'session-id'),id);
 const key=await evaluate('document.querySelector(".ask-form").dataset.inputKey');
 assert.match(await evaluate('document.querySelector(".ask-form").textContent'),/Extension hook prompt #1.*source/s);
 await click(`${field(0)}.querySelector('input[type="text"]')`);await command('Input.insertText',{text:'PRIVATE_BROWSER_INPUT'});
 await click(`${field(1)}.querySelector('input[value="remote"]')`);
 await capture('browser-form-draft');
 await evaluate('window.probeSockets.find(s=>s.readyState===1).close(1000,"fixture reconnect")');
 await wait('!document.querySelector("#reconnect-chat").hidden');
 assert.equal(await evaluate(`${field(0)}.querySelector('input[type="text"]').disabled`),true);
 await click('document.querySelector("#reconnect-chat")');
 await wait('window.probeSockets.length>=2 && window.probeSockets.at(-1).readyState===1');
 await wait(`document.querySelector('.ask-form')?.dataset.inputKey===${JSON.stringify(key)}`);
 assert.equal(await evaluate(`${field(0)}.querySelector('input[type="text"]').value`),'PRIVATE_BROWSER_INPUT');
 assert.equal(await evaluate(`${field(1)}.querySelector('input[value="remote"]').checked`),true);
 await capture('browser-form-reconnected');
 await wait('document.querySelector("#extension-widget").textContent.includes("waiting for an answer")');
 await click(`${field(2)}.querySelector('input[value="No"]')`);
 await click(`${field(3)}.querySelector('input[type="text"]')`);await command('Input.insertText',{text:'7'});
 await click(`document.querySelector('.ask-form button[type="submit"]')`);
 await wait('!window.extensionState.chat.busy && !document.querySelector(".ask-form")');
 await wait('document.querySelector("#extension-widget").textContent.includes("LIVE_RESULT")');
 const widget=await evaluate('document.querySelector("#extension-widget").textContent');
 assert(widget.includes('LIVE_PROGRESS') && widget.includes('3/4') && widget.includes('DISPLAY_RESULT_ONLY'));
 assert(!widget.includes('FORM_DISPLAY_ONLY') && !widget.includes('PRIVATE_BROWSER_INPUT'));
 await capture('browser-widget-finished');
 const answer=fs.readFileSync(path.join(root,'workspace/hook-answer.json'),'utf8');
 assert.deepEqual(JSON.parse(answer).form_answer.values,{name:'PRIVATE_BROWSER_INPUT',target:'remote',confirm:false,count:7});
 const context=await(await fetch(`${address}/api/sessions/${id}/context?workspace=${encodeURIComponent(path.join(root,'workspace'))}`)).json();
 assert(context.extension_ui.reports.some(r=>r.item.text.includes('Typed extension setup · answered')));
 assert(!JSON.stringify(context.extension_ui).includes('PRIVATE_BROWSER_INPUT'));
 await click(`document.querySelector('nav button[data-tab="context"]')`);
 await wait('document.querySelector("#view").textContent.includes("Typed extension setup · answered")');
 await capture('browser-form-context');
 await click(`document.querySelector('nav button[data-tab="chat"]')`);
 await wait('!!document.querySelector("#chat-input")');
 await wait('document.querySelector("#extension-widget").textContent.includes("LIVE_RESULT")');
 assert(!(await evaluate('document.querySelector("#stream").textContent')).includes('rook:extension-ui:v1'));
 await capture('browser-widget-restored');
 await click('document.querySelector("#chat-input")');await command('Input.insertText',{text:'STOP_EXTENSION'});await click('document.querySelector("#send")');
 await wait('document.querySelectorAll(".ask-form fieldset").length===4');
 await click('document.querySelector("#stop")');
 await wait('!window.extensionState.chat.busy && !document.querySelector(".ask-form")');
 await wait('document.querySelector("#extension-widget").textContent.includes("interrupted")');
 assert.equal(fs.readFileSync(path.join(root,'workspace/hook-answer.json'),'utf8'),answer);
 const stopped=await(await fetch(`${address}/api/sessions/${id}/context?workspace=${encodeURIComponent(path.join(root,'workspace'))}`)).json();
 assert(stopped.extension_ui.reports.some(r=>r.item.text.includes('interrupted')));
 await capture('browser-form-stopped');
 const requestFiles=fs.readdirSync(root).filter(name=>/^request-\d{3}\.json$/.test(name));
 assert(requestFiles.length<=48);
 for(const name of requestFiles){const file=path.join(root,name);assert(fs.statSync(file).size<=4*1024*1024);const request=fs.readFileSync(file,'utf8');for(const marker of ['PRIVATE_BROWSER_INPUT','FORM_DISPLAY_ONLY','DISPLAY_RESULT_ONLY'])assert(!request.includes(marker));}
 fs.writeFileSync(path.join(root,'browser-proof.json'),JSON.stringify({session:id,reconnected_key:key,typed_answer:true,stop:true,live_widgets:true,progress_result_clear:true,reopen:true},null,2));
 console.log('Browser forms: typed values, source, retained draft after socket reconnect, Context and Stop verified');
}catch(error){await capture('browser-form-failed').catch(()=>{});throw error;}finally{socket.close();}
