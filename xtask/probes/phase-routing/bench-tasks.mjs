import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';

export const tasks = [
  {
    name: 'rename',
    seed: {
      'lib.mjs': 'export function fetchRows(source) { return Array.from(source); }\nexport function fetchRowsCached(source) { return fetchRows(source); }\n',
      'app.mjs': "import { fetchRows, fetchRowsCached } from './lib.mjs';\nexport function run() { return [...fetchRows([1, 2]), ...fetchRowsCached([3])]; }\n",
    },
    prompt: 'Rename fetchRows to loadRows in every definition, import and call site. fetchRowsCached is a different function and keeps its name and behavior. Preserve the public run function and its behavior. Change only lib.mjs and app.mjs.',
    checks: `assert.equal(typeof lib.loadRows, 'function'); assert.equal(lib.fetchRows, undefined);
      assert.deepEqual(lib.loadRows(new Set([1, 2])), [1, 2]);
      assert.deepEqual(lib.fetchRowsCached([3]), [3]); assert.deepEqual(app.run(), [1, 2, 3]);`,
  },
  {
    name: 'ports',
    seed: {
      'lib.mjs': "export function parsePort(line) { return Number(line.split(':')[1]); }\n",
      'app.mjs': "import { parsePort } from './lib.mjs';\nexport function parsePorts(lines) { return {ports: lines.map(parsePort), errors: []}; }\n",
    },
    prompt: 'Fix parsePort in lib.mjs: accept strings with exactly one colon, a nonempty host, and a decimal digit port in 1..65535. Allow surrounding whitespace on the host and port. Reject all other values with Error. Fix parsePorts in app.mjs: ignore blank strings, collect valid port numbers in ports, and collect {line: original zero-based index} in errors for each invalid nonblank line. Preserve order. Change only these two files.',
    checks: `for (const [text, port] of [['host:1',1], [' host : 8080 ',8080], ['x:65535',65535], ['x:0002',2]]) assert.equal(lib.parsePort(text), port);
      for (const text of ['host','host:',':80','host:0','x:65536','x:-1','x:1.5','x:1e2','x:80:90','x:NaN',null,42]) assert.throws(() => lib.parsePort(text));
      assert.deepEqual(app.parsePorts(['a:2',' ','bad','b:65535','x:0','','c:3']), {ports:[2,65535,3], errors:[{line:2},{line:4}]});`,
  },
  {
    name: 'money',
    seed: {
      'lib.mjs': 'export function toCents(text) { return Math.round(Number(text) * 100); }\n',
      'records.json': JSON.stringify({version:1,records:[{id:'a',amount:'12.34',label:'one'},{id:'b',amount:'-0.05',label:'two'},{id:'c',amount:'3',label:'three'}]}, null, 2)+'\n',
    },
    prompt: 'Fix toCents in lib.mjs: accept only strings matching an optional minus sign, one or more decimal digits, and an optional decimal point with one or two digits. Return exact integer cents; reject other inputs and amounts whose cents are not a safe integer with Error. Migrate records.json to version 2: replace amount with amount_cents using this conversion, preserving each id, label and record order. Change only these two files.',
    checks: `for (const [text, cents] of [['0',0], ['12.34',1234], ['-0.05',-5], ['3',300], ['1.2',120], ['90071992547409.91',Number.MAX_SAFE_INTEGER]]) assert.equal(lib.toCents(text), cents);
      for (const text of ['1.005','1e2',' 1','1.','+1','NaN','90071992547409.92',null,1]) assert.throws(() => lib.toCents(text));
      assert.deepEqual(records, {version:2,records:[{id:'a',amount_cents:1234,label:'one'},{id:'b',amount_cents:-5,label:'two'},{id:'c',amount_cents:300,label:'three'}]});`,
  },
];

export function seed(task, workspace) {
  fs.mkdirSync(workspace, { recursive: true });
  for (const [name, body] of Object.entries(task.seed)) fs.writeFileSync(path.join(workspace, name), body);
}

export function score(task, workspace) {
  try {
    for (const name of Object.keys(task.seed)) assert(fs.statSync(path.join(workspace, name)).size <= 64 * 1024, 'candidate file exceeds scorer admission');
    const url = name => JSON.stringify(pathToFileURL(path.join(workspace, name)).href);
    const imports = `import assert from 'node:assert/strict'; import fs from 'node:fs';
      const lib = await import(${url('lib.mjs')});
      ${task.name === 'money' ? `const records = JSON.parse(fs.readFileSync(${JSON.stringify(path.join(workspace, 'records.json'))}, 'utf8'));` : `const app = await import(${url('app.mjs')});`}`;
    const result = spawnSync(process.execPath, ['--input-type=module', '-e', `${imports}\n${task.checks}`], {
      cwd: workspace, stdio: ['ignore', 'pipe', 'pipe'], encoding: 'utf8', timeout: 30000, maxBuffer: 128 * 1024, windowsHide: true,
    });
    return { passed: result.status === 0 && !result.error, exit: result.status, signal: result.signal,
      reason: result.error?.code ?? (result.status === 0 ? null : result.stderr.slice(-2000)) };
  } catch (error) {
    return { passed: false, exit: null, reason: error.code ?? error.message };
  }
}
