import { writeFileSync } from 'node:fs';
const [base, marker, output] = process.argv.slice(2);
const report = {started:false, done:false, stopped:null, frames:0};
let finished = false;
const socket = new WebSocket(base.replace(/^http/, 'ws') + '/api/chat');
const finish = error => {
  if (finished) return;
  finished = true; clearTimeout(timer); report.error = error ? String(error).slice(0,1000) : null;
  writeFileSync(output, JSON.stringify(report)); socket.close();
  setTimeout(() => process.exit(error ? 1 : 0),20);
};
const timer = setTimeout(() => finish('owned chat timeout'),90000);
socket.addEventListener('open',() => socket.send(JSON.stringify({type:'prompt', text:marker, id:marker, options:{}})));
socket.addEventListener('error',() => finish('owned WebSocket error'));
socket.addEventListener('message',({data}) => {
  if (++report.frames > 512 || data.length > 1024*1024) return finish('owned frame limit');
  try {
    const event = JSON.parse(data);
    if (event.type === 'started') report.started = true;
    if (['error','failed','approval','ask','secret_request'].includes(event.type)) return finish(event.message || event.type);
    if (event.type === 'done') { report.done = true; report.stopped = event.stopped; finish(event.stopped === 'end_turn' ? null : event.stopped); }
  } catch (error) { finish(error); }
});
