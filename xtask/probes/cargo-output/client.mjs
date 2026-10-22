import {writeFileSync} from 'node:fs';
const [base, marker, output] = process.argv.slice(2);
const status = {session:null, done:false, stopped:null, frames:0, bytes:0, error:null};
const socket = new WebSocket(base.replace(/^http/, 'ws') + '/api/chat');
let finished = false;
const finish = error => {
  if (finished) return;
  finished = true;
  clearTimeout(timer);
  status.error = error;
  writeFileSync(output, JSON.stringify(status));
  socket.close();
  setTimeout(() => process.exit(error ? 1 : 0), 20);
};
const timer = setTimeout(() => finish('owned chat deadline'), 110_000);
socket.addEventListener('open', () => socket.send(JSON.stringify({type:'prompt', session:null, text:marker, id:marker, options:{}})));
socket.addEventListener('error', () => finish('owned websocket failed'));
socket.addEventListener('message', ({data}) => {
  status.frames++;
  status.bytes += Buffer.byteLength(data);
  if (status.frames > 512 || status.bytes > 1024*1024) return finish('owned frame/capture limit');
  let event;
  try { event = JSON.parse(data); } catch { return finish('invalid owned JSON frame'); }
  if (event.type === 'started') status.session = event.session;
  if (event.type === 'done') {status.done = true; status.stopped = event.stopped; finish(null);}
  if (['failed','approval','ask','secret_request','error'].includes(event.type)) finish('unexpected event: '+event.type);
});
