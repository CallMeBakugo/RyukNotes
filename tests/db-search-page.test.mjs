import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
import ts from 'typescript';
import { DatabaseSync } from 'node:sqlite';
const adapter = readFileSync(new URL('../src/lib/api-db.ts',import.meta.url),'utf8');
function load(source, globals) {
  const exports = {};
  runInNewContext(ts.transpileModule(source,{compilerOptions:{module:ts.ModuleKind.CommonJS}}).outputText,{exports,...globals});
  return exports;
}
test('DB search forwards page boundaries and preserves native highlights and exact targets',async()=>{
  const calls=[],page={total:65,results:[{path:'n',title:'😀 title',snippet:'near needle',title_matches:[],snippet_matches:[{start:5,end:11}],match_target:{field:'body',start:400,end:406,text:'needle',before:'near ',after:' end'},score:1}]};
  const api=load(adapter,{require:name=>name.includes('vault-open-status')?{recordVaultOpenStatus(){}}:{invoke:async(command,args)=>{calls.push([command,args]);return page;}}});
  assert.equal(await api.dbSearchNotes('needle',20,40),page);
  assert.deepEqual(structuredClone(calls),[['db_search_page',{query:'needle',limit:20,offset:40}]]);
  const source=readFileSync(new URL('../src/lib/api.ts',import.meta.url),'utf8');
  const ast=ts.createSourceFile('api.ts',source,ts.ScriptTarget.Latest,true);
  const fn=ast.statements.find(n=>ts.isFunctionDeclaration(n)&&n.name.text==='searchNotes').getText(ast);
  const boundary=load(fn,{...api,isDbVault:()=>true});
  assert.equal(await boundary.searchNotes('needle',20,60),page);
  assert.equal(calls.at(-1)[1].offset,60);
});
test('native SQL uses a contentless persistent index, deterministic pages, and hydrated query-aware Unicode highlights',()=>{
  const source=readFileSync(new URL('../src-tauri/src/vault/db_search.rs',import.meta.url),'utf8');
  const db=new DatabaseSync(':memory:');
  try {
    db.exec("CREATE TABLE notes(id TEXT,title TEXT,content TEXT,trashed INTEGER); CREATE VIRTUAL TABLE notes_fts USING fts5(title,content,content='',contentless_delete=1,tokenize='unicode61')");
    const insert=db.prepare('INSERT INTO notes VALUES(?,?,?,?)'),index=db.prepare('INSERT INTO notes_fts(rowid,title,content) VALUES(?,?,?)');
    for(let n=0;n<66;n++){const id=`n${String(n).padStart(3,'0')}`,body='padding '.repeat(60)+'😀 café needle end';const row=insert.run(id,'Title',body,n===65?1:0);index.run(row.lastInsertRowid,'Title',body);}
    const sql=source.match(/snapshot.prepare\("(SELECT n.id,n.title,n.content,rank[^"\n]+)"/)[1];
    const ids=[];for(const offset of [0,20,40,60]){const page=db.prepare(sql).all('needle',20,offset);assert.equal(page.length,Math.min(20,65-offset));ids.push(...page.map(r=>r.id));}
    assert.equal(new Set(ids).size,65);
    db.exec("CREATE VIRTUAL TABLE page USING fts5(title,content,tokenize='unicode61')");
    db.prepare('INSERT INTO page VALUES(?,?)').run('😀 Needle title','😀 café needle');
    const highlight=source.match(/scratch.query_row\("(SELECT highlight[^"\n]+)"/)[1];
    const marked=db.prepare(highlight).get('"café needle"','[OPEN]','[CLOSE]');
    assert.equal(Object.values(marked)[1],'😀 [OPEN]café needle[CLOSE]');
    assert.throws(()=>db.prepare(sql).all('"',20,0));
  } finally { db.close(); }
});
