import test from 'node:test';
import assert from 'node:assert/strict';
import { streamedMarkdown, MAX_MODEL_CHARACTERS } from '../dist/streamed-markdown.js';

function harness(maximum = MAX_MODEL_CHARACTERS) {
  let next = 0, valid = true;
  const frames = new Map(), timers = new Map(), rendered = [];
  const writer = streamedMarkdown({ maximum, render: (text, clipped) => rendered.push({ text, clipped }),
    isCurrent: () => valid, requestFrame: cb => { const id = ++next; frames.set(id, cb); return id; },
    cancelFrame: id => frames.delete(id), schedule: (cb, ms) => {
      assert.equal(ms, 100); const id = ++next; timers.set(id, cb); return id;
    }, unschedule: id => timers.delete(id) });
  return { writer, rendered, frames, timers, invalidate: () => { valid = false; } };
}

test('token bursts render a full split fence once per frame and do not queue token arrays', () => {
  const h = harness();
  const text = 'Paragraph\n\n```rust\nlet owl = "🦉";\n```\n\nFinal';
  for (const unit of text.split('')) h.writer.append(unit);
  assert.equal(h.rendered.length, 0); assert.equal(h.frames.size, 1); assert.equal(h.timers.size, 1);
  h.frames.values().next().value();
  assert.deepEqual(h.rendered, [{ text, clipped: false }]);
  assert.equal(h.frames.size, 0); assert.equal(h.timers.size, 0);
  h.writer.replace(text); h.writer.flush();
  assert.equal(h.rendered.length, 1, 'Done with the same reply must not parse it again');
});

test('background fallback flushes without a frame and retired callbacks cannot flush a newer burst', () => {
  const h = harness(); h.writer.append('one');
  const oldFrame = h.frames.values().next().value;
  h.timers.values().next().value();
  assert.equal(h.rendered.at(-1).text, 'one');
  h.writer.append('two'); oldFrame();
  assert.equal(h.rendered.length, 1); assert.equal(h.frames.size, 1);
  h.writer.flush(); assert.equal(h.rendered.at(-1).text, 'onetwo');
  const obsolete = oldFrame; h.writer.discard(); obsolete(); h.writer.append('lost');
  assert.equal(h.rendered.length, 2); assert.equal(h.frames.size, 0); assert.equal(h.timers.size, 0);
});

test('viewport ownership invalidation releases the buffer and never renders into a new view', () => {
  const h = harness(); h.writer.append('old session'); h.invalidate();
  h.frames.values().next().value(); h.writer.replace('late final'); h.writer.flush();
  assert.deepEqual(h.rendered, []); assert.equal(h.timers.size, 0);
});

test('a huge arriving delta reaches the default bound before concatenation and later tokens stay discarded', () => {
  const h = harness(); const huge = 'a'.repeat(MAX_MODEL_CHARACTERS + 500000);
  assert(huge.length > MAX_MODEL_CHARACTERS);
  h.writer.append(huge); h.writer.flush();
  assert.equal(h.rendered[0].text.length, MAX_MODEL_CHARACTERS); assert.equal(h.rendered[0].clipped, true);
  h.writer.append(huge); h.writer.flush();
  assert.equal(h.rendered.length, 1); assert.equal(h.frames.size, 0);
  h.writer.replace('authoritative shorter reply'); h.writer.flush();
  assert.deepEqual(h.rendered.at(-1), { text: 'authoritative shorter reply', clipped: false });
});

test('admission never cuts a surrogate pair including a pair split between deltas', () => {
  for (const chunks of [['abc🦉tail'], ['abc\ud83e', '\udd89tail']]) {
    const h = harness(4);
    for (const part of chunks) h.writer.append(part);
    h.writer.flush(); assert.deepEqual(h.rendered.at(-1), { text: 'abc', clipped: true });
  }
  const h = harness(5); h.writer.append('abc🦉'); h.writer.flush();
  assert.deepEqual(h.rendered.at(-1), { text: 'abc🦉', clipped: false });
});
