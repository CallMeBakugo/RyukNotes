// Cross-runtime wire/SQL fixtures for the native format's exact table/column
// contract. Rust restore/ledger orchestration still requires the native suite.
import assert from 'node:assert/strict';
import test from 'node:test';
import { DatabaseSync } from 'node:sqlite';
import { createHash, randomUUID } from 'node:crypto';
import { readFileSync, mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
const native=readFileSync(new URL('../src-tauri/src/vault/portable_ancestry.rs',import.meta.url),'utf8');
const specs=JSON.parse(native.match(/const TABLES: &str = r#"([\s\S]*?)"#;/)[1]);
const capabilities=JSON.parse(`[${native.match(/const CAPABILITIES:[^=]+?=\s*&\[([\s\S]*?)\];/)[1].replace(/,\s*$/,'')}]`);
const base=readFileSync(new URL('../src-tauri/src/vault/db.rs',import.meta.url),'utf8').match(/const SCHEMA: &str = r#"([\s\S]*?)"#;/)[1];
const ddl=readFileSync(new URL('../src-tauri/src/vault/ancestry.sql',import.meta.url),'utf8');
const sha=bytes=>createHash('sha256').update(bytes).digest('hex');
function fixture(t) {
  const root=mkdtempSync(join(tmpdir(),'ryuk-portable-'));
  const path=join(root,'source.rn'), db=new DatabaseSync(path);
  db.exec('PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;');db.exec(base);db.exec(ddl);
  const other=new DatabaseSync(path);other.exec('PRAGMA foreign_keys=ON; PRAGMA busy_timeout=0;');
  t.after(()=>{other.close();db.close();rmSync(root,{recursive:true,force:true});});
  const vault=randomUUID(),epoch=randomUUID(),inc=randomUUID(),revision=randomUUID(),token=randomUUID(),asset=randomUUID();
  const bytes=Buffer.from([0,255,13,10,42]);
  const body=` \r\n雪\t\n[file](.rn-attachments/${asset})\r\n`;
  db.prepare('INSERT INTO vault_identity VALUES(1,?,?,?)').run(vault,epoch,'2020');
  db.exec("INSERT INTO meta VALUES('schema_version','3')");
  db.prepare('INSERT INTO attachments VALUES(?,?,?,?,?,?,?)').run(asset,'mixed.bin','application/octet-stream',bytes.length,sha(bytes),bytes,'2020');
  db.prepare('INSERT INTO notes VALUES(?,?,?,?,?,?,?)').run('n','雪 title',body,1,0,'2020','2021');
  db.prepare('INSERT INTO note_revision_counters VALUES(?,?)').run('n',9007199254740993n);
  db.prepare('INSERT INTO note_incarnations VALUES(?,?,?,NULL)').run(inc,'n','2020');
  const extra={attachments:[asset],generatedSummaryV1:null,notebook:null,session:null};
  const state={body:[...Buffer.from(body)],title:'雪 title',tags:['tag'],pinned:true,lifecycle:'live',created:'2020',modified:'2021',extra};
  db.prepare('INSERT INTO revisions VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)').run(revision,inc,9007199254740993n,epoch,token,sha(JSON.stringify([1,state])),'gui',randomUUID(),randomUUID(),'n',sha('request'),'create','known','2021');
  db.prepare('INSERT INTO revision_states VALUES(?,?,?,?,?,?,?,?,?)').run(revision,Buffer.from(body),'雪 title','["tag"]',1,'live','2020','2021',JSON.stringify(extra));
  db.prepare('INSERT INTO note_revision_heads VALUES(?,?,?)').run('n',inc,revision);
  db.exec("INSERT INTO tags VALUES('tag'); INSERT INTO note_tags VALUES('n','tag')");
  db.prepare('INSERT INTO note_attachments VALUES(?,?)').run('n',asset);
  db.prepare('INSERT INTO revision_attachments VALUES(?,?)').run(revision,asset);
  db.prepare('INSERT INTO versions(id,note_id,content,created) VALUES(?,?,?,?)').run('curated','n',body,'2020');
  return {root,db,other,vault,epoch,asset,bytes,body,revision};
}
function rows(db,spec) {
  const s=db.prepare(`SELECT ${spec.columns.join(',')} FROM ${spec.name} ORDER BY ${spec.columns.slice(0,spec.key).join(',')}`);
  s.setReadBigInts(true);return s.all().map(row=>spec.columns.map(c=>row[c]));
}
function encode(db,root,afterTable=()=>{}) {
  const files=new Map();
  const emit=bytes=>{const hash=sha(bytes);const f={file:`.ryuknotes/ancestry/sha256/${hash}.bin`,sha256:hash,size:bytes.length};files.set(f.file,{descriptor:f,bytes:Buffer.from(bytes)});return f;};
  db.exec('BEGIN');
  try {
    const tables=specs.map(spec=>{
      const values=rows(db,spec);
      const encoded=values.map(row=>emit(Buffer.from(JSON.stringify(row.map(v=>v===null?{kind:'null'}:typeof v==='bigint'?{kind:'integer',value:v.toString()}:typeof v==='string'?{kind:'text',value:v}:{kind:'blob',value:emit(v)})))));
      afterTable(spec);return {name:spec.name,columns:spec.columns,rows:encoded};
    });
    const identity=db.prepare('SELECT vault_uuid,storage_epoch FROM vault_identity').get();
    db.exec('COMMIT');
    const sidecars=[...files.values()].map(f=>f.descriptor).sort((a,b)=>a.file.localeCompare(b.file));
    for(const {descriptor,bytes} of files.values()){const path=join(root,descriptor.file);mkdirSync(join(root,'.ryuknotes/ancestry/sha256'),{recursive:true});writeFileSync(path,bytes);}
    return {format:2,ancestry:{capabilities,vaultUuid:identity.vault_uuid,storageEpoch:identity.storage_epoch,storageEpochs:[identity.storage_epoch],tables,sidecars}};
  } catch(e){db.exec('ROLLBACK');throw e;}
}
function readSidecar(root,file){const bytes=readFileSync(join(root,file.file));assert.equal(bytes.length,file.size);assert.equal(sha(bytes),file.sha256);return bytes;}
function decode(root,index,destination) {
  assert.equal(index.format,2);assert.deepEqual(index.ancestry.capabilities,capabilities);
  const parsed=index.ancestry.tables.map((table,i)=>{
    assert.equal(table.name,specs[i].name);assert.deepEqual(table.columns,specs[i].columns);
    return table.rows.map(file=>JSON.parse(readSidecar(root,file)).map(v=>v.kind==='null'?null:v.kind==='integer'?BigInt(v.value):v.kind==='blob'?readSidecar(root,v.value):v.value));
  });
  // Preflight precedes opening a destination, just like the native boundary.
  const db=new DatabaseSync(destination);db.exec('PRAGMA foreign_keys=ON; BEGIN IMMEDIATE;');
  try {
    db.exec(base);db.exec(ddl);db.exec('PRAGMA defer_foreign_keys=ON');
    specs.forEach((spec,i)=>{const statement=db.prepare(`INSERT INTO ${spec.name}(${spec.columns.join(',')}) VALUES(${spec.columns.map(()=>'?').join(',')})`);for(const row of parsed[i])statement.run(...row);});
    assert.deepEqual(db.prepare('PRAGMA foreign_key_check').all(),[]);db.exec('COMMIT');return db;
  }catch(e){db.exec('ROLLBACK');db.close();throw e;}
}
test('native column/wire contract preserves graph, exact CRLF/Unicode bodies, metadata, counters and deduplicated bytes',t=>{
  const f=fixture(t), index=encode(f.db,f.root), second=encode(f.db,f.root);assert.deepEqual(index,second);
  const copy=decode(f.root,index,join(f.root,'copy.rn'));t.after(()=>copy.close());
  for(const spec of specs)assert.deepEqual(rows(copy,spec),rows(f.db,spec),spec.name);
  assert.equal(copy.prepare('SELECT content FROM notes').get().content,f.body);
  const bodyFile=index.ancestry.tables.find(t=>t.name==='revision_states').rows[0];
  const wire=JSON.parse(readSidecar(f.root,bodyFile));assert.equal(wire[1].kind,'blob');assert.equal(readSidecar(f.root,wire[1].value).toString(),f.body);
  const counter=index.ancestry.tables.find(t=>t.name==='note_revision_counters').rows[0];assert.equal(JSON.parse(readSidecar(f.root,counter))[1].value,'9007199254740993');
  assert.equal(copy.prepare('SELECT COUNT(*) AS n FROM attachments').get().n,1);
  assert.deepEqual(Buffer.from(copy.prepare('SELECT data FROM attachments').get().data),f.bytes);
});
test('WAL archive table reads share a snapshot across an independent metadata commit',t=>{
  const f=fixture(t);let raced=false;
  const index=encode(f.db,f.root,spec=>{if(spec.name==='meta'&&!raced){f.other.exec("INSERT INTO tags VALUES('racing-unused-tag')");raced=true;}});
  const tags=index.ancestry.tables.find(t=>t.name==='tags').rows.map(file=>JSON.parse(readSidecar(f.root,file))[0].value);
  assert.deepEqual(tags,['tag']);assert.equal(f.db.prepare("SELECT COUNT(*) AS n FROM tags WHERE name='racing-unused-tag'").get().n,1);
});
test('unknown capabilities and corrupted authoritative sidecars refuse before opening the destination',t=>{
  const f=fixture(t),index=encode(f.db,f.root),dest=join(f.root,'absent.rn');
  const future=structuredClone(index);future.ancestry.capabilities.push('future');assert.throws(()=>decode(f.root,future,dest));
  const descriptor=index.ancestry.tables.find(t=>t.name==='revision_states').rows[0];writeFileSync(join(f.root,descriptor.file),'damage');
  assert.throws(()=>decode(f.root,index,dest));
  assert.throws(()=>readFileSync(dest),/ENOENT/);
});
