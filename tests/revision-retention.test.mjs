// Execute the native SQL against disposable on-disk WAL databases. These tests
// validate SQL/locking predicates, not the unexecuted Rust/ledger orchestration.
import assert from 'node:assert/strict';
import test from 'node:test';
import { DatabaseSync } from 'node:sqlite';
import { readFileSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
const native = readFileSync(new URL('../src-tauri/src/vault/revision_retention.rs', import.meta.url), 'utf8');
const base = readFileSync(new URL('../src-tauri/src/vault/db.rs', import.meta.url), 'utf8').match(/const SCHEMA: &str = r#"([\s\S]*?)"#;/)[1];
const graph = readFileSync(new URL('../src-tauri/src/vault/ancestry.sql', import.meta.url), 'utf8');
const eligible = native.match(/let eligible="([\s\S]*?)";/)[1];
const marker = native.match(/"(INSERT INTO revision_pruning[^"\n]+)"/)[1].replace('{eligible}', eligible);
const removeState = native.match(/"(DELETE FROM revision_states[^"\n]+)"/)[1];
const removeBlobs = native.match(/"(DELETE FROM attachments AS a[\s\S]*?)",\[\]/)[1];
function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), 'ryuk-retention-'));
  const path = join(root, 'fixture.rn');
  const db = new DatabaseSync(path); db.exec('PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;');
  db.exec(base); db.exec(graph);
  db.exec("INSERT INTO vault_identity VALUES(1,'vault','epoch','2020-01-01'); INSERT INTO note_revision_counters VALUES('n',130); INSERT INTO note_incarnations VALUES('life','n','2020-01-01',NULL)");
  const insert = db.prepare('INSERT INTO revisions VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)');
  const state = db.prepare('INSERT INTO revision_states VALUES(?,?,?,?,?,?,?,?,?)');
  for (let n=1;n<=130;n++) {
    insert.run(`r${n}`,'life',n,'epoch',`commit${n}`,'hash','gui','instance',`op${n}`,'n','request',n===1?'create':'update','known','2020-01-01T00:00:00Z');
    state.run(`r${n}`,new Uint8Array([65]),'Title','[]',0,'live','2020','2020','{"attachments":[]}');
    if (n>1) db.prepare("INSERT INTO revision_parents VALUES(?,?,'predecessor',0)").run(`r${n}`,`r${n-1}`);
  }
  db.exec("INSERT INTO note_revision_heads VALUES('n','life','r130')");
  const other = new DatabaseSync(path); other.exec('PRAGMA foreign_keys=ON; PRAGMA busy_timeout=0;');
  t.after(() => { other.close(); db.close(); rmSync(root,{recursive:true,force:true}); });
  return {db,other};
}
const ids = (db,cutoff='2021-01-01') => db.prepare(eligible).all(cutoff).map(r=>r.revision_id);
function gcStates(db,cutoff='2021-01-01') {
  let count=0;
  for(const id of ids(db,cutoff)) {
    if(db.prepare(marker).run(cutoff,'2021-04-01',id).changes) count+=db.prepare(removeState).run(id).changes;
  }
  return count;
}
test('native retention SQL enforces age AND newest-100, with non-expiring head/review/base pins',t=>{
  const {db}=fixture(t);
  assert.equal(ids(db,'2019-01-01').length,0);assert.equal(ids(db).length,30);
  db.exec("INSERT INTO revision_pins VALUES('base','r1','recovery','owner','1900'); INSERT INTO revision_pins VALUES('review','r2','review','review-owner','1900')");
  db.exec('BEGIN IMMEDIATE');assert.equal(gcStates(db),28);db.exec('COMMIT');
  assert.equal(db.prepare('SELECT COUNT(*) AS n FROM revision_states').get().n,102);
  for (const table of ['revisions','revision_parents','note_revision_counters']) {
    assert.equal(db.prepare(`SELECT COUNT(*) AS n FROM ${table}`).get().n,table==='revisions'?130:table==='revision_parents'?129:1);
  }
  assert.equal(db.prepare('SELECT high_water FROM note_revision_counters').get().high_water,130);
  assert.equal(db.prepare('SELECT COUNT(*) AS n FROM revision_pins').get().n,2);
  db.exec('DELETE FROM versions');assert.equal(ids(db).length,0);
});
test('WAL independent connections serialize pin publication and pruning; marker failures roll back links too',t=>{
  const {db,other}=fixture(t);
  db.exec('BEGIN IMMEDIATE');assert.equal(ids(db).includes('r1'),true);
  assert.throws(()=>other.exec("INSERT INTO revision_pins VALUES('racing','r1','review','other','now')"),/locked/);
  db.exec('ROLLBACK');other.exec("INSERT INTO revision_pins VALUES('racing','r1','review','other','now')");
  assert.equal(ids(db).includes('r1'),false);
  db.exec("CREATE TEMP TRIGGER disk_failure BEFORE DELETE ON revision_states WHEN OLD.revision_id='r3' BEGIN SELECT RAISE(ABORT,'fixture disk failure'); END;");
  db.exec('BEGIN IMMEDIATE');assert.throws(()=>gcStates(db),/fixture disk failure/);db.exec('ROLLBACK');
  assert.equal(db.prepare('SELECT COUNT(*) AS n FROM revision_pruning').get().n,0);
  assert.equal(db.prepare('SELECT COUNT(*) AS n FROM revision_states').get().n,130);
  assert.equal(db.prepare('PRAGMA foreign_key_check').all().length,0);
});
test('native BLOB GC recognizes every durable ownership domain, including recovery receipts and private-byte holds',t=>{
  const {db}=fixture(t);
  db.exec("INSERT INTO notes VALUES('n','Title','body',0,0,'2020','2020')");
  for(const id of ['note','revision','private','stage','curated','fork','apply','pending','orphan']) {
    db.prepare('INSERT INTO attachments VALUES(?,?,?,?,?,?,?)').run(id,'file.bin','application/octet-stream',1,id,new Uint8Array([42]),'2020');
  }
  db.exec("INSERT INTO note_attachments VALUES('n','note'); INSERT INTO revision_attachments VALUES('r1','revision'); INSERT INTO versions(id,note_id,content,created) VALUES('v','n','[asset](curated)','2020')");
  db.prepare('INSERT INTO meta VALUES(?,?)').run('recovery_assets:v3:owner',JSON.stringify(['private']));
  db.prepare('INSERT INTO meta VALUES(?,?)').run('attachment_stage:v3:n:stage',JSON.stringify({attachmentId:'stage'}));
  for(const [prefix,id] of [['recovery_fork:v1:','fork'],['review_apply:v1:','apply'],['pending_summary:v1:','pending']]) db.prepare('INSERT INTO meta VALUES(?,?)').run(prefix+'owner',JSON.stringify({body:`[asset](${id})`}));
  // Unrelated non-JSON metadata must not be parsed as hold data.
  db.exec("INSERT INTO meta VALUES('preference','not-json')");
  db.exec('BEGIN IMMEDIATE');assert.equal(db.prepare(removeBlobs).run().changes,1);db.exec('COMMIT');
  assert.equal(db.prepare("SELECT COUNT(*) AS n FROM attachments WHERE id='private'").get().n,1);
  db.exec("DELETE FROM notes WHERE id='n'");
  db.exec('BEGIN IMMEDIATE');db.prepare(removeBlobs).run();db.exec('COMMIT');
  assert.equal(db.prepare("SELECT COUNT(*) AS n FROM attachments WHERE id IN ('revision','private','stage')").get().n,3);
  db.exec("UPDATE meta SET value='broken' WHERE key='recovery_assets:v3:owner'");
  // A candidate with no earlier ownership match must visit the malformed hold.
  db.prepare('INSERT INTO attachments VALUES(?,?,?,?,?,?,?)').run('candidate','file.bin','application/octet-stream',1,'candidate',new Uint8Array([1]),'2020');
  db.exec('BEGIN IMMEDIATE');assert.throws(()=>db.prepare(removeBlobs).run(),/malformed JSON/);db.exec('ROLLBACK');
  assert.equal(db.prepare("SELECT COUNT(*) AS n FROM attachments WHERE id='candidate'").get().n,1);
});
