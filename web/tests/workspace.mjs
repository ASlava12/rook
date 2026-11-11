import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { md, MARKDOWN_NODES } from '../dist/lib.js';
import { drafts, DRAFT_BYTES, DRAFT_TOTAL_BYTES } from '../dist/drafts.js';
import { followViewport } from '../dist/scroll-follow.js';
import { locationState, folderName } from '../dist/shell.js';

class Node {
  constructor(tag,text=''){this.tag=tag;this.text=text;this.nodeType=tag==='#text'?3:1;this.children=[];this.attrs={};}
  append(...children){this.children.push(...children);}
  setAttribute(name,value){this.attrs[name]=value;}
  addEventListener(){}
  get textContent(){return this.text+this.children.map(child=>child.textContent).join('');}
  all(){return this.children.flatMap(child=>[child,...child.all()]);}
}
globalThis.document={createElement:tag=>new Node(tag),createTextNode:text=>new Node('#text',text),createDocumentFragment:()=>new Node('#fragment')};
const storage=new Map();
globalThis.sessionStorage={getItem:key=>storage.get(key)??null,setItem:(key,value)=>storage.set(key,value)};

test('answers render code, headings, tables and ordered lists while HTML and unsafe links remain inert',()=>{
  const rendered=md('# Result\n\n| File | State |\n| --- | --- |\n| a.js | **fixed** |\n\n1. Read\n2. Check\n\n> Evidence\n\n```js\nconsole.log("<script>");\n```\n\n[docs](https://example.org/docs) [unsafe](javascript:alert) <img src=x onerror=alert(1)>');
  const nodes=rendered.all();
  for(const tag of ['h1','table','ol','blockquote','pre','code','strong'])assert(nodes.some(node=>node.tag===tag),tag);
  assert.equal(nodes.filter(node=>node.tag==='a').length,1);
  assert.equal(nodes.find(node=>node.tag==='a').attrs.href,'https://example.org/docs');
  assert.equal(nodes.find(node=>node.tag==='a').attrs.rel,'noopener noreferrer');
  assert(!nodes.some(node=>['img','script','iframe'].includes(node.tag)));
  assert(rendered.textContent.includes('<img src=x onerror=alert(1)>'));
  assert(nodes.some(node=>node.tag==='button'&&node.attrs['aria-label']==='Copy code'));
});

test('a reached Markdown node budget stops repeated formatting with an explicit saved-history notice',()=>{
  const count=MARKDOWN_NODES+20;
  assert(count>MARKDOWN_NODES);
  const rendered=md(Array.from({length:count},(_,index)=>`- item ${index}`).join('\n'));
  assert.match(rendered.textContent,/Displayed formatting limit reached/);
  assert(rendered.all().filter(node=>node.nodeType===1).length<=MARKDOWN_NODES+1);
});

test('draft recovery is session scoped and refuses oversized escaping without replacing the previous draft',()=>{
  storage.clear();
  assert(drafts.save('one','/project','unsubmitted text'));
  assert.equal(drafts.get('two','/project'),'');
  assert.equal(drafts.get('one','/project'),'unsubmitted text');
  const oversized='\u0000'.repeat(Math.ceil(DRAFT_BYTES/6));
  assert(JSON.stringify(oversized).length>DRAFT_BYTES,'the serialized draft reaches the cap');
  assert.equal(drafts.save('one','/project',oversized),false);
  assert.equal(drafts.get('one','/project'),'unsubmitted text');
  for(let index=0;index<16;index++)assert(drafts.save(`session-${index}`,'/project','x'.repeat(40000)));
  const saved=JSON.parse(storage.get('rook:workspace-drafts:v1'));
  assert(saved.length<=8);assert(Buffer.byteLength(JSON.stringify(saved))<=DRAFT_TOTAL_BYTES);
  assert.equal(drafts.get('session-15','/project'),'x'.repeat(40000));
  assert.equal(drafts.get('one','/project'),'','old entries retire at the retention bound');
});

test('scrolling up holds the reading position until the reader chooses latest',()=>{
  let handler,unread=false;
  const viewport={scrollHeight:4000,clientHeight:500,scrollTop:3500,addEventListener:(_,cb)=>{handler=cb;},removeEventListener:()=>{handler=null;}};
  const follower=followViewport(viewport,value=>{unread=value;});
  viewport.scrollTop=800;handler();
  const followed=follower.before();viewport.scrollHeight+=500;follower.after(followed);
  assert.equal(viewport.scrollTop,800);assert.equal(unread,true);
  follower.latest();assert.equal(viewport.scrollTop,4500);assert.equal(unread,false);
  follower.dispose();assert.equal(handler,null);
});

test('session links reject invalid identity and excessive state while accepting Windows project paths',()=>{
  const id='01J00000000000000000000000';
  const parsed=locationState(`#chat?${new URLSearchParams({session:id,workspace:'C:\\Projects\\demo'})}`);
  assert.equal(parsed.session,id);assert.equal(parsed.workspace,'C:\\Projects\\demo');assert.equal(folderName(parsed.workspace),'demo');
  assert.equal(locationState('#not-a-view?session=garbage').tab,'chat');
  assert.equal(locationState('#chat?session=ZZZZZZZZZZZZZZZZZZZZZZZZZZ').session,null);
  assert.equal(locationState('#chat?workspace='+ 'x'.repeat(9000)).workspace,null);
});

test('the independent UI includes every referenced module, stylesheet and navigation renderer',()=>{
  const dist=new URL('../dist/',import.meta.url),page=fs.readFileSync(new URL('index.html',dist),'utf8');
  const files=fs.readdirSync(dist).filter(name=>name.endsWith('.js'));
  const sources=files.map(name=>fs.readFileSync(new URL(name,dist),'utf8'));
  for(const source of sources)for(const match of source.matchAll(/from ['"]\.\/([^'"]+)['"]/g))assert(fs.existsSync(new URL(match[1],dist)),match[1]);
  for(const match of page.matchAll(/(?:src|href)="\/([^"#]+)"/g))assert(fs.existsSync(new URL(match[1],dist)),match[1]);
  const app=fs.readFileSync(new URL('app.js',dist),'utf8');
  const tabs=[...page.matchAll(/data-tab="([^"]+)"/g)].map(match=>match[1]);
  assert(tabs.length>=13);
  for(const tab of tabs)assert(app.includes(`${tab}: render`),`${tab} has a renderer`);
});
