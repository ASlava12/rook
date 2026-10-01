// One small Stop request per tab. Save it before the socket write so a lost
// acknowledgement can be retried with the same goal generation and caller ID.
const KEY = 'rook:pending-stop';
const MAX_SAVED = 512;
const validId = value => typeof value === 'string' && /^[A-Za-z0-9_-]{1,64}$/.test(value);

export function stopRetry(suppliedStorage) {
  let storage = suppliedStorage;
  let pending = null;
  let restoreError = null;
  let persisted = false;
  try {
    storage ??= globalThis.sessionStorage;
    const raw = storage?.getItem(KEY);
    if (raw) {
      if (raw.length > MAX_SAVED) throw new Error('Saved Stop exceeds its limit');
      const saved = JSON.parse(raw);
      if (!validId(saved?.session) || !validId(saved?.id) || !validId(saved.generation)) {
        throw new Error('Saved Stop is invalid');
      }
      pending = { session: saved.session, id: saved.id, generation: saved.generation };
      persisted = true;
    }
  } catch (error) { restoreError = String(error); }

  const persist = () => {
    try {
      storage?.removeItem(KEY);
      if (pending) storage?.setItem(KEY, JSON.stringify(pending));
      persisted = !!storage;
      return persisted;
    } catch { persisted = false; return false; }
  };
  return {
    candidate: () => pending,
    error: () => restoreError,
    remember(session, generation) {
      if (restoreError) throw new Error(`${restoreError}; discard the saved Stop after inspection`);
      if (!validId(session) || !validId(generation)) {
        throw new Error('Stop needs a valid observed session and goal generation');
      }
      if (pending) {
        if (pending.session !== session || pending.generation !== generation) {
          throw new Error(`Resolve the saved Stop for session ${pending.session} before stopping another turn`);
        }
        return { frame: this.retry(), persisted };
      }
      pending = { session, id: crypto.randomUUID(), generation };
      return { frame: this.retry(), persisted: persist() };
    },
    retry() {
      return pending && { type: 'stop', id: pending.id, generation: pending.generation };
    },
    settled(id) {
      if (pending?.id === id) { pending = null; restoreError = null; persist(); }
    },
    discard() { pending = null; restoreError = null; persist(); },
  };
}
