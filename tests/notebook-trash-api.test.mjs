import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
import ts from 'typescript';
const source=readFileSync(new URL('../src/lib/api.ts',import.meta.url),'utf8');
const ast=ts.createSourceFile('api.ts',source,ts.ScriptTarget.Latest,true);
const names=['deleteNotebook','restoreNotebook','permanentDelete','permanentDeleteNotebook'];
const functions=ast.statements.filter(n=>ts.isFunctionDeclaration(n)&&names.includes(n.name.text)).map(n=>n.getText(ast)).join('\n');
test('DB notebook trash and restoration carry vault and operation identity; filesystem routing stays intact',async()=>{
  const exports={},calls=[];let db=true;
  runInNewContext(ts.transpileModule(functions,{compilerOptions:{module:ts.ModuleKind.CommonJS}}).outputText,{exports,crypto,isDbVault:()=>db,invoke:async(command,args)=>{calls.push([command,args]);return '/v.rn/Work';}});
  await exports.deleteNotebook('/v.rn','/v.rn/Work');await exports.restoreNotebook('/v.rn','notebook-trash:id');await exports.permanentDeleteNotebook('/v.rn','notebook-trash:id');
  assert.deepEqual(calls.map(c=>c[0]),['db_trash_notebook','db_resolve_notebook_trash','db_resolve_notebook_trash']);
  for(const [,args]of calls){assert.equal(args.vaultPath,'/v.rn');assert.match(args.operationId,/^[a-f0-9-]{36}$/);}
  assert.equal(calls[1][1].remove,false);assert.equal(calls[2][1].remove,true);
  await exports.permanentDelete('/v.rn','note-id');assert.equal(calls.at(-1)[0],'db_delete_note');
  await exports.permanentDelete('/v.rn','notebook-trash:note-id');assert.equal(calls.at(-1)[0],'db_delete_note','note IDs never select notebook deletion by prefix');
  db=false;await exports.deleteNotebook('/directory','/directory/Work');await exports.restoreNotebook('/directory','/directory/.trash/Work');assert.deepEqual(calls.slice(-2).map(c=>c[0]),['delete_notebook','restore_notebook']);
});
test('DB trash lists grouped notebooks without hiding notes needed for individual conflict resolution',async()=>{
  const adapter=readFileSync(new URL('../src/lib/api-db.ts',import.meta.url),'utf8');
  const exports={},group={path:'notebook-trash:id',name:'Work',note_count:1,modified:'now'};
  runInNewContext(ts.transpileModule(adapter,{compilerOptions:{module:ts.ModuleKind.CommonJS}}).outputText,{exports,require:name=>name.includes('vault-open-status')?{recordVaultOpenStatus(){}}:{invoke:async(command)=>command==='db_notebook_trash'?[group]:[{id:'n',title:'Note',trashed:true,tags:[],pinned:false,created:'now',modified:'now',preview:'body'}]}});
  const trash=await exports.dbGetTrash();assert.equal(trash.notebooks[0],group);assert.equal(trash.notes[0].path,'n');
});
