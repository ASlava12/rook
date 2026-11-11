import { $, el, api, state, nav } from './lib.js';
import { icon } from './icons.js';
import { diffViewer } from './diff-view.js';
import { backgroundPanel } from './background.js';

const textError = error => error?.error || error?.message || String(error);
const compact = value => Number(value || 0).toLocaleString();

// One owner and one outstanding inspector request. A late result cannot repaint
// the next session; opening the inspector never starts a model or captures a diff.
export function workbench(tools, putDraft) {
  let owner = null, tab = 'changes', pending = null, epoch = 0, timer = null, disposed = false;
  let shown = false, chosenVisibility = false, previousBusy = false, review = null;
  const panels = Object.fromEntries(['changes','agents','context','tools'].map(name=>[name,el('div',{hidden:name!==tab})]));
  panels.tools.className='chat-tools'; panels.tools.append(...tools);
  const title = el('span',{},'Session details');
  const expand = el('button',{class:'icon-button',type:'button','aria-label':'Expand session details',onclick:()=>{
    const wide = root.dataset.wide!=='true'; root.dataset.wide=String(wide); expand.setAttribute('aria-pressed',String(wide));
  }},icon('panel'));
  const tabs = ['changes','agents','context','tools'].map(name=>el('button',{
    class:'inspector-tab',type:'button',role:'tab',tabindex:name===tab?'0':'-1','aria-selected':String(name===tab),onclick:()=>select(name),
  },name[0].toUpperCase()+name.slice(1)));
  const root=el('aside',{class:'inspector',hidden:true,'aria-label':'Session details'},
    el('div',{class:'inspector-head'},title,el('div',{},expand,el('button',{class:'icon-button',type:'button','aria-label':'Close session details',onclick:()=>toggle(false)},icon('close')))),
    el('div',{class:'inspector-tabs',role:'tablist','aria-label':'Session detail views',onkeydown:event=>{
      const current=tabs.indexOf(document.activeElement);
      if(current<0||!['ArrowLeft','ArrowRight','Home','End'].includes(event.key))return;
      event.preventDefault();const index=event.key==='Home'?0:event.key==='End'?tabs.length-1:(current+(event.key==='ArrowRight'?1:-1)+tabs.length)%tabs.length;
      select(['changes','agents','context','tools'][index]);tabs[index].focus();
    }},...tabs),
    el('div',{class:'inspector-content'},...Object.values(panels)));
  function owns(token, session, name=tab) { return !disposed&&token===epoch&&session===state.chat.session&&session===owner&&name===tab&&shown&&root.isConnected; }
  function empty(panel,symbol,heading,description) { panel.replaceChildren(el('div',{class:'inspector-empty'},icon(symbol),el('p',{},heading),el('p',{class:'muted'},description))); }
  function cancel() { epoch++; pending?.controller.abort(); pending=null; clearTimeout(timer); }
  function select(name) {
    cancel();tab=name; tabs.forEach((button,index)=>{const selected=['changes','agents','context','tools'][index]===name;button.setAttribute('aria-selected',String(selected));button.setAttribute('tabindex',selected?'0':'-1');});
    Object.entries(panels).forEach(([key,node])=>{node.hidden=key!==name;});
    if(name==='tools')return;
    load();
  }
  function toggle(value=!shown) {
    chosenVisibility=true;shown=value;root.hidden=!shown;
    $('#inspector-toggle')?.setAttribute('aria-expanded',String(shown));
    if(shown)load();else{cancel();$('#chat-input')?.focus();}
  }
  function schedule() {
    clearTimeout(timer);
    if(!disposed&&shown&&state.chat.busy&&['changes','context'].includes(tab)) timer=setTimeout(()=>load(),5000);
  }
  async function request(path, body, session, name, apply) {
    if(pending)return;
    const token=++epoch, controller=new AbortController(), timeout=setTimeout(()=>controller.abort(),15000);
    const request={controller};pending=request;
    try {
      const result=await api(path,body,controller.signal);
      if(owns(token,session,name))apply(result);
    } catch(error) { if(owns(token,session,name)&&error?.name!=='AbortError')panels[name].replaceChildren(el('p',{class:'bad',role:'alert'},textError(error)),el('button',{onclick:()=>load()},'Try again')); }
    finally {clearTimeout(timeout);if(pending===request)pending=null;schedule();}
  }
  function toolbar(panel,label) {
    const button=el('button',{class:'icon-button',type:'button','aria-label':`Refresh ${tab}`,onclick:()=>{review=null;load();}},icon('refresh'));
    panel.append(el('div',{class:'inspector-toolbar'},el('span',{},label),button));
  }
  function drawChanges(changes, session) {
    const panel=panels.changes,files=(changes.files||[]).slice(0,256);
    panel.replaceChildren();toolbar(panel,'Changes recorded for this session');
    if(!files.length)empty(panel,'changes','No file edits yet','Changes made by this session will appear here.');
    else {
      panel.append(el('div',{class:'change-summary'},el('span',{},`${files.length} changed ${files.length===1?'file':'files'}`),el('span',{},`+${files.reduce((n,file)=>n+file.lines_added,0)} / −${files.reduce((n,file)=>n+file.lines_removed,0)}`)),
        ...files.map(file=>el('button',{class:'file-change',type:'button',onclick:()=>capture(session,[file.path])},icon('changes'),el('span',{class:'file-name'},file.path),el('span',{class:'added'},`+${file.lines_added}`),el('span',{class:'removed'},`−${file.lines_removed}`))),
        el('div',{class:'row'},el('button',{type:'button',onclick:()=>capture(session,files.map(file=>file.path))},'Review these changes')));
    }
    if(changes.files?.length>files.length)panel.append(el('p',{class:'muted'},'More paths are available in Session history.'));
    if(changes.watched===false)panel.append(el('p',{class:'warn'},'Workspace scan was limited; additional command changes may be unrecorded.'));
    if(changes.written_by_commands?.length)panel.append(el('details',{},el('summary',{},'Paths observed after commands'),el('p',{class:'muted'},'These observations do not provide a saved diff.'),...changes.written_by_commands.slice(0,128).map(path=>el('p',{class:'sub'},path))));
  }
  function capture(session, paths) {
    request(`/api/sessions/${encodeURIComponent(session)}/reviews`,{action:'capture',selection:{target:'working',paths}},session,'changes',reply=>{review=reply;showReview(session,reply);});
  }
  function showReview(session,saved) {
    const panel=panels.changes;panel.replaceChildren();
    panel.append(el('div',{class:'inspector-toolbar'},el('button',{onclick:()=>{review=null;load();}},'← Changed files'),el('span',{},`Saved source ${saved.source?.slice(0,8)}`)));
    if(!saved.files?.length){panel.append(el('p',{class:'muted'},saved.error||'There is no textual patch for this selection.'));return;}
    const notice=el('p',{class:'muted',role:'status'}), selected=el('p',{class:'muted'},'Select a line number to attach feedback.');
    const comment=el('textarea',{rows:3,maxlength:8192,'aria-label':'Line feedback',placeholder:'What should be changed on this line?'});
    let line=null;
    const save=el('button',{type:'button',disabled:true,onclick:async()=>{
      if(!line||!comment.value.trim())return;save.disabled=true;
      const source=saved.source, token=epoch;
      try {
        await api(`/api/sessions/${encodeURIComponent(session)}/reviews`,{action:'comment',review:saved.id,comment:{source,...line,text:comment.value}});
        if(owns(token,session,'changes')){notice.textContent='Line feedback saved. Add it to your message when ready.';comment.value='';}
      }catch(error){if(owns(token,session,'changes'))notice.textContent=textError(error);}
      finally{if(owns(token,session,'changes'))save.disabled=!line;}
    }},'Save line feedback');
    const viewer=diffViewer(session,saved,(path,side,number)=>{line={path,side,line:number};selected.textContent=`${path}:${number} · ${side}`;save.disabled=false;comment.focus();});
    queueMicrotask(()=>{if(viewer.isConnected)viewer.querySelector('.diff-files button')?.click();});
    panel.append(viewer,el('div',{class:'feedback'},selected,comment,save,el('button',{type:'button',onclick:()=>{
      request(`/api/sessions/${encodeURIComponent(session)}/reviews`,{action:'feedback',review:saved.id,source:saved.source},session,'changes',reply=>{putDraft(session,reply.prompt);notice.textContent='Feedback added to your draft. Review it before sending.';});
    }},'Add feedback to message'),notice));
  }
  function drawContext(usage) {
    const panel=panels.context, percent=Math.min(100,Math.round(100*usage.live_tokens/Math.max(1,usage.usable)));
    const coverage=usage.cost_coverage;
    panel.replaceChildren();toolbar(panel,'Context & recorded usage');
    const pair=(label,value)=>el('div',{class:'context-pair'},el('span',{},label),el('strong',{},String(value)));
    panel.append(el('div',{class:'context-number'},`${percent}%`,el('span',{class:'muted'},' of usable context')),
      el('div',{class:'context-meter',role:'progressbar','aria-label':'Context used','aria-valuenow':percent,'aria-valuemin':0,'aria-valuemax':100},el('span',{style:`width:${percent}%`})),
      pair('Live estimate',`${compact(usage.live_tokens)} / ${compact(usage.usable)} tokens`),pair('Context window',compact(usage.window)),pair('Compactions',usage.compactions||0));
    if(usage.needs_compaction)panel.append(el('p',{class:'warn'},'Context will compact on the next turn.'));
    const receipt=usage.last_response?.receipt;
    if(receipt)panel.append(pair('Selected model',receipt.selected||'Unknown'),pair('Dispatched model',receipt.dispatch?.model||'Unknown'));
    if(coverage)panel.append(pair('Known response subtotal',coverage.known_subtotal_usd==null?'Unknown':`$${Number(coverage.known_subtotal_usd).toFixed(6)}`),
      el('p',{class:'muted'},'Configured-rate estimate for priced receipts. Coverage may be partial; total spend can be unknown.'));
    panel.append(el('button',{type:'button',onclick:()=>{state.session=owner;nav.go('context');}},'View full context & cost coverage'));
  }
  function load() {
    if(disposed||!shown||tab==='tools'||pending)return;
    const session=state.chat.session;
    if(!session){empty(panels[tab],tab==='agents'?'agents':'changes','Start a session','Send a message to see the session’s changes, agents and context.');return;}
    if(tab==='agents'){panels.agents.replaceChildren(backgroundPanel(session));return;}
    if(tab==='changes'&&review)return;
    request(`/api/sessions/${encodeURIComponent(session)}/${tab==='changes'?'changes':'context'}`,undefined,session,tab,reply=>tab==='changes'?drawChanges(reply,session):drawContext(reply));
  }
  function update() {
    const session=state.chat.session, changed=session!==owner;
    if(changed){cancel();owner=session;review=null;Object.entries(panels).forEach(([name,node])=>{if(name!=='tools')node.replaceChildren();});
      if(!chosenVisibility)shown=!!session&&!window.matchMedia('(max-width:1180px)').matches;
      root.hidden=!shown;$('#inspector-toggle')?.setAttribute('aria-expanded',String(shown));}
    if(changed||previousBusy!==state.chat.busy){if(tab==='changes')review=null;load();}
    root.hidden=!shown;$('#inspector-toggle')?.setAttribute('aria-expanded',String(shown));
    previousBusy=state.chat.busy;schedule();
  }
  root.addEventListener('keydown',event=>{if(event.key==='Escape'&&!event.defaultPrevented){event.preventDefault();toggle(false);}});
  return {root,update,toggle,dispose:()=>{disposed=true;cancel();}};
}
