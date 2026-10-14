import assert from 'node:assert/strict';
import { test } from 'node:test';
import { Worker } from 'node:worker_threads';
import { dependency, execute, LIMITS } from './runtime.mjs';

const directory = process.env.ROOK_QUICKJS_DIRECTORY;
assert.ok(directory, 'set ROOK_QUICKJS_DIRECTORY to the pinned quickjs-wasi@3.6.2 package');
const engine = await dependency(directory);
const fixture = async (_name, args) => JSON.stringify({ content: args.path ?? 'fixture-file.txt', is_error: false });

test('dependent and parallel calls return only selected output', async () => {
  const result = await execute(engine, `
    const first = await tools.list_dir({});
    const rows = await Promise.all([tools.read_file({path:first.content}),tools.read_file({path:'other.txt'})]);
    text(rows.map(row => row.content).join(','));
  `, fixture);
  assert.equal(result.ok, true, result.error);
  assert.deepEqual(result.output, ['fixture-file.txt,other.txt']);
  assert.equal(result.metrics.calls, 3);
  assert.equal(result.metrics.pendingPeak, 2);
});

test('return and console use the same bounded selected-output sink', async () => {
  const result = await execute(engine, `console.log('selected', {number:3}); return {answer:7};`, fixture);
  assert.equal(result.ok, true, result.error);
  assert.deepEqual(result.output, ['selected {"number":3}', '{"answer":7}']);
  assert.equal(result.metrics.copiedOutputBytes, Buffer.byteLength(result.output.join('')));
});

test('oversized return and console values are refused before host copying', async () => {
  for (const source of [`return 'x'.repeat(20000);`, `console.log('x'.repeat(20000));`]) {
    const result = await execute(engine, source, fixture);
    assert.equal(result.ok, false);
    assert.match(result.error, /output character limit/);
    assert.equal(result.metrics.refusedOutputUnits, 20000);
    assert.equal(result.metrics.copiedOutputBytes, 0);
  }
});

test('Node/network/filesystem/module globals are absent in the guest', async () => {
  const result = await execute(engine, `text([typeof process,typeof require,typeof fetch,typeof WebAssembly,typeof __bridge,typeof global].join(','));`, fixture);
  assert.equal(result.ok, true, result.error);
  assert.deepEqual(result.output, ['undefined,undefined,undefined,undefined,undefined,undefined']);
});

test('guest Function constructors remain inside the VM', async () => {
  const result = await execute(engine, `text(tools.read_file.constructor('return typeof process')());`, fixture);
  assert.equal(result.ok, true, result.error);
  assert.deepEqual(result.output, ['undefined']);
});

test('source ceiling is reached before evaluation or host invocation', async () => {
  let invoked = 0;
  const result = await execute(engine, ' '.repeat(LIMITS.sourceBytes + 1), () => { invoked++; });
  assert.equal(result.error, 'source limit reached');
  assert.equal(invoked, 0);
});

test('argument ceiling is reached before a large VM string is copied', async () => {
  const result = await execute(engine, `await tools.read_file({path:'я'.repeat(20000)});`, fixture);
  assert.equal(result.ok, false);
  assert.match(result.error, /arguments character limit/);
  assert.ok(result.metrics.refusedArgumentUnits > LIMITS.argumentUnits);
  assert.equal(result.metrics.calls, 0);
  assert.equal(result.metrics.copiedArgumentBytes, 0);
});

test('output ceiling is reached before a large VM string is copied', async () => {
  const result = await execute(engine, `text('🚀'.repeat(20000));`, fixture);
  assert.equal(result.ok, false);
  assert.match(result.error, /output character limit/);
  assert.ok(result.metrics.refusedOutputUnits > LIMITS.outputUnits);
  assert.equal(result.metrics.copiedOutputBytes, 0);
});

test('aggregate UTF-8 output bytes and item count have separate reached bounds', async () => {
  const bytes = await execute(engine, `for(let i=0;i<5;i++)text('я'.repeat(10000));`, fixture);
  assert.equal(bytes.ok, false);
  assert.equal(bytes.output.length, 3);
  assert.equal(bytes.metrics.copiedOutputBytes, 60000);
  assert.match(bytes.error, /output byte limit/);
  const items = await execute(engine, `for(let i=0;i<65;i++)text('');`, fixture);
  assert.equal(items.ok, false);
  assert.equal(items.output.length, LIMITS.outputItems);
  assert.match(items.error, /output item limit/);
});

test('call ceiling cannot be caught and used to start further effects', async () => {
  let invoked = 0;
  const result = await execute(engine, `
    for(let i=0;i<65;i++){try{await tools.read_file({path:'x'})}catch{}}
    await tools.read_file({path:'must not run'});
  `, async (...args) => { invoked++; return fixture(...args); });
  assert.equal(result.ok, false);
  assert.match(result.error, /call limit/);
  assert.equal(result.metrics.calls, LIMITS.calls);
  assert.equal(invoked, LIMITS.calls);
});

