import test from 'node:test';
import assert from 'node:assert/strict';
import { promptRetry } from '../dist/prompt-retry.js';

function storage() {
  const values = new Map();
  return {
    getItem: key => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
    removeItem: key => values.delete(key),
  };
}

test('an uncertain prompt reloads with its original frame and retry identity', () => {
  const saved = storage();
  const first = promptRetry(saved);
  const frame = { type: 'prompt', session: null, id: 'caller-one', text: '/goal inspect',
    options: { attachments: [{ type: 'text', name: 'a', text: 'original' }] } };
  first.remember(frame);
  first.started('created-session');
  first.disconnected();
  const reloaded = promptRetry(saved);
  assert.deepEqual(reloaded.candidate(), frame);
  reloaded.completed('created-session');
  assert.deepEqual(reloaded.candidate(), frame, 'an unrelated terminal event cannot acknowledge it');
  assert.deepEqual(reloaded.retry(), frame);
  reloaded.started('created-session');
  reloaded.completed('another-session');
  assert.deepEqual(reloaded.candidate(), frame);
  reloaded.retry();
  reloaded.settled();
  assert.equal(promptRetry(saved).candidate(), null);
});

test('a new request waits for explicit discard and known completion clears the saved frame', () => {
  const saved = storage();
  const pending = promptRetry(saved);
  pending.remember({ type: 'prompt', session: 'one', id: 'first', text: 'first', options: {} });
  assert.throws(() => pending.remember({ type: 'prompt', session: 'two', id: 'second', text: 'second', options: {} }));
  pending.started('one');
  pending.completed('one');
  pending.remember({ type: 'prompt', session: 'two', id: 'second', text: 'second', options: {} });
  assert.equal(promptRetry(saved).candidate().id, 'second');
  pending.discard();
  assert.equal(promptRetry(saved).candidate(), null);
});

test('storage quota failures keep the current tab retryable without resurrecting an old frame', () => {
  const saved = storage();
  const limited = { ...saved, setItem() { throw new Error('quota'); } };
  const next = promptRetry(limited);
  assert.equal(next.remember({ type: 'prompt', session: null, id: 'new', text: 'new', options: {} }), false);
  assert.equal(next.candidate().id, 'new');
  assert.equal(promptRetry(saved).candidate(), null);
});
