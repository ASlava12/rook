// Actual browser controls against the Python probe's owned daemon.
import fs from 'node:fs';
import assert from 'node:assert/strict';
const [base, session, profile] = process.argv.slice(2);
const deadline = Date.now()+30000;
while(!fs.existsSync(`${profile}/DevToolsActivePort`)) {
  assert(Date.now()<deadline,'owned Chrome did not start'); await new Promise(r=>setTimeout(r,50));
}
const port=fs.readFileSync(`${profile}/DevToolsActivePort`,'utf8').split('\n')[0];
const tabs=await(await fetch(`http://127.0.0.1:${port}/json/list`)).json();
const socket=new WebSocket(tabs.find(t=>t.type==='page').webSocketDebuggerUrl);
await new Promise((resolve,reject)=>{socket.addEventListener('open',resolve,{once:true});socket.addEventListener('error',reject,{once:true});});
let next=0;const pending=new Map();
socket.addEventListener('message',e=>{
 const v=JSON.parse(e.data);
 if(v.method==='Page.javascriptDialogOpening') { command('Page.handleJavaScriptDialog',{accept:true}).catch(()=>{}); return; }
 const p=pending.get(v.id);if(!p)return;pending.delete(v.id);clearTimeout(p.timer);
 v.error?p.reject(new Error(JSON.stringify(v.error))):p.resolve(v.result);
});
function command(method,params={}) {return new Promise((resolve,reject)=>{const id=++next,timer=setTimeout(()=>{pending.delete(id);reject(new Error(`${method} timed out`));},30000);pending.set(id,{resolve,reject,timer});socket.send(JSON.stringify({id,method,params}));});}
async function evaluate(expression) {const r=await command('Runtime.evaluate',{expression,awaitPromise:true,returnByValue:true});if(r.exceptionDetails)throw new Error(r.exceptionDetails.exception?.description||r.exceptionDetails.text);return r.result.value;}
async function wait(expression) {const until=Date.now()+30000;while(Date.now()<until){if(await evaluate(expression))return;await new Promise(r=>setTimeout(r,50));}throw new Error(`condition timed out: ${expression}`);}
async function click(expression) {const point=await evaluate(`(()=>{const e=${expression};if(!e)throw new Error('missing click target');e.scrollIntoView({block:'center'});const r=e.getBoundingClientRect();return{x:r.left+r.width/2,y:r.top+r.height/2};})()`);await command('Input.dispatchMouseEvent',{type:'mousePressed',button:'left',clickCount:1,...point});await command('Input.dispatchMouseEvent',{type:'mouseReleased',button:'left',clickCount:1,...point});}
const button=name=>`[...document.querySelectorAll('#view button')].find(b=>b.textContent===${JSON.stringify(name)})`;
const summary=name=>`[...document.querySelectorAll('#view summary')].find(b=>b.textContent===${JSON.stringify(name)})`;
try {
 await command('Page.enable');await command('Emulation.setDeviceMetricsOverride',{width:1300,height:1500,deviceScaleFactor:1,mobile:false});
 await command('Page.navigate',{url:base});await wait('!!document.querySelector("#picker select") && !!document.querySelector("#settings select")');
 await evaluate(`(()=>{const picker=document.querySelector('#picker select');picker.value=${JSON.stringify(session)};picker.dispatchEvent(new Event('change'));})()`);
 await wait('document.querySelector("#settings")?.textContent?.includes("Selected: B / high")');
 await evaluate(`(()=>{window.pairedWrites=[];const original=window.fetch;window.fetch=(url,options)=>{if(String(url).endsWith('/model-selection')&&options?.method==='POST')window.pairedWrites.push(JSON.parse(options.body));return original(url,options);};})()`);
 await click(summary('Prompt presets'));await wait(`!!${button('Load draft')}`);
 await click(button('Load draft'));await wait('document.querySelector("#chat-input").value==="R17_PRESET_DRAFT"');
 await click(summary('Background work · session family'));
 await wait("document.querySelector('#background-controls')?.textContent.includes('Examined')");
 assert.equal(await evaluate('document.querySelector("#chat-input").value'),'R17_PRESET_DRAFT');
 assert.equal(await evaluate('document.querySelector("#picker select").value'),session);
 assert(await evaluate("document.querySelector('#background-controls').textContent.includes('No observed executor')"));
 const writes=await evaluate('window.pairedWrites');assert.equal(writes.length,1);assert.equal(writes[0].model,'A');assert.equal(writes[0].effort,'max');
 const view=await(await fetch(`${base}/api/sessions/${session}/model-selection`)).json();assert.equal(view.selected,'A');assert.equal(view.selected_effort,'max');assert.equal(view.last_effective_effort,'high');
 // Transport is owned by this test: never write to the operator's clipboard.
 await evaluate(`Object.defineProperty(navigator,'clipboard',{configurable:true,value:undefined})`);
 await click(button('Copy last answer'));await wait(`!!document.querySelector('textarea[aria-label="Saved answer to copy manually"]')`);
 assert.equal(await evaluate(`document.querySelector('textarea[aria-label="Saved answer to copy manually"]').value`),'Hello from model B');
 await evaluate(`Object.defineProperty(navigator,'clipboard',{configurable:true,value:{writeText:async text=>{window.copiedAnswer=text;}}})`);
 await click(button('Copy last answer'));await wait('window.copiedAnswer === "Hello from model B"');
 await click(summary('Drain before updating daemon'));await click(button('Drain'));
 await wait(`(()=>{const text=document.querySelector('details pre[role=status]').textContent;if(text.includes('ready'))return true;${button('Refresh status')}.click();return false;})()`);
 await click(button('Cancel observed drain'));await wait('document.querySelector("details pre[role=status]").textContent.includes("cancelled")');
 // Hold a real HTTP acknowledgement while another session becomes visible.
 await evaluate(`(()=>{const previous=window.fetch;window.fetch=async(url,options)=>{const response=await previous(url,options);if(String(url).endsWith('/model-selection')&&options?.method==='POST'&&!window.modelAckReady){window.modelAckReady=true;await new Promise(resolve=>window.releaseModelAck=resolve);const read=response.json.bind(response);response.json=async()=>{const value=await read();window.modelJsonRead=true;return value;};}return response;};const select=[...document.querySelectorAll('#settings label')].find(e=>e.textContent.startsWith('requested effort')).querySelector('select');select.value='high';select.dispatchEvent(new Event('change'));})()`);
 await wait('window.modelAckReady && !!window.releaseModelAck');
 await evaluate(`(async()=>{const {state}=await import('/lib.js');window.originalModelState={session:state.chat.session,settings:state.chat.settings,modelPickNotice:state.chat.modelPickNotice};state.chat.session='other-session';state.chat.settings={...state.chat.settings,model:'B',effort:'low'};state.chat.modelPickNotice='other-session-marker';window.releaseModelAck();})()`);
 await wait('window.modelJsonRead');
 assert.deepEqual(await evaluate(`(async()=>{const {state}=await import('/lib.js');return {model:state.chat.settings.model,effort:state.chat.settings.effort,notice:state.chat.modelPickNotice};})()`),{model:'B',effort:'low',notice:'other-session-marker'});
 await evaluate(`(async()=>{const {state}=await import('/lib.js');Object.assign(state.chat,window.originalModelState);})()`);
 await command('Page.reload');await wait('!!document.querySelector("#picker select")');
 await evaluate(`(()=>{const picker=document.querySelector('#picker select');picker.value=${JSON.stringify(session)};picker.dispatchEvent(new Event('change'));})()`);
 await wait('document.querySelector("#settings")?.textContent?.includes("Selected: A / high")');
 console.log(JSON.stringify({browser_preset_atomic_pair:true,background_panel_retains_draft_and_session:true,copy_saved_api:true,clipboard_fallback:true,clipboard_owned_transport:true,drain_cancel:true,reload_selected_pair:true,late_acknowledgement_keeps_other_session_settings:true}));
} finally {socket.close();}
