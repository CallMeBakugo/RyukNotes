import assert from 'node:assert/strict';
import test from 'node:test';
import { ConflictReviewModel } from '../src/lib/utils/conflict-review.ts';
const deferred = () => { let resolve, reject; const promise = new Promise((a,b) => { resolve=a; reject=b; }); return {promise,resolve,reject}; };
const note = (body, token, extra={}) => ({ path:'n', content:body, raw:body, meta:{ id:'n', title:'Title', tags:[], pinned:false, created:'date', modified:'date', ...extra, accepted:{token,vault:{path:'/v.rn',file:'inode'}, body, title:extra.title??'Title', tags:extra.tags??[], pinned:extra.pinned??false, trashed:false} } });
function fixture(overrides={}) {
  let revision=1, current=true, writer=0, adopted=0;
  const stored=[]; const canceled=[]; let nextId=0;
  let response=()=>({available:true,reviewId:`r${++nextId}`,revision,base:note('base\r\n','B'),local:note('local\r\n','B'),remote:note('remote\r\n',`R${writer}`)});
  const deps={
    open:async()=>response(), checkpoint:async(id,sequence,proposal)=>stored.push({id,sequence,proposal:structuredClone(proposal)}),
    cancel:async id=>{canceled.push(id);}, apply:async()=>note('merged\r\n','committed'),
    localRevision:()=>revision,ownerIsCurrent:()=>current,
    adopt:async()=>{adopted++;return true;}, ...overrides,
  };
  const model=new ConflictReviewModel(deps);
  return {model,stored,canceled,deps,setResponse(fn){response=fn;},type(){revision++;model.invalidate('local changed');},writer(){writer++;model.observe(note('new remote\r\n',`R${writer}`));},away(){current=false;},get adopted(){return adopted;}};
}
function ready(model) {
  for(const h of model.state.plan.hunks) if(h.kind==='overlapping')model.choose(h.id,'local');
  for(const m of model.state.metadata) if(!m.choice)model.chooseMetadata(m.field,'local');
  model.build();
}
test('review opens immutable snapshots and cancellation never applies or mutates a live buffer',async()=>{
  const f=fixture();await f.model.open();
  assert.equal(Object.isFrozen(f.model.state.reviewed.local.meta.tags),true);
  assert.throws(()=>{f.model.state.reviewed.local.content='mutated';});
  assert.equal(f.model.state.status,'ready');
  assert.equal(await f.model.close(),true);assert.equal(f.adopted,0);assert.deepEqual(f.canceled,['r1']);
});
test('late open and cancellation are fenced by window/pane generation',async()=>{
  const opening=deferred();const f=fixture({open:()=>opening.promise});const pending=f.model.open();
  await f.model.close();opening.resolve({available:true,reviewId:'late',revision:1,base:note('b','B'),local:note('l','B'),remote:note('r','R')});await pending;
  assert.equal(f.model.state.visible,false);assert.deepEqual(f.canceled,['late']);assert.equal(f.adopted,0);
});
test('typing during review invalidates apply but preserves manual result edits through renewed review',async()=>{
  const f=fixture();await f.model.open();ready(f.model);
  f.model.edit([{from:0,to:5,insert:'my edited result'}]);const result=f.model.state.result;
  f.type();assert.equal(f.model.state.status,'stale');assert.equal(await f.model.apply(),false);
  await f.model.open(true);assert.equal(f.model.state.result,result);assert.equal(f.model.state.manual,true);assert.equal(f.model.state.built,false);
  assert.ok(f.stored.some(s=>s.proposal.body===result));assert.equal(f.adopted,0);await f.model.close();
});
test('exact successful CAS is the only path to adoption; another writer rejects stale apply',async()=>{
  const f=fixture();await f.model.open();ready(f.model);assert.equal(await f.model.apply(),true);assert.equal(f.adopted,1);
  const g=fixture({apply:async()=>{throw {kind:'changed'};}});await g.model.open();ready(g.model);
  g.model.edit([{from:0,to:0,insert:'manual '}]);const result=g.model.state.result;
  assert.equal(await g.model.apply(),false);assert.equal(g.adopted,0);assert.equal(g.model.state.result,result);assert.equal(g.model.state.status,'stale');
  assert.ok(g.stored.some(s=>s.proposal.body===result));await g.model.close();
});
test('a writer or newer owner generation during apply cannot adopt or erase the proposal',async()=>{
  const gate=deferred();const f=fixture({apply:()=>gate.promise});await f.model.open();ready(f.model);
  const applying=f.model.apply();await new Promise(r=>setTimeout(r,0));f.writer();gate.resolve(note('committed','C'));
  assert.equal(await applying,false);assert.equal(f.adopted,0);assert.equal(f.model.state.status,'stale');assert.ok(f.model.state.result);await f.model.close();
});
test('unresolved base routes to Keep both and never constructs an inferred common base',async()=>{
  const f=fixture({open:async()=>({available:false,reason:'No verified base. Use Keep both.'})});await f.model.open();
  assert.equal(f.model.state.status,'unavailable');assert.equal(f.model.state.plan,null);assert.equal(f.model.canApply,false);assert.match(f.model.state.message,/Keep both/);await f.model.close();
});
test('metadata conflicts require explicit scalar/tag choices and tag union is never implicit',async()=>{
  const f=fixture({open:async()=>({available:true,reviewId:'meta',revision:1,base:note('base','B',{tags:['base']}),local:note('local','B',{title:'Local',tags:['local'],pinned:true}),remote:note('remote','R',{title:'Remote',tags:['remote']})})});
  await f.model.open();for(const h of f.model.state.plan.hunks)f.model.choose(h.id,'local');
  assert.equal(f.model.complete,false);f.model.chooseMetadata('title','remote');f.model.chooseMetadata('tags','union');f.model.build();
  await f.model.preserve();const proposal=f.stored.at(-1).proposal;
  assert.equal(proposal.title,'Remote');assert.deepEqual(proposal.tags,['local','remote']);assert.equal(proposal.pinned,true);await f.model.close();
});
test('journal failure retains candidate memory and refuses safe disposal even after hiding review',async()=>{
  let fail=true;const f=fixture({checkpoint:async()=>{if(fail)throw new Error('disk full');}});await f.model.open();ready(f.model);
  const result=f.model.state.result;assert.equal(await f.model.close(),false);assert.equal(f.model.state.result,result);assert.equal(f.model.state.journalError,true);
  await f.model.open();assert.equal(f.model.state.visible,true);assert.equal(f.model.state.result,result);
  fail=false;assert.equal(await f.model.close(),true);
});
test('separate windows keep choices, generations and results isolated',async()=>{
  const a=fixture(),b=fixture();await a.model.open();await b.model.open();ready(a.model);ready(b.model);
  a.model.edit([{from:0,to:0,insert:'window A '}]);b.model.edit([{from:0,to:0,insert:'window B '}]);
  a.type();assert.equal(a.model.state.status,'stale');assert.equal(b.model.state.status,'ready');
  a.away();assert.equal(await a.model.apply(),false);assert.equal(b.model.canApply,true);
  assert.match(a.model.state.result,/window A/);assert.match(b.model.state.result,/window B/);await a.model.close();await b.model.close();
});

