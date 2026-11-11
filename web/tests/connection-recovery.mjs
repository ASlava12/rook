import test from 'node:test';
import assert from 'node:assert/strict';
import {connectionRecovery} from '../dist/connection-recovery.js';

function fixture(inspect){
  let next=0,reopens=0,visible=true;const tasks=new Map(),states=[];
  const recovery=connectionRecovery({inspect,reopen:()=>reopens++,changed:state=>states.push(state.kind),visible:()=>visible,
    schedule:(fn,delay)=>{const id=++next;tasks.set(id,{fn,delay});return id;},cancel:id=>tasks.delete(id)});
  return {recovery,tasks,states,get reopens(){return reopens;},hide(){visible=false;},
    async tick(){const [id,task]=tasks.entries().next().value;tasks.delete(id);await task.fn();return task.delay;}};
}

test('recovery backs off to a fixed ceiling with one pending retry and no prompt submission',async()=>{
  const f=fixture(async()=>({kind:'offline'}));f.recovery.closed();
  const delays=[];for(let at=0;at<10;at++){assert.equal(f.tasks.size,1);delays.push(await f.tick());}
  assert.deepEqual(delays,[1000,2000,5000,10000,20000,30000,30000,30000,30000,30000]);
  assert.equal(f.reopens,0);f.recovery.dispose();assert.equal(f.tasks.size,0);
});
test('a healthy daemon only reopens the observer and an unavailable project stops automatic retries',async()=>{
  const ready=fixture(async()=>({kind:'ready'}));ready.recovery.closed();await ready.tick();assert.equal(ready.reopens,1);
  ready.recovery.opened();assert.equal(ready.states.at(-1),'ready');assert.equal(ready.tasks.size,0);
  const project=fixture(async()=>({kind:'project'}));project.recovery.closed();await project.tick();
  assert.equal(project.states.at(-1),'project');assert.equal(project.reopens,0);assert.equal(project.tasks.size,0);
});
test('disposal aborts an old probe and its late result cannot reconnect another view',async()=>{
  let release,signal;const f=fixture(current=>{signal=current;return new Promise(resolve=>release=resolve);});
  f.recovery.closed();const pending=f.tick();f.recovery.dispose();assert(signal.aborted);
  release({kind:'ready'});await pending;assert.equal(f.reopens,0);assert.equal(f.tasks.size,0);
});
test('hidden views do not probe and a manual retry cancels the existing timer',async()=>{
  let probes=0;const f=fixture(async()=>{probes++;return {kind:'ready'};});
  f.hide();f.recovery.closed();await f.tick();assert.equal(probes,0);assert.equal(f.tasks.size,1);
  f.recovery.dispose();assert.equal(f.tasks.size,0);
  const manual=fixture(async()=>({kind:'ready'}));manual.recovery.closed();await manual.recovery.retry();
  assert.equal(manual.reopens,1);assert.equal(manual.tasks.size,0);
});
