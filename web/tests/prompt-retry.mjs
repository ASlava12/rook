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

test('saved goal admission precedes Started and never binds an unrelated caller or session', () => {
  const saved = storage(), pending = promptRetry(saved);
  const frame = { type:'prompt', session:null, id:'caller', text:'/goal inspect', options:{} };
  pending.remember(frame);
  pending.saved({session:'foreign', id:'other'});
  pending.saved({session:'', id:'caller'});
  pending.completed('foreign');
  assert.deepEqual(pending.candidate(), frame, 'an unrelated receipt cannot bind a later Done');
  pending.retry(); pending.saved({session:'source', id:'caller'});
  assert.equal(pending.candidate(), null);
  assert.equal(promptRetry(saved).candidate(), null, 'the durable acknowledgement clears tab storage');
  pending.remember({...frame, session:'source'});
  pending.saved({session:'target', id:'caller'});
  assert.ok(pending.candidate());
  pending.disconnected();pending.saved({session:'source', id:'caller'});
  assert.ok(pending.candidate(), 'uncertain sends require explicit retry');
  pending.retry();pending.saved({session:'source', id:'caller'});
  assert.equal(pending.candidate(), null);
  pending.remember({...frame, session:'target', id:'successor'});
  pending.saved({session:'source', id:'caller'});
  assert.equal(pending.candidate().id,'successor', 'a late source receipt cannot settle the target prompt');
});

test('durable admission frees a prompt before completion only for its exact caller and session', () => {
  const saved = storage();
  const pending = promptRetry(saved);
  pending.remember({ type: 'prompt', session: 'one', id: 'caller', text: 'original', options: {} });
  pending.admitted('caller', 'one');
  assert.ok(pending.candidate(), 'a receipt without the matching start is insufficient');
  pending.started('one');
  pending.admitted('other', 'one');
  pending.admitted('caller', 'two');
  assert.ok(pending.candidate(), 'unrelated receipts preserve the retry');
  pending.admitted('caller', 'one');
  assert.equal(pending.candidate(), null);
  assert.equal(promptRetry(saved).candidate(), null);
  pending.remember({ type: 'prompt', session: 'two', id: 'new', text: 'next branch', options: {} });
  pending.started('two');
  pending.admitted('caller', 'one');
  assert.equal(pending.candidate().id, 'new', 'a late old receipt cannot clear the new prompt');
  pending.disconnected();
  pending.admitted('new', 'two');
  assert.equal(pending.candidate().id, 'new', 'an unobserved receipt cannot settle an uncertain send');
});

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
