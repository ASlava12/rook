// The shell: tabs, the health line, and which module draws which tab.
import { $, el, api, state, nav, errorCard } from './lib.js';
import { renderChat } from './chat.js';
import { renderSessions } from './sessions.js';
import { renderSearch, renderMemory, renderSkills, renderJobs, renderStore } from './views.js';

const tabs = {
  chat: renderChat,
  sessions: renderSessions,
  search: renderSearch,
  memory: renderMemory,
  skills: renderSkills,
  jobs: renderJobs,
  store: renderStore,
};

export function render() {
  const v = $('#view');
  v.replaceChildren(el('p', { class: 'empty' }, 'loading…'));
  document.querySelectorAll('nav button').forEach(b =>
    b.setAttribute('aria-selected', String(b.dataset.tab === state.tab)));
  (tabs[state.tab] || tabs.chat)().catch(e => v.replaceChildren(errorCard(e)));
}

export function go(tab) {
  state.tab = tabs[tab] ? tab : 'chat';
  history.replaceState(null, '', `#${state.tab}`);
  render();
}
nav.go = go;

async function boot() {
  try {
    const h = await api('/api/health');
    $('#meta').textContent = `v${h.version} · ${h.os}/${h.arch} · ${h.workspace}`;
  } catch {
    $('#meta').textContent = 'backend unreachable';
  }
  document.querySelectorAll('nav button').forEach(b => b.addEventListener('click', () => go(b.dataset.tab)));
  // The tab is in the fragment, so a reload lands where it left and a link
  // to the sessions tab is a link.
  const wanted = location.hash.replace(/^#/, '');
  go(tabs[wanted] ? wanted : 'chat');
}

boot();
