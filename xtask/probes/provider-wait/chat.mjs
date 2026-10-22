import { writeFile } from 'node:fs/promises';
const [base, session, output] = process.argv.slice(2);
const report = { session, frames: 0, bytes: 0, events: [], error: null };
const start = performance.now();
let finished = false;
const socket = new WebSocket(`${base.replace(/^http/, 'ws')}/api/chat?live_snapshots=true`);
const deadline = setTimeout(() => finish('owned observation deadline'), 115000);
async function finish(error) {
  if (finished) return;
  finished = true;
  clearTimeout(deadline);
  report.error = error;
  socket.close();
  try {
    await writeFile(output, JSON.stringify(report, null, 2));
    setTimeout(() => process.exit(error ? 1 : 0), 30);
  } catch (failure) { console.error(failure); process.exit(1); }
}
socket.addEventListener('open', () => socket.send(JSON.stringify({ type: 'attach', session })));
socket.addEventListener('error', () => finish('owned websocket error'));
socket.addEventListener('message', ({data}) => {
  if (finished) return;
  report.frames++;
  report.bytes += Buffer.byteLength(data);
  if (report.frames > 512 || report.bytes > 1024 * 1024) return finish('capture bound exceeded');
  const event = JSON.parse(data);
  report.events.push({elapsed: (performance.now() - start) / 1000,
    epoch: Date.now() / 1000, type: event.type, state: event.state,
    stopped: event.stopped, message: event.message, said: event.said});
  if (event.type === 'attached') socket.send(JSON.stringify({type: 'prompt', session,
    text: 'Return OWNED_PROVIDER_WAIT_OK without using any tools.',
    id: 'owned-provider-wait', options: {}}));
  if (event.type === 'failed' || event.type === 'error') return finish(event.message || event.type);
  if (event.type === 'done') finish(null);
});
