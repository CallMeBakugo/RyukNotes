import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { createContext, Script } from 'node:vm';
import { parse } from 'svelte/compiler';
import { transformWithEsbuild } from 'vite';
import { RevisionSaveQueue } from '../src/lib/utils/revision-save.ts';
import { beginNoteNavigation, isCurrentNoteNavigation } from '../src/lib/utils/note-navigation.ts';

const source = await readFile(new URL('../src/lib/components/NoteList.svelte', import.meta.url), 'utf8');
const ast = parse(source, { modern: true });
const declaration = ast.instance.content.body.find(node =>
  node.type === 'ExportNamedDeclaration' && node.declaration?.id?.name === 'handleCreateNote'
).declaration;
const { code } = await transformWithEsbuild(source.slice(declaration.start, declaration.end), 'new-note.ts', {
  loader: 'ts', format: 'esm', target: 'esnext'
});
const createScript = new Script(`${code}\nhandleCreateNote;`);

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
}

function fixture(overrides = {}) {
  const vault = '/vault';
  const entry = { path: '/vault/Untitled.md', relative_path: 'Untitled.md', preview: '',
    meta: { id: 'created-id', title: 'Untitled', tags: [], pinned: false,
      created: '2026-10-05T10:00:00Z', modified: '2026-10-05T10:00:00Z' } };
  const created = { path: entry.path, meta: entry.meta, content: '',
    raw: '---\nid: created-id\ntitle: Untitled\ntags: []\npinned: false\ncreated: 2026-10-05T10:00:00Z\nmodified: 2026-10-05T10:00:00Z\n---\n\n' };
  const selected = [];
  const focused = [];
  const errors = [];
  const context = createContext({
    ownerVault: vault,
    ownerIsCurrent: () => context.$appConfig.active_vault === vault,
    beginNavigation: beginNoteNavigation,
    navigationIsCurrent: token => context.ownerIsCurrent() && isCurrentNoteNavigation(token),
    $appConfig: { active_vault: vault }, $viewMode: 'all', $sortMode: 'modified',
    $activeNotebook: null, $activeNotePath: null, $activeNote: null, $editorDirty: false, $notes: [],
    noteCache: new Map(), onBeforeNoteSwitch: async () => true,
    createNote: async (origin, notebook, title) => {
      assert.equal(origin, vault);
      assert.equal(notebook, null);
      assert.equal(title, 'Untitled');
      return entry;
    },
    readNote: async (origin, path) => {
      assert.equal(origin, vault);
      assert.equal(path, entry.path);
      return created;
    },
    onNoteMoved() {},
    onNoteSelected: (path, content) => selected.push({ path, content }),
    onNoteCreated: token => focused.push(token),
    console: { error: (...args) => errors.push(args) },
    ...overrides
  });
  return { context, entry, created, selected, focused, errors, create: createScript.runInContext(context) };
}

for (const mode of ['rich', 'source']) {
  test(`the first ${mode} edit saves before any watcher event using the created file's exact baseline`, async () => {
    const f = fixture();
    const saves = new RevisionSaveQueue();
    let saving;
    f.context.onNoteSelected = (path, content) => {
      assert.equal(path, f.entry.path);
      saves.startSession(f.context.$activeNote.raw);
      saves.markChanged();
      f.context.$editorDirty = true;
      saving = saves.save(saves.capture(), async expectedRaw => {
        assert.equal(expectedRaw, f.created.raw, 'first save must accept the file created on disk');
        return `${expectedRaw}First ${mode} edit\n`;
      });
    };
    await f.create();
    assert.equal(await saving, true);
    assert.equal(saves.dirty, false);
    assert.equal(f.context.$activeNote.raw, f.created.raw);
    assert.equal(f.focused.length, 1);
  });
}

test('creation loads current disk content and metadata instead of fabricating an empty note', async () => {
  const f = fixture();
  const current = { ...f.created, meta: { ...f.created.meta, title: 'External title', tags: ['external'] },
    content: 'External content', raw: '---\ntitle: External title\n---\nExternal content\n' };
  f.context.readNote = async () => current;
  await f.create();
  assert.equal(f.context.$activeNote, current);
  assert.equal(f.selected[0].content, current.content);
});

test('a delayed new-note read cannot replace a newer selected draft or steal title focus', async () => {
  const read = deferred();
  let reads = 0;
  const f = fixture({ readNote: () => { reads++; return read.promise; } });
  const creating = f.create();
  await new Promise(resolve => setImmediate(resolve));
  const newer = { path: '/vault/Other.md', content: 'Newer unsaved draft', raw: 'Other baseline' };
  beginNoteNavigation();
  f.context.$activeNote = newer;
  f.context.$activeNotePath = newer.path;
  f.context.$editorDirty = true;
  read.resolve(f.created);
  await creating;
  assert.equal(reads, 1);
  assert.equal(f.context.$activeNote, newer);
  assert.equal(f.context.$editorDirty, true);
  assert.equal(f.selected.length, 0);
  assert.equal(f.focused.length, 0);
});

test('a delayed new-note read cannot publish into another vault', async () => {
  const read = deferred();
  let reads = 0;
  const f = fixture({ readNote: () => { reads++; return read.promise; } });
  const creating = f.create();
  await new Promise(resolve => setImmediate(resolve));
  f.context.$appConfig = { active_vault: '/other-vault' };
  f.context.$activeNote = null;
  f.context.$activeNotePath = null;
  read.resolve(f.created);
  await creating;
  assert.equal(reads, 1);
  assert.equal(f.context.$activeNote, null);
  assert.equal(f.selected.length, 0);
  assert.equal(f.focused.length, 0);
});

test('a failed read keeps the existing draft and never opens a fabricated new note', async () => {
  const old = { path: '/vault/Old.md', content: 'Draft', raw: 'Old baseline' };
  const f = fixture({ $activeNote: old, $activeNotePath: old.path,
    readNote: async () => { throw new Error('Read failed'); } });
  await f.create();
  assert.equal(f.context.$activeNote, old);
  assert.equal(f.selected.length, 0);
  assert.equal(f.focused.length, 0);
  assert.equal(f.errors.length, 1);
});

test('typing while the new-note read is pending must pass another save flush before selection', async () => {
  const read = deferred();
  let flushes = 0;
  const old = { path: '/vault/Old.md', content: 'New draft', raw: 'Old baseline' };
  const f = fixture({ $activeNote: old, $activeNotePath: old.path,
    readNote: () => read.promise, onBeforeNoteSwitch: async () => ++flushes === 1 });
  const creating = f.create();
  await new Promise(resolve => setImmediate(resolve));
  f.context.$editorDirty = true;
  read.resolve(f.created);
  await creating;
  assert.equal(flushes, 2);
  assert.equal(f.context.$activeNote, old);
  assert.equal(f.context.$editorDirty, true);
  assert.equal(f.selected.length, 0);
  assert.equal(f.focused.length, 0);
});
