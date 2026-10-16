import {el,api} from './lib.js';

// A hunk page remains source addressed. Never compare against live workspace text.
export function patchRows(patch) {
  const rows=[]; let before=null,after=null;
  for(const line of patch.split('\n')) {
    const hunk=/^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(line);
    if(hunk){before=Number(hunk[1]);after=Number(hunk[2]);rows.push({kind:'hunk',text:line});continue;}
    if(before===null || line.startsWith('\\') || !/^[ +-]/.test(line)){rows.push({kind:'meta',text:line});continue;}
    const kind=line[0]==='+'?'add':line[0]==='-'?'delete':'context';
    rows.push({kind,text:line.slice(1),before:kind==='add'?null:before++,after:kind==='delete'?null:after++});
  }
  return rows;
}
export function splitRows(rows){
  const result=[];let removed=[],added=[];
  const flush=()=>{for(let i=0;i<Math.max(removed.length,added.length);i++)result.push({old:removed[i]||null,new:added[i]||null});removed=[];added=[];};
  for(const row of rows){
    if(row.kind==='delete')removed.push(row);
    else if(row.kind==='add')added.push(row);
    else{flush();result.push({old:row,new:row});}
  }
  flush();return result;
}
export function diffViewer(owner,review,putLine) {
  const root=el('section',{class:'diff-viewer'});
  const tree=el('div',{class:'diff-files',role:'list','aria-label':'Saved changed files'});
  const body=el('div',{class:'diff-patch'});
  const mode=el('select',{'aria-label':'Diff layout'},['auto','unified','split'].map(value=>el('option',{value},value)));
  let page=null,epoch=0;
  function render() {
    if(!page)return;
    const rows=patchRows(page.diff);
    const line=(side,number)=>number===null||number===undefined?el('span'):el('button',{type:'button',class:'diff-line',onclick:()=>putLine(page.path,side,number)},String(number));
    const unified=el('div',{class:'diff-unified'},...rows.map(row=>el('div',{class:`diff-row ${row.kind}`},line('before',row.before),line('after',row.after),el('pre',{},row.text))));
    const paired=el('div',{class:'diff-split'},...splitRows(rows).map(pair=>el('div',{class:'diff-pair'},
      el('div',{class:`diff-cell ${pair.old?.kind||''}`},line('before',pair.old?.before),el('pre',{},pair.old?.text||'')),
      el('div',{class:`diff-cell ${pair.new?.kind||''}`},line('after',pair.new?.after),el('pre',{},pair.new?.text||'')))));
    body.setAttribute('data-layout',mode.value);
    body.replaceChildren(el('p',{class:'sub'},`${page.path} · source ${page.source} · byte ${page.offset}`),unified,paired,
      page.next!==null?el('button',{type:'button',onclick:()=>load(page.path,page.next)},'Next diff page'):null);
  }
  async function load(path,offset=0){
    const token=++epoch;
    try {
      const reply=await api(`/api/sessions/${encodeURIComponent(owner)}/reviews`,{action:'diff',review:review.id,path,offset});
      if(token!==epoch||!root.isConnected)return;
      if(reply.source!==review.source)throw new Error('Saved review source changed; reopen the review.');
      page=reply;render();
    }catch(error){if(token===epoch)body.replaceChildren(el('p',{class:'warn'},error.error||String(error)));}
  }
  mode.addEventListener('change',render);
  tree.append(...review.files.map(file=>el('button',{type:'button',onclick:()=>load(file.path)},file.path)));
  root.append(mode,el('div',{class:'diff-layout'},tree,body));
  return root;
}
