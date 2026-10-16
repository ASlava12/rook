// Export selection, filtering and HTML rendering are shared with CLI/TUI in core.
import { api } from './lib.js';

export async function exportHistoryHtml(session, from = 0, through = null, read = api, profile = 'share') {
  for (const value of [from, through].filter(value => value != null)) {
    if (!Number.isSafeInteger(value) || value < 0) throw new Error('Export event must be a nonnegative integer.');
  }
  if (!['share', 'share-tools', 'archive'].includes(profile)) throw new Error('Unknown export profile.');
  return read(`/api/sessions/${encodeURIComponent(session)}/export`, { from, through, profile });
}

export async function downloadHistoryHtml(session, from, through, profile = 'share', prepared = null) {
  const result = prepared || await exportHistoryHtml(session, from, through, api, profile);
  const url = URL.createObjectURL(new Blob([result.html], { type: 'text/html;charset=utf-8' }));
  try {
    const link = document.createElement('a');
    link.href = url;
    link.download = `rook-session-${session}-${result.from}-${result.through}.html`;
    document.body.append(link); link.click(); link.remove();
  } finally { setTimeout(() => URL.revokeObjectURL(url), 1000); }
  return result;
}
