import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { runInNewContext } from 'node:vm';
import ts from 'typescript';

const adapterSource = await readFile(new URL('../src/lib/api-db.ts', import.meta.url), 'utf8');
const apiSource = await readFile(new URL('../src/lib/api.ts', import.meta.url), 'utf8');
const ast = ts.createSourceFile('api.ts', apiSource, ts.ScriptTarget.Latest, true);
const names = ['openVault', 'chooseExternalVault', 'restoreExternalVault', 'importNote', 'moveNote', 'getNotes', 'getNotebooks', 'getNoteVersions', 'getNoteVersionContent', 'createVersion', 'readNote'];
const source = ast.statements.filter(node => ts.isFunctionDeclaration(node) && names.includes(node.name.text)).map(node => node.getText(ast)).join('\n');
function load(source, globals) {
  const exports = {};
  const { outputText } = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } });
  runInNewContext(outputText, { exports, ...globals });
  return exports;
}
function fixture(invoke) {
  const adapter = load(adapterSource, { require: name => {
    if (name === './utils/vault-open-status') return { recordVaultOpenStatus: () => {} };
    assert.equal(name, '@tauri-apps/api/core');
    return { invoke };
  } });
  const api = load(source, { invoke, ...adapter });
  return { ...adapter, ...api };
}
const native = { id: 'new-id', title: 'Imported', content: 'body', tags: ['x'], pinned: true, created: '2026-01-01T00:00:00Z', modified: '2026-01-02T00:00:00Z' };

test('single DB import forwards the accepted snapshot and references and synthesizes NoteContent', async () => {
  const references = [{ from: 0, to: 9, href: 'image.png', html: false }];
  const f = fixture(async (command, args) => {
    assert.equal(command, 'db_import_note');
    assert.equal(args.vaultPath, '/v.rn');
    assert.equal(args.sourcePath, '/source.md');
    assert.equal(args.notebookRelative, 'Research');
    assert.equal(args.expectedRaw, 'accepted');
    assert.equal(args.references, references);
    return native;
  });
  f.setDbMode(true);
  const note = await f.importNote('/v.rn', '/source.md', 'Research', 'accepted', references);
  assert.equal(note.path, 'new-id');
  assert.equal(note.meta.id, 'new-id');
  assert.equal(note.meta.title, 'Imported');
  assert.equal(note.meta.pinned, true);
  assert.equal(note.content, 'body');
  assert.equal(note.raw, 'body');
});

test('DB move changes membership through the native command without rewriting or deleting the note', async () => {
  const calls = [];
  const f = fixture(async (command, args) => {
    calls.push(command);
    assert.equal(command, 'db_move_note');
    assert.equal(JSON.stringify(args), JSON.stringify({ vaultPath: '/v.rn', notePath: 'id', destNotebook: '/v.rn/B' }));
    return 'id';
  });
  f.setDbMode(true);
  assert.equal(await f.moveNote('/v.rn', 'id', '/v.rn/B'), 'id');
  assert.deepEqual(calls, ['db_move_note']);
});

test('successful directory opens reset DB mode; rejected opens preserve the prior mode', async () => {
  let fail = false;
  const f = fixture(async command => {
    assert.equal(command, 'open_vault');
    if (fail) throw new Error('open failed');
  });
  f.setDbMode(true);
  await f.openVault('/directory');
  assert.equal(f.isDbVault(), false);
  f.setDbMode(true);
  fail = true;
  await assert.rejects(f.openVault('/missing'), /open failed/);
  assert.equal(f.isDbVault(), true);
});

test('directory chooser cancellation preserves mode and successful external opens reset it', async () => {
  let result = null;
  const f = fixture(async () => result);
  f.setDbMode(true);
  assert.equal(await f.chooseExternalVault(), null);
  assert.equal(f.isDbVault(), true);
  result = { path: '/external', bookmarkId: 'b', name: 'External' };
  await f.chooseExternalVault();
  assert.equal(f.isDbVault(), false);
  f.setDbMode(true);
  await f.restoreExternalVault('b');
  assert.equal(f.isDbVault(), false);
});

test('DB import and move failures propagate without a filesystem fallback', async () => {
  const calls = [];
  const f = fixture(async command => { calls.push(command); throw new Error('rejected'); });
  f.setDbMode(true);
  await assert.rejects(f.importNote('/v.rn', '/source.md', null, 'raw', []), /rejected/);
  await assert.rejects(f.moveNote('/v.rn', 'id', '/v.rn/missing'), /rejected/);
  assert.deepEqual(calls, ['db_import_note', 'db_move_note']);
});

test('filesystem import and move retain their existing commands', async () => {
  const calls = [];
  const f = fixture(async command => { calls.push(command); return command === 'move_note' ? '/folder/note.md' : {}; });
  await f.importNote('/v', '/source.md', null, 'raw', []);
  await f.moveNote('/v', '/v/note.md', '/v/folder');
  assert.deepEqual(calls, ['import_note', 'move_note']);
});

test('DB notebook filtering and all history operations use their native counterparts', async () => {
  const calls = [];
  const f = fixture(async command => { calls.push(command); return command === 'db_get_note_version_content' ? 'prior' : []; });
  f.setDbMode(true);
  await f.getNotes('/v.rn/Research');
  await f.getNotebooks();
  await f.getNoteVersions('/v.rn', 'id');
  assert.equal(await f.getNoteVersionContent('/v.rn', 'id', 'timestamp'), 'prior');
  await f.createVersion('/v.rn', 'id', 'id');
  assert.deepEqual(calls, ['db_get_notes', 'db_get_notebooks', 'db_get_note_versions', 'db_get_note_version_content', 'db_create_version']);
});

test('external Markdown can open in the scoped viewer while a DB vault is active', async () => {
  const calls = [];
  const external = { path: '/source.md', raw: 'raw', content: 'body', meta: {} };
  const f = fixture(async (command, args) => {
    calls.push(command);
    if (command === 'db_get_note') return null;
    assert.equal(command, 'read_note');
    assert.equal(args.vaultPath, '/v.rn');
    assert.equal(args.path, '/source.md');
    return external;
  });
  f.setDbMode(true);
  assert.equal(await f.readNote('/v.rn', '/source.md'), external);
  assert.deepEqual(calls, ['db_get_note', 'read_note']);
});
