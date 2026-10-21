// Keep one ordinary chat connection through deferred owner replacement.
import { writeFile, rename } from 'node:fs/promises';
import { setTimeout as delay } from 'node:timers/promises';

const [base, session, output, mode = 'observe'] = process.argv.slice(2);
const status = { attached: false, started: 0, deferred: 0, queued: 0, requested: false, error: null, events: [] };
let pending = null, publishing = null;
async function publish(snapshot) {
  await writeFile(`${output}.tmp`, snapshot);
  for (let attempt = 0; ; attempt++) {
    try { await rename(`${output}.tmp`, output); return; }
    catch (error) {
      // A Python reader briefly holds the destination without delete sharing on
      // Windows. Keep snapshots atomic and retry only that bounded contention.
      if (process.platform !== 'win32' || !['EPERM', 'EACCES'].includes(error.code) || attempt >= 40) throw error;
      await delay(5);
    }
  }
}
const save = () => {
  pending = JSON.stringify(status);
  if (!publishing) publishing = (async () => {
    while (pending !== null) {
      const snapshot = pending; pending = null;
      await publish(snapshot);
    }
  })().finally(() => { publishing = null; });
  return publishing;
};
const socket = new WebSocket(`${base.replace(/^http/, 'ws')}/api/chat?live_snapshots=true`);
let count = 0, finished = false;
const finish = error => {
  if (finished) return;
  finished = true; clearTimeout(deadline); status.error = error; socket.close();
  save().then(() => setTimeout(() => process.exit(error ? 1 : 0), 20), failure => {
    console.error(failure); process.exit(1);
  });
};
const deadline = setTimeout(() => finish('chat did not follow the next deferred owner'), 20_000);
socket.addEventListener('open', () => socket.send(JSON.stringify({ type: 'attach', session })));
socket.addEventListener('error', () => finish('owned chat websocket failed'));
socket.addEventListener('message', ({ data }) => {
  if (++count > 512) return finish('owned chat frame limit exceeded');
  const event = JSON.parse(data);
  if (status.events.length < 64) status.events.push({ type: event.type, stopped: event.stopped });
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
  save().catch(error => finish(String(error)));
  if (status.deferred >= 2) finish(null);
});
