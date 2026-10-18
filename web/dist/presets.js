import {el, api, state} from './lib.js';

// Loading edits the draft; it never submits or changes a permission stance.
export function presetPanel(input, applySettings) {
  const root = el('details', {}, el('summary', {}, 'Prompt presets'));
  const notice = el('p', {class:'sub', role:'status'});
  const list = el('div');
  const name = el('input', {'aria-label':'Preset name', maxlength:64, placeholder:'Preset name'});
  const keepSettings = el('input', {type:'checkbox', 'aria-label':'Include model and effort'});
  let previous = null;
  let epoch = 0;
  async function refresh() {
    const turn = ++epoch;
    try {
      const reply = await api('/api/presets', {action:'list'});
      if (turn !== epoch || !root.isConnected) return;
      list.replaceChildren(...reply.items.map(item => el('div', {class:'row'},
        el('span', {}, item.name),
        el('button', {type:'button', onclick:async () => {
          const owner=state.chat.session;
          const draft=input.value;
          try {
            const current = await api('/api/presets', {action:'get',name:item.name});
            if (!root.isConnected || turn !== epoch || state.chat.session !== owner) return;
            if (input.value !== draft) { notice.textContent='The draft changed while loading. Review the newer draft before loading a preset.'; return; }
            if (current.preset.model || current.preset.effort) {
              if (!confirm(`Load ${item.name}? Model: ${current.preset.model || 'current'}; effort: ${current.preset.effort || 'current'}.`)) return;
              const applied = await applySettings(current.preset);
              if (applied === false) { notice.textContent='Settings were not confirmed. The previous draft is retained; inspect or retry the model selection.'; return; }
              if (!root.isConnected || turn !== epoch || state.chat.session !== owner) return;
            }
            if (input.value !== draft) { notice.textContent='Preset settings were confirmed; the newer draft was retained.'; return; }
            previous = input.value;
            input.value = current.preset.prompt;
            input.dispatchEvent(new Event('input'));
            notice.textContent='Preset loaded into the draft. Review it before sending. Restore draft returns the previous text.';
          } catch (e) { notice.textContent=e.error || String(e); }
        }}, 'Load draft'),
        el('button', {type:'button', onclick:async () => {
          if (!confirm(`Replace ${item.name} with the current draft?`)) return;
          await save(item.name,item.revision);
        }}, 'Replace'),
        el('button', {type:'button', onclick:async () => {
          if (!confirm(`Delete preset ${item.name}?`)) return;
          try { await api('/api/presets',{action:'delete',name:item.name,expected:item.revision}); await refresh(); }
          catch(e) { notice.textContent=e.error || String(e); }
        }}, 'Delete'))));
    } catch(e) { notice.textContent=e.error || String(e); }
  }
  async function save(presetName,expected=null) {
    if (!input.value.trim()) { notice.textContent='Write a prompt before saving.'; return; }
    const settings=state.chat.settings;
    const preset={prompt:input.value, model:keepSettings.checked ? settings?.model || null : null,
      effort:keepSettings.checked ? settings?.effort || null : null};
    try { await api('/api/presets',{action:'save',name:presetName,preset,expected}); notice.textContent='Saved; the current draft is retained.'; await refresh(); }
    catch(e) {notice.textContent=e.error || String(e);}
  }
  root.addEventListener('toggle', () => { if(root.open) refresh(); else epoch++; });
  root.append(list, el('div',{class:'row'},name,el('label',{},keepSettings,'Include model and effort'),
    el('button',{type:'button',onclick:()=>save(name.value.trim())},'Save current draft'),
    el('button',{type:'button',onclick:()=>{if(previous!==null){const draft=input.value;input.value=previous;previous=draft;input.dispatchEvent(new Event('input'));}}},'Restore draft')),notice);
  return root;
}
