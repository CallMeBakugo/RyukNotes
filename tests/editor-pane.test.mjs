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

const workspaceModule = load(await readFile(new URL('../src/lib/stores/editor-workspace.ts', import.meta.url), 'utf8'), {
  'svelte/store': stores,
  svelte: {
    getContext: key => context.get(key),
    setContext: (key, value) => { context.set(key, value); return value; },
  },
  './editor-pane': pane,
});

test('workspace keeps stable contexts, opens one empty second pane and never exceeds two', async () => {
  const workspace = workspaceModule.createEditorWorkspace();
  const first = stores.get(workspace).panes[0];
  first.context.activeNotePath.set('/vault/first.md');
  first.context.editorDirty.set(true);
  workspace.openSplit();
  const split = stores.get(workspace);
  assert.equal(split.panes.length, 2);
  assert.equal(split.panes[0], first);
  assert.equal(split.activePaneId, split.panes[1].id);
  for (const key of ['activeNote', 'activeNotePath', 'viewerNote']) {
    assert.equal(stores.get(split.panes[1].context[key]), null);
  }
  assert.equal(stores.get(split.panes[1].context.editorDirty), false);
  workspace.openSplit();
  assert.equal(stores.get(workspace).panes[1], split.panes[1]);
  workspace.activate(first.id);
  assert.equal(stores.get(workspace.activePane), first);
  workspace.activate('absent');
  assert.equal(stores.get(workspace.activePane), first);
  await workspace.closeSecond(async () => true);
  assert.equal(stores.get(workspace).panes.length, 1);
  assert.equal(stores.get(workspace).panes[0], first);
  assert.equal(stores.get(first.context.editorDirty), true);
  workspace.openSplit();
  assert.notEqual(stores.get(workspace).panes[1].id, split.panes[1].id);
});

test('same-note selection activates its existing owner without touching either draft', () => {
  const workspace = workspaceModule.createEditorWorkspace();
  const first = stores.get(workspace).panes[0];
  const content = { path: '/vault/a.md', raw: 'disk', content: 'unsaved draft' };
  first.context.activeNote.set(content);
  first.context.activeNotePath.set(content.path);
  first.context.editorDirty.set(true);
  workspace.openSplit();
  const second = stores.get(workspace).panes[1];
  second.context.activeNotePath.set('/vault/b.md');
  assert.equal(workspace.activateOpenNote('\\vault\\a.md'), true);
  assert.equal(stores.get(workspace).activePaneId, first.id);
  assert.equal(stores.get(first.context.activeNote), content);
  assert.equal(stores.get(first.context.editorDirty), true);
  assert.equal(stores.get(second.context.activeNotePath), '/vault/b.md');
  assert.equal(workspace.activateOpenNote('/vault/unopened.md'), false);
  assert.equal(stores.get(workspace).activePaneId, first.id);
});

test('shared controls follow the selected pane but a captured host context never changes', async () => {
  const workspace = workspaceModule.createEditorWorkspace();
  context = new Map();
  workspaceModule.provideEditorWorkspace(workspace);
  const active = workspaceModule.getActiveEditorPane();
  const first = stores.get(active);
  const seen = [];
  const unsubscribe = active.subscribe(value => seen.push(value));
  workspace.openSplit();
  const second = stores.get(active);
  assert.notEqual(second, first);
  assert.equal(first, stores.get(workspace).panes[0].context);
  await workspace.closeSecond(async () => true);
  assert.equal(stores.get(active), first);
  assert.equal(seen.length, 3);
  unsubscribe();
  context = new Map();
  assert.equal(stores.get(workspaceModule.getActiveEditorPane()), pane.defaultEditorPane);
});

test('split ratio is bounded by available width and recovers from invalid measurements', () => {
  const workspace = workspaceModule.createEditorWorkspace();
  workspace.resize(-1, 1000);
  assert.equal(stores.get(workspace).splitRatio, 0.24);
  workspace.resize(9, 1000);
  assert.equal(stores.get(workspace).splitRatio, 0.76);
  workspace.resize(0.2, 400);
  assert.equal(stores.get(workspace).splitRatio, 0.5);
  workspace.resize(NaN, 1000);
  assert.equal(stores.get(workspace).splitRatio, 0.5);
  workspace.resize(0.8, 0);
  assert.equal(stores.get(workspace).splitRatio, 0.5);
  workspace.resize(0.9, 4000);
  assert.equal(stores.get(workspace).splitRatio, 0.8);
  workspace.resize(0, 4000);
  assert.equal(stores.get(workspace).splitRatio, 0.2);
});


test('a rejected pane close preserves both contexts and focuses the unsaved owner', async () => {
  const workspace = workspaceModule.createEditorWorkspace();
  workspace.openSplit();
  const before = stores.get(workspace);
  const second = before.panes[1];
  second.context.editorDirty.set(true);
  workspace.activate(before.panes[0].id);
  const closed = await workspace.closeSecond(async owner => {
    assert.equal(owner, second);
    return false;
  });
  assert.equal(closed, false);
  assert.equal(stores.get(workspace).panes, before.panes);
  assert.equal(stores.get(workspace).activePaneId, second.id);
  assert.equal(stores.get(second.context.editorDirty), true);
});

test('a delayed close cannot remove a newly opened replacement pane', async () => {
  const workspace = workspaceModule.createEditorWorkspace();
  workspace.openSplit();
  let finish;
  const delayed = workspace.closeSecond(() => new Promise(resolve => { finish = resolve; }));
  await workspace.closeSecond(async () => true);
  workspace.openSplit();
  const replacement = stores.get(workspace).panes[1];
  finish(true);
  assert.equal(await delayed, false);
  assert.equal(stores.get(workspace).panes[1], replacement);
});
