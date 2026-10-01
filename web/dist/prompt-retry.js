// One uncertain socket prompt per tab. The exact frame matters: a retry with
// changed options is a different request, even if its visible text is equal.
import { jsonWithin } from './lib.js';

const KEY = 'rook:pending-prompt';
const LIMIT = 16 * 1024 * 1024;

export function promptRetry(suppliedStorage) {
  let pending = null;
  let inFlight = false;
  let startedSession = null;
  let storage = suppliedStorage;
  try {
    storage ??= globalThis.sessionStorage;
    const raw = storage?.getItem(KEY);
    if (raw && raw.length <= LIMIT && new TextEncoder().encode(raw).length <= LIMIT) {
      const frame = JSON.parse(raw);
      if (frame?.type === 'prompt' && typeof frame.id === 'string' &&
          /^[A-Za-z0-9_-]{1,64}$/.test(frame.id) && typeof frame.text === 'string' &&
          (frame.session === null || typeof frame.session === 'string') &&
          frame.options && typeof frame.options === 'object') pending = frame;
    }
  } catch { /* Storage can be unavailable; the current page still retains it. */ }

  const persist = () => {
    try {
      if (!storage) return false;
      storage.removeItem(KEY);
      if (pending) storage.setItem(KEY, JSON.stringify(pending));
      return true;
    } catch {
      // A large attachment may exceed the browser's storage quota.
      return false;
    }
  };
  return {
    candidate: () => pending,
    sending: () => inFlight,
    remember(frame) {
      if (pending) throw new Error('Resolve the saved prompt first: retry it or discard it');
      if (!jsonWithin(frame, LIMIT)) throw new Error('prompt exceeds socket limit');
      pending = frame;
      inFlight = true;
      startedSession = null;
      return persist();
    },
    retry() { if (pending) { inFlight = true; startedSession = null; } return pending; },
    started(session) {
      if (inFlight && pending && (pending.session === null || pending.session === session)) {
        startedSession = session;
      }
    },
    completed(session) {
      if (inFlight && startedSession !== null && startedSession === session) this.settled();
      else this.disconnected();
    },
    disconnected() { inFlight = false; startedSession = null; },
    settled() { if (inFlight) { pending = null; inFlight = false; startedSession = null; persist(); } },
    discard() { pending = null; inFlight = false; startedSession = null; persist(); },
  };
}
