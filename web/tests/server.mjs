import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { startWeb } from '../serve.mjs';

test('the standalone UI proxies real API bytes, rejects foreign origins, and serves no asset outside its root',async t=>{
  const directory=fs.mkdtempSync(path.join(os.tmpdir(),'rook-web-server-'));
  fs.writeFileSync(path.join(directory,'index.html'),'<title>Rook standalone fixture</title>');
  fs.writeFileSync(path.join(directory,'app.js'),'export const ready = true;');
  fs.writeFileSync(path.join(directory,'mcp-oauth-callback.html'),'<title>callback</title>');
  fs.writeFileSync(path.join(directory,'large.js'),'');fs.truncateSync(path.join(directory,'large.js'),8*1024*1024+1);
  const calls=[];
  const daemon=http.createServer(async(req,res)=>{
    let body='';for await(const chunk of req)body+=chunk;
    calls.push({url:req.url,body,origin:req.headers.origin});
    res.writeHead(202,{'content-type':'application/json'});res.end(JSON.stringify({accepted:true,body}));
  });
  await new Promise(resolve=>daemon.listen(0,'127.0.0.1',resolve));
  const target=`http://127.0.0.1:${daemon.address().port}`;
  const web=await startWeb({daemon:target,port:0,assets:directory}),url=`http://127.0.0.1:${web.address().port}`;
  t.after(()=>{web.closeAllConnections();web.close();daemon.closeAllConnections();daemon.close();assert(directory.startsWith(path.resolve(os.tmpdir())+path.sep));fs.rmSync(directory,{recursive:true,force:true});});
  const index=await fetch(url);assert.match(await index.text(),/standalone fixture/);
  const code=await fetch(url+'/app.js');assert.match(code.headers.get('content-type'),/javascript/);
  assert.equal((await fetch(url+'/missing.js')).status,404);
  assert.equal((await fetch(url+'/sessions/example')).status,200);
  assert.equal((await fetch(url+'/large.js')).status,413,'the sparse asset exceeds the reached 8 MiB cap');
  assert.equal((await fetch(url+'/..%2Foutside.txt')).status,400);
  assert.equal((await fetch(url+'/mcp-oauth-callback.html')).headers.get('cache-control'),'no-store');
  const payload='{"text":"Привет 🦉"}';
  const response=await fetch(url+'/api/work',{method:'POST',headers:{origin:url,'content-type':'application/json'},body:payload});
  assert.equal(response.status,202);assert.equal((await response.json()).body,payload);assert.equal(calls[0].origin,url);
  const refused=await fetch(url+'/api/work',{method:'POST',headers:{origin:'https://foreign.example'},body:'must not reach daemon'});
  assert.equal(refused.status,403);assert.equal(calls.length,1);
  const untrustedHost=await new Promise((resolve,reject)=>{const req=http.get(url,{headers:{host:'attacker.example'}},res=>{res.resume();resolve(res.statusCode);});req.on('error',reject);});
  assert.equal(untrustedHost,403);
});

test('the UI can proxy a loopback IPv6 daemon without resolving brackets as a hostname',async t=>{
  const daemon=http.createServer((req,res)=>res.end('IPv6 daemon'));
  try{await new Promise((resolve,reject)=>{daemon.once('error',reject);daemon.listen(0,'::1',resolve);});}
  catch(error){if(['EADDRNOTAVAIL','EAFNOSUPPORT'].includes(error.code)){t.skip('IPv6 loopback unavailable');return;}throw error;}
  const web=await startWeb({daemon:`http://[::1]:${daemon.address().port}`,port:0});
  t.after(()=>{web.closeAllConnections();web.close();daemon.closeAllConnections();daemon.close();});
  const response=await fetch(`http://127.0.0.1:${web.address().port}/api/health`);
  assert.equal(response.status,200);assert.equal(await response.text(),'IPv6 daemon');
});
