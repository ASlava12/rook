import test from 'node:test';
import assert from 'node:assert/strict';
import { admitDraftFiles } from '../dist/draft-files.js';

const file = (name, size) => ({ name, size, type: '', arrayBuffer() { throw new Error('admission must not read pixels or text'); } });

test('draft file admission checks count before indexing or copying the selection', () => {
  const oversized = { length: 5, get 0() { throw new Error('selection was indexed before admitting its count'); } };
  assert(oversized.length > 4);
  assert.throws(() => admitDraftFiles(oversized), /At most 4/);
  assert.throws(() => admitDraftFiles([file('a.txt', 1)], Array(4).fill({ type: 'image' })), /At most 4/);
});

test('admission checks image and combined text size without reading file bytes', () => {
  const bigImage = file('image.png', 2 * 1024 * 1024 + 1);
  assert(bigImage.size > 2 * 1024 * 1024);
  assert.throws(() => admitDraftFiles([bigImage]), /2 MiB/);
  const texts = [file('a.txt', 128 * 1024), file('b.txt', 128 * 1024 + 1)];
  assert(texts.reduce((sum, entry) => sum + entry.size, 0) > 256 * 1024);
  assert.throws(() => admitDraftFiles(texts), /256 KiB/);
  assert.throws(() => admitDraftFiles([file('a.txt', 256 * 1024)], [{ type: 'text', text: 'я' }]), /256 KiB/);
  assert.throws(() => admitDraftFiles([file('a.txt', -1)]), /Invalid attachment size/);
});

test('accepted files retain their identities without allocating their content', () => {
  const files = [file('image.png', 2 * 1024 * 1024), file('text.txt', 256 * 1024)];
  const kept = admitDraftFiles(files);
  assert.notEqual(kept, files);
  assert.equal(kept[0], files[0]);
  assert.equal(kept[1], files[1]);
});
