import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { runInNewContext } from 'node:vm';
import ts from 'typescript';

const adapterSource = await readFile(new URL('../src/lib/api-db.ts', import.meta.url), 'utf8');
const apiSource = await readFile(new URL('../src/lib/api.ts', import.meta.url), 'utf8');
const apiAst = ts.createSourceFile('api.ts', apiSource, ts.ScriptTarget.Latest, true);
const saveDeclaration = apiAst.statements.find(n => ts.isFunctionDeclaration(n) && n.name?.text === 'saveNote');
function load(source, globals) {
  const exports = {};
  runInNewContext(ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText, { exports, ...globals });
  return exports;
}
const vault = { path: '/vault.rn', file: 'dev:inode' };
const original = { id: 'n1', title: 'Note', content: 'baseline', tags: ['a'], pinned: false,
  trashed: false, created: '2026-01-01', modified: '2026-01-01' };
const clone = value => JSON.parse(JSON.stringify(value));
const token = note => JSON.stringify([note.id, note.title, note.content, [...note.tags].sort(), note.pinned, note.trashed]);
const receipt = note => ({ ...clone(note), accepted: { vault, token: token(note) } });
function fixture() {
  let stored = clone(original);
  let reads = 0;
  const requests = [];
  let interleave;
  let error;
  const adapter = load(adapterSource, { require: name => {
    if (name === './utils/vault-open-status') return { recordVaultOpenStatus: () => {} };
    assert.equal(name, '@tauri-apps/api/core');
    return { invoke: async (command, args) => {
      if (command === 'db_get_note') { reads++; return stored && receipt(stored); }
      if (command === 'db_writer_identity') return vault;
      assert.equal(command, 'db_save_note');
      const request = args.request;
      requests.push(clone(request));
      if (error) throw error;
      if (request.policy.kind === 'create') {
        if (stored?.id === request.id) throw { kind: 'changed', message: 'Create refused' };
      } else {
        if (!stored) throw { kind: 'deleted', message: 'Note deleted' };
        if (stored.trashed) throw { kind: 'trashed', message: 'Note trashed' };
        if (request.policy.expectedToken !== token(stored)) throw { kind: 'changed', message: 'Stored state changed', current: receipt(stored) };
      }
      stored = { ...stored, ...clone(request), trashed: false, modified: 'acknowledged' };
      const acknowledged = receipt(stored);
      if (interleave) interleave();
      return acknowledged;
    } };
  } });
  const api = load(saveDeclaration.getText(apiAst), { isDbVault: () => true, dbSaveNote: adapter.dbSaveNote });
  return { ...adapter, save: api.saveNote, requests, reads: () => reads, state: () => stored,
    change: next => { stored = next; }, interleave: fn => { interleave = fn; }, fail: value => { error = value; } };
}

test('DB save forwards the captured full-state baseline and returns its exact receipt without reading back', async () => {
  const f = fixture();
  const loaded = await f.dbReadNote('n1', '/vault.rn');
  const saved = await f.save('/vault.rn', 'n1', loaded.meta, 'GUI content', loaded.raw);
  assert.equal(f.reads(), 1);
  assert.equal(saved.raw, 'GUI content');
  assert.equal(saved.meta.accepted.body, 'GUI content');
  assert.equal(f.requests[0].policy.expectedToken, token(original));
  assert.equal(f.requests[0].vaultPath, '/vault.rn');
  assert.deepEqual(f.requests[0].vault, vault);
  assert.equal(Object.isFrozen(loaded.meta.accepted.tags), true);
});

test('a writer between commit and adapter completion cannot become the queue baseline', async () => {
  const f = fixture();
  const loaded = await f.dbReadNote('n1');
  f.interleave(() => f.change({ ...original, content: 'racing CLI' }));
  const saved = await f.save('/vault.rn', 'n1', loaded.meta, 'our commit', loaded.raw);
  assert.equal(saved.raw, 'our commit');
  assert.equal(f.state().content, 'racing CLI');
  assert.equal(f.reads(), 1);
  await assert.rejects(f.save('/vault.rn', 'n1', saved.meta, 'later edit', saved.raw), e => e.kind === 'changed');
  assert.equal(f.state().content, 'racing CLI');
});

