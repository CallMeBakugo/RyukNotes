import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import ts from 'typescript';
import { runInNewContext } from 'node:vm';
import test from 'node:test';

const adapterSource = await readFile(new URL('../src/lib/api-db.ts', import.meta.url), 'utf8');
const apiSource = await readFile(new URL('../src/lib/api.ts', import.meta.url), 'utf8');
const sessionSource = await readFile(new URL('../src/lib/utils/note-window-session.ts', import.meta.url), 'utf8');
// Execute production code in independent module runtimes, as separate webviews do.
function load(source, names, globals) {
  const exports = {};
  const js = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  runInNewContext(js, { exports, ...globals, require: () => ({ recordVaultOpenStatus: () => {}, ...globals }) });
  return Object.fromEntries(names.map(name => [name, exports[name]]));
}
function runtime(invoke) {
  const adapter = load(adapterSource, ['isDbVault', 'setDbMode', 'dbAttachVault', 'looksLikeDbVault', 'dbReadNote', 'DbWriteError'], { invoke });
  const readSource = apiSource.slice(apiSource.indexOf('export async function readNote('), apiSource.indexOf('export async function saveNote('));
  const api = load(readSource, ['readNote'], { ...adapter, invoke });
  const session = load(sessionSource, ['openNoteWindowSession'], { ...adapter, ...api });
  return { ...adapter, ...session };
}
const vault = { path: '/vault.rn', file: '1:2' };
const receipt = { id: 'uuid', title: 'Note', content: 'body', tags: [], pinned: false, trashed: false,
  created: 'now', modified: 'now', accepted: { vault, token: 'accepted' } };

test('detached DB runtime attaches before reads, independently of the main runtime', async () => {
  const calls = [];
  let release;
  const pending = new Promise(resolve => { release = resolve; });
  const main = runtime(() => { throw new Error('main must not be called'); });
  main.setDbMode(true);
  const detached = runtime(async (command, args) => {
    calls.push([command, args]);
    if (command === 'db_writer_identity') { await pending; return vault; }
    assert.equal(command, 'db_get_note'); // Never FS read or global db_open_vault.
    return receipt;
  });
  assert.equal(detached.isDbVault(), false);
  const opening = detached.openNoteWindowSession('/vault.rn', 'uuid');
  assert.deepEqual(calls.map(c => c[0]), ['db_writer_identity']);
  assert.equal(detached.isDbVault(), false);
  release();
  const note = await opening;
  assert.equal(note.content, 'body');
  assert.equal(note.meta.accepted.vault.file, vault.file);
  assert.equal(note.meta.accepted.token, 'accepted');
  assert.equal(calls[0][1].vaultPath, '/vault.rn');
  assert.equal(calls[1][1].vaultPath, '/vault.rn');
  assert.equal(detached.isDbVault(), true);
  assert.equal(main.isDbVault(), true);
});

test('directory detached runtime reads through filesystem adapter', async () => {
  const detached = runtime(async (command, args) => {
    assert.equal(command, 'read_note');
    assert.equal(args.vaultPath, '/notes');
    assert.equal(args.path, '/notes/note.md');
    return { content: 'filesystem', meta: {} };
  });
  detached.setDbMode(true); // Explicit initialization also clears stale local mode.
  assert.equal((await detached.openNoteWindowSession('/notes', '/notes/note.md')).content, 'filesystem');
  assert.equal(detached.isDbVault(), false);
});

test('failed originating-vault handshake fails closed before note reads', async () => {
  const detached = runtime(async command => {
    assert.equal(command, 'db_writer_identity');
    throw { kind: 'vault_replaced', message: 'Origin no longer open' };
  });
  await assert.rejects(detached.openNoteWindowSession('/vault.rn', 'uuid'), error => error.kind === 'vault_replaced');
  assert.equal(detached.isDbVault(), false);
});

test('replacement between handshake and first read cannot mount an accepted buffer', async () => {
  const detached = runtime(async command => command === 'db_writer_identity' ? vault :
    { ...receipt, accepted: { vault: { ...vault, file: 'replaced' }, token: 'new' } });
  await assert.rejects(detached.openNoteWindowSession('/vault.rn', 'uuid'), error => error.kind === 'vault_replaced');
});
