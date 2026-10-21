import {writeFileSync} from 'node:fs';
const [base, workspace, marker, output] = process.argv.slice(2);
const status = {session:null, done:false, stopped:null, approvals:0, error:null, frames:0};
const socket = new WebSocket(base.replace(/^http/, 'ws') + '/api/chat?workspace=' + encodeURIComponent(workspace));
let finished = false;
const save = () => writeFileSync(output, JSON.stringify(status));
const finish = error => {
  if (finished) return;
  finished = true; clearTimeout(timer); status.error = error; save(); socket.close();
  setTimeout(() => process.exit(error ? 1 : 0), 20);
};
const timer = setTimeout(() => finish('owned chat deadline'), 110_000);
socket.addEventListener('open', () => socket.send(JSON.stringify({type:'prompt', session:null, text:marker, id:marker, options:{}})));
socket.addEventListener('error', () => finish('owned websocket failed'));
socket.addEventListener('message', ({data}) => {
  if (++status.frames > 256 || data.length > 1024*1024) return finish('owned frame limit');
  const event = JSON.parse(data);
  if (event.type === 'started') {status.session = event.session; save();}
  if (event.type === 'approval') {
    if (event.tool !== 'lsp install' || !String(event.action).includes('api.github.com') || ++status.approvals > 1)
      return finish('unexpected fixture approval');
    socket.send(JSON.stringify({type:'approval', id:event.id, decision:'once'}));
  }
  if (event.type === 'done') {status.done = true; status.stopped = event.stopped; finish(null);}
  if (['failed','ask','secret_request','error','tool_done'].includes(event.type)) finish('unexpected event: '+event.type);
});
