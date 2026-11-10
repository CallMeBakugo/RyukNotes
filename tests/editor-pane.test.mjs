import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { runInNewContext } from 'node:vm';
import * as stores from 'svelte/store';
import ts from 'typescript';
import * as editorEvents from '../src/lib/utils/editor-events.ts';
import * as navigation from '../src/lib/utils/note-navigation.ts';
import * as naturalSort from '../src/lib/utils/natural-sort.ts';

function load(source, imports, globals = {}) {
  const exports = {};
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  });
  runInNewContext(outputText, {
    exports,
    ...globals,
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
  '../utils/editor-events': editorEvents,
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

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

test('navigation captures its destination and only newer intents in that pane cancel it', async () => {
  const workspace = workspaceModule.createEditorWorkspace();
  const left = stores.get(workspace).panes[0];
  const a = workspace.captureNavigation();
  const load = deferred();
  const publishing = load.promise.then(value => { if (a.isCurrent()) a.context.activeNotePath.set(value); });
  workspace.openSplit();
  const right = stores.get(workspace).panes[1];
  const b = workspace.captureNavigation();
  const newerB = workspace.captureNavigation();
  assert.equal(a.isCurrent(), true);
  assert.equal(b.isCurrent(), false);
  assert.equal(newerB.isCurrent(), true);
  load.resolve('left-note');
  await publishing;
  assert.equal(stores.get(left.context.activeNotePath), 'left-note');
  assert.equal(stores.get(right.context.activeNotePath), null);
  const newerA = workspace.captureNavigation(left.context);
  assert.equal(a.isCurrent(), false);
  assert.equal(newerA.isCurrent(), true);
  assert.equal(newerB.isCurrent(), true);
});

test('same-note destination capture uses the existing owner and cancels its older load', () => {
  const workspace = workspaceModule.createEditorWorkspace();
  const left = stores.get(workspace).panes[0];
  left.context.activeNotePath.set('already-open');
  const pending = workspace.captureNavigation();
  workspace.openSplit();
  const rightPending = workspace.captureNavigation();
  const selecting = workspace.captureForPath('already-open');
  assert.equal(selecting.context, left.context);
  assert.equal(pending.isCurrent(), false);
  assert.equal(rightPending.isCurrent(), true);
  assert.equal(workspace.activateOpenNote('already-open', selecting.context), true);
  assert.equal(selecting.isCurrent(), true);
});

function barrierFixture() {
  const workspace = workspaceModule.createEditorWorkspace();
  workspace.openSplit();
  const owners = stores.get(workspace).panes;
  owners[0].context.activeNotePath.set('/vault/folder/a.md');
  owners[1].context.activeNotePath.set('/vault/folder/b.md');
  const locks = new Set();
  const calls = [];
  for (const owner of owners) {
    workspace.registerEditor(owner.id, {
      flushSave: async () => { owner.context.editorDirty.set(false); return true; },
      beginPathMutation: async () => {
        calls.push(owner.id);
        owner.context.editorDirty.set(false);
        locks.add(owner.id);
        return () => locks.delete(owner.id);
      },
    });
  }
  return { workspace, owners, locks, calls };
}

test('all-pane maintenance locks every owner through reconciliation and aggregates inactive dirtiness', async () => {
  const f = barrierFixture();
  f.owners[0].context.editorDirty.set(true);
  f.owners[1].context.editorDirty.set(true);
  assert.equal(stores.get(f.workspace.anyDirty), true);
  const oldIntent = f.workspace.captureNavigation();
  const reload = deferred();
  f.workspace.onReconcile(async owners => {
    assert.equal(owners.length, 2);
    assert.equal(f.locks.size, 2);
    await reload.promise;
  });
  const release = await f.workspace.acquireMutation();
  assert.ok(release);
  assert.equal(stores.get(f.workspace.anyDirty), false);
  assert.equal(f.locks.size, 2);
  assert.equal(f.workspace.isBlocked(), true);
  assert.equal(oldIntent.isCurrent(), false);
  assert.equal(f.workspace.captureNavigation().isCurrent(), false);
  assert.equal(await f.workspace.acquireMutation(), null);
  const finishing = release();
  assert.equal(f.locks.size, 2);
  reload.resolve();
  await finishing;
  await release();
  assert.equal(f.locks.size, 0);
  assert.equal(f.workspace.isBlocked(), false);
  assert.equal(oldIntent.isCurrent(), false, 'pre-maintenance reads stay invalid after unlock');
});

test('failed acquisition rolls back earlier locks and retains the unsaved inactive pane', async () => {
  const f = barrierFixture();
  const second = f.owners[1];
  second.context.editorDirty.set(true);
  f.workspace.registerEditor(second.id, { beginPathMutation: async () => null });
  let mutations = 0;
  const release = await f.workspace.acquireMutation();
  if (release) { mutations++; await release(); }
  assert.equal(mutations, 0);
  assert.equal(f.locks.size, 0);
  assert.equal(f.workspace.isBlocked(), false);
  assert.equal(stores.get(second.context.editorDirty), true);
  assert.equal(stores.get(f.workspace.anyDirty), true);
});

test('targeted mutations resolve every affected owner and do not lock sibling prefixes', async () => {
  const f = barrierFixture();
  f.owners[1].context.activeNotePath.set('/vault/folder-other/b.md');
  const release = await f.workspace.acquireMutation(['/vault/folder']);
  assert.ok(release);
  assert.equal(f.calls.length, 1);
  assert.equal(f.calls[0], f.owners[0].id);
  await release();
  f.workspace.remapPaths('/vault/folder', '/vault/moved');
  assert.equal(stores.get(f.owners[0].context.activeNotePath), '/vault/moved/a.md');
  assert.equal(stores.get(f.owners[1].context.activeNotePath), '/vault/folder-other/b.md');
});

test('flush-all rechecks aggregate dirtiness after the final save and handles rejected saves', async () => {
  const f = barrierFixture();
  const pending = deferred();
  f.workspace.registerEditor(f.owners[1].id, { flushSave: () => pending.promise });
  const flushing = f.workspace.flushAll();
  await new Promise(resolve => setImmediate(resolve));
  f.owners[0].context.editorDirty.set(true);
  pending.resolve(true);
  assert.equal(await flushing, false);
  f.workspace.registerEditor(f.owners[1].id, { flushSave: async () => { throw new Error('disk failure'); } });
  assert.equal(await f.workspace.flushAll(), false);
});


// Execute the actual shared-control open function, with deferred API reads, rather
// than a second implementation of its async navigation algorithm.
const layoutSource = await readFile(new URL('../src/lib/components/AppLayout.svelte', import.meta.url), 'utf8');
const layoutScript = layoutSource.slice(layoutSource.indexOf('>') + 1, layoutSource.indexOf('</script>'));
const layoutAst = ts.createSourceFile('layout.ts', layoutScript, ts.ScriptTarget.Latest, true);
const openDeclaration = layoutAst.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === 'selectNoteFromSwitcher');

function controlFixture() {
  const workspace = workspaceModule.createEditorWorkspace();
  const left = stores.get(workspace).panes[0];
  workspace.openSplit();
  const right = stores.get(workspace).panes[1];
  workspace.activate(left.id);
  const reads = new Map();
  const opened = [];
  const controls = load(openDeclaration.getText(layoutAst) + '\nexports.open = selectNoteFromSwitcher;', {}, {
    get: stores.get,
    ownerVault: '/vault',
    beginNavigation: path => workspace.captureForPath(path),
    navigationIsCurrent: token => token.isCurrent(),
    activateOpenNote: (path, token) => workspace.activateOpenNote(path, token?.context),
    editorFor: () => ({ flushSave: async () => true }),
    readNote: async (_, path) => {
      const read = deferred();
      reads.set(path, read);
      return read.promise;
    },
    handleNoteSelected: (path, _, __, token) => opened.push({ path, id: token.paneId }),
  });
  return { workspace, left, right, reads, opened, open: controls.open };
}

test('shared-control loads complete in their captured panes when focus changes during both reads', async () => {
  const f = controlFixture();
  const a = f.open('/vault/a.md');
  await new Promise(resolve => setImmediate(resolve));
  f.workspace.activate(f.right.id);
  const b = f.open('/vault/b.md');
  await new Promise(resolve => setImmediate(resolve));
  f.reads.get('/vault/b.md').resolve({ path: '/vault/b.md', content: 'right' });
  assert.equal(await b, true);
  f.reads.get('/vault/a.md').resolve({ path: '/vault/a.md', content: 'left' });
  assert.equal(await a, true);
  assert.equal(stores.get(f.left.context.activeNotePath), '/vault/a.md');
  assert.equal(stores.get(f.right.context.activeNotePath), '/vault/b.md');
  assert.equal(stores.get(f.workspace).activePaneId, f.right.id);
});

test('an older shared-control read cannot overwrite a newer intent in its destination', async () => {
  const f = controlFixture();
  const old = f.open('/vault/old.md');
  await new Promise(resolve => setImmediate(resolve));
  const latest = f.open('/vault/latest.md');
  await new Promise(resolve => setImmediate(resolve));
  f.reads.get('/vault/latest.md').resolve({ path: '/vault/latest.md', content: 'new' });
  assert.equal(await latest, true);
  f.reads.get('/vault/old.md').resolve({ path: '/vault/old.md', content: 'stale' });
  assert.equal(await old, false);
  assert.equal(stores.get(f.left.context.activeNotePath), '/vault/latest.md');
});

test('shared-control selection of an already-open note preserves its unsaved buffer without reading', async () => {
  const f = controlFixture();
  const draft = { path: '/vault/open.md', content: 'unsaved' };
  f.right.context.activeNote.set(draft);
  f.right.context.activeNotePath.set(draft.path);
  f.right.context.editorDirty.set(true);
  assert.equal(await f.open(draft.path), true);
  assert.equal(stores.get(f.workspace).activePaneId, f.right.id);
  assert.equal(stores.get(f.right.context.activeNote), draft);
  assert.equal(stores.get(f.right.context.editorDirty), true);
  assert.equal(f.reads.size, 0);
});

test('focus intent captured with navigation expires on away-and-back without cancelling the load', () => {
  const workspace = workspaceModule.createEditorWorkspace();
  const first = stores.get(workspace).panes[0];
  const intent = workspace.captureNavigation();
  assert.equal(intent.focusIsCurrent(), true);
  workspace.openSplit();
  workspace.activate(first.id);
  assert.equal(intent.isCurrent(), true);
  assert.equal(intent.focusIsCurrent(), false);
  const latest = workspace.captureNavigation();
  editorEvents.editorFocus.invalidate();
  assert.equal(latest.isCurrent(), true);
  assert.equal(latest.focusIsCurrent(), false);
});

test('attachment operation leases survive pane focus changes but never cross a maintenance epoch', async () => {
  const f = barrierFixture();
  const captured = f.workspace.captureOperation();
  const navigation = f.workspace.captureNavigation(f.owners[0].context);
  f.workspace.activate(f.owners[1].id);
  assert.equal(captured(), true);
  assert.equal(navigation.isCurrent(), true);
  const release = await f.workspace.acquireMutation();
  assert.ok(release);
  assert.equal(captured(), false);
  const during = f.workspace.captureOperation();
  await release();
  assert.equal(captured(), false);
  // Even operations captured while blocked cannot become valid on unlock.
  assert.equal(during(), false);
  const fresh = f.workspace.captureOperation();
  assert.equal(fresh(), true);
  f.workspace.dispose();
  assert.equal(fresh(), false);
});
