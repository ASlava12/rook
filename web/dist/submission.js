// One bounded outbox per tab. Persist the destination before the first write:
// retrying after a daemon restart or a lost response must not resolve a new goal.
import { api } from './lib.js';

const KEY = 'rook.queue-submission';
const MAX_BYTES = 8 * 1024 * 1024;
let attempt = null, inFlight = false, restoreError = null;
try {
  const saved = sessionStorage.getItem(KEY);
  if (saved) {
    if (saved.length > MAX_BYTES * 6 + 1024) throw new Error('Saved submission exceeds its limit');
    const value = JSON.parse(saved);
    if (typeof value.session !== 'string' || value.session.length > 64 ||
        typeof value.id !== 'string' || !/^[a-zA-Z0-9_-]{1,64}$/.test(value.id) ||
        typeof value.text !== 'string' || value.text.length > MAX_BYTES ||
        new TextEncoder().encode(value.text).length > MAX_BYTES ||
        value.target !== null && (typeof value.target !== 'string' || value.target.length > 128)) {
      throw new Error('Saved submission is invalid');
    }
    attempt = value;
  }
} catch (error) { restoreError = String(error); }

export const pendingSubmission = () => attempt;
export const submissionError = () => restoreError;
export function forgetSubmission() {
  if (inFlight) throw new Error('Wait for the active submission request to finish');
  sessionStorage.removeItem(KEY);
  attempt = null; restoreError = null;
}

async function request(path, body) {
  // These are finite queue operations, not the model's streamed response.
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 30000);
  try { return await api(path, body, controller.signal); }
  finally { clearTimeout(timeout); }
}

export async function retrySubmission() {
  if (restoreError) throw new Error(restoreError);
  if (!attempt) throw new Error('No pending submission');
  if (inFlight) throw new Error('The previous submission is still awaiting confirmation');
  inFlight = true;
  try {
    const base = `/api/sessions/${encodeURIComponent(attempt.session)}/queue`;
    if (attempt.target === null) {
      const page = await request(base);
      if (typeof page.submission_target !== 'string' || !page.submission_target || page.submission_target.length > 128) throw new Error('Restart the daemon with the current build to enable scoped submission');
      if (new TextEncoder().encode(attempt.text).length > page.max_message_bytes) throw new Error(`Message exceeds work.max_message_bytes (${page.max_message_bytes})`);
      const prepared = { ...attempt, target: page.submission_target };
      sessionStorage.setItem(KEY, JSON.stringify(prepared));
      attempt = prepared;
    }
    const entry = await request(base, { action: 'submit', target: attempt.target, id: attempt.id, text: attempt.text });
    if (entry.reference !== `${attempt.target}.${attempt.id}` || entry.receipt?.id !== attempt.id ||
        typeof entry.receipt.text !== 'string' || !Number.isSafeInteger(entry.receipt.revision) ||
        entry.truncated !== false) throw new Error('Missing or invalid submission receipt; retry the saved request');
    const { session, text } = attempt;
    sessionStorage.removeItem(KEY);
    attempt = null;
    return { session, entry, text };
  } finally { inFlight = false; }
}

export async function submitSteering(session, text) {
  if (restoreError) throw new Error(`${restoreError}; open Message queue to inspect or forget the saved send`);
  if (attempt) {
    if (attempt.session !== session || attempt.text !== text) throw new Error(`Resolve the pending send in session ${attempt.session} first (Message queue → Retry submission)`);
  } else {
    if (!session) throw new Error('Wait for the session to open before sending a correction');
    if (!text.trim() || text.length > MAX_BYTES || new TextEncoder().encode(text).length > MAX_BYTES) throw new Error('Queued message must contain text and fit in 8 MiB');
    const value = { session, text, id: crypto.randomUUID(), target: null };
    sessionStorage.setItem(KEY, JSON.stringify(value));
    attempt = value;
  }
  return retrySubmission();
}
