import assert from 'node:assert/strict';
import test from 'node:test';
import { createPortableRecovery } from '../src/lib/utils/portable-recovery.ts';
test('failed recovery retries the exact destination, mode and operation identity under a new barrier',async()=>{
  let failure=true,begins=0,releases=0;const calls=[];
  const recovery=createPortableRecovery({begin:async()=>{begins++;return async()=>{releases++;};},restore:async r=>{calls.push(r);if(failure)throw Error('disk full');return {epoch:'new'};}});
  await assert.rejects(recovery.run({mode:'independent_copy',source:'/export',destination:'/new.rn'}),/disk full/);
  const pending={...recovery.pending};failure=false;
  const result=await recovery.run();
  assert.deepEqual(calls,[pending,pending]);assert.deepEqual(result.request,pending);assert.equal(recovery.pending,null);assert.equal(begins,2);assert.equal(releases,2);
});
test('a refusing dirty-window barrier never invokes restore; a pending call cannot run concurrently',async()=>{
  let writes=0;const recovery=createPortableRecovery({begin:async()=>null,restore:async()=>{writes++;}});
  await assert.rejects(recovery.run({mode:'raw_recovery',source:'/moved.rn',destination:'/new.rn'}),/every window/);assert.equal(writes,0);assert.ok(recovery.pending);
  let finish;const blocked=createPortableRecovery({begin:async()=>async()=>{},restore:()=>new Promise(r=>{finish=r;})});
  const first=blocked.run({mode:'full_restore',source:'/export',destination:'/new.rn'});await new Promise(r=>setImmediate(r));
  const id=blocked.pending.operationId;await assert.rejects(blocked.run({mode:'independent_copy',source:'/other',destination:'/other.rn'}),/already running/);assert.equal(blocked.pending.operationId,id);finish({});await first;
});
test('a lost maintenance-release acknowledgment retains the committed operation for idempotent retry',async()=>{
  let fail=true;const calls=[];
  const recovery=createPortableRecovery({begin:async()=>async()=>{if(fail)throw Error('release reply lost');},restore:async r=>{calls.push(r);return {epoch:'committed'};}});
  await assert.rejects(recovery.run({mode:'full_restore',source:'/export',destination:'/new.rn'}),/release reply lost/);
  const operation=recovery.pending.operationId;fail=false;await recovery.run();
  assert.equal(calls[1].operationId,operation);assert.equal(recovery.pending,null);
});
