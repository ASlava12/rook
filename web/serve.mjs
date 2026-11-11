// Standalone UI host. Rust builds and rookd do not need Node or these assets.
import http from 'node:http';
import https from 'node:https';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const ASSETS = fileURLToPath(new URL('./dist/', import.meta.url));
const MIME = {'.html':'text/html; charset=utf-8','.js':'text/javascript; charset=utf-8','.css':'text/css; charset=utf-8','.svg':'image/svg+xml','.png':'image/png','.ico':'image/x-icon','.json':'application/json','.woff2':'font/woff2'};
const MAX_ASSET_BYTES = 8 * 1024 * 1024;
const fail = (response,status,message) => { if(response.headersSent){response.destroy();return;} response.writeHead(status,{'content-type':'application/json','cache-control':'no-store'});response.end(JSON.stringify({error:message})); };

export async function startWeb({ daemon = 'http://127.0.0.1:7717', port = 3000, bind = '127.0.0.1', assets = ASSETS } = {}) {
  const target = new URL(daemon);
  if (!['http:','https:'].includes(target.protocol)||target.username||target.password||target.search||target.hash) throw new Error('Daemon must be an HTTP(S) base URL without credentials, query or fragment');
  if(!/^(?:localhost|127(?:\.\d{1,3}){3}|\[::1\])$/.test(target.hostname))throw new Error('The local UI connects to a loopback daemon. Forward a remote daemon through SSH, or deploy the static UI behind your authenticated reverse proxy.');
  if (!['127.0.0.1','::1','localhost'].includes(bind)) throw new Error('The local UI binds to loopback. Use an authenticated reverse proxy for network access.');
  if (!Number.isInteger(port)||port<0||port>65535) throw new Error('Port must be 0..65535');
  const base = await fs.promises.realpath(assets);
  const transport = target.protocol === 'https:' ? https : http;
  const agent = new transport.Agent({keepAlive:true,maxSockets:16,maxFreeSockets:4});
  const upgraded = new Set();
  const allowed = request => {
    const localPort=server.address()?.port;
    const authorities=[`127.0.0.1:${localPort}`,`localhost:${localPort}`,`[::1]:${localPort}`];
    if(!authorities.includes(request.headers.host))return false;
    const origin=request.headers.origin;
    return !origin||authorities.some(authority=>origin===`http://${authority}`);
  };
  const proxyOptions = request => {
    // Preserve the already-validated UI authority. OAuth callbacks use this
    // same public origin; changing it to the daemon port breaks sign-in.
    const headers={...request.headers};
    delete headers['proxy-authorization'];delete headers['proxy-connection'];
    return {protocol:target.protocol,hostname:target.hostname.replace(/^\[|\]$/g,''),port:target.port||undefined,
      path:`${target.pathname.replace(/\/$/,'')}${request.url}`,method:request.method,headers,agent};
  };
  const server=http.createServer({maxHeaderSize:16384},async(request,response)=>{
    if(!allowed(request)){fail(response,403,'Untrusted UI Host or Origin');return;}
    if(request.url==='/web-runtime.json'&&['GET','HEAD'].includes(request.method)){
      response.writeHead(200,{'content-type':'application/json','cache-control':'no-store'});
      response.end(request.method==='HEAD'?undefined:JSON.stringify({daemon:target.href.replace(/\/$/,'')}));return;
    }
    if(request.url?.startsWith('/api/')||request.url==='/api'){
      const upstream=transport.request(proxyOptions(request),reply=>{
        clearTimeout(deadline);response.writeHead(reply.statusCode,reply.headers);reply.pipe(response);
        reply.on('error',()=>response.destroy());
      });
      const deadline=setTimeout(()=>{upstream.destroy();fail(response,504,'Daemon did not respond. A write may already be accepted; inspect its receipt before retrying.');},30000);
      upstream.on('error',()=>{clearTimeout(deadline);fail(response,502,'Cannot reach Rook daemon. Start rookd or check --daemon.');});
      request.on('aborted',()=>upstream.destroy());response.on('close',()=>upstream.destroy());
      request.pipe(upstream);return;
    }
    if(!['GET','HEAD'].includes(request.method)){fail(response,405,'Static UI supports GET and HEAD');return;}
    let pathname;
    try {pathname=decodeURIComponent(new URL(request.url,'http://localhost').pathname);}catch{fail(response,400,'Invalid asset path');return;}
    if(pathname.length>2048||pathname.includes('\\')||pathname.includes('\0')||pathname.split('/').includes('..')){fail(response,400,'Invalid asset path');return;}
    let name=pathname==='/'?'index.html':pathname.slice(1);
    try {
      let filename;
      try {filename=await fs.promises.realpath(path.join(base,name));}
      catch(error){if(error.code!=='ENOENT'||path.extname(name))throw error;name='index.html';filename=await fs.promises.realpath(path.join(base,name));}
      const relative=path.relative(base,filename);
      if(relative.startsWith('..')||path.isAbsolute(relative)){fail(response,403,'Asset is outside the UI directory');return;}
      const meta=await fs.promises.stat(filename);
      if(!meta.isFile()||meta.size>MAX_ASSET_BYTES){fail(response,413,'Asset exceeds the static-file limit');return;}
      response.writeHead(200,{'content-type':MIME[path.extname(filename)]||'application/octet-stream','content-length':meta.size,
        'cache-control':name==='mcp-oauth-callback.html'?'no-store':'no-cache','x-content-type-options':'nosniff','referrer-policy':'no-referrer'});
      if(request.method==='HEAD'){response.end();return;}
      const stream=fs.createReadStream(filename);stream.on('error',()=>response.destroy());response.on('close',()=>stream.destroy());stream.pipe(response);
    } catch(error) {fail(response,error.code==='ENOENT'?404:500,error.code==='ENOENT'?'No such UI asset':'Could not read UI asset');}
  });
  server.maxConnections=128;server.headersTimeout=15000;server.requestTimeout=30000;
  server.on('upgrade',(request,socket,head)=>{
    if(!allowed(request)||!request.url?.startsWith('/api/')||request.headers.upgrade?.toLowerCase()!=='websocket'){
      socket.end('HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n');return;
    }
    upgraded.add(socket);socket.once('close',()=>upgraded.delete(socket));
    const upstream=transport.request({...proxyOptions(request),agent:undefined});
    const timeout=setTimeout(()=>{upstream.destroy();socket.destroy();},15000);
    upstream.on('upgrade',(reply,connection,received)=>{
      clearTimeout(timeout);
      if(socket.destroyed){connection.destroy();return;}
      socket.write(`HTTP/1.1 101 Switching Protocols\r\n${Object.entries(reply.headers).map(([name,value])=>`${name}: ${value}\r\n`).join('')}\r\n`);
      if(head.length)connection.write(head);if(received.length)socket.write(received);
      socket.pipe(connection);connection.pipe(socket);
      socket.on('error',()=>connection.destroy());connection.on('error',()=>socket.destroy());
      socket.on('close',()=>connection.destroy());connection.on('close',()=>socket.destroy());
    });
    upstream.on('response',reply=>{clearTimeout(timeout);socket.end(`HTTP/1.1 ${reply.statusCode} Daemon refused upgrade\r\nConnection: close\r\n\r\n`);reply.resume();});
    upstream.on('error',()=>{clearTimeout(timeout);socket.destroy();});socket.on('close',()=>upstream.destroy());upstream.end();
  });
  const closeHttpConnections=server.closeAllConnections.bind(server);
  server.closeAllConnections=()=>{for(const socket of upgraded)socket.destroy();closeHttpConnections();};
  server.on('close',()=>agent.destroy());
  await new Promise((resolve,reject)=>{server.once('error',reject);server.listen(port,bind,()=>{server.removeListener('error',reject);resolve();});});
  return server;
}

if(process.argv[1]&&import.meta.url===pathToFileURL(path.resolve(process.argv[1])).href){
  const args=process.argv.slice(2),options={};
  for(let at=0;at<args.length;at++){
    if(args[at]==='--help'){console.log('node web/serve.mjs [--daemon http://127.0.0.1:7717] [--port 3000] [--bind 127.0.0.1]');process.exit(0);}
    const key={'--daemon':'daemon','--port':'port','--bind':'bind'}[args[at]];
    if(!key||!args[at+1])throw new Error(`Unknown or incomplete option: ${args[at]}`);
    options[key]=key==='port'?Number(args[++at]):args[++at];
  }
  const server=await startWeb(options);
  console.log(`Rook web: http://${options.bind==='::1'?'[::1]':options.bind||'127.0.0.1'}:${server.address().port}`);
  console.log(`Daemon:   ${options.daemon||'http://127.0.0.1:7717'}`);
  for(const signal of ['SIGINT','SIGTERM'])process.on(signal,()=>{server.close();server.closeAllConnections();});
}
