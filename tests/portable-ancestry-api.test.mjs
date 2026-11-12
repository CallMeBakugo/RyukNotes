import assert from 'node:assert/strict';
import test from 'node:test';
import { readFile } from 'node:fs/promises';
import { runInNewContext } from 'node:vm';
import ts from 'typescript';
const source = await readFile(new URL('../src/lib/api-db.ts', import.meta.url), 'utf8');
const js = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
function fixture() {
  const calls=[]; const exports={}; let failure;
  const receipt={identity:{vault_uuid:'new-vault',storage_epoch:'new-epoch'},ledger_was_missing:true,requires_recovery_review:true};
  runInNewContext(js,{exports,require:()=>({invoke:async(command,args)=>{calls.push([command,structuredClone(args)]);if(failure)throw failure;return receipt;}})});
  return {api:exports,calls,receipt,fail(error){failure=error;}};
}
test('explicit restore/copy preserve destination, mode and operation identity without changing the displayed DB session',async()=>{
  const f=fixture();f.api.setDbMode(true);
  for(const mode of ['full_restore','independent_copy']) {
    const reply=await f.api.dbRestoreAncestryExport('/archive','/new.rn','/private-ledger',mode,'retained-operation');
    assert.equal(reply,f.receipt);assert.deepEqual(f.calls.at(-1),['db_restore_ancestry_export',{sourceDir:'/archive',destination:'/new.rn',ledgerRoot:'/private-ledger',mode,operationId:'retained-operation'}]);
    assert.equal(f.api.isDbVault(),true);
  }
});
test('reconciliation is an explicit current-vault operation; rejected imports never fall back to Markdown or switch modes',async()=>{
  const f=fixture();f.api.setDbMode(true);f.fail(new Error('immutable identity collision'));
  await assert.rejects(f.api.dbReconcileAncestryExport('/archive','/displayed.rn','same-operation'),/collision/);
  assert.deepEqual(f.calls,[['db_reconcile_ancestry_export',{sourceDir:'/archive',vaultPath:'/displayed.rn',operationId:'same-operation'}]]);
  assert.equal(f.api.isDbVault(),true);
  f.fail(new Error('unknown capability'));
  await assert.rejects(f.api.dbRestoreAncestryExport('/archive','/new.rn','/ledger','full_restore','same-operation'),/unknown capability/);
  assert.equal(f.calls.length,2);assert.equal(f.api.isDbVault(),true);
});
