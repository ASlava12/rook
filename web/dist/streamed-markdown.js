// One bounded source buffer; no array of token chunks or copy in a DOM attribute.
// Reparse the whole admitted prefix so split fences remain meaningful.
export const MAX_MODEL_CHARACTERS = 1 << 20;

export function streamedMarkdown({ render, isCurrent, maximum = MAX_MODEL_CHARACTERS,
  requestFrame = globalThis.requestAnimationFrame?.bind(globalThis),
  cancelFrame = globalThis.cancelAnimationFrame?.bind(globalThis),
  schedule = setTimeout, unschedule = clearTimeout } = {}) {
  if (!Number.isSafeInteger(maximum) || maximum < 1 || maximum > MAX_MODEL_CHARACTERS)
    throw new RangeError('Invalid live Markdown character bound');
  let text = '', clipped = false, dirty = false, closed = false;
  let frame = null, timer = null, generation = 0;
  const high = value => value >= 0xd800 && value <= 0xdbff;
  const low = value => value >= 0xdc00 && value <= 0xdfff;
  function cancel() {
    generation++;
    if (frame !== null) cancelFrame?.(frame);
    if (timer !== null) unschedule(timer);
    frame = timer = null;
  }
  function discard() { cancel(); text = ''; dirty = false; closed = true; }
  function flush() {
    cancel();
    if (closed) return;
    if (!isCurrent()) { discard(); return; }
    if (dirty) { dirty = false; render(text, clipped); }
  }
  function enqueue() {
    if (frame !== null || timer !== null) return;
    const expected = generation;
    const run = () => { if (generation === expected) flush(); };
    if (requestFrame) frame = requestFrame(run);
    // Background tabs can suspend animation frames indefinitely. This is only a
    // fallback, not a typing delay; boundaries always flush synchronously.
    timer = schedule(run, 100);
  }
  function append(value) {
    if (closed || typeof value !== 'string' || !value.length || clipped) return;
    if (!isCurrent()) { discard(); return; }
    const room = maximum - text.length;
    let admitted = Math.min(value.length, room);
    if (admitted < value.length) {
      clipped = true;
      if (high(value.charCodeAt(admitted - 1)) && low(value.charCodeAt(admitted))) admitted--;
    }
    // Admission precedes substring/concatenation, including a huge single delta.
    if (admitted) text += admitted === value.length ? value : value.slice(0, admitted);
    if (clipped && high(text.charCodeAt(text.length - 1))) text = text.slice(0, -1);
    dirty = true; enqueue();
  }
  function replace(value) {
    if (closed) return;
    if (!clipped && value === text) return;
    cancel();
    text = ''; clipped = false; dirty = true;
    append(value);
  }
  return { append, replace, flush, discard };
}
