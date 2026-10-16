import {el,api,jsonWithin,state} from './lib.js';

export function appHost(owner,opened,prepareDraft) {
 const root=el('section',{class:'mcp-app-host'});
 const pending=el('div',{class:'mcp-app-approval',role:'status'});
 const status=el('p',{class:'sub'},`${opened.server} · ${opened.uri} · connection ${opened.generation}`);
 const frame=el('iframe',{src:'/mcp-app-sandbox.html',sandbox:'allow-scripts',referrerpolicy:'no-referrer',
  allow:"camera 'none'; microphone 'none'; geolocation 'none'; clipboard-write 'none'",title:`MCP App: ${opened.server}`,style:'width:100%;height:480px;border:1px solid var(--line)'});
 let closed=false,loaded=false,initialized=false,busy=false,requests=0,windowStart=Date.now();
 let waiting=null,silenced=false;
 const endpoint=`/api/sessions/${encodeURIComponent(owner)}/mcp-apps`;
 const reply=(id,result,error)=>{if(!closed)frame.contentWindow?.postMessage({jsonrpc:'2.0',id,...(error?{error:{code:-32000,message:error}}:{result})},'*');};
 async function close(){
  if(closed)return;closed=true;window.removeEventListener('message',receive);observer.disconnect();
  if(waiting?.nonce)api(endpoint,{action:'confirm',app:opened.app,nonce:waiting.nonce,approved:false}).catch(()=>{});
  waiting=null;frame.remove();pending.replaceChildren();status.textContent+=' · closed';root.remove();
  await api(endpoint,{action:'close',app:opened.app}).catch(()=>{});
 }
 async function confirmCall(id,request,approved){
  if(waiting!==request||closed)return;
  if(!approved)silenced=true;
  waiting=null;pending.replaceChildren();status.textContent=`${opened.server} · ${opened.uri} · ${approved ? 'Running approved call…' : 'Declined; close and reopen to allow new requests.'}`;
  try{const result=await api(endpoint,{action:'confirm',app:opened.app,nonce:request.nonce,approved});reply(id,result);}
  catch(error){reply(id,null,error.error||String(error));status.textContent='App call did not complete; inspect its effects before reopening.';}
  busy=false;
 }
 async function receive(event){
  if(closed||event.source!==frame.contentWindow||event.origin!=='null'||!root.isConnected)return;
  const message=event.data;
  if(!message||message.jsonrpc!=='2.0'||!jsonWithin(message,65536))return;
  if(Date.now()-windowStart>60000){windowStart=Date.now();requests=0;}
  if(++requests>64){close();return;}
  const {id,method,params}=message;
  if(method==='ui/notifications/sandbox-proxy-ready'&&!loaded){loaded=true;frame.contentWindow.postMessage({jsonrpc:'2.0',method:'ui/notifications/sandbox-resource-ready',params:{html:opened.html}},'*');return;}
  if(method==='ui/initialize'&&!initialized){
   if(params?.protocolVersion!=='2026-01-26'){reply(id,null,'Unsupported MCP Apps protocol version');return;}
   initialized=true;reply(id,{protocolVersion:'2026-01-26',hostInfo:{name:'rook',version:opened.version || 'unknown'},hostCapabilities:{serverTools:{},sandbox:{permissions:{},csp:opened.sandbox}},hostContext:{displayMode:'inline',platform:'web',theme:matchMedia('(prefers-color-scheme: dark)').matches?'dark':'light',containerDimensions:{width:root.clientWidth,maxHeight:480}}});return;
  }
  if(method==='ui/notifications/initialized'||method==='notifications/message')return;
  if(id===undefined)return;
  if(!initialized){reply(id,null,'Initialize the app first');return;}
  if(!['tools/call','ui/message'].includes(method)){reply(id,null,'This host does not support that app capability');return;}
  if(silenced){reply(id,null,'Requests were declined or expired; close and explicitly reopen the app.');return;}
  if(busy){reply(id,null,'Answer or dismiss the current app request first');return;}
  busy=true;
  if(method==='ui/message'){
   const content=params?.content;
   if(!Array.isArray(content)||content.length>8||content.some(item=>item.type!=='text'||typeof item.text!=='string')||!jsonWithin(content,8192)){busy=false;reply(id,null,'Only bounded text messages are supported');return;}
   pending.replaceChildren(el('p',{},`${opened.server} requests a message in your draft:`),el('pre',{},content.map(item=>item.text).join('\n')),
    el('button',{type:'button',onclick:()=>{if(closed)return;if(state.chat.session!==owner||state.chat.busy){reply(id,null,'Open the owning idle conversation first');}else{prepareDraft(content.map(item=>item.text).join('\n'));reply(id,{});}pending.replaceChildren();busy=false;}},'Put in draft'),
    el('button',{type:'button',onclick:()=>{pending.replaceChildren();busy=false;silenced=true;reply(id,null,'User declined');}},'Dismiss'));
   return;
  }
  try{
   const request=await api(endpoint,{action:'prepare',app:opened.app,tool:params?.name,arguments:params?.arguments||{}});
   if(closed){api(endpoint,{action:'confirm',app:opened.app,nonce:request.nonce,approved:false}).catch(()=>{});return;}
   waiting=request;
   pending.replaceChildren(el('p',{},`${request.server} requests ${request.tool} in this conversation:`),el('pre',{},request.arguments),
    el('button',{type:'button',onclick:()=>confirmCall(id,request,true)},'Allow this call'),
    el('button',{type:'button',onclick:()=>confirmCall(id,request,false)},'Decline'));
   setTimeout(()=>{if(waiting===request)confirmCall(id,request,false);},120000);
  }catch(error){busy=false;reply(id,null,error.error||String(error));}
 }
 const observer=new MutationObserver(()=>{if(!root.isConnected)close();});
 observer.observe(document.body,{childList:true,subtree:true});
 window.addEventListener('message',receive);
 root.append(status,el('button',{type:'button',onclick:close},'Close app'),frame,pending);
 return root;
}

export function appsPanel(owner,prepareDraft){
 const root=el('details',{},el('summary',{},'MCP Apps'));
 const list=el('div'),views=el('div');let epoch=0;
 root.addEventListener('toggle',async()=>{
  const token=++epoch;if(!root.open)return;
  try{
   const reply=await api(`/api/sessions/${encodeURIComponent(owner)}/mcp-apps`,{action:'list'});
   if(token!==epoch||!root.isConnected)return;
   list.replaceChildren(...reply.items.map(item=>el('div',{class:'row'},el('span',{},`${item.server} · ${item.uri}`),el('button',{type:'button',onclick:async()=>{
    if(views.childElementCount>=4){list.append(el('p',{},'Close an existing view first.'));return;}
    try{const opened=await api(`/api/sessions/${encodeURIComponent(owner)}/mcp-apps`,{action:'open',server:item.server,uri:item.uri,generation:item.generation});
     if(!root.isConnected||token!==epoch){api(`/api/sessions/${encodeURIComponent(owner)}/mcp-apps`,{action:'close',app:opened.app}).catch(()=>{});return;}
     views.append(appHost(owner,opened,prepareDraft));
    }catch(error){list.append(el('p',{class:'warn'},error.error||String(error)));}
   }},'Open app'),el('p',{class:'sub'},item.fallback))));
   if(!reply.items.length)list.append(el('p',{class:'sub'},'No configured tools declare an interactive app.'));
  }catch(error){list.replaceChildren(el('p',{class:'sub'},error.error||String(error)));}
 });
 root.append(list,views);return root;
}
