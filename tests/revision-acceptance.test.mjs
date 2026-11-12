import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { randomUUID } from 'node:crypto';
import { runInNewContext } from 'node:vm';
import test from 'node:test';
import ts from 'typescript';

const source=await readFile(new URL('../src/lib/api-db.ts',import.meta.url),'utf8');
function adapter(invoke) {
  const exports={};
  runInNewContext(ts.transpileModule(source,{compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022}}).outputText,
    {exports,crypto:{randomUUID},require:name=>{assert.equal(name,'@tauri-apps/api/core');return {invoke};}});
  return exports;
}
const vault={path:'/fixture.rn',file:'file-identity'};
const vaultUuid=randomUUID(), epoch=randomUUID(), incarnation=randomUUID();
function receipt(body,number,title='Title') {
  const revision={incarnationId:incarnation,revisionId:randomUUID(),revision:String(number),commitToken:randomUUID(),stateSha256:'a'.repeat(64),storageEpoch:epoch};
  return {id:'n',title,content:body,tags:[],pinned:false,trashed:false,created:'created',modified:`modified-${number}`,
    accepted:{vault,vaultUuid,storageEpoch:epoch,revision,token:`revision-v3.${vaultUuid}.${epoch}.${incarnation}.${revision.revisionId}.${revision.commitToken}.${revision.stateSha256}`}};
}
const clone=value=>JSON.parse(JSON.stringify(value));

test('lost reply retries one operation and adopts only that exact receipt after a later writer',async()=>{
  let saved=receipt('A',1);const operations=new Map();const requests=[];let lose=true;
  const api=adapter(async(command,args)=>{
    if(command==='db_get_note')return clone(saved);
    assert.equal(command,'db_save_note');const request=clone(args.request);requests.push(request);
    if(operations.has(request.operationId))return clone(operations.get(request.operationId));
    if(request.policy.expectedToken!==saved.accepted.token)throw {kind:'changed',message:'stale'};
    saved=receipt(request.content,2);operations.set(request.operationId,clone(saved));
    if(lose){lose=false;throw {kind:'io',message:'lost IPC reply'};}
    return clone(saved);
  });
  api.setDbMode(true);
  const base=await api.dbReadNote('n',vault.path);
  assert.ok(Object.isFrozen(base.meta.accepted.revision));
  await assert.rejects(api.dbSaveNote('n',base.meta,'B','A',vault.path),error=>error.kind==='io');
  const own=clone(saved);saved=receipt('C',3,'Other writer title');
  const retry=await api.dbSaveNote('n',base.meta,'B','A',vault.path);
  assert.equal(retry.content,'B');assert.equal(retry.meta.accepted.token,own.accepted.token);
  assert.equal(requests[0].operationId,requests[1].operationId);
  assert.equal(requests[1].policy.expectedToken,base.meta.accepted.token);
  await assert.rejects(api.dbSaveNote('n',retry.meta,'D','B',vault.path),error=>error.kind==='changed');
  assert.equal(requests[2].policy.expectedToken,own.accepted.token);
  assert.equal(base.raw,'A');
});

test('late commit reply after vault-session invalidation cannot advance a buffer baseline',async()=>{
  const baseReceipt=receipt('A',1);let release;let writes=0;
  const api=adapter(async(command)=>{
    if(command==='db_get_note')return clone(baseReceipt);
    writes++;return new Promise(resolve=>{release=resolve;});
  });
  api.setDbMode(true);const base=await api.dbReadNote('n',vault.path);
  const saving=api.dbSaveNote('n',base.meta,'B','A',vault.path);
  api.setDbMode(false);release(receipt('B',2));
  await assert.rejects(saving,error=>error.kind==='vault_replaced');
  await assert.rejects(api.dbSaveNote('n',base.meta,'C','A',vault.path),error=>error.kind==='vault_replaced');
  assert.equal(writes,1);assert.equal(base.raw,'A');assert.equal(base.meta.accepted.body,'A');
});

test('reordered no-op acknowledgment cannot replace a newer accepted revision',async()=>{
  const baseReceipt=receipt('A',1);const replies=[];const requests=[];
  const api=adapter(async(command,args)=>{
    if(command==='db_get_note')return clone(baseReceipt);
    requests.push(clone(args.request));return new Promise(resolve=>replies.push(resolve));
  });
  const base=await api.dbReadNote('n',vault.path);
  const first=api.dbSaveNote('n',base.meta,'A','A',vault.path); // legitimate no-op at head A
  const second=api.dbSaveNote('n',base.meta,'B','A',vault.path);
  const committed=receipt('B',2);replies[1](committed);
  const next=await second;
  replies[0](baseReceipt);
  await assert.rejects(first,error=>error.kind==='changed');
  const third=api.dbSaveNote('n',base.meta,'C','B',vault.path);
  assert.equal(requests[2].policy.expectedToken,committed.accepted.token);
  replies[2](receipt('C',3));await third;
  assert.equal(next.raw,'B');
});

test('task requests carry v3 source and failures retain their original expected revision',async()=>{
  const saved=receipt('- [ ] task',1);const requests=[];
  const api=adapter(async(command,args)=>{
    if(command==='db_get_note')return clone(saved);
    requests.push(clone(args.request));throw {kind:'changed',message:'metadata changed'};
  });
  const base=await api.dbReadNote('n',vault.path);
  await assert.rejects(api.dbSaveNote('n',base.meta,'- [x] task',base.raw,vault.path,'task'),error=>error.kind==='changed');
  await assert.rejects(api.dbSaveNote('n',base.meta,'- [x] task',base.raw,vault.path,'task'),error=>error.kind==='changed');
  assert.equal(requests[0].writerSource,'task');assert.equal(requests[0].operationId,requests[1].operationId);
  assert.equal(requests[1].policy.expectedToken,saved.accepted.token);
});
