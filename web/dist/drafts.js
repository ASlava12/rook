import { jsonWithin } from './lib.js';

const KEY = 'rook:workspace-drafts:v1';
export const DRAFT_BYTES = 65536;
export const DRAFT_TOTAL_BYTES = 262144;
const MAX_DRAFTS = 8;
const owner = (session, workspace) => session || `new:${workspace || ''}`;

function read() {
  try {
    const raw = sessionStorage.getItem(KEY);
    if (!raw || raw.length > DRAFT_TOTAL_BYTES) return [];
    const rows = JSON.parse(raw);
    if (!Array.isArray(rows) || rows.length > MAX_DRAFTS || !jsonWithin(rows, DRAFT_TOTAL_BYTES)) return [];
    return rows.filter(row => row && typeof row.owner === 'string' && row.owner.length <= 4100 &&
      typeof row.text === 'string' && jsonWithin(row.text, DRAFT_BYTES));
  } catch { return []; }
}

// Text drafts only. File objects and private credential inputs never enter browser storage.
export const drafts = {
  get(session, workspace) { return read().find(row => row.owner === owner(session, workspace))?.text || ''; },
  save(session, workspace, text) {
    if (typeof text !== 'string' || text.length > DRAFT_BYTES || !jsonWithin(text, DRAFT_BYTES)) return false;
    const key = owner(session, workspace);
    const rows = read().filter(row => row.owner !== key);
    if (text) rows.push({ owner: key, text });
    while (rows.length > MAX_DRAFTS || !jsonWithin(rows, DRAFT_TOTAL_BYTES)) rows.shift();
    try { sessionStorage.setItem(KEY, JSON.stringify(rows)); return true; }
    catch { return false; }
  },
};
