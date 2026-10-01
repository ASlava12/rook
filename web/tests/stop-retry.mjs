import test from 'node:test';
import assert from 'node:assert/strict';
import { stopRetry } from '../dist/stop-retry.js';

function storage() {
  const values = new Map();
  return {
    getItem: key => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
    removeItem: key => values.delete(key),
  };
}

test('a saved goal Stop retries exactly after a page reload and settles by caller ID', () => {
  const saved = storage();
  const first = stopRetry(saved);
  const { frame, persisted } = first.remember('session-one', 'generation-one');
  assert.equal(persisted, true);
  assert.equal(frame.type, 'stop');
  assert.equal(frame.generation, 'generation-one');
  const restored = stopRetry(saved);
  assert.deepEqual(restored.retry(), frame);
  assert.deepEqual(restored.remember('session-one', 'generation-one').frame, frame);
  assert.throws(() => restored.remember('session-one', 'generation-two'), /Resolve the saved Stop/);
  restored.settled('another-id');
  assert.deepEqual(restored.retry(), frame);
  restored.settled(frame.id);
  assert.equal(restored.candidate(), null);
  assert.equal(stopRetry(saved).candidate(), null);
});

test('oversized or malformed saved Stop cannot be copied into a retry', () => {
  const saved = storage();
  saved.setItem('rook:pending-stop', 'x'.repeat(513));
  assert.match(stopRetry(saved).error(), /exceeds its limit/);
  saved.setItem('rook:pending-stop', JSON.stringify({ session: 's', id: 'id', generation: null }));
  const retry = stopRetry(saved);
  assert.match(retry.error(), /invalid/);
  assert.equal(retry.candidate(), null);
  retry.discard();
  assert.equal(retry.error(), null);
});

test('a failed tab-storage write never claims that a Stop was saved', () => {
  const unavailable = { getItem: () => null, removeItem() {}, setItem() { throw new Error('quota'); } };
  const retry = stopRetry(unavailable);
  const first = retry.remember('session-one', 'generation-one');
  assert.equal(first.persisted, false);
  const second = retry.remember('session-one', 'generation-one');
  assert.equal(second.persisted, false);
  assert.deepEqual(second.frame, first.frame);
});
