import {writeFileSync} from 'node:fs';
const [base, marker, output] = process.argv.slice(2);
const status = {session: null, failed: null, stopped: null, tool_done: 0, frames: 0};
const socket = new WebSocket(base.replace(/^http/, 'ws') + '/api/chat');
let finished = false;
const finish = error => {
  if (finished) return;
  finished = true; clearTimeout(timer); status.fixture_error = error;
  writeFileSync(output, JSON.stringify(status)); socket.close();
  setTimeout(() => process.exit(error ? 1 : 0), 20);
};
const timer = setTimeout(() => finish('owned chat timeout'), 45_000);
socket.addEventListener('open', () => socket.send(JSON.stringify({type:'prompt', session:null, text:marker, id:marker, options:marker.endsWith('FINAL') ? {output:'final.txt'} : {}})));
socket.addEventListener('error', () => finish('owned websocket failed'));
socket.addEventListener('message', ({data}) => {
  if (++status.frames > 256 || data.length > 1024*1024) return finish('owned frame limit');
  const event = JSON.parse(data);
  if (event.type === 'started') status.session = event.session;
  if (event.type === 'tool_done') status.tool_done++;
  if (event.type === 'failed') {status.failed = String(event.message).slice(0,2000); finish(null);}
  if (event.type === 'done') {status.stopped = event.stopped; finish(null);}
  if (['approval','ask','secret_request','error'].includes(event.type)) finish('unexpected input/error: '+event.type);
});
