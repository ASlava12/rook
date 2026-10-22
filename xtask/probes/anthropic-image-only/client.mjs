import { writeFileSync } from 'node:fs';
const [base, session, marker, output] = process.argv.slice(2);
const result = { started: false, done: false, stopped: null, error: null, frames: 0 };
const socket = new WebSocket(base.replace(/^http/, 'ws') + '/api/chat');
let finished = false;
const finish = error => {
  if (finished) return;
  finished = true; clearTimeout(timer);
  result.error = error ? String(error).slice(0, 1000) : null;
  writeFileSync(output, JSON.stringify(result)); socket.close();
  setTimeout(() => process.exit(error ? 1 : 0), 20);
};
const timer = setTimeout(() => finish('owned chat deadline exceeded'), 90_000);
socket.addEventListener('open', () => socket.send(JSON.stringify({ type: 'prompt', session, text: marker, id: marker, options: {} })));
socket.addEventListener('error', () => finish('owned WebSocket failed'));
socket.addEventListener('message', ({ data }) => {
  if (++result.frames > 512 || data.length > 1024 * 1024) return finish('owned frame limit exceeded');
  const event = JSON.parse(data);
  if (event.type === 'started') result.started = true;
  if (['error', 'failed', 'approval', 'ask', 'secret_request'].includes(event.type)) return finish(event.message || event.type);
  if (event.type === 'done') {
    result.done = true; result.stopped = event.stopped;
    finish(event.stopped === 'end_turn' ? null : 'unexpected stop: ' + event.stopped);
  }
});
