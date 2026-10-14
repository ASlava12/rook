import test from 'node:test';
import assert from 'node:assert/strict';
import { learningPanel } from '../dist/learning.js';
class Node {
  constructor(tag, text = '') {
    this.tag = tag; this.nodeType = tag === '#text' ? 3 : 1;
    this.text = text; this.children = []; this.listeners = {}; this.isConnected = true; this.value = '';
  }
  append(...children) { this.children.push(...children); if (this.tag === 'select' && !this.value) this.value = this.children[0]?.value || ''; }
  replaceChildren(...children) { this.children = children; }
  setAttribute(name, value) { this[name] = value; }
  addEventListener(name, handler) { this.listeners[name] = handler; }
  get textContent() { return this.text + this.children.map(child => child.textContent).join(''); }
  find(predicate) { if (predicate(this)) return this; for (const child of this.children) { const found = child.find?.(predicate); if (found) return found; } return null; }
  button(label) { return this.find(node => node.tag === 'button' && node.textContent === label); }
}
globalThis.document = { createElement: tag => new Node(tag), createTextNode: text => new Node('#text', text) };
const flush = () => new Promise(resolve => setImmediate(resolve));

const response = data => ({ ok: true, json: async () => data });
const saved = { id:'proposal-one', state:'proposed', draft:{ target:{kind:'memory',text:'Respond in Russian.'},reason:'User preference',evidence:[7] }, source:{result:12,turn:'turn-one',signature:'source-one',events:[{seq:7,kind:'user_message',text:'Respond in Russian.',truncated:false}]} };
test('knowledge application preserves the owner, proposal and exact reviewed source token', async()=>{
  const calls=[];
  globalThis.fetch=async(path,options)=>{const action=JSON.parse(options.body);calls.push({path,action});if(action.action==='list')return response({items:[saved]});if(action.action==='prepare')return response({proposal:saved.id,token:'review-token',source:saved.source,draft:saved.draft,diff:{before:null,after:saved.draft.target.text}});if(action.action==='apply')return response({...saved,state:'applied'});return response(saved);};
  const panel=learningPanel('original-owner');panel.button('List knowledge proposals').listeners.click();await flush();panel.button('proposal-one · proposed · memory').listeners.click();await flush();panel.button('Preview change').listeners.click();await flush();
  assert.equal(calls.some(c=>c.action.action==='apply'),false);panel.button('Apply reviewed change').listeners.click();await flush();assert.deepEqual(calls.at(-1),{path:'/api/sessions/original-owner/learning',action:{action:'apply',proposal:saved.id,token:'review-token'}});assert.ok(panel.button('Preview rollback'));
});
test('a reviewed draft revision stays attached to the old proposal and its evidence',async()=>{
  const calls=[];globalThis.fetch=async(path,options)=>{const action=JSON.parse(options.body);calls.push(action);if(action.action==='list')return response({items:[saved]});if(action.action==='revise')return response({...saved,id:'revision',predecessor:saved.id,draft:action.draft});return response(saved);};
  const panel=learningPanel('owner');panel.button('List knowledge proposals').listeners.click();await flush();panel.button('proposal-one · proposed · memory').listeners.click();await flush();const draft={...saved.draft,reason:'Reviewed exact quotation'};panel.find(n=>n['aria-label']==='Reviewed learning draft').value=JSON.stringify(draft);panel.button('Save reviewed revision').listeners.click();await flush();assert.deepEqual(calls.at(-1),{action:'revise',proposal:saved.id,draft});assert.match(panel.textContent,/Revises proposal proposal-one/);
});
test('late proposal replies never repaint a departed owner',async()=>{
  let deliver;globalThis.fetch=async()=>new Promise(resolve=>{deliver=resolve;});const panel=learningPanel('old-owner');panel.button('List knowledge proposals').listeners.click();await flush();panel.isConnected=false;deliver(response({items:[saved]}));await flush();assert.equal(panel.button('proposal-one · proposed · memory'),null);assert.match(panel.textContent,/Reading knowledge proposal/);
});
