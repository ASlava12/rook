import { parentPort, workerData } from 'node:worker_threads';
import { dependency, execute } from './runtime.mjs';

const engine = await dependency(workerData.directory);
parentPort.postMessage({ ready: true });
const calls = [];
const result = await execute(engine, workerData.source, async (name, args) => {
  calls.push({ name, args });
  if (workerData.delay) await new Promise(resolve => setTimeout(resolve, workerData.delay));
  if (args.fail) throw new Error('fixture tool refused');
  return JSON.stringify({ content: args.path ?? 'fixture-file.txt', is_error: false });
}, workerData.options);
parentPort.postMessage({ result, calls });
