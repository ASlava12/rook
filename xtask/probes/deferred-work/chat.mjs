// Keep one ordinary chat connection through deferred owner replacement.
import { writeFileSync, renameSync } from 'node:fs';

const [base, session, output, mode = 'observe'] = process.argv.slice(2);
const status = { attached: false, started: 0, deferred: 0, queued: 0, requested: false, error: null };
const save = () => { writeFileSync(`${output}.tmp`, JSON.stringify(status)); renameSync(`${output}.tmp`, output); };
const socket = new WebSocket(`${base.replace(/^http/, 'ws')}/api/chat?live_snapshots=true`);
let count = 0, finished = false;
const finish = error => {
  if (finished) return;
  finished = true; clearTimeout(deadline); status.error = error; save(); socket.close();
  setTimeout(() => process.exit(error ? 1 : 0), 20);
};
const deadline = setTimeout(() => finish('chat did not follow the next deferred owner'), 20_000);
socket.addEventListener('open', () => socket.send(JSON.stringify({ type: 'attach', session })));
socket.addEventListener('error', () => finish('owned chat websocket failed'));
socket.addEventListener('message', ({ data }) => {
  if (++count > 512) return finish('owned chat frame limit exceeded');
  const event = JSON.parse(data);
  if (event.type === 'attached') status.attached = true;
  if (event.type === 'started') status.started++;
  if (event.type === 'error' || event.type === 'failed') return finish(event.message || 'chat rejected continuation');
  if (event.type === 'done' && event.stopped === 'work_queued') status.queued++;
  if (event.type === 'done' && event.stopped === 'work_deferred') {
    status.deferred++;
    if (mode === 'continue' && !status.requested) {
      status.requested = true;
      socket.send(JSON.stringify({ type: 'prompt', session, text: '/continue', id: 'native-early-resume', options: {} }));
    }
  }
  save();
  if (status.deferred >= 2) finish(null);
});
