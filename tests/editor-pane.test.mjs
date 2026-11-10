import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { runInNewContext } from 'node:vm';
import * as stores from 'svelte/store';
import ts from 'typescript';
import * as navigation from '../src/lib/utils/note-navigation.ts';
import * as naturalSort from '../src/lib/utils/natural-sort.ts';

function load(source, imports) {
  const exports = {};
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  });
  runInNewContext(outputText, {
    exports,
    require: name => {
      assert.ok(name in imports, `Unexpected dependency: ${name}`);
      return imports[name];
    },
  });
  return exports;
}

const app = load(await readFile(new URL('../src/lib/stores/app.ts', import.meta.url), 'utf8'), {
  'svelte/store': stores,
  '$lib/platform': { isMobile: false },
  '$lib/utils/natural-sort': naturalSort,
});
let context;
const pane = load(await readFile(new URL('../src/lib/stores/editor-pane.ts', import.meta.url), 'utf8'), {
  'svelte/store': stores,
  svelte: {
    getContext: key => context.get(key),
    setContext: (key, value) => { context.set(key, value); return value; },
  },
  './app': app,
  '../utils/note-navigation': navigation,
});

// Context calls are injected; the stores/history/navigation below are production code.
test('single-editor fallback and detached default provider preserve exact legacy dependencies', () => {
  context = new Map();
  assert.equal(pane.getEditorPane(), pane.defaultEditorPane);
  assert.equal(pane.provideEditorPane(), pane.defaultEditorPane);
  assert.equal(pane.getEditorPane(), pane.defaultEditorPane);
  for (const key of ['activeNote', 'activeNotePath', 'viewerNote', 'editorDirty',
    'sourceMode', 'readOnly', 'navHistory', 'canGoBack', 'canGoForward']) {
    assert.equal(pane.defaultEditorPane[key], app[key], key);
  }
  for (const key of ['beginNoteNavigation', 'isCurrentNoteNavigation', 'cancelNoteNavigation']) {
    assert.equal(pane.defaultEditorPane[key], navigation[key], key);
  }
  const token = navigation.beginNoteNavigation();
  assert.equal(pane.defaultEditorPane.isCurrentNoteNavigation(token), true);
  pane.defaultEditorPane.beginNoteNavigation();
  assert.equal(navigation.isCurrentNoteNavigation(token), false);
});

test('provided pane dependencies remain bound while another pane context is selected', () => {
  const left = pane.createEditorPane();
  const right = pane.createEditorPane();
  context = new Map();
  pane.provideEditorPane(left);
  const captured = pane.getEditorPane();
  context = new Map();
  pane.provideEditorPane(right);
  assert.equal(pane.getEditorPane(), right);
  assert.equal(captured, left);

  const content = { path: 'left-note', raw: 'accepted', content: 'draft', meta: { id: 'left' } };
  captured.activeNote.set(content);
  captured.activeNotePath.set(content.path);
  captured.viewerNote.set({ path: 'external.md', content: 'viewer' });
  for (const key of ['editorDirty', 'sourceMode', 'readOnly']) captured[key].set(true);
  assert.equal(stores.get(left.activeNote), content);
  assert.equal(stores.get(right.activeNote), null);
  assert.equal(stores.get(right.activeNotePath), null);
  assert.equal(stores.get(right.viewerNote), null);
  for (const key of ['editorDirty', 'sourceMode', 'readOnly']) {
    assert.equal(stores.get(left[key]), true);
    assert.equal(stores.get(right[key]), false);
    assert.equal(stores.get(app[key]), false);
  }
  assert.equal(stores.get(app.activeNote), null);
});

test('pane history traversal/remapping and navigation cancellation do not cross owners', () => {
  const left = pane.createEditorPane();
  const right = pane.createEditorPane();
  left.navHistory.push('/vault/old/a.md');
  left.navHistory.push('/vault/old/b.md');
  right.navHistory.push('/vault/right.md');
  assert.equal(stores.get(left.canGoBack), true);
  assert.equal(stores.get(right.canGoBack), false);
  assert.equal(left.navHistory.go(-1), '/vault/old/a.md');
  left.navHistory.push('/vault/old/a.md'); // Consume the history traversal's skip.
  assert.equal(stores.get(left.canGoForward), true);
  left.navHistory.remap('/vault/old', '/vault/new');
  assert.equal(left.navHistory.go(1), '/vault/new/b.md');
  assert.equal(stores.get(right.navHistory).stack[0], '/vault/right.md');
  left.navHistory.clear();
  assert.equal(stores.get(right.navHistory).index, 0);

  const token = left.beginNoteNavigation();
  const rightToken = right.beginNoteNavigation();
  right.cancelNoteNavigation(rightToken);
  assert.equal(left.isCurrentNoteNavigation(token), true);
  assert.equal(right.isCurrentNoteNavigation(rightToken), false);
  const newer = left.beginNoteNavigation();
  left.cancelNoteNavigation(token);
  assert.equal(left.isCurrentNoteNavigation(newer), true);
  left.cancelNoteNavigation(newer);
  assert.equal(left.isCurrentNoteNavigation(newer), false);
});