test('edits arriving across renew awaits are carried forward rather than overwritten by an older capture',async()=>{
  const cancel=deferred();const f=fixture({cancel:()=>cancel.promise});await f.model.open();ready(f.model);
  const renewing=f.model.open(true);await new Promise(r=>setTimeout(r,0));
  f.model.edit([{from:0,to:0,insert:'latest '}]);const expected=f.model.state.result;
  cancel.resolve();await renewing;assert.equal(f.model.state.result,expected);assert.equal(f.model.state.built,false);
  await f.model.close();
});

test('a post-commit adoption failure republishes an unresolved result generation before Keep both can leave',async()=>{
  const f=fixture({adopt:async()=>false});await f.model.open();ready(f.model);
  assert.equal(await f.model.apply(),false);assert.equal(f.model.state.status,'stale');
  assert.ok(f.stored.length>=2);assert.ok(f.stored.at(-1).sequence>f.stored[0].sequence);
  assert.equal(f.stored.at(-1).proposal.body,f.model.state.result);assert.equal(await f.model.close(),true);
});

test('post-commit journal cleanup errors do not let a cached durable version suppress preservation retries',async()=>{
  let disk=false;
  const f=fixture({apply:async()=>{disk=true;throw new Error('commit succeeded but retirement failed');},checkpoint:async()=>{if(disk)throw new Error('disk');}});
  await f.model.open();ready(f.model);assert.equal(await f.model.apply(),false);
  assert.equal(f.model.state.journalError,true);assert.equal(await f.model.close(),false);
  disk=false;assert.equal(await f.model.preserve(),true);assert.equal(await f.model.close(),true);
});

