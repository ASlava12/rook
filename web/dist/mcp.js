// The panel controls the same workspace equipment that serves chat turns.
import { el, api } from './lib.js';

export function mcpPanel(session) {
  const query = session ? `?session=${encodeURIComponent(session)}` : '';
  const root = el('section', { 'aria-label': 'MCP connections' });
  const notice = el('p', { role: 'status', 'aria-live': 'polite' });
  const rows = el('div');
  const consent = el('div', { 'aria-label': 'MCP sign-ins' });
  let watching = false;
  let pending = false;
  function busy(value) {
    pending = value;
    root.setAttribute('aria-busy', String(value));
    for (const button of root.querySelectorAll('button')) button.disabled = value;
  }
  function display(report) {
    rows.replaceChildren(...report.servers.map(server => el('article', { class: 'entry' },
      el('strong', {}, server.name),
      el('p', {}, `${server.state}${server.reconnecting ? ' (reconnecting)' : ''} · ${server.tools} tools · ${server.active_requests} active requests · generation ${server.generation}`),
      server.error ? el('p', { class: 'warn' }, server.error) : null,
      server.last_request_error ? el('p', { class: 'warn' }, `Last request: ${server.last_request_error}`) : null,
      el('button', { type: 'button', onclick: () => load(server.name) }, `Reconnect ${server.name}`),
      server.transport === 'http' ? el('button', { type: 'button', onclick: () => login(server.name) }, `Sign in to ${server.name}`) : null,
      server.transport === 'http' ? el('button', { type: 'button', onclick: () => logout(server.name) }, `Sign out of ${server.name}`) : null)));
    if (!report.servers.length) rows.append(el('p', {}, 'No MCP servers installed. Add one with rook config edit.'));
    if (report.issue) notice.textContent = report.issue;
  }
  async function load(name) {
    if (pending) return;
    busy(true);
    notice.textContent = name ? 'Reconnecting; active turns keep their current tools…' : 'Reading MCP connections…';
    try {
      const report = name
        ? await api(`/api/mcp/${encodeURIComponent(name)}/reconnect${query}`, {})
        : await api(`/api/mcp${query}`);
      if (root.isConnected) { notice.textContent = ''; display(report); }
    } catch (error) {
      if (root.isConnected) notice.textContent = error.error || String(error);
    } finally { busy(false); }
  }
  async function login(name) {
    if (pending) return;
    busy(true);
    notice.textContent = 'Preparing sign-in…';
    try {
      await api(`/api/mcp/${encodeURIComponent(name)}/login${query}`, {
        redirect_uri: `${location.origin}/mcp-oauth-callback.html`,
      });
      notice.textContent = 'Open the sign-in link below to authorize this server.';
    } catch (error) { notice.textContent = error.error || String(error); }
    finally { busy(false); watch(); }
  }
  async function logout(name) {
    if (pending) return;
    busy(true);
    try {
      await api(`/api/mcp/${encodeURIComponent(name)}/logout${query}`, {});
      notice.textContent = 'Saved credentials removed. Already dispatched requests may finish.';
    } catch (error) { notice.textContent = error.error || String(error); }
    finally { busy(false); watch(); }
  }
  async function cancel(id) {
    try {
      const response = await fetch(`/api/mcp/oauth/${encodeURIComponent(id)}`, { method: 'DELETE' });
      if (!response.ok) throw await response.json();
      await signIns();
    } catch (error) { notice.textContent = error.error || String(error); }
  }
  async function signIns() {
    const attempts = await api(`/api/mcp/oauth${query}`);
    if (!root.isConnected) return false;
    consent.replaceChildren(...attempts.map(attempt => el('article', { class: 'entry' },
      el('strong', {}, `${attempt.server}: ${attempt.status}`),
      attempt.error ? el('p', { class: 'warn' }, attempt.error) : null,
      attempt.authorization_url ? el('p', {}, el('a', {
        href: attempt.authorization_url, target: '_blank', rel: 'noopener noreferrer',
      }, `Open sign-in for ${attempt.server}`)) : null,
      ['starting', 'waiting', 'completing'].includes(attempt.status)
        ? el('p', {}, `Expires in ${attempt.expires_in} seconds.`) : null,
      attempt.status !== 'completing' ? el('button', { type: 'button', onclick: () => cancel(attempt.id) },
        attempt.status === 'waiting' ? `Cancel sign-in for ${attempt.server}` : `Dismiss ${attempt.server} sign-in`) : null)));
    return attempts.some(attempt => ['starting', 'waiting', 'completing'].includes(attempt.status));
  }
  async function watch() {
    if (watching || !root.isConnected) return;
    watching = true;
    try {
      if (await signIns()) {
        setTimeout(() => { watching = false; watch(); }, 2000);
        return;
      }
      // A callback installs the new connection. Refresh its tool count too.
      const report = await api(`/api/mcp${query}`);
      if (root.isConnected) display(report);
    } catch (error) { if (root.isConnected) notice.textContent = error.error || String(error); }
    watching = false;
  }
  const name = el('input', { 'aria-label': 'Configured MCP server name', maxlength: 256, placeholder: 'Server name from config' });
  const form = el('form', { class: 'row', onsubmit: event => {
    event.preventDefault();
    if (name.value.trim()) load(name.value.trim());
  } }, name, el('button', { type: 'submit' }, 'Connect from config'));
  root.append(el('p', { class: 'sub' }, 'Connection changes apply to future turns. Active requests shown belong to the currently installed connection.'),
    el('button', { type: 'button', onclick: () => load() }, 'Refresh MCP status'), form, notice, consent, rows);
  // Give the caller a chance to mount the panel before applying the answer.
  queueMicrotask(() => { load(); watch(); });
  return root;
}
