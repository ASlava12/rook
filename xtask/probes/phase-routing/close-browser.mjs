import fs from 'node:fs';
import path from 'node:path';
const root=fs.readFileSync('target/phase-live-browser-root.txt','utf8').trim();
const port=fs.readFileSync(path.join(root,'edge/DevToolsActivePort'),'utf8').split('\n')[0];
const version=await(await fetch(`http://127.0.0.1:${port}/json/version`)).json();
const socket=new WebSocket(version.webSocketDebuggerUrl);
await new Promise((resolve,reject)=>{socket.addEventListener('open',resolve,{once:true});socket.addEventListener('error',reject,{once:true});});
await new Promise((resolve,reject)=>{const timer=setTimeout(()=>reject(new Error('Browser did not close')),30000);socket.addEventListener('close',()=>{clearTimeout(timer);resolve();},{once:true});socket.send(JSON.stringify({id:1,method:'Browser.close'}));});
