import { $, el, api, state, nav, ago } from './lib.js';
import { icon } from './icons.js';

const SESSION = /^[0-7][0-9A-HJKMNP-TV-Z]{25}$/i;
const RECENT_LIMIT = 80;
const THEMES = ['system', 'light', 'dark'];
const labels = { chat:'Workspace', tasks:'Tasks', sessions:'Session history', context:'Context & usage', search:'Search project', memory:'Memory', jobs:'Background jobs', checkpoints:'Checkpoints', integrations:'API & MCP', skills:'Skills', prices:'Model pricing', secrets:'Credentials', store:'Storage', docs:'Documentation' };
const symbols = { chat:'chat', tasks:'task', sessions:'clock', context:'panel', search:'search', memory:'code', jobs:'agents', checkpoints:'branch', integrations:'link', skills:'settings', prices:'code', secrets:'settings', store:'folder', docs:'code' };
export const folderName = value => String(value || '').replace(/[\\/]+$/, '').split(/[\\/]/).at(-1) || 'Workspace';

export function locationState(fragment) {
  if (String(fragment).length > 8192) return { tab:'chat', session:null, workspace:null };
  const [tab, query = ''] = String(fragment).replace(/^#/, '').split('?');
  const params = new URLSearchParams(query), session = params.get('session'), workspace = params.get('workspace');
  return { tab: Object.hasOwn(labels, tab) ? tab : 'chat', session: SESSION.test(session || '') ? session : null,
    workspace: workspace && workspace.length <= 4096 && !/[\u0000-\u001f]/.test(workspace) ? workspace : null };
}

let toastTimer;
export function toast(text) {
  const node = $('#toast'); if (!node) return;
  node.textContent = String(text).slice(0, 1000); node.hidden = false;
  clearTimeout(toastTimer); toastTimer = setTimeout(() => { node.hidden = true; }, 5000);
}

export function workspaceShell(openSession) {
  let recent = [], attention = new Set(), online = false, loading = false, loaded = false;
  let poll = null, changeRefresh = null, query = '', selected = 0, results = [], seenSession, seenBusy, lastRefresh = 0;
  let healthWorkspace = '', theme = 'system';
  const palette = $('#command-palette'), search = $('#command-query');
  const mobileMedia=window.matchMedia('(max-width:760px)');
  const mobile = () => mobileMedia.matches;
  try { const saved = localStorage.getItem('rook:theme'); if (THEMES.includes(saved)) theme = saved; } catch { /* System default remains usable. */ }
  function applyTheme() {
    if (theme === 'system') document.documentElement.removeAttribute('data-theme');
    else document.documentElement.dataset.theme = theme;
    const next = THEMES[(THEMES.indexOf(theme) + 1) % THEMES.length];
    $('#theme-toggle').replaceChildren(icon(theme === 'dark' ? 'moon' : 'sun'));
    $('#theme-toggle').title = `Theme: ${theme}. Switch to ${next}`;
    $('#theme-toggle').setAttribute('aria-label', `Theme: ${theme}. Switch to ${next}`);
  }
  function cycleTheme() { theme = THEMES[(THEMES.indexOf(theme) + 1) % THEMES.length]; try { localStorage.setItem('rook:theme', theme); } catch { /* Theme applies for this page. */ } applyTheme(); }
  function sidebar(show) {
    document.body.classList.toggle(mobile() ? 'sidebar-open' : 'sidebar-collapsed', mobile() ? show : !show);
    $('#sidebar-toggle').setAttribute('aria-expanded', String(show));
    $('#sidebar-scrim').hidden = !show || !mobile();
  }
  function route() {
    const params = new URLSearchParams();
    const session = state.tab === 'chat' ? state.chat.session : state.session;
    if (SESSION.test(session || '')) params.set('session', session);
    if (state.chat.workspace) params.set('workspace', state.chat.workspace);
    const fragment=`#${state.tab}${params.size ? `?${params}` : ''}`;
    if(location.hash!==fragment)history.replaceState(null, '', fragment);
    try { sessionStorage.setItem('rook:workspace-selection', JSON.stringify({ session:state.chat.session, workspace:state.chat.workspace })); } catch { /* Deep links still work. */ }
  }
  function useSession(id) {
    const item = recent.find(row => row.id === id);
    if (item?.workspace) state.chat.nextWorkspace = item.workspace;
    openSession(id);
    if(mobile())sidebar(false);
  }
  function drawRecent() {
    const workspace = state.chat.workspace || healthWorkspace;
    const rows = recent.slice(0,30);
    $('#recent-sessions').replaceChildren(...rows.map(row => el('button', {
      class:'session-link', 'aria-current':String(row.id === state.chat.session), title:row.title || row.id,
      'data-session':row.id,
      onclick:()=>useSession(row.id),
    }, el('span', { class:`status-dot${attention.has(row.id) ? ' waiting' : row.id === state.chat.session && state.chat.busy ? ' running' : ''}`, 'aria-label':attention.has(row.id) ? 'Needs your attention' : row.id === state.chat.session && state.chat.busy ? 'Running' : 'Saved session' }),
    el('span', { class:'session-copy' }, el('span', { class:'session-name' }, String(row.title || 'Untitled session').slice(0, 180)),
      el('span', { class:'session-age' }, attention.has(row.id) ? 'Waiting for you' : `${ago(row.updated_at)}${row.workspace&&row.workspace!==workspace?` · ${folderName(row.workspace)}`:''}${row.parent ? ' · branch' : ''}`)))));
    if (!rows.length) $('#recent-sessions').append(el('p', { class:'sidebar-empty' }, loaded ? 'Your sessions will appear here.' : 'Loading sessions…'));
  }
  function update() {
    const session = state.chat.session, item = recent.find(row => row.id === session);
    $('#workspace-name').textContent = folderName(state.chat.workspace || healthWorkspace);
    $('#workspace-location').title = state.chat.workspace || healthWorkspace || 'Choose a project';
    $('#session-title').textContent = state.tab === 'chat' ? item?.title || (session ? 'Session' : 'New session') : labels[state.tab];
    const status = $('#session-state');
    const disconnected=!state.chat.connected;
    status.className = `session-state${disconnected?' offline':state.chat.waiting ? ' waiting' : state.chat.busy ? ' running' : ''}`;
    status.textContent = disconnected?(state.chat.connection==='project'?'Choose folder':state.chat.connection==='offline'?'Offline':'Connecting'):state.chat.waiting ? 'Needs your attention' : state.chat.busy ? 'Working' : 'Ready';
    $('#copy-session-link').disabled = !session;
    $('#inspector-toggle').hidden = state.tab !== 'chat';
    if (session && state.tab==='chat') state.session = session;
    route(); drawRecent();
    if (seenSession !== session || seenBusy !== state.chat.busy) {
      seenSession = session; seenBusy = state.chat.busy;
      clearTimeout(changeRefresh);
      changeRefresh=setTimeout(()=>refresh(),Math.max(0,1200-(Date.now()-lastRefresh)));
      schedule();
    }
  }
  async function refresh() {
    if (loading) return;
    loading = true; lastRefresh = Date.now();
    const controller = new AbortController(), timeout = setTimeout(() => controller.abort(), 15000);
    try {
      const values = await Promise.allSettled([api('/api/sessions?tree=true', undefined, controller.signal), api('/api/attention?limit=80', undefined, controller.signal), api('/api/health', undefined, controller.signal)]);
      if (values[0].status === 'fulfilled') {
        recent = values[0].value.items.slice().sort((a,b)=>b.updated_at-a.updated_at).slice(0,RECENT_LIMIT); loaded = true;
        state.chat.sessions = recent;
      }
      if (values[1].status === 'fulfilled') attention = new Set(values[1].value.items.filter(row=>row.live).slice(0,80).map(row=>row.session));
      online = values[2].status === 'fulfilled';
      if (online) {
        const health = values[2].value; healthWorkspace = health.workspace;
        if (!state.chat.workspace) state.chat.workspace = health.workspace;
        $('#meta').textContent = `Local daemon · v${health.version}`;
        $('#meta').title = `${health.os}/${health.arch} · ${health.workspace}`;
      } else $('#meta').textContent = 'Daemon unavailable · retry';
      $('#daemon-dot').classList.toggle('online', online);
      update(); if (palette.open) drawCommands();
    } finally { clearTimeout(timeout); loading = false; }
  }
  function schedule() {
    clearTimeout(poll);
    poll = setTimeout(async()=>{ if (!document.hidden) await refresh(); schedule(); }, state.chat.busy ? 5000 : 20000);
  }
  function closePalette() { palette.close(); $('#chat-input')?.focus(); }
  function openPalette() { if (!palette.open) palette.showModal(); search.value=''; query=''; selected=0; drawCommands(); search.focus(); }
  function commands() {
    return [
      { label:'New session', detail:'Start a conversation in this project', symbol:'plus', action:()=>useSession(null) },
      { label:'Choose project', detail:'Open a folder on this computer', symbol:'folder', action:()=>projectDialog() },
      { label:'Show session details', detail:'Changes, agents, context and session tools', symbol:'panel', action:()=>nav.inspect() },
      { label:'Change theme', detail:`Current: ${theme}`, symbol:'sun', action:cycleTheme },
      ...Object.entries(labels).map(([tab,label])=>({label,detail:'Go to view',symbol:symbols[tab],action:()=>nav.go(tab)})),
      ...recent.map(row=>({ label:String(row.title || 'Untitled session').slice(0,180), detail:`${folderName(row.workspace)} · ${ago(row.updated_at)}`, symbol:'chat', action:()=>useSession(row.id) })),
    ].filter(row=>`${row.label} ${row.detail}`.toLowerCase().includes(query.toLowerCase())).slice(0,50);
  }
  function drawCommands() {
    results = commands(); selected = Math.min(selected,Math.max(0,results.length-1));
    $('#command-results').replaceChildren(...results.map((row,index)=>el('button',{
      class:'command-result',role:'option','aria-selected':String(index===selected),id:`command-${index}`,tabindex:'-1',
      onclick:()=>{closePalette();row.action();},
    },icon(row.symbol),el('span',{class:'command-copy'},el('span',{},row.label),el('small',{},row.detail)))));
    search.setAttribute('aria-activedescendant',results.length?`command-${selected}`:'');
    if (!results.length) $('#command-results').append(el('p',{class:'empty'},'No matching sessions or commands.'));
  }
  function projectDialog() {
    const dialog=$('#project-dialog'), input=$('#project-path');
    input.value=state.chat.workspace || healthWorkspace; $('#project-error').textContent='';
    const projects=[...new Set([healthWorkspace,...recent.map(row=>row.workspace)].filter(Boolean))].slice(0,12);
    $('#recent-projects').replaceChildren(...projects.map(value=>el('button',{type:'button',onclick:()=>{input.value=value;input.focus();}},value)));
    if (!dialog.open) dialog.showModal(); input.focus(); input.select();
  }
  $('#project-form').addEventListener('submit',async event=>{
    event.preventDefault(); const input=$('#project-path'), value=input.value.trim(), button=event.submitter;
    if (!value || value.length>4096 || /[\u0000-\u001f]/.test(value)) return;
    if (button) button.disabled=true;
    try {
      await api(`/api/files?limit=1&workspace=${encodeURIComponent(value)}`);
      state.chat.nextWorkspace=value;
      if(openSession(null)===false){$('#project-error').textContent='Send or remove the selected attachments before changing projects. Your draft is retained.';return;}
      if (state.chat.workspace!==value) return;
      $('#project-dialog').close(); if(mobile())sidebar(false); update();
    } catch(error) { $('#project-error').textContent=error.error || error.message || String(error); }
    finally { if(button)button.disabled=false; }
  });
  $('#project-cancel').onclick=()=>$('#project-dialog').close();
  $('#workspace-location').onclick=projectDialog;
  $('#quick-find').onclick=openPalette;
  $('#new-session').onclick=()=>useSession(null);
  $('#refresh-sessions').onclick=()=>refresh();
  $('#theme-toggle').onclick=cycleTheme;
  $('#sidebar-toggle').onclick=()=>sidebar(mobile()?!document.body.classList.contains('sidebar-open'):document.body.classList.contains('sidebar-collapsed'));
  $('#sidebar-close').onclick=$('#sidebar-scrim').onclick=()=>sidebar(false);
  $('#copy-session-link').onclick=async()=>{ try { await navigator.clipboard.writeText(location.href); toast('Session link copied'); } catch { toast('Clipboard unavailable. Copy the address from your browser.'); } };
  $('#inspector-toggle').onclick=()=>nav.inspect();
  search.addEventListener('input',()=>{query=search.value;selected=0;drawCommands();});
  search.addEventListener('keydown',event=>{
    if(event.key==='ArrowDown'||event.key==='ArrowUp') {event.preventDefault();selected=(selected+(event.key==='ArrowDown'?1:-1)+Math.max(1,results.length))%Math.max(1,results.length);drawCommands();$(`#command-${selected}`)?.scrollIntoView({block:'nearest'});}
    else if(event.key==='Enter'&&results[selected]){event.preventDefault();const action=results[selected].action;closePalette();action();}
  });
  palette.addEventListener('click',event=>{if(event.target===palette)closePalette();});
  document.addEventListener('keydown',event=>{
    if((event.ctrlKey||event.metaKey)&&event.key.toLowerCase()==='k'){event.preventDefault();openPalette();}
    else if((event.ctrlKey||event.metaKey)&&event.shiftKey&&event.key.toLowerCase()==='o'){event.preventDefault();useSession(null);}
    else if(event.altKey&&event.key.toLowerCase()==='i'){event.preventDefault();nav.inspect();}
    else if(event.key==='Escape'&&document.body.classList.contains('sidebar-open')){event.preventDefault();sidebar(false);}
  });
  document.addEventListener('visibilitychange',()=>{if(!document.hidden)refresh();});
  window.addEventListener('pagehide',()=>{clearTimeout(poll);clearTimeout(changeRefresh);});
  window.addEventListener('pageshow',()=>schedule());
  $('#brand-icon').append(icon('rook')); $('#workspace-icon').append(icon('folder'));
  $('#sidebar-toggle').append(icon('panel')); $('#copy-session-link').append(icon('link')); $('#inspector-toggle').append(icon('changes'));
  $('#new-session').prepend(icon('plus')); $('#quick-find').prepend(icon('search'));
  document.querySelectorAll('nav button[data-tab]').forEach(button=>button.prepend(icon(symbols[button.dataset.tab])));
  applyTheme(); sidebar(!mobile());
  mobileMedia.addEventListener('change',()=>sidebar(!mobile()));
  search.setAttribute('role','combobox');search.setAttribute('aria-controls','command-results');search.setAttribute('aria-expanded','true');search.setAttribute('aria-autocomplete','list');
  return { update, refresh, start:()=>{refresh();schedule();}, navigate:()=>{if(mobile())sidebar(false);update();} };
}
