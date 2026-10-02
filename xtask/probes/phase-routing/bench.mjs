// Real inference only. Mock-provider frontend probes are separate.
import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import { randomUUID, createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { performance } from 'node:perf_hooks';
import { tasks, seed, score } from './bench-tasks.mjs';
import { turn } from './bench-process.mjs';

const options = new Map();
for (let n = 2; n < process.argv.length; n += 2) {
  assert(process.argv[n].startsWith('--') && process.argv[n + 1], 'arguments must be --name value pairs');
  assert(!options.has(process.argv[n]), 'duplicate option');
  options.set(process.argv[n], process.argv[n + 1]);
}
for (const name of options.keys()) assert(['--source', '--implementation-source', '--implementation-model', '--repeats', '--tasks', '--window', '--output-dir'].includes(name), `unknown option ${name}`);
assert(options.has('--source'), '--source must name a configured model');
assert(options.has('--implementation-source') !== options.has('--implementation-model'), 'choose one implementation source or physical model');
const repeats = Number(options.get('--repeats') ?? 2);
const window = Number(options.get('--window') ?? 32768);
assert(Number.isInteger(repeats) && repeats >= 1 && repeats <= 5, 'repeats must be 1..5');
assert(Number.isInteger(window) && window >= 16384 && window <= 262144, 'window must be 16384..262144');
const selected = options.has('--tasks') ? tasks.filter(task => options.get('--tasks').split(',').includes(task.name)) : tasks;
assert(selected.length && (!options.has('--tasks') || options.get('--tasks').split(',').every(name => tasks.some(task => task.name === name))), 'unknown task');
const rook = path.resolve(`target/debug/rook${process.platform === 'win32' ? '.exe' : ''}`);
assert(fs.existsSync(rook), 'build rook-cli before running the comparison');
function execute(args, env, timeout = 60000) {
  return spawnSync(rook, args, { env, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'], timeout,
    maxBuffer: 4 * 1024 * 1024, windowsHide: true });
}
// Parse in memory. Never print or save the user's complete configuration.
const shown = execute(['--json', 'config', 'show'], process.env);
assert(shown.status === 0 && !shown.error, 'could not read the configured model sources');
const config = JSON.parse(shown.stdout);
function source(name) {
  const value = config.models[name];
  assert(value, `model source ${name} is not configured`);
  const endpoint = value.endpoint ? config.endpoints[value.endpoint] : value;
  assert(endpoint, 'configured endpoint is missing');
  const fields = ['api', 'metadata_api', 'url', 'key', 'parallel', 'key_in_the_clear', 'proxy'];
  const out = Object.fromEntries(fields.filter(key => endpoint[key] != null).map(key => [key, endpoint[key]]));
  for (const key of ['model', 'input_usd_per_million', 'output_usd_per_million', 'cache_read_usd_per_million', 'cache_write_usd_per_million']) if (value[key] != null) out[key] = value[key];
  out.context_window = window;
  out.parallel = 1;
  if (out.key?.startsWith('secret:')) {
    assert(process.env.ROOK_PHASE_BENCH_KEY, 'scratch stores cannot resolve user secret references; supply ROOK_PHASE_BENCH_KEY');
    out.key = 'env:ROOK_PHASE_BENCH_KEY';
  }
  return out;
}
const analysis = source(options.get('--source'));
const implementation = options.has('--implementation-source') ? source(options.get('--implementation-source')) : {
  ...analysis, model: options.get('--implementation-model'),
};
if (options.has('--implementation-model')) for (const key of Object.keys(implementation)) if (key.endsWith('_usd_per_million')) delete implementation[key];
assert(analysis.model !== implementation.model || analysis.url !== implementation.url, 'comparison needs distinct physical sources');
function table(name, values) {
  return `[${name}]\n` + Object.entries(values).map(([key, value]) => `${key}=${JSON.stringify(value)}\n`).join('');
}
const root = path.resolve(options.get('--output-dir') ?? `target/phase-bench-${randomUUID()}`);
fs.mkdirSync(root);
if (!options.has('--output-dir')) fs.writeFileSync('target/pi-phase-bench-root.txt', root);
const revision = spawnSync('git', ['rev-parse', 'HEAD'], {encoding:'utf8', maxBuffer:4096, windowsHide:true});
const scripts = ['bench.mjs', 'bench-tasks.mjs', 'bench-process.mjs'].map(name => {
  const file = new URL(name, import.meta.url);
  assert(fs.statSync(file).size <= 64 * 1024, 'oversized benchmark source');
  return fs.readFileSync(file);
});
const report = { version: 1, root, started_at: new Date().toISOString(), source_commit: revision.status === 0 ? revision.stdout.trim() : null,
  rook_version: execute(['--version'], process.env).stdout.trim(), scripts_sha256: createHash('sha256').update(Buffer.concat(scripts)).digest('hex'),
  analysis_source: options.get('--source'), implementation_source: options.get('--implementation-source') ?? null,
  analysis_model: analysis.model, implementation_model: implementation.model, repeats, window,
  protocol: 'two-stage: inspect/write DESIGN.txt without changing code, then implement the same task',
  limits: { steps_per_turn: 8, output_tokens_per_generation: 1536, seconds_per_turn: 900, subagents: 0 }, runs: [] };
function save() { fs.writeFileSync(path.join(root, 'report.json'), JSON.stringify(report, null, 2)); }
save();
runs: for (let repeat = 0; repeat < repeats; repeat++) for (const task of selected) {
  // Alternate arm order so the fixed source is not always the warm first run.
  for (const arm of repeat % 2 ? ['routed', 'fixed'] : ['fixed', 'routed']) {
    const dir = path.join(root, `${task.name}-${repeat}-${arm}`);
    const workspace = path.join(dir, 'workspace');
    const home = path.join(dir, 'home');
    fs.mkdirSync(home, { recursive: true }); seed(task, workspace);
    const run = { task: task.name, repeat, arm, dir, stages: [] };
    report.runs.push(run); save();
    assert(!score(task, workspace).passed, 'the seeded defect must fail the independent oracle');
    const policy = { ...analysis, implementation_model: arm === 'routed' ? 'implementation' : '' };
    const written = table('agent', { model: 'analysis', max_steps: 8, max_output_tokens: 1536,
      effort: 'none', plan_first: false, one_script: false, install_servers: false,
      max_subagents_per_turn: 0, max_parallel_subagents: 1, stream_idle_timeout_secs: 120 }) +
      table('sandbox', { stance: 'auto', command_timeout_secs: 30, max_output_bytes: 16384,
        max_background_jobs: 0, allow_outside_workspace: false }) +
      table('models.analysis', policy) + table('models.implementation', implementation);
    fs.writeFileSync(path.join(home, 'config.toml'), written);
    const env = { ...process.env, ROOK_HOME: home, ROOK_LOG: 'error' };
    let session;
    for (const [stage, prompt] of [
      ['analysis', `${task.prompt}\nFor this first turn, only inspect the files and write DESIGN.txt with a short implementation plan and invariants. Do not modify the existing files yet. Then finish. Do not delegate.`],
      ['implementation', `${task.prompt}\nImplement the plan now. Use the existing public exports. Do not delegate. Report what you changed.`],
    ]) {
      console.log(`${task.name} #${repeat} ${arm} ${stage}: starting`);
      const item = { stage, started_at: new Date().toISOString(), running: true };
      run.stages.push(item); save();
      const started = performance.now();
      const output = await turn(rook, ['--workspace', workspace, '--yes', '--json', 'run', ...(session ? ['--session', session] : []), prompt], env, {
        stdout: path.join(dir, `${stage}-stdout.json`), stderr: path.join(dir, `${stage}-stderr.txt`), pid: path.join(dir, `${stage}.pid`),
      });
      const seconds = (performance.now() - started) / 1000;
      Object.assign(item, { running: false, seconds, exit: output.status, signal: output.signal, process_error: output.error?.code ?? null });
      try {
        try {
          const printed = JSON.parse(output.stdout); session = printed.session;
          const outcome = printed.outcome ?? printed;
          assert(typeof outcome.stopped === 'string' &&
            Number.isSafeInteger(outcome.input_tokens) && outcome.input_tokens >= 0 &&
            Number.isSafeInteger(outcome.output_tokens) && outcome.output_tokens >= 0, 'missing or unsafe turn facts');
          item.outcome = { stopped: outcome.stopped, steps: outcome.steps, input_tokens: outcome.input_tokens, output_tokens: outcome.output_tokens, cached_tokens: outcome.cached_tokens };
        } catch {
          // A provider error can leave durable work but no final stdout JSON.
          // This home belongs to one case; recover only an unambiguous session.
          const listed = execute(['--workspace', workspace, '--json', 'session', 'ls'], env);
          assert(listed.status === 0 && !listed.error, 'failed-session recovery read failed');
          fs.writeFileSync(path.join(dir, `${stage}-sessions.json`), listed.stdout);
          const sessions = JSON.parse(listed.stdout);
          assert(sessions.length === 1, 'cannot identify the failed session without guessing');
          session = sessions[0].id; item.session_recovered_after_error = true;
        }
        assert(/^[0-9A-HJKMNP-TV-Z]{26}$/.test(session), 'missing saved session id');
        run.session = session;
        const context = execute(['--workspace', workspace, '--json', 'session', 'context', session], env);
        assert(context.status === 0 && !context.error, 'saved context inspection failed');
        const usage = JSON.parse(context.stdout);
        item.coverage = usage.cost_coverage; item.last_response = usage.last_response;
        fs.writeFileSync(path.join(dir, `${stage}-context.json`), context.stdout);
      } catch (error) { item.inspection_error = error.message; }
      if (stage === 'analysis') {
        run.plan_protocol_valid = fs.existsSync(path.join(workspace, 'DESIGN.txt')) && Object.entries(task.seed).every(([name, body]) => {
          const file = path.join(workspace, name);
          return fs.existsSync(file) && fs.statSync(file).size <= 64 * 1024 && fs.readFileSync(file, 'utf8') === body;
        });
      }
      save();
      console.log(`${task.name} #${repeat} ${arm} ${stage}: exit=${item.exit}, ${seconds.toFixed(1)}s`);
      if (output.error || ![0, 2].includes(output.status) || !session || !item.outcome) break;
    }
    run.quality = score(task, workspace);
    run.seconds = run.stages.reduce((sum, stage) => sum + stage.seconds, 0);
    if (session) {
      const journal = execute(['--workspace', workspace, '--json', 'session', 'show', session, '--limit', '512', '--max-body', '4096'], env);
      fs.writeFileSync(path.join(dir, 'journal.json'), journal.stdout ?? '');
      run.journal_exit = journal.status;
      if (journal.status === 0 && !journal.error) {
        const entries = JSON.parse(journal.stdout);
        run.receipts = entries.filter(entry => ['rook:model-route:v1', 'rook:model-aux:v1'].includes(entry.label)).map(entry => {
          try { return { seq: entry.seq, kind: entry.label, receipt: JSON.parse(entry.body) }; }
          catch { return { seq: entry.seq, kind: entry.label, unreadable: true }; }
        });
      }
    }
    const last = run.stages.at(-1);
    // Coverage is cumulative. Never add the earlier snapshot to the final one.
    run.final_coverage = last.coverage ?? null;
    const expectedReceipts = last.coverage ? last.coverage.main_receipts + last.coverage.auxiliary_receipts : null;
    run.journal_receipts_complete = expectedReceipts != null && run.journal_exit === 0 && (run.receipts?.length ?? 0) === expectedReceipts;
    let confirmed = 0, receiptInput = 0, receiptOutput = 0;
    for (const row of run.receipts ?? []) {
      const receipt = row.kind === 'rook:model-aux:v1' ? row.receipt?.receipt : row.receipt;
      if (receipt?.complete && receipt.usage_reported &&
        Number.isSafeInteger(receipt.usage?.input_tokens) && Number.isSafeInteger(receipt.usage?.output_tokens)) {
        confirmed++; receiptInput += receipt.usage.input_tokens; receiptOutput += receipt.usage.output_tokens;
      }
    }
    run.receipt_tokens = { confirmed_receipts: confirmed,
      known_input: confirmed && Number.isSafeInteger(receiptInput) ? receiptInput : null,
      known_output: confirmed && Number.isSafeInteger(receiptOutput) ? receiptOutput : null };
    const missing = run.stages.some(stage => !stage.outcome);
    run.tokens = {
      input: missing ? null : run.stages.reduce((sum, stage) => sum + stage.outcome.input_tokens, 0),
      output: missing ? null : run.stages.reduce((sum, stage) => sum + stage.outcome.output_tokens, 0),
      missing_turn_outcome: missing,
    };
    run.implementation_dispatch_verified = arm === 'fixed' || (run.stages.length === 2 &&
      last.last_response?.receipt?.dispatch?.model === implementation.model &&
      last.last_response?.receipt?.phase === 'implementation');
    save(); console.log(`${task.name} #${repeat} ${arm}: quality=${run.quality.passed}, plan_protocol=${run.plan_protocol_valid}`);
    if (run.stages.some(stage => stage.process_error || stage.inspection_error || ![0, 2].includes(stage.exit) || !stage.outcome)) {
      report.abort_reason = 'A real run failed; inspect its saved stderr/history before scheduling more cases.';
      break runs;
    }
  }
}
report.finished_at = new Date().toISOString(); save();
console.log(`Retained real-model evidence: ${root}`);
// A completed comparison may include quality failures; inspect report.json.
if (report.abort_reason || report.runs.some(run => run.stages.length !== 2 || !run.plan_protocol_valid || !run.implementation_dispatch_verified)) process.exitCode = 1;
