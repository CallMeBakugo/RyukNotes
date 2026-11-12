import assert from 'node:assert/strict';
import test from 'node:test';
import { readFile } from 'node:fs/promises';
import { runInNewContext } from 'node:vm';
import { get } from 'svelte/store';
import ts from 'typescript';
import { sameAcceptedNote } from '../src/lib/utils/accepted-note.ts';
import { vaultOpenStatus, recordVaultOpenStatus } from '../src/lib/utils/vault-open-status.ts';
const source=await readFile(new URL('../src/lib/api-db.ts',import.meta.url),'utf8');
const js=ts.transpileModule(source,{compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022}}).outputText;
function fixture(invoke) {
  const exports={};runInNewContext(js,{exports,require:name=>name==='./utils/vault-open-status'?{recordVaultOpenStatus}:{invoke}});return exports;
}
test('migration status survives the vault picker/startup handoff and clears on a later normal or directory open',async()=>{
  let schema=2;
  const api=fixture(async(command,args)=>{
    assert.equal(command,'db_open_vault');assert.equal(args.path,'/disposable.rn');
    return {schema:3,previousSchema:schema,status:schema<3?`Vault upgraded from schema ${schema} to 3; private pre-migration snapshot saved.`:null,vaultUuid:'vault',storageEpoch:'epoch'};
  });
  await api.dbOpenVault('/disposable.rn');assert.equal(api.isDbVault(),true);assert.match(get(vaultOpenStatus),/schema 2 to 3/);
  schema=3;await api.dbOpenVault('/disposable.rn');assert.equal(get(vaultOpenStatus),'');
  schema=1;await api.dbOpenVault('/disposable.rn');assert.match(get(vaultOpenStatus),/schema 1 to 3/);
  api.setDbMode(false);assert.equal(get(vaultOpenStatus),'');
});
test('failed migration never enables DB mode or advertises success; ordinary restore reports the fresh epoch',async()=>{
  const api=fixture(async(command)=>{if(command==='db_restore_backup')return {requires_recovery_review:true};throw new Error('foreign-key validation failed: note_tags rowid=7 count=1');});
  await assert.rejects(api.dbOpenVault('/disposable.rn'),/note_tags/);assert.equal(api.isDbVault(),false);assert.equal(get(vaultOpenStatus),'');
  api.setDbMode(true);await api.dbRestoreBackup('/backup.rn','/disposable.rn');assert.match(get(vaultOpenStatus),/new storage epoch/);
  recordVaultOpenStatus(null);
});
test('equal Markdown after restore is still a different accepted state; filesystem equality stays unchanged',()=>{
  const a={path:'n',raw:'same',content:'same',meta:{accepted:{token:'epoch-A'}}};
  const b={...a,meta:{accepted:{token:'epoch-B'}}};
  assert.equal(sameAcceptedNote(a,b),false);assert.equal(sameAcceptedNote(a,a),true);
  const fs={...a,meta:{}};assert.equal(sameAcceptedNote(fs,{...fs}),true);
  assert.equal(sameAcceptedNote(null,a),false);
});
