// The shell: tabs, the health line, and which module draws which tab.
import { $, el, api, state, nav, errorCard } from './lib.js';
import { renderChat, continueIn } from './chat.js';
import { workspaceShell, locationState } from './shell.js';
import { drafts } from './drafts.js';
import { renderIntegrations } from './integrations.js';
import { renderTasks } from './tasks.js';
import { renderSessions } from './sessions.js';
import { renderContext } from './context.js';
import { renderPrices } from './prices.js';
import { renderSearch, renderMemory, renderSkills, renderJobs, renderStore, renderCheckpoints, renderDocs, renderSecrets } from './views.js';

const tabs = {
  chat: renderChat,
  sessions: renderSessions,
  context: renderContext,
  prices: renderPrices,
  tasks: renderTasks,
  integrations: renderIntegrations,
  search: renderSearch,
  memory: renderMemory,
  skills: renderSkills,
  jobs: renderJobs,
  checkpoints: renderCheckpoints,
  docs: renderDocs,
  secrets: renderSecrets,
  store: renderStore,
};

export function render() {
  const v = $('#view');
  nav.disposeChat();
  v.dataset.tab = state.tab;
  v.replaceChildren(el('p', { class: 'empty' }, 'loading…'));
  document.querySelectorAll('nav button[data-tab]').forEach(b => {
    b.setAttribute('aria-selected', String(b.dataset.tab === state.tab));
    if (b.dataset.tab === state.tab) b.closest('details')?.setAttribute('open','');
  });
  const selected = state.tab;
  (tabs[selected] || tabs.chat)().catch(e => { if(state.tab===selected)v.replaceChildren(errorCard(e)); });
}

export function go(tab) {
  state.tab = tabs[tab] ? tab : 'chat';
  shell.navigate();
  render();
}
nav.go = go;
const shell = workspaceShell(continueIn);
nav.chatChanged = () => shell.update();

async function boot() {
  let saved = {};
  try { const raw=sessionStorage.getItem('rook:workspace-selection');if(raw&&raw.length<8192)saved=JSON.parse(raw); } catch { /* Explicit links and a fresh session remain usable. */ }
  const wanted = locationState(location.hash);
  const previous=locationState(`#chat?${new URLSearchParams({session:saved.session||'',workspace:saved.workspace||''})}`);
  state.chat.session=location.hash.includes('?')?wanted.session:previous.session;
  state.session=state.chat.session;
  state.chat.workspace=wanted.workspace || previous.workspace;
  state.chat.draft=drafts.get(state.chat.session,state.chat.workspace);
  document.querySelectorAll('nav button[data-tab]').forEach(b => b.addEventListener('click', () => go(b.dataset.tab)));
  window.addEventListener('hashchange',()=>{
    const route=locationState(location.hash);
    if(route.tab==='chat'&&(route.session!==state.chat.session||(route.workspace&&route.workspace!==state.chat.workspace))){state.chat.nextWorkspace=route.workspace||state.chat.workspace;continueIn(route.session);}
    else go(route.tab);
  });
  shell.start();
  go(wanted.tab);
}

boot();