for (const change of [{ title: 'CLI title' }, { tags: ['cli'] }, { pinned: true }, { trashed: true }]) {
  test(`metadata/lifecycle change ${Object.keys(change)[0]} cannot be hidden by an unchanged body`, async () => {
    const f = fixture();
    const loaded = await f.dbReadNote('n1');
    f.change({ ...original, ...change });
    await assert.rejects(f.save('/vault.rn', 'n1', { ...loaded.meta, title: 'GUI title' }, 'baseline', loaded.raw),
      e => e instanceof f.DbWriteError && e.kind === (change.trashed ? 'trashed' : 'changed') && e.message.startsWith('Save conflict:'));
    assert.equal(f.state().title, change.title ?? original.title);
  });
}

test('unrelated reads cannot promote an editor baseline, while its own sequential acknowledgments can', async () => {
  const f = fixture();
  const first = await f.dbReadNote('n1');
  const own = await f.save('/vault.rn', 'n1', first.meta, 'one', first.raw);
  // The queue can have captured this same metadata object before its first save finished.
  const next = await f.save('/vault.rn', 'n1', first.meta, 'two', own.raw);
  assert.equal(next.raw, 'two');
  f.change({ ...f.state(), title: 'external title' });
  await f.dbReadNote('n1');
  await assert.rejects(f.save('/vault.rn', 'n1', next.meta, 'three', next.raw), e => e.kind === 'changed');
  assert.equal(f.state().title, 'external title');
});

test('deleted and replaced vault states retain typed errors with only narrow legacy message compatibility', async () => {
  const f = fixture();
  const loaded = await f.dbReadNote('n1');
  f.change(null);
  await assert.rejects(f.save('/vault.rn', 'n1', loaded.meta, 'draft', loaded.raw), e => e.kind === 'deleted');
  f.fail({ kind: 'vault_replaced', message: 'Captured vault changed' });
  await assert.rejects(f.save('/vault.rn', 'n1', loaded.meta, 'draft', loaded.raw), e => e.kind === 'vault_replaced');
  f.fail({ kind: 'io', message: 'Disk unavailable' });
  await assert.rejects(f.save('/vault.rn', 'n1', loaded.meta, 'draft', loaded.raw), e => e.kind === 'io' && !e.message.includes('Save conflict:'));
});

test('updates without a captured acceptance handle fail closed rather than inventing a fresh base', async () => {
  const f = fixture();
  await assert.rejects(f.save('/vault.rn', 'n1', { ...original }, 'new', 'baseline'), e => e.kind === 'invalid_policy');
  assert.equal(f.requests.length, 0);
  assert.equal(f.reads(), 0);
});

test('task updates use full-state update policy and never fall back to an unchecked upsert', async () => {
  const f = fixture();
  f.change({ ...original, content: '- [ ] task' });
  await f.dbSetTaskDone('n1', 0, '- [ ] task', true, '/vault.rn');
  assert.equal(f.state().content, '- [x] task');
  assert.equal(f.requests[0].policy.kind, 'update');
});

test('daily creation races select the existing identity without deleting or replacing it', async () => {
  const calls = [];
  let read = 0;
  const id = 'daily-2026-11-11';
  const daily = { ...original, id, content: 'created in the other window' };
  const adapter = load(adapterSource, { require: () => ({ invoke: async (command) => {
    calls.push(command);
    if (command === 'db_get_note') return read++ === 0 ? null : receipt(daily);
    if (command === 'db_writer_identity') return vault;
    assert.equal(command, 'db_save_note');
    throw { kind: 'changed', message: 'Already created' };
  } }) });
  const entry = await adapter.dbCreateDailyNote('2026-11-11', '/vault.rn');
  assert.equal(entry.path, id);
  assert.equal(entry.preview, daily.content);
  assert.ok(!calls.includes('db_delete_note'));
});
