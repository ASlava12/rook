import { api } from './lib.js';

// Remove the authorization code from the address/history before any further
// navigation. Only the daemon sees the callback; tokens never reach this page.
const callback = window.location.href;
history.replaceState(null, '', window.location.pathname);
const status = document.getElementById('status');
try {
  const report = await api('/api/mcp/oauth/complete', { callback });
  status.textContent = report.status === 'connected'
    ? `Signed in and connected to ${report.server}. Return to Rook.`
    : report.error || 'Sign-in did not complete. Start again in Rook.';
} catch (error) {
  status.textContent = error.error || 'Sign-in could not be completed. Start again in Rook.';
}
