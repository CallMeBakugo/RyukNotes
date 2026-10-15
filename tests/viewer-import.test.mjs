import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { createContext, Script } from 'node:vm';
import { parse } from 'svelte/compiler';
import { transformWithEsbuild } from 'vite';

const source = await readFile(new URL('../src/lib/components/Editor.svelte', import.meta.url), 'utf8');
const declaration = parse(source, { modern: true }).instance.content.body.find(node =>
  node.type === 'FunctionDeclaration' && node.id.name === 'viewerImportTo'
);
const { code } = await transformWithEsbuild(source.slice(declaration.start, declaration.end), 'viewer-import.ts', {
  loader: 'ts', format: 'esm', target: 'esnext'
});
const importScript = new Script(`${code}\nviewerImportTo;`);

function fixture(path = '/external/External.md') {
  const original = { path, meta: { title: 'External' },
    content: '![Image](images/photo.png)', raw: '---\ncustom: keep\n---\n![Image](images/photo.png)' };
  const imported = { ...original, path: '/vault/External.md',
    content: '![Image](.ryuknotes/attachments/unique-photo.png)',
    raw: '---\ncustom: keep\n---\n![Image](.ryuknotes/attachments/unique-photo.png)' };
  let current = true;
  const calls = [];
  const loaded = [];
  const messages = [];
  const files = new Map([[path, original]]);
  const context = createContext({
    beginEditorNoteNavigation: () => () => current,
    $viewerNote: { path, content: original.content }, $activeNote: original,
    $appConfig: { active_vault: '/vault' }, $readOnly: true, $focusMode: true,
    $activeNotePath: path, $editorDirty: false, viewerImportBusy: false, viewerImportPickerOpen: true,
    readNote: async (_vault, target) => {
      if (!files.has(target)) throw new Error('Not found');
      return files.get(target);
    },
    copyFileTo: async (origin, target) => files.set(target, { ...files.get(origin), path: target }),
    externalNoteReferences: () => [{ from: 24, to: 40, href: 'images/photo.png', html: false }],
    importNote: async (...args) => { calls.push(args); files.set(imported.path, imported); return imported; },
    loadNote: (target, content) => loaded.push({ target, content }),
    viewerFlash: message => messages.push(message), console: { error() {} }
  });
  return { context, original, imported, calls, loaded, messages,
    cancel: () => { current = false; }, run: importScript.runInContext(context) };
}

test('notebook destinations use portable relative paths rather than duplicating the vault path', async () => {
  const ast = parse(source, { modern: true });
  const declaration = ast.instance.content.body.flatMap(node => node.declarations ?? []).find(node => node.id.name === 'viewerFlatNotebooks');
  const expression = declaration.init.arguments[0];
  const transformed = await transformWithEsbuild(source.slice(expression.start, expression.end), 'viewer-notebooks.ts', { loader: 'ts', format: 'esm' });
  const list = new Script(`(${transformed.code.replace(/;\s*$/, '')})()`).runInNewContext({
    viewerImportPickerOpen: true,
    $notebooks: [{ path: 'C:\\Vault\\Projects', relative_path: 'Projects', name: 'Projects', children: [
      { path: 'C:\\Vault\\Projects\\Nested', relative_path: 'Projects\\Nested', name: 'Nested', children: [] }
    ] }]
  });
  assert.deepEqual(Array.from(list, entry => entry.path), ['Projects', 'Projects/Nested']);
});

test('Windows viewer imports use the native filename instead of treating a full path as a filename', async () => {
  const f = fixture('C:\\Users\\writer\\External.md');
  await f.run('');
  assert.equal(f.context.$activeNotePath, '/vault/External.md');
  assert.equal(f.context.$viewerNote, null);
});

test('viewer import uses the accepted raw note and semantic attachment references in one native operation', async () => {
  const f = fixture();
  await f.run('Projects');
  assert.equal(f.calls.length, 1, 'copying Markdown alone must not bypass attachment import');
  const [vault, path, notebook, raw, references] = f.calls[0];
  assert.equal(vault, '/vault');
  assert.equal(path, f.original.path);
  assert.equal(notebook, 'Projects');
  assert.equal(raw, f.original.raw);
  assert.equal(references[0].href, 'images/photo.png');
  assert.equal(f.loaded[0].content, f.imported.content);
  assert.equal(f.original.content, '![Image](images/photo.png)');
});

test('failed native import leaves the original read-only viewer selected and allows retry', async () => {
  const f = fixture();
  f.context.importNote = async () => { throw new Error('Attachment permission denied'); };
  await f.run('');
  assert.equal(f.context.$activeNote, f.original);
  assert.equal(f.context.$viewerNote.path, f.original.path);
  assert.equal(f.context.$readOnly, true);
  assert.equal(f.context.viewerImportBusy, false);
  assert.match(f.messages[0], /Attachment permission denied/);
});

test('an import completion cannot steal selection after the viewer is closed or navigation changes', async () => {
  const f = fixture();
  let finish;
  f.context.importNote = () => new Promise(resolve => { finish = resolve; });
  const importing = f.run('');
  await new Promise(resolve => setImmediate(resolve));
  f.cancel();
  const newer = { path: '/vault/Newer.md', content: 'Unsaved draft' };
  f.context.$activeNote = newer;
  f.context.$activeNotePath = newer.path;
  f.context.$viewerNote = null;
  if (finish) finish(f.imported);
  await importing;
  assert.equal(typeof finish, 'function', 'the viewer must delegate to the native import');
  assert.equal(f.context.$activeNote, newer);
  assert.equal(f.loaded.length, 0);
});
