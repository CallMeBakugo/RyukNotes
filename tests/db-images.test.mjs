import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { runInNewContext } from 'node:vm';
import ts from 'typescript';
import { parse } from 'svelte/compiler';
import { encodeMarkdownPath } from '../src/lib/utils/paths.ts';

const adapterSource = await readFile(new URL('../src/lib/api-db.ts', import.meta.url), 'utf8');
const apiSource = await readFile(new URL('../src/lib/api.ts', import.meta.url), 'utf8');
const apiAst = ts.createSourceFile('api.ts', apiSource, ts.ScriptTarget.Latest, true);
const imageDeclaration = apiAst.statements.find(node => ts.isFunctionDeclaration(node) && node.name.text === 'saveImage');

function load(source, globals) {
  const exports = {};
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  });
  runInNewContext(outputText, { exports, ...globals });
  return exports;
}

function fixture(invoke) {
  const adapter = load(adapterSource, {
    require: name => {
      assert.equal(name, '@tauri-apps/api/core');
      return { invoke };
    },
  });
  return load(imageDeclaration.getText(apiAst), {
    looksLikeDbVault: path => path.endsWith('.rn'),
    dbSaveImage: adapter.dbSaveImage,
    invoke,
  });
}

test('DB image insertion forwards the captured note and vault, leaving bytes unchanged', async () => {
  const bytes = [137, 80, 78, 71, 13, 10, 26, 10];
  const calls = [];
  const api = fixture(async (command, args) => {
    calls.push([command, args]);
    return '.rn-attachments/54fd232f-c42a-432b-8ba8-e1b346d23a07';
  });
  const stored = await api.saveImage('/notes/vault.rn', 'clipboard.png', bytes, 'captured-note');
  assert.equal(stored, '.rn-attachments/54fd232f-c42a-432b-8ba8-e1b346d23a07');
  assert.equal(calls.length, 1);
  assert.equal(calls[0][0], 'db_save_image');
  assert.equal(calls[0][1].vaultPath, '/notes/vault.rn');
  assert.equal(calls[0][1].noteId, 'captured-note');
  assert.equal(calls[0][1].name, 'clipboard.png');
  assert.equal(calls[0][1].data, bytes);
});

test('DB insertion refuses a missing owner before invoking native code', async () => {
  const api = fixture(() => assert.fail('must not insert without a captured owner'));
  await assert.rejects(api.saveImage('/notes/vault.rn', 'clipboard.png', []), /Select a note/);
});

test('filesystem images keep the original command and argument shape', async () => {
  const api = fixture(async (command, args) => {
    assert.equal(command, 'save_image');
    assert.equal(JSON.stringify(args), JSON.stringify({ vaultPath: '/notes', name: 'file.png', data: [1] }));
    return '.ryuknotes/attachments/file.png';
  });
  assert.equal(await api.saveImage('/notes', 'file.png', [1], 'irrelevant'), '.ryuknotes/attachments/file.png');
});

test('DB command rejection propagates without a filesystem fallback', async () => {
  const error = new Error('vault switched');
  let calls = 0;
  const api = fixture(async command => {
    calls++;
    assert.equal(command, 'db_save_image');
    throw error;
  });
  await assert.rejects(api.saveImage('/notes/vault.rn', 'clipboard.png', [1], 'n1'), e => e === error);
  assert.equal(calls, 1);
});


const editorSource = await readFile(new URL('../src/lib/components/Editor.svelte', import.meta.url), 'utf8');
const editorAst = parse(editorSource, { modern: true });
const insertionNames = ['handleFileDrop', 'handleFilePaste', 'insertFile', 'uploadAsset', 'insertClipboardImage'];
const insertionSource = editorAst.instance.content.body
  .filter(node => node.type === 'FunctionDeclaration' && insertionNames.includes(node.id.name))
  .map(node => editorSource.slice(node.start, node.end)).join('\n');

function editorFixture(invoke, clipboard) {
  const origin = { vault: '/notes/vault.rn', path: 'captured-note', noteId: 'captured-note', sourceMode: true };
  let current = origin;
  let complete;
  const inserted = new Promise(resolve => { complete = resolve; });
  const api = fixture(invoke);
  const editor = load(`${insertionSource}\nexport { handleFileDrop, handleFilePaste, insertClipboardImage };`, {
    workspace: null,
    captureOrigin: () => origin,
    canInsert: candidate => candidate === current,
    readClipboardImage: clipboard,
    saveImage: api.saveImage,
    saveAttachment: () => assert.fail('images must not use the filesystem attachment command'),
    encodeMarkdownPath,
    source: { state: { selection: { main: { from: 0, to: 0 } } }, replace: text => complete(text) },
    console,
  });
  return { ...editor, inserted, switchNote: () => { current = { ...origin, noteId: 'other' }; } };
}

for (const input of ['drop', 'paste']) {
  test(`${input} uses the captured owner and inserts a durable DB image reference`, async () => {
    const reference = '.rn-attachments/54fd232f-c42a-432b-8ba8-e1b346d23a07';
    const f = editorFixture(async (command, args) => {
      assert.equal(command, 'db_save_image');
      assert.equal(args.noteId, 'captured-note');
      assert.equal(args.vaultPath, '/notes/vault.rn');
      assert.equal(JSON.stringify(args.data), '[137,80,78,71,13,10,26,10]');
      return reference;
    });
    const file = { name: 'clipboard.png', type: 'image/png', arrayBuffer: async () => Uint8Array.from([137,80,78,71,13,10,26,10]).buffer };
    let prevented = false;
    const event = { preventDefault: () => { prevented = true; } };
    if (input === 'drop') {
      event.dataTransfer = { files: [file] };
      assert.equal(f.handleFileDrop(event), true);
    } else {
      event.clipboardData = { items: [{ getAsFile: () => file }] };
      assert.equal(f.handleFilePaste(event), true);
    }
    assert.equal(prevented, true);
    assert.equal(await f.inserted, `![clipboard.png](${reference}) `);
  });
}

test('a clipboard read completing after a note switch cannot insert or attach to the new note', async () => {
  let resolveClipboard;
  const clipboard = new Promise(resolve => { resolveClipboard = resolve; });
  const f = editorFixture(() => assert.fail('stale clipboard read must not invoke storage'), () => clipboard);
  const pending = f.insertClipboardImage();
  f.switchNote();
  resolveClipboard([137, 80, 78, 71]);
  assert.equal(await pending, false);
});