test('in-flight bound is reached before starting an additional host tool', async () => {
  let invoked = 0;
  const result = await execute(engine, `await Promise.all(Array.from({length:9},()=>tools.read_file({})));`, async (...args) => {
    invoked++; await new Promise(resolve => setTimeout(resolve, 10)); return fixture(...args);
  });
  assert.equal(result.ok, false);
  assert.match(result.error, /pending call limit/);
  assert.equal(result.metrics.calls, LIMITS.pending);
  assert.equal(result.metrics.pendingPeak, LIMITS.pending);
  // The fatal condition may cancel admitted work before invocation.
  assert.ok(invoked <= LIMITS.pending);
});

test('host errors reject a guest promise as data', async () => {
  const result = await execute(engine, `try{await tools.read_file({})}catch{text('refused')}`, async () => { throw new Error('fixture refused'); });
  assert.equal(result.ok, true, result.error);
  assert.deepEqual(result.output, ['refused']);
});

test('arbitrary rejection objects are not stringified or read through getters', async () => {
  const result = await execute(engine, `
    throw new Proxy({}, { get(){text('getter ran');throw 'bad getter'} });
  `, fixture);
  assert.equal(result.ok, false);
  assert.equal(result.error, 'guest rejected');
  assert.deepEqual(result.output, []);
});

test('tool cancellation reaches a pending host invocation when the script expires', async () => {
  let started = false, cancelled = false;
  const result = await execute(engine, `await tools.read_file({});`, async (_name, _args, { signal }) => {
    started = true;
    await new Promise(resolve => signal.addEventListener('abort', () => {
      cancelled = true;
      resolve();
    }, { once: true }));
    return '{}';
  }, { limits: { milliseconds: 50 } });
  assert.equal(started, true);
  assert.equal(result.ok, false);
  assert.equal(result.error, 'deadline reached');
  assert.equal(cancelled, true);
});

test('dynamic module imports do not load host filesystem or Node modules', async () => {
  const result = await execute(engine, `await import('node:fs');`, fixture);
  assert.equal(result.ok, false);
  assert.equal(result.metrics.calls, 0);
});

test('too-large host results cannot enter the VM or become output', async () => {
  const result = await execute(engine, `text(await tools.read_file({}));`, async () => 'x'.repeat(LIMITS.resultBytes + 1));
  assert.equal(result.ok, false);
  assert.match(result.error, /host result limit/);
  assert.equal(result.metrics.calls, 1);
  assert.deepEqual(result.output, []);
});

test('a never-settling promise has no hidden tool or timer to wait for', async () => {
  const result = await execute(engine, `await new Promise(()=>{});`, fixture);
  assert.equal(result.ok, false);
  assert.equal(result.error, 'unsettleable promise');
});

test('the VM memory ceiling is exercised by an actual growing allocation', async () => {
  const result = await execute(engine, `const retained=[];for(let i=0;i<10000;i++)retained.push(new Array(10000).fill(i));`, fixture);
  assert.equal(result.ok, false);
  assert.match(result.error, /out of memory/);
  assert.equal(result.metrics.calls, 0);
});

test('stack overflow fails inside the VM rather than trapping the host', async () => {
  const result = await execute(engine, `function f(){return f()}f();`, fixture);
  assert.equal(result.ok, false);
  // Async functions report their uncaught error through promise rejection.
  assert.match(result.error, /guest rejected|stack/);
  assert.equal((await execute(engine, `text('still usable')`, fixture)).ok, true);
});

test('an infinite loop including catch stops at its actual deadline', async () => {
  const started = performance.now();
  const result = await execute(engine, `while(true){try{for(let i=0;i<10000;i++){} }catch{}}`, fixture, { limits: { milliseconds: 50 } });
  assert.equal(result.ok, false);
  assert.match(result.error, /deadline/);
  assert.ok(result.metrics.interrupts > 0);
  assert.ok(performance.now() - started < 2000);
});

test('cancellation from another thread interrupts guest CPU execution', { timeout: 10000 }, async () => {
  const cancel = new SharedArrayBuffer(4);
  const worker = new Worker(new URL('./runtime-worker.mjs', import.meta.url), { workerData: {
    directory, source: `while(true){}`, options: { cancel, limits: { milliseconds: 5000 } },
  } });
  try {
    const result = await new Promise((resolve, reject) => {
      worker.on('error', reject);
      worker.on('message', message => {
        if (message.ready) setTimeout(() => Atomics.store(new Int32Array(cancel), 0, 1), 50);
        if (message.result) resolve(message.result);
      });
      worker.on('exit', code => { if (code) reject(new Error(`worker exited ${code}`)); });
    });
    assert.equal(result.ok, false);
    assert.equal(result.error, 'cancelled');
    assert.ok(result.metrics.interrupts > 0);
  } finally {
    await worker.terminate();
  }
});