test('unsupported Unicode in manual input stays in memory and cannot become an applied proposal',async()=>{
  const f=fixture();await f.model.open();ready(f.model);
  f.model.edit([{from:0,to:0,insert:'\ud800'}]);
  assert.equal(f.model.state.result.charCodeAt(0),0xd800);assert.equal(f.model.state.status,'stale');assert.equal(f.model.canApply,false);
  f.model.edit([{from:0,to:1,insert:''}]);assert.equal(f.model.state.result.isWellFormed(),true);await f.model.close();
});

test('typing while close preservation is in flight prevents disposable close and retains the newest result',async()=>{
  const gate=deferred();const f=fixture({checkpoint:()=>gate.promise});await f.model.open();ready(f.model);
  const closing=f.model.close();await new Promise(r=>setTimeout(r,0));
  f.model.edit([{from:0,to:0,insert:'last keystroke '}]);const latest=f.model.state.result;
  gate.resolve();assert.equal(await closing,false);assert.equal(f.model.state.result,latest);
  await f.model.open();assert.equal(f.model.state.result,latest);await f.model.close();
});

test('late image responses cannot bind to a canceled or different owner generation',async()=>{
  const image=deferred();const f=fixture({asset:()=>image.promise});await f.model.open();
  const loading=f.model.preview('.rn-attachments/image');await f.model.close();image.resolve({mime:'image/png',data:[1,2,3]});
  await assert.rejects(loading,/Stale/);
  const other=fixture({asset:async()=>({mime:'image/png',data:[4]})});await other.model.open();
  assert.deepEqual((await other.model.preview('.rn-attachments/image')).data,[4]);other.away();
  await assert.rejects(other.model.preview('.rn-attachments/image'),/owned/);await other.model.close();
});


test('v3 review requires coherently pinned base and remote identities and preserves edited artifacts on refusal', async () => {
  const f = fixture(); await f.model.open(); ready(f.model);
  f.model.edit([{from:0,to:0,insert:'preserve me '}]); const result = f.model.state.result;
  const base = note('base','B'), remote = note('remote','R');
  base.meta.accepted.vaultUuid = remote.meta.accepted.vaultUuid = 'v';
  base.meta.accepted.storageEpoch = remote.meta.accepted.storageEpoch = 'e';
  base.meta.accepted.revision = { revisionId:'B', incarnationId:'life', commitToken:'base-token' };
  remote.meta.accepted.revision = { revisionId:'R', incarnationId:'life', commitToken:'remote-token' };
  f.setResponse(() => ({available:true, reviewId:'unproven', revision:1, base, local:note('local','B'), remote}));
  await f.model.open(true);
  assert.equal(f.model.state.status,'unavailable'); assert.equal(f.model.canApply,false);
  assert.equal(f.model.state.result,result); assert.ok(f.canceled.includes('unproven'));
  f.setResponse(() => ({available:true, reviewId:'pinned', revision:1, base, local:note('local','B'), remote,
    ancestry:{base:{vaultUuid:'v',storageEpoch:'e',incarnationId:'life',revisionId:'B',commitToken:'base-token'},
      remote:{vaultUuid:'v',storageEpoch:'e',incarnationId:'life',revisionId:'R',commitToken:'remote-token'}}}));
  await f.model.open(true); assert.equal(f.model.state.status,'ready'); assert.equal(f.model.state.result,result);
  await f.model.close();
});
