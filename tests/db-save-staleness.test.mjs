import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { runInNewContext } from 'node:vm';
import ts from 'typescript';

const adapterSource = await readFile(new URL('../src/lib/api-db.ts', import.meta.url), 'utf8');
const apiSource = await readFile(new URL('../src/lib/api.ts', import.meta.url), 'utf8');
const apiAst = ts.createSourceFile('api.ts', apiSource, ts.ScriptTarget.Latest, true);
const saveDeclaration = apiAst.statements.find(node => ts.isFunctionDeclaration(node) && node.name.text === 'saveNote');

function load(source, globals) {
  const exports = {};
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  });
  runInNewContext(outputText, { exports, ...globals });
  return exports;
}

function fixture() {
  let stored = 'CLI content';
  let reads = 0;
  const conflict = new Error('Save conflict: note changed in database. Your edits have not been written.');
  const adapter = load(adapterSource, {
    require: name => {
      assert.equal(name, '@tauri-apps/api/core');
      return { invoke: async (command, args) => {
        if (command === 'db_save_note') {
          assert.equal(args.id, 'n1');
          if (args.expectedRaw !== stored) throw conflict;
          stored = args.content;
        } else {
          assert.equal(command, 'db_get_note');
          reads++;
          return { id: 'n1', title: 'Note', content: stored, tags: [] };
        }
      } };
    },
  });
  const api = load(saveDeclaration.getText(apiAst), {
    isDbVault: () => true,
    dbSaveNote: adapter.dbSaveNote,
  });
  return { save: api.saveNote, conflict, stored: () => stored, reads: () => reads };
}

test('DB save forwards the baseline and propagates stale rejection without reading back', async () => {
  const f = fixture();
  await assert.rejects(f.save('/vault.rn', 'n1', { id: 'n1', title: 'Note' }, 'GUI content', 'old content'),
    error => error === f.conflict);
  assert.equal(f.stored(), 'CLI content');
  assert.equal(f.reads(), 0);
});

test('DB save with the current baseline succeeds and returns the new raw content', async () => {
  const f = fixture();
  const saved = await f.save('/vault.rn', 'n1', { id: 'n1', title: 'Note' }, 'GUI content', 'CLI content');
  assert.equal(saved.raw, 'GUI content');
  assert.equal(f.stored(), 'GUI content');
  assert.equal(f.reads(), 1);
});
