import assert from 'node:assert/strict';
import test from 'node:test';
import { readFile } from 'node:fs/promises';
import { runInNewContext } from 'node:vm';
import ts from 'typescript';
const source = await readFile(new URL('../src/lib/api-db.ts', import.meta.url), 'utf8');
function fixture(content) {
  const vault = { path: '/fixture.rn', file: 'fixture' };
  const exports = {};
  const note = () => ({ id: 'n', title: 'Note', content, tags: [], pinned: false, created: 'now', modified: 'now', accepted: { vault, token: 'fixture-token' } });
  runInNewContext(ts.transpileModule(source, {compilerOptions: {module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022}}).outputText, {exports, require: name => name.includes('vault-open-status') ? {recordVaultOpenStatus(){}} : {invoke: async (cmd,args) => {
    if(cmd==='db_get_note') return note();
    assert.equal(cmd,'db_save_note'); content=args.request.content; return note();
  }}});
  return {api:exports, body:()=>content};
}
test('task metadata preserves indentation, internal spacing, hard breaks and CRLF', async () => {
  const f=fixture('- [ ] Parent\r\n\t  - [ ] Child  spaced  \r\n');
  await f.api.dbSetTaskPriority('n',1,'\t  - [ ] Child  spaced  ','high','/fixture.rn');
  assert.equal(f.body(),'- [ ] Parent\r\n\t  - [ ] Child  spaced !high  \r\n');
  await f.api.dbSetTaskDue('n',1,'\t  - [ ] Child  spaced !high  ','2026-12-01','/fixture.rn');
  assert.equal(f.body(),'- [ ] Parent\r\n\t  - [ ] Child  spaced !high due:2026-12-01  \r\n');
});
test('a moved duplicate task is refused instead of changing the first match', async () => {
  const f=fixture('changed\n- [ ] duplicate\n- [ ] duplicate');
  await assert.rejects(f.api.dbSetTaskDone('n',0,'- [ ] duplicate',true,'/fixture.rn'),/ambiguous/);
  assert.equal(f.body(),'changed\n- [ ] duplicate\n- [ ] duplicate');
});
