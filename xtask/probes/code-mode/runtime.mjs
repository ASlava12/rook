// Independent feasibility probe. This is not wired into Rook tool dispatch.
import { readFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';

export const LIMITS = Object.freeze({
  sourceBytes: 32768, memoryBytes: 16 * 1024 * 1024, stackBytes: 256 * 1024,
  calls: 64, pending: 8, argumentUnits: 16384, argumentBytes: 65536,
  resultBytes: 65536, outputUnits: 16384, outputBytes: 65536, outputItems: 64,
  milliseconds: 2000,
});

export async function dependency(directory) {
  const { QuickJS } = await import(pathToFileURL(`${directory}/dist/index.js`));
  const wasm = await WebAssembly.compile(await readFile(`${directory}/quickjs.wasm`));
  return { QuickJS, wasm };
}

function discardDiagnostics(memory) {
  return { fd_write(_fd, iovs, count, written) {
    const view = new DataView(memory.buffer);
    let size = 0;
    for (let at = 0; at < count; at++) size += view.getUint32(iovs + at * 8 + 4, true);
    view.setUint32(written, size, true);
    return 0;
  } };
}

// The VM receives primitives, JSON, and these two closed-over host functions.
// No filesystem/network extensions, module loader, Node globals or credentials.
const PRELUDE = `(function(call, emit, names) {
  const stringify = JSON.stringify.bind(JSON), parse = JSON.parse.bind(JSON);
  const then = Promise.prototype.then;
  const tools = Object.create(null);
  for (const name of parse(names)) {
    tools[name] = Object.freeze((args) =>
      then.call(call(name, stringify(args ?? {})), (json) => parse(json)));
  }
  const text = Object.freeze((value) => emit(typeof value === 'string' ? value : stringify(value)));
  const logging = Object.freeze((...values) => text(values.map(value =>
    typeof value === 'string' ? value : stringify(value)).join(' ')));
  const console = Object.freeze({ log: logging, info: logging, warn: logging, error: logging, debug: logging });
  Object.freeze(tools);
  return Object.freeze({ tools, text, console, run: async (fn) => {
    const returned = await fn(tools, text, console);
    if (returned !== undefined) text(returned);
  } });
})`;

export async function execute(engine, source, invoke, options = {}) {
  const limits = { ...LIMITS, ...options.limits };
  const names = options.names ?? ['read_file', 'list_dir'];
  const cancel = options.cancel ? new Int32Array(options.cancel) : null;
  const metrics = { calls: 0, pendingPeak: 0, copiedArgumentBytes: 0, copiedOutputBytes: 0,
    refusedArgumentUnits: 0, refusedOutputUnits: 0, interrupts: 0 };
  const output = [];
  if (Buffer.byteLength(source) > limits.sourceBytes) {
    return { ok: false, error: 'source limit reached', output, metrics };
  }
  const until = performance.now() + limits.milliseconds;
  let fatal = '', pending = 0, settled = false;
  const jobs = new Set();
  const abort = new AbortController();
  const fail = (message) => { fatal ||= message; throw new RangeError(message); };
  const stopped = () => {
    if (fatal || (cancel && Atomics.load(cancel, 0)) || performance.now() >= until) {
      fatal ||= cancel && Atomics.load(cancel, 0) ? 'cancelled' : 'deadline reached';
      metrics.interrupts++;
      return true;
    }
    return false;
  };
  const guard = () => { if (stopped()) throw new Error(fatal); };
  const vm = await engine.QuickJS.create({ wasm: engine.wasm, memoryLimit: limits.memoryBytes,
    maxStackSize: limits.stackBytes, interruptHandler: stopped, wasi: discardDiagnostics });
  const primitive = (handle, maximumUnits, kind) => {
    guard();
    if (!handle?.isString) return fail(`${kind} must be a primitive string`);
    const units = handle.getProp('length').consume(value => value.toNumber());
    if (units > maximumUnits) {
      if (kind === 'arguments') metrics.refusedArgumentUnits = units;
      if (kind === 'output') metrics.refusedOutputUnits = units;
      return fail(`${kind} character limit reached`);
    }
    // At most 4 * maximumUnits bytes can be copied; arbitrary guest objects
    // never reach toString(), and large guest strings are rejected first.
    return handle.toString();
  };
  const call = vm.newFunction('call', (name, arguments_) => {
    guard();
    if (metrics.calls >= limits.calls) return fail('call limit reached');
    if (pending >= limits.pending) return fail('pending call limit reached');
    const label = primitive(name, 128, 'tool name');
    if (!names.includes(label)) return fail('tool was not injected');
    const json = primitive(arguments_, limits.argumentUnits, 'arguments');
    const bytes = Buffer.byteLength(json);
    if (bytes > limits.argumentBytes) return fail('argument byte limit reached');
    metrics.copiedArgumentBytes += bytes;
    metrics.calls++;
    pending++;
    metrics.pendingPeak = Math.max(metrics.pendingPeak, pending);
    const deferred = vm.newPromise();
    const work = Promise.resolve().then(() => {
      guard();
      return invoke(label, JSON.parse(json), { signal: abort.signal });
    }).then(
      result => {
        if (settled) return;
        guard();
        if (typeof result !== 'string' || Buffer.byteLength(result) > limits.resultBytes) {
          return fail('host result limit reached');
        }
        vm.withScope(() => deferred.resolve(vm.newString(result)));
      },
      error => {
        if (settled) return;
        // Host errors are bounded data; they cannot introduce new capabilities.
        vm.withScope(() => deferred.reject(vm.newError(String(error).slice(0, 1024))));
      },
    ).catch(error => { fatal ||= String(error).slice(0, 1024); }).finally(() => {
      jobs.delete(work);
      pending--;
    });
    jobs.add(work);
    return deferred.handle;
  });
  const emit = vm.newFunction('emit', value => {
    guard();
    if (output.length >= limits.outputItems) return fail('output item limit reached');
    const text = primitive(value, limits.outputUnits, 'output');
    const bytes = Buffer.byteLength(text);
    if (metrics.copiedOutputBytes + bytes > limits.outputBytes) return fail('output byte limit reached');
    metrics.copiedOutputBytes += bytes;
    output.push(text);
    return vm.undefined;
  });
  let api, program, running, resolution;
  try {
    vm.withScope(scope => {
      const factory = vm.evalCode(PRELUDE, 'rook-code-mode-prelude.js');
      api = scope.escape(vm.callFunction(factory, vm.undefined, call, emit, vm.newString(JSON.stringify(names))));
    });
    program = vm.evalCode(`(async function(tools, text, console) { 'use strict';\n${source}\n})`, 'rook-code-mode.js');
    // Calling guest code can create asynchronous host promises. A temporary
    // handle scope here would also free their unresolved resolver handles.
    const run = api.getProp('run');
    try {
      running = vm.callFunction(run, api, program);
    } finally {
      run.dispose();
    }
    let done;
    const resolving = vm.resolvePromise(running).then(value => { done = value; });
    while (!done) {
      guard();
      vm.executePendingJobs();
      await Promise.resolve();
      if (done) break;
      if (jobs.size === 0) {
        // A host result may have settled after the first drain. Run its VM
        // reactions before deciding that no tool can wake the script.
        vm.executePendingJobs();
        await Promise.resolve();
        if (done) break;
        if (jobs.size) continue;
        return { ok: false, error: 'unsettleable promise', output, metrics };
      }
      // A host tool may be pending. Do not silently raise the script deadline.
      await Promise.race([...jobs, new Promise(resolve => setTimeout(resolve, 5))]);
    }
    await resolving;
    resolution = done;
    guard();
    // Return values are serialized inside the VM through the same bounded
    // sink as text/console; arbitrary objects never reach a host dumper.
    let error = null;
    if (done.error) {
      error = 'guest rejected';
      // Do not stringify arbitrary rejection values or invoke their getters.
      if (done.error.isError) {
        const message = done.error.getOwnPropertyDescriptor('message');
        try {
          if (message?.value?.isString) {
            const units = message.value.getProp('length').consume(value => value.toNumber());
            if (units <= 1024) error += `: ${message.value.toString()}`;
          }
        } finally {
          message?.value?.dispose(); message?.get?.dispose(); message?.set?.dispose();
        }
      }
    }
    return { ok: !done.error, error, output, metrics };
  } catch (error) {
    return { ok: false, error: fatal || String(error).slice(0, 1024), output, metrics };
  } finally {
    settled = true;
    abort.abort();
    resolution?.value?.dispose();
    resolution?.error?.dispose();
    running?.dispose(); program?.dispose(); api?.dispose();
    call.dispose(); emit.dispose();
    vm.dispose();
  }
}
