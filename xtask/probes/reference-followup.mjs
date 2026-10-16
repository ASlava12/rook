// Native, owned Chrome/daemon probe. No operator model, vault or daemon is used.
// ROOK_FOLLOWUP_TARGET=... ROOK_PLAYWRIGHT_MODULE=... ROOK_CHROME=... node this-file
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import http from 'node:http';
import {spawn,execFileSync} from 'node:child_process';
import {pathToFileURL} from 'node:url';
import assert from 'node:assert/strict';
const {chromium}=await import(pathToFileURL(process.env.ROOK_PLAYWRIGHT_MODULE).href);
const root=fs.mkdtempSync(path.join(os.tmpdir(),'rook-followup-native-'));
const home=path.join(root,'home'),workspace=path.join(root,'workspace'),source=path.join(root,'source');
for(const dir of [home,workspace,path.join(source,'fixture-skill','references')])fs.mkdirSync(dir,{recursive:true});
fs.writeFileSync(path.join(source,'fixture-skill','SKILL.md'),'---\nname: fixture-skill\ndescription: Use when the fixture resource is needed.\n---\nRead references/data.txt.\n');
fs.writeFileSync(path.join(source,'fixture-skill','references','data.txt'),'SCOPED_RESOURCE_CONTENT');
const requests=[];let exfiltrations=0;
const llm=http.createServer((req,res)=>{
 if(req.url.startsWith('/exfil')){exfiltrations++;res.end('owned trap');return;}
 let raw='';req.on('data',bytes=>{raw+=bytes;if(raw.length>1048576)req.destroy();});req.on('end',()=>{
  if(req.url.endsWith('/models')){res.setHeader('Content-Type','application/json');res.end(JSON.stringify({data:[{id:'fixture'}]}));return;}
  const request=JSON.parse(raw||'{}');requests.push(request);
  if(request.stream){res.writeHead(200,{'Content-Type':'text/event-stream'});res.end('data: '+JSON.stringify({id:'fixture',object:'chat.completion.chunk',model:'fixture',choices:[{index:0,delta:{content:'Fixture ready'},finish_reason:null}]})+'\n\ndata: '+JSON.stringify({id:'fixture',object:'chat.completion.chunk',model:'fixture',choices:[{index:0,delta:{},finish_reason:'stop'}],usage:{prompt_tokens:10,completion_tokens:3}})+'\n\ndata: [DONE]\n\n');}
  else{res.setHeader('Content-Type','application/json');res.end(JSON.stringify({model:'fixture',choices:[{message:{role:'assistant',content:'Fixture ready'},finish_reason:'stop'}],usage:{prompt_tokens:10,completion_tokens:3}}));}
 });
});
await new Promise(resolve=>llm.listen(0,'127.0.0.1',resolve));const llmAddress=`http://127.0.0.1:${llm.address().port}`;
const env={...process.env,ROOK_HOME:home,ROOK_NOTIFY:'off'};
const target=path.resolve(process.env.ROOK_FOLLOWUP_TARGET);
const binary=path.join(target,'debug',process.platform==='win32'?'rook.exe':'rook');
fs.writeFileSync(path.join(home,'config.toml'),`skill_sources = [${JSON.stringify(source)}]\n[agent]\nmodel = "fixture"\ninstall_servers = false\n[models.fixture]\napi = "openai"\nurl = "${llmAddress}/v1"\nmodel = "fixture"\n[mcp_connections]\napps_enabled = true\n[[mcp]]\nname = "fixture"\ncommand = "python3"\nargs = [${JSON.stringify(path.resolve('crates/rook-core/tests/fixtures/mcp_apps.py'))},${JSON.stringify(path.join(root,'calls'))}]\n`);
execFileSync('git',['init','-q',workspace]);execFileSync('git',['-C',workspace,'config','user.email','fixture@example.invalid']);execFileSync('git',['-C',workspace,'config','user.name','Fixture']);
fs.writeFileSync(path.join(workspace,'example.txt'),'old line\n');execFileSync('git',['-C',workspace,'add','example.txt']);execFileSync('git',['-C',workspace,'commit','-qm','fixture']);
execFileSync(binary,['-C',workspace,'skills','install','fixture-skill'],{env,stdio:'pipe'});
const daemon=spawn(path.join(target,'debug',process.platform==='win32'?'rookd.exe':'rookd'),['--port','0','--workspace',workspace],{env,stdio:['ignore',fs.openSync(path.join(root,'daemon.out'),'w'),fs.openSync(path.join(root,'daemon.err'),'w')]});
let browser;let passed=false;
const wait=async(check)=>{const deadline=Date.now()+30000;while(Date.now()<deadline){if(await check())return;await new Promise(r=>setTimeout(r,50));}throw new Error(`probe timeout; artifacts ${root}`);};
try{
 await wait(()=>fs.existsSync(path.join(home,'rookd.addr')));const base=fs.readFileSync(path.join(home,'rookd.addr'),'utf8').trim();
 const api=async(route,body)=>{const response=await fetch(base+route,body===undefined?{}:{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body)});const data=await response.json();assert(response.ok,JSON.stringify(data));return data;};
 browser=await chromium.launch({headless:true,executablePath:process.env.ROOK_CHROME});const page=await browser.newPage({viewport:{width:1400,height:1000}});
 page.on('dialog',dialog=>dialog.accept());page.on('pageerror',error=>console.error('PAGE ERROR:',error.message));await page.goto(base);await page.locator('#chat-input').fill('Start the owned fixture');await page.locator('#send').click();
 await wait(async()=>page.evaluate(async()=>{const {state}=await import('/lib.js');return /^[0-9A-Z]{26}$/.test(state.chat.session||'')&&!state.chat.busy;}));
 const owner=await page.evaluate(async()=>String((await import('/lib.js')).state.chat.session));assert.match(owner,/^[0-9A-Z]{26}$/);
 // Presets preserve drafts, require explicit replacement, and use ordinary settings.
 const presets=page.getByText('Prompt presets',{exact:true});await presets.click();await page.locator('#chat-input').fill('Saved review request');await page.getByLabel('Preset name',{exact:true}).fill('review-fixture');await page.getByText('Save current draft',{exact:true}).click();
 await page.getByText('Load draft',{exact:true}).waitFor();assert.equal(await page.locator('#chat-input').inputValue(),'Saved review request');
 await page.locator('#chat-input').fill('Keep this draft');await page.getByText('Load draft',{exact:true}).click();await wait(async()=>await page.locator('#chat-input').inputValue()==='Saved review request');
 await page.getByText('Restore draft',{exact:true}).click();assert.equal(await page.locator('#chat-input').inputValue(),'Keep this draft');
 // A real MCP app initializes in two opaque frames and calls nothing until consent.
 await page.getByText('MCP connections',{exact:true}).click();await page.getByText('MCP Apps',{exact:true}).click();await page.getByText('Open app',{exact:true}).click();
 let app;await wait(()=>{app=page.frames().find(frame=>frame.url()==='about:srcdoc');return !!app;});
 await wait(async()=>{for(const frame of page.frames()){if(await frame.locator('#refresh').count()){app=frame;return await frame.locator('body').getAttribute('data-ready')==='yes';}}return false;});
 await app.locator('#refresh').click();await page.getByText('Allow this call',{exact:true}).waitFor();assert(!fs.existsSync(path.join(root,'calls')));
 await page.getByText('Decline',{exact:true}).click();await wait(async()=>!await page.getByText('Allow this call',{exact:true}).count());assert(!fs.existsSync(path.join(root,'calls')));
 await page.getByText('Close app',{exact:true}).click();await page.getByText('Open app',{exact:true}).click();
 await wait(async()=>{for(const frame of page.frames()){if(await frame.locator('#refresh').count()){app=frame;return await frame.locator('body').getAttribute('data-ready')==='yes';}}return false;});
 await app.locator('#refresh').click();await page.getByText('Allow this call',{exact:true}).click();await app.locator('#count').filter({hasText:'1'}).waitFor();assert.equal(fs.readFileSync(path.join(root,'calls'),'utf8').trim().split('\n').length,1);
 const boundaries=await app.evaluate(async()=>{let parentBlocked=false,fetchBlocked=false;try{void parent.document.body;}catch{parentBlocked=true;}try{await fetch('/api/secrets');}catch{fetchBlocked=true;}return {parentBlocked,fetchBlocked};});
 assert.deepEqual(boundaries,{parentBlocked:true,fetchBlocked:true});
 await app.evaluate(url=>{const image=new Image();image.src=url;document.body.append(image);const link=document.createElement('a');link.href=url;link.id='exfil-link';link.textContent='owned navigation test';document.body.append(link);},llmAddress+'/exfil?value=owned-test');
 await app.locator('#exfil-link').click();await new Promise(r=>setTimeout(r,200));assert.equal(exfiltrations,0,'CSP must block self-navigation and image egress');
 await page.getByText('Close app',{exact:true}).click();await wait(()=>page.frames().length===1);
 // Frozen diffs remain stable after the live workspace changes, across viewport resize.
 fs.writeFileSync(path.join(workspace,'example.txt'),'new <script>window.INJECTED=true</script>\n');
 await page.evaluate(async owner=>{const {reviewPanel}=await import('/reviews.js');document.querySelector('#view').replaceChildren(reviewPanel(owner,()=>{}));},owner);
 await page.getByLabel('Selected paths',{exact:true}).fill('example.txt');await page.getByText('Capture selected changes',{exact:true}).click();await page.getByRole('button',{name:'example.txt',exact:true}).click();await page.locator('.diff-pair').first().waitFor();
 assert.equal(await page.evaluate(()=>window.INJECTED),undefined);assert.equal(await page.locator('.diff-split').isVisible(),true);
 const patch=await page.locator('.diff-unified').textContent();assert(patch.includes('new <script>'));
 fs.writeFileSync(path.join(workspace,'example.txt'),'newer unsaved text\n');await page.setViewportSize({width:480,height:900});assert.equal(await page.locator('.diff-unified').isVisible(),true);assert.equal(await page.locator('.diff-split').isVisible(),false);
 assert.equal(await page.locator('.diff-unified').textContent(),patch);
 // Operator resource reads carry the content receipt and reject same-version replacements.
 const resources=await api('/api/skills/fixture-skill/resources');const resource=await api('/api/skills/fixture-skill/resource',{identity:resources.identity,path:'references/data.txt',offset:0});assert.equal(resource.text,'SCOPED_RESOURCE_CONTENT');
 fs.writeFileSync(path.join(home,'skills','fixture-skill','references','data.txt'),'changed under same version');
 const stale=await fetch(base+'/api/skills/fixture-skill/resource',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({identity:resources.identity,path:'references/data.txt',offset:0})});assert(!stale.ok);
 // The response includes revisions and names beyond the bounded stored catalog.
 for(let n=0;n<8;n++)await api('/api/presets',{action:'save',name:`near${n}`,preset:{prompt:'x'.repeat(32700),model:null,effort:null},expected:null});
 const catalog=JSON.parse(execFileSync(binary,['presets'],{env,encoding:'utf8',stdio:['ignore','pipe','pipe']}));
 assert.equal(catalog.items.length,9);assert(Buffer.byteLength(JSON.stringify(catalog))>262144,'response must cross the storage cap to exercise transport overhead');
 passed=true;console.log(JSON.stringify({passed:true,checks:['preset-save-load-restore','app-real-initialization','no-call-before-consent','decline','one-shot-call','parent-and-fetch-isolation','image-and-navigation-egress-blocked','saved-diff-resize','literal-html-diff','skill-resource-stale-receipt','preset-near-capacity-cli-daemon'],modelRequests:requests.length}));
}finally{
 if(browser)await browser.close();daemon.kill('SIGTERM');await Promise.race([new Promise(resolve=>daemon.once('exit',resolve)),new Promise(resolve=>setTimeout(resolve,3000))]);if(daemon.exitCode===null)daemon.kill('SIGKILL');llm.close();
 if(passed)fs.rmSync(root,{recursive:true,force:true});else console.error(`Owned probe artifacts: ${root}`);
}
