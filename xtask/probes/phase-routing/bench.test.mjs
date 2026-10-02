import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import http from 'node:http';
import { fileURLToPath } from 'node:url';
import { tasks, seed, score } from './bench-tasks.mjs';
import { turn } from './bench-process.mjs';

function workspace(t, task) {
  const parent = path.resolve(os.tmpdir());
  const dir = fs.mkdtempSync(path.join(parent, 'rook-phase-oracle-'));
  assert.equal(path.dirname(path.resolve(dir)), parent);
  t.after(() => fs.rmSync(dir, { recursive: true }));
  seed(task, dir);
  return dir;
}
const solutions = {
  rename: {
    'lib.mjs': 'export const loadRows = source => [...source];\nexport const fetchRowsCached = source => loadRows(source);\n',
    'app.mjs': "import { loadRows, fetchRowsCached } from './lib.mjs';\nexport const run = () => [...loadRows([1,2]), ...fetchRowsCached([3])];\n",
  },
  ports: {
    'lib.mjs': `export function parsePort(line) {
      if (typeof line !== 'string') throw Error('invalid');
      const parts = line.split(':');
      if (parts.length !== 2 || !parts[0].trim() || !/^\\d+$/.test(parts[1].trim())) throw Error('invalid');
      const port = Number(parts[1].trim());
      if (!Number.isInteger(port) || port < 1 || port > 65535) throw Error('invalid');
      return port;
    }`,
    'app.mjs': `import { parsePort } from './lib.mjs';
      export function parsePorts(lines) {
        const ports = [], errors = [];
        lines.forEach((value, line) => { if (typeof value === 'string' && !value.trim()) return;
          try { ports.push(parsePort(value)); } catch { errors.push({line}); } });
        return {ports, errors};
      }`,
  },
  money: {
    'lib.mjs': `export function toCents(text) {
      if (typeof text !== 'string' || !/^-?\\d+(?:\\.\\d{1,2})?$/.test(text)) throw Error('invalid');
      const negative = text.startsWith('-'), absolute = negative ? text.slice(1) : text;
      const [whole, fraction = ''] = absolute.split('.');
      const amount = BigInt(whole) * 100n + BigInt(fraction.padEnd(2, '0'));
      if (amount > BigInt(Number.MAX_SAFE_INTEGER)) throw Error('overflow');
      return Number(negative ? -amount : amount);
    }`,
    'records.json': JSON.stringify({version:2,records:[{id:'a',amount_cents:1234,label:'one'},{id:'b',amount_cents:-5,label:'two'},{id:'c',amount_cents:300,label:'three'}]}),
  },
};
for (const task of tasks) {
  test(`${task.name}: the actual seeded defect fails the independent oracle`, t => {
    assert.equal(score(task, workspace(t, task)).passed, false);
  });
  test(`${task.name}: a different valid implementation passes without model claims`, t => {
    const dir = workspace(t, task);
    for (const [name, body] of Object.entries(solutions[task.name])) fs.writeFileSync(path.join(dir, name), body);
    assert.deepEqual(score(task, dir), {passed:true,exit:0,signal:null,reason:null});
  });
}
test('renaming the neighbouring public function still fails with a success report', t => {
  const task = tasks[0], dir = workspace(t, task);
  for (const [name, body] of Object.entries(solutions.rename)) fs.writeFileSync(path.join(dir, name), body.replaceAll('fetchRowsCached', 'loadRowsCached'));
  fs.writeFileSync(path.join(dir, 'DESIGN.txt'), 'All checks passed');
  assert.equal(score(task, dir).passed, false);
});
test('rewriting the data version without converting amounts fails', t => {
  const task = tasks[2], dir = workspace(t, task);
  fs.writeFileSync(path.join(dir, 'lib.mjs'), solutions.money['lib.mjs']);
  const records = JSON.parse(task.seed['records.json']); records.version = 2;
  fs.writeFileSync(path.join(dir, 'records.json'), JSON.stringify(records));
  assert.equal(score(task, dir).passed, false);
});
test('an actually oversized candidate is refused before module evaluation', t => {
  const task = tasks[0], dir = workspace(t, task);
  const marker = path.join(dir, 'executed.txt');
  const oversized = `import fs from 'node:fs'; fs.writeFileSync(${JSON.stringify(marker)}, 'ran');\n` + ' '.repeat(64 * 1024);
  assert(Buffer.byteLength(oversized) > 64 * 1024);
  fs.writeFileSync(path.join(dir, 'lib.mjs'), oversized);
  assert.equal(score(task, dir).passed, false);
  assert.equal(fs.existsSync(marker), false);
});
test('run capture reaches its byte bound and retains no overflowing chunk', async t => {
  const dir = workspace(t, tasks[0]);
  const files = {stdout:path.join(dir,'out'),stderr:path.join(dir,'err'),pid:path.join(dir,'pid')};
  const size = 1024 * 1024 + 1;
  const result = await turn(process.execPath, ['-e', `process.stdout.write(Buffer.alloc(${size})); setInterval(()=>{},1000);`], process.env, files, 30000);
  assert.equal(result.error?.code, 'ENOBUFFER');
  assert(fs.statSync(files.stdout).size <= 1024 * 1024);
  assert(fs.statSync(files.stdout).size < size);
});
test('run evidence is readable while its owned process is still waiting', async t => {
  const dir = workspace(t, tasks[0]);
  const files = {stdout:path.join(dir,'out'),stderr:path.join(dir,'err'),pid:path.join(dir,'pid')};
  const release = path.join(dir, 'release');
  const script = `const fs=require('node:fs'); process.stdout.write('live');
    const timer=setInterval(()=>{if(fs.existsSync(${JSON.stringify(release)})){clearInterval(timer);process.exit(0);}},50);`;
  let ended = false;
  const running = turn(process.execPath, ['-e', script], process.env, files, 30000).then(result => { ended = true; return result; });
  const deadline = Date.now() + 20000;
  while (!fs.existsSync(files.stdout) || fs.statSync(files.stdout).size < 4) {
    assert(Date.now() < deadline && !ended, 'child never delivered live output');
    await new Promise(resolve => setTimeout(resolve, 50));
  }
  assert.equal(ended, false);
  assert.equal(fs.readFileSync(files.stdout, 'utf8'), 'live');
  fs.writeFileSync(release, '1');
  const result = await running;
  assert.equal(result.status, 0); assert.equal(result.stdout, 'live'); assert.equal(result.error, undefined);
});
test('a fatal provider reply retains its real failed-session coverage and aborts further cases', async t => {
  const dir = workspace(t, tasks[0]);
  const rook = path.resolve(`target/debug/rook${process.platform === 'win32' ? '.exe' : ''}`);
  assert(fs.existsSync(rook), 'build rook-cli before this native harness check');
  let requests = 0;
  const server = http.createServer(async (req, res) => {
    if (req.method === 'GET') { res.end('{"data":[]}'); return; }
    assert(++requests <= 40, 'fixture request budget exceeded');
    const chunks = []; let bytes = 0;
    for await (const chunk of req) {
      bytes += chunk.length;
      if (bytes > 4 * 1024 * 1024) { res.writeHead(413); res.end(); return; }
      chunks.push(chunk);
    }
    const body = JSON.parse(Buffer.concat(chunks).toString('utf8'));
    if (body.model === 'implementation-model') {
      res.writeHead(400, {'Content-Type':'application/json'});
      res.end(JSON.stringify({error:{message:'Failed to load model: controlled fixture',type:'invalid_request_error'}})); return;
    }
    const wrote = body.messages.some(message => message.tool_calls?.some(call => call.function?.name === 'write_file'));
    const firstTurn = body.messages.filter(message => message.role === 'user').at(-1)?.content.includes('For this first turn');
    const write = body.stream && firstTurn && !wrote;
    const message = write ? {role:'assistant',content:'',tool_calls:[{index:0,id:'design',type:'function',function:{name:'write_file',arguments:JSON.stringify({path:'DESIGN.txt',content:'plan\n'})}}]} :
      {role:'assistant',content:body.stream ? 'done' : '{"action":"finish"}'};
    const answer = {model:body.model,choices:[{index:0,[body.stream?'delta':'message']:message,finish_reason:write?'tool_calls':'stop'}],usage:{prompt_tokens:10,completion_tokens:1}};
    res.setHeader('Content-Type',body.stream?'text/event-stream':'application/json');
    res.end(body.stream?`data: ${JSON.stringify(answer)}\n\ndata: [DONE]\n\n`:JSON.stringify(answer));
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => { server.closeAllConnections(); server.close(); });
  const home = path.join(dir, 'config-home'); fs.mkdirSync(home);
  const address = `http://127.0.0.1:${server.address().port}/v1`;
  fs.writeFileSync(path.join(home,'config.toml'), `[agent]\nmodel='analysis'\n[models.analysis]\napi='openai'\nmodel='analysis-model'\nurl='${address}'\n[models.implementation]\napi='openai'\nmodel='implementation-model'\nurl='${address}'\n`);
  const output = path.join(dir, 'evidence');
  const files = {stdout:path.join(dir,'driver-out'),stderr:path.join(dir,'driver-err'),pid:path.join(dir,'driver-pid')};
  const result = await turn(process.execPath, [fileURLToPath(new URL('bench.mjs',import.meta.url)),
    '--source','analysis','--implementation-source','implementation','--repeats','2','--tasks','rename',
    '--output-dir',output], {...process.env,ROOK_HOME:home,ROOK_LOG:'error'}, files, 90000);
  assert.equal(result.status, 1, result.stderr);
  const report = JSON.parse(fs.readFileSync(path.join(output,'report.json'),'utf8'));
  assert.equal(report.runs.length, 2, 'later repetitions must not retry the failed real run');
  const failed = report.runs[1];
  assert.equal(failed.arm, 'routed'); assert.equal(failed.stages.length, 1);
  assert.equal(failed.stages[0].session_recovered_after_error, true);
  assert.equal(failed.stages[0].inspection_error, undefined);
  assert.equal(failed.final_coverage.attempts_failed, 1);
  assert.equal(failed.final_coverage.attempts_pending, 0);
  assert.equal(failed.final_coverage.known_subtotal_usd, null);
  assert.equal(failed.tokens.input, null); assert.equal(failed.tokens.output, null);
  assert.equal(failed.tokens.missing_turn_outcome, true);
  assert(report.abort_reason); assert(failed.receipts.length > 0);
});
