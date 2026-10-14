import { el, api, state, nav } from './lib.js';
export function recipesPanel(session) {
  const content=el('div'),panel=el('section',{},el('h2',{},'Composite recipes'),
    el('p',{},'Selected recipes can run named stages in separate sessions. Interrupted stages require review; recovery does not replay them.'));
  let busy=false,epoch=0;
  const button=(label,action)=>el('button',{onclick:()=>request(action)},label);
  const open=child=>el('button',{onclick:()=>{state.session=child;nav.go('sessions');}},'Open stage session');
  async function request(action){
    if(busy)return;
    if(action.action==='resolve'){const note=prompt('Explain what you reviewed in the original child sessions. This resolves the recipe receipt and does not acknowledge or repeat their original operations.');if(!note?.trim()||new TextEncoder().encode(note).length>512)return;action={...action,note};}
    if(action.action==='forget'&&!confirm('Forget this resolved recipe receipt? Original source and child session history remain.'))return;
    busy=true;const generation=++epoch;content.replaceChildren(el('p',{},'Reading recipe receipts…'));
    try{
      const reply=await api(`/api/sessions/${session}/recipes`,action);
      if(generation!==epoch||!panel.isConnected)return;
      if(reply.calls){content.replaceChildren(...reply.calls.map(c=>el('article',{},el('p',{},`${c.id} · ${c.mode} · ${c.state}`),button('Inspect composition',{action:'inspect',id:c.id}))),...(reply.after?[button('More compositions',{action:'list',after:reply.after})]:[]));}
      else if(reply.bytes_base64!=null){const bytes=Uint8Array.from(atob(reply.bytes_base64),c=>c.charCodeAt(0));content.replaceChildren(el('pre',{},new TextDecoder().decode(bytes)),...(reply.next_offset!=null?[button('Next source page',{...action,offset:reply.next_offset})]:[]),button('Inspect composition',{action:'inspect',id:reply.id}));}
      else if(reply.stages){
        const rows=reply.stages.map(s=>el('article',{},el('p',{},`${s.id} · ${s.recipe} · ${s.state}`),...(s.child?[open(s.child)]:[]),el('p',{},s.error||''),
          button('Read stage parameters',{action:'parameters',id:reply.id,stage:s.id,offset:0}),...(s.output!=null?[button('Read stage output',{action:'read',id:reply.id,seq:s.output,offset:0})]:[])));
        const controls=[];if(reply.state==='interrupted')controls.push(button('Resolve reviewed interruption',{action:'resolve',id:reply.id}));if(['completed','failed','reviewed'].includes(reply.state))controls.push(button('Forget resolved composition',{action:'forget',id:reply.id}));
        content.replaceChildren(el('p',{},`${reply.id} · ${reply.mode} · ${reply.state}`),...rows,...(reply.definitions||[]).map(seq=>button(`Read definition #${seq}`,{action:'read',id:reply.id,seq,offset:0})),...controls);
      }else content.replaceChildren(el('pre',{},JSON.stringify(reply,null,2)));
    }catch(e){if(generation===epoch&&panel.isConnected)content.replaceChildren(el('p',{class:'warn'},e.error||String(e)));}
    finally{if(generation===epoch)busy=false;}
  }
  panel.append(button('List compositions',{action:'list',after:null}),content);return panel;
}
