// Run with `node --test web/tests/json-size.mjs`; no browser or packages needed.
import test from 'node:test';
import assert from 'node:assert/strict';
import { jsonWithin } from '../dist/lib.js';

test('admission matches actual encoded JSON at the exact byte boundary', () => {
  const cases = [null, true, false, 0, -0, 1e30, NaN, Infinity, '',
    'Привет 👩‍💻', '\u0000\b\t\n\f\r"\\', '\ud800', '\udc00',
    [1, , undefined, '界'], { absent: undefined, nested: [false, { '\n': 'é' }] }];
  let seed = 1729;
  for (let n = 0; n < 200; n++) {
    let text = '';
    for (let i = 0; i < 100; i++) {
      seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
      text += String.fromCharCode(seed >>> 16);
    }
    cases.push({ type: 'prompt', text, options: { attachments: [{ type: 'text', name: text, text }] } });
  }
  for (const value of cases) {
    const bytes = Buffer.byteLength(JSON.stringify(value));
    assert.equal(jsonWithin(value, bytes), true);
    assert.equal(jsonWithin(value, bytes - 1), false);
  }
});

test('escaped oversized drafts are refused before serialization', () => {
  const text = '\u0001'.repeat(3 * 1024 * 1024), limit = 16 * 1024 * 1024;
  assert(Buffer.byteLength(text) < limit);
  assert(Buffer.byteLength(JSON.stringify({ text })) > limit);
  const stringify = JSON.stringify;
  try {
    JSON.stringify = () => { throw new Error('admission must precede serialization'); };
    assert.equal(jsonWithin({ type: 'prompt', text }, limit), false);
  } finally { JSON.stringify = stringify; }
});

test('deep JSON is bounded without recursive call-stack growth', () => {
  let value = 0;
  for (let depth = 0; depth < 10000; depth++) value = [value];
  assert.equal(jsonWithin(value, 20001), true);
  assert.equal(jsonWithin(value, 20000), false);
});
