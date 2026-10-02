import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';

function read(file, limit = 256 * 1024) {
  assert(fs.statSync(file).size <= limit, `oversized fixture artifact: ${file}`);
  return fs.readFileSync(file, 'utf8');
}
const proof = [];
for (const mode of ['local', 'shared', 'browser']) {
  const root = read(`target/phase-live-${mode}-root.txt`, 4096).trim();
  const frame = name => read(path.join(root, `${name}.txt`));
  const names = fs.readdirSync(root).filter(name => /^request-\d+\.json$/.test(name));
  assert.equal(names.length, 3, `${mode}: inspection must not cause generation`);
  const requests = names.sort().map(name => JSON.parse(read(path.join(root, name), 4 * 1024 * 1024)));
  assert.deepEqual(requests.filter(r => r.stream).map(r => r.model), ['analysis-model', 'implementation-model']);
  assert.equal(read(path.join(root, 'workspace/routing.txt')), `${mode.toUpperCase()}_PHASE\n`);
  assert.equal(read(path.join(root, 'workspace/untouched.txt')), 'untouched phase workspace');
  const live = frame(mode === 'browser' ? 'browser-during-implementation' : `${mode}-live-home`);
  assert.match(live, /Selected: analysis · phase analysis/);
  assert.match(live, /Dispatched: analysis \/ analysis-model/);
  assert.match(live, /[Ww]indow 32768/);
  const pending = mode === 'browser' ? live : frame(`${mode}-live-cost`);
  assert.match(pending, /2 started · 1 completed/);
  assert.match(pending, /1 pending/);
  const completed = frame(mode === 'browser' ? 'browser-complete' : `${mode}-complete-cost`);
  assert.match(completed, /Dispatched: implementation \/ implementation-model/);
  assert.match(completed, /Known subtotal: USD 0\.00007000/);
  assert.match(completed, /3 started · 3 completed/);
  if (mode === 'browser') {
    assert.match(frame('browser-reloaded'), /Known subtotal: USD 0\.00007000/);
    assert.match(completed, /Reported model: server-implementation-model/);
    assert.match(completed, /Receipt and attempt subtotals overlap; do not add them/);
  } else {
    const ids = JSON.parse(read(path.join(root, 'recovery-ids.json'), 4096));
    for (const branch of ['before', 'after', 'parent']) {
      const before = branch === 'before';
      assert(frame(`${mode}-reopen-${branch}-chat`).split('\n')[0].includes(ids[branch]), `${mode}: branch switch was not observed in the header`);
      const home = frame(`${mode}-reopen-${branch}-home`);
      assert.match(home, before ? /[Ww]indow 65536/ : /[Ww]indow 32768/);
      assert.match(home, before ? /Selected: analysis · phase analysis/ : /Selected: analysis · phase implementation/);
      const cost = frame(`${mode}-reopen-${branch}-cost`);
      assert.match(cost, before ? /Known subtotal: USD 0\.00003800/ : /Known subtotal: USD 0\.00007000/);
    }
  }
  proof.push({ mode, root, session: read(path.join(root, 'session-id'), 64).trim(), requests: names.length });
}
fs.writeFileSync('target/pi-phase-live-proof.json', JSON.stringify(proof, null, 2));
console.log('Actual browser/local/shared phase frames and saved-prefix recovery verified.');
