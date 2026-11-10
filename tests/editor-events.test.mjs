import assert from 'node:assert/strict';
import test from 'node:test';
import { readFile } from 'node:fs/promises';
import { runInNewContext } from 'node:vm';
import ts from 'typescript';
import { compile } from 'svelte/compiler';
import { render } from 'svelte/server';
import * as server from 'svelte/internal/server';
import * as svelte from 'svelte';
import { get } from 'svelte/store';
import * as history from '../src/lib/utils/history-comparison.ts';
import * as events from '../src/lib/utils/editor-events.ts';

const editorSource = await readFile(new URL('../src/lib/components/Editor.svelte', import.meta.url), 'utf8');
const layoutSource = await readFile(new URL('../src/routes/+layout.svelte', import.meta.url), 'utf8');
const historySource = await readFile(new URL('../src/lib/components/HistoryReview.svelte', import.meta.url), 'utf8');

function execute(code, globals = {}, imports = {}) {
  const exports = {};
  const context = { exports, ...globals, require: name => {
    assert.ok(name in imports, `Unexpected import ${name}`);
    return imports[name];
  } };
  runInNewContext(ts.transpileModule(code, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText, context);
  return { exports, context };
}
function editorFunction(name, globals) {
  const script = editorSource.slice(editorSource.indexOf('>') + 1, editorSource.indexOf('</script>'));
  const ast = ts.createSourceFile('Editor.ts', script, ts.ScriptTarget.Latest, true);
  const node = ast.statements.find(n => ts.isFunctionDeclaration(n) && n.name?.text === name);
  assert.ok(node, name);
  const loaded = execute(node.getText(ast) + `\nexports.handler = ${name};`, globals);
  return { handler: loaded.exports.handler, context: loaded.context };
}

test('one open-note broadcast retains its owner even if its handler switches the active pane', () => {
  const state = { activePaneId: 'left', panes: [] };
  const left = {};
  const right = {};
  state.panes = [{ id: 'left', context: left }, { id: 'right', context: right }];
  const workspace = { subscribe: fn => { fn(state); return () => {}; }, isBlocked: () => false };
  const calls = [];
  const handler = paneContext => editorFunction('handleOpenNote', {
    workspace, paneContext, get, componentId: 'component', editorDisposed: false,
    openNoteRequest: events.openNoteRequest,
    editorOpenNotes: events.createOpenNoteRouter(),
    navigateToWikiLink: path => { calls.push(path); state.activePaneId = 'right'; },
  });
  const router = events.createOpenNoteRouter();
  const a = handler(left);
  const b = handler(right);
  a.context.editorOpenNotes = router;
  b.context.editorOpenNotes = router;
  const event = { detail: 'note.md' };
  a.handler(event);
  b.handler(event);
  a.handler(event);
  assert.deepEqual(calls, ['note.md']);
  state.activePaneId = 'left';
  const targeted = { detail: { path: 'right.md', paneId: 'right' } };
  a.handler(targeted);
  b.handler(targeted);
  assert.deepEqual(calls, ['note.md', 'right.md']);
  a.handler({ detail: null });
  b.handler({ detail: { path: 'bad', paneId: 'missing' } });
  assert.equal(calls.length, 2);
});

test('layout link resolution uses the clicked pane before resolving a relative note path', () => {
  const script = layoutSource.slice(layoutSource.indexOf('>') + 1, layoutSource.indexOf('</script>'));
  const ast = ts.createSourceFile('layout.ts', script, ts.ScriptTarget.Latest, true);
  const node = ast.statements.find(n => ts.isFunctionDeclaration(n) && n.name?.text === 'resolveAndHandleLink');
  const sent = [];
  const handler = execute(node.getText(ast) + '\nexports.open = resolveAndHandleLink;', {
    get: key => key === 'config' ? { active_vault: '/vault', vaults: [] } : '/wrong-pane/note.md',
    appConfig: 'config', activeNotePath: 'path', isAndroid: false,
    noteAssetRoot: note => { assert.equal(note, '/vault/right/current.md'); return '/vault'; },
    resolveFileReference: (href, note) => {
      assert.equal(href, 'next.md');
      assert.equal(note, '/vault/right/current.md');
      return { path: '/vault/right/next.md' };
    },
    window: { dispatchEvent: event => sent.push(event) },
    CustomEvent: class { constructor(type, init) { this.type = type; this.detail = init.detail; } },
  }).exports.open;
  handler('next.md', { closest: () => ({ dataset: { paneId: 'right', notePath: '/vault/right/current.md' } }) });
  assert.equal(sent[0].type, 'ryuknotes-open-note');
  assert.equal(sent[0].detail.path, '/vault/right/next.md');
  assert.equal(sent[0].detail.paneId, 'right');
});

function domFixture() {
  const root = (left, right) => ({
    getBoundingClientRect: () => ({ left, right, top: 0, bottom: 200 }),
    contains(node) { return node === this || node?.parent === this; },
    closest() { return this; },
  });
  const left = root(0, 100);
  const right = root(105, 205);
  const document = {
    overlays: [], activeElement: null,
    querySelectorAll() { return this.overlays; },
    defaultView: { getComputedStyle: element => ({ visibility: element.visibility ?? 'visible' }) },
    elementFromPoint(x, y) {
      if (y < 0 || y >= 200) return null;
      return x >= 0 && x < 100 ? left : x >= 105 && x < 205 ? right : null;
    },
  };
  return { left, right, document };
}

test('native drops hit-test physical coordinates once, including an inactive pane and separator misses', () => {
  const f = domFixture();
  const router = events.createNativeDropRouter();
  const calls = [];
  router.register('left', { element: () => f.left, insert: paths => calls.push(['left', paths]) });
  const remove = router.register('right', { element: () => f.right, insert: paths => calls.push(['right', paths]) });
  assert.equal(router.dispatch(['image.png'], { x: 250, y: 100 }, 2, f.document), true);
  assert.deepEqual(calls, [['right', ['image.png']]]);
  assert.equal(router.dispatch(['other.png'], { x: 204, y: 100 }, 2, f.document), false);
  assert.equal(router.dispatch(['other.png'], { x: 250, y: 100 }, 0, f.document), false);
  f.document.overlays = [{ getClientRects: () => [1] }];
  assert.equal(router.dispatch(['blocked.png'], { x: 250, y: 100 }, 2, f.document), false);
  f.document.overlays = [];
  remove();
  assert.equal(router.dispatch(['removed.png'], { x: 250, y: 100 }, 2, f.document), false);
  assert.equal(calls.length, 1);
  assert.equal(editorSource.includes('.onDragDropEvent('), false);
  assert.equal(layoutSource.split('.onDragDropEvent(').length - 1, 1);
});

test('F3 and search advancement require active pane ownership and yield to overlays and other inputs', () => {
  const f = domFixture();
  f.document.activeElement = f.left;
  assert.equal(events.ownsEditorSearch(f.left, true, f.document), true);
  assert.equal(events.ownsEditorSearch(f.right, false, f.document), false);
  assert.equal(events.ownsEditorSearch(f.right, true, f.document), false);
  f.document.overlays = [{ getClientRects: () => [1] }];
  assert.equal(events.ownsEditorSearch(f.left, true, f.document), false);
  f.document.overlays = [{ getClientRects: () => [1], visibility: 'hidden' }];
  assert.equal(events.ownsEditorSearch(f.left, true, f.document), true);
  f.document.activeElement = { closest: () => null, matches: () => true };
  assert.equal(events.ownsEditorSearch(f.left, true, f.document), false);
  let advanced = 0;
  const common = { noteSearchOpen: true, noteSearchNext: () => advanced++, noteSearchPrev: () => advanced-- };
  const inactive = editorFunction('handleNoteSearchKey', { ...common, ownsNoteSearch: () => false }).handler;
  const active = editorFunction('handleNoteSearchKey', { ...common, ownsNoteSearch: () => true }).handler;
  const event = { key: 'F3', preventDefault() { this.defaultPrevented = true; }, stopImmediatePropagation() {} };
  inactive(event);
  active(event);
  active(event);
  assert.equal(advanced, 1);
  const blocked = editorFunction('navigateNoteSearch', { ownsNoteSearch: () => false, noteSearchOpen: true }).handler;
  assert.doesNotThrow(() => blocked(1), 'inactive search must not even touch scan/selection/focus state');
});

class EventTargetFixture {
  listeners = new Map();
  addEventListener(name, fn) { const list = this.listeners.get(name) ?? new Set(); list.add(fn); this.listeners.set(name, list); }
  removeEventListener(name, fn) { this.listeners.get(name)?.delete(fn); }
  emit(name, event = {}) { for (const fn of this.listeners.get(name) ?? []) fn(event); }
}
test('new pointer, keyboard and focus intents cancel delayed restoration; accepted auto-focus keeps its intent', () => {
  const doc = new EventTargetFixture();
  const win = new EventTargetFixture();
  const stop = events.trackEditorFocus(doc, win);
  const old = events.editorFocus.capture();
  doc.emit('pointerdown');
  assert.equal(old(), false);
  const auto = events.editorFocus.capture();
  assert.equal(events.restoreEditorFocus(auto, () => doc.emit('focusin')), true);
  assert.equal(auto(), true);
  doc.emit('keydown', { key: 'Tab' });
  assert.equal(auto(), false);
  const typing = events.editorFocus.capture();
  doc.emit('keydown', { key: 'a' });
  assert.equal(typing(), false);
  let stolen = false;
  assert.equal(events.restoreEditorFocus(auto, () => { stolen = true; }), false);
  assert.equal(stolen, false);
  const beforeBlur = events.editorFocus.capture();
  win.emit('blur');
  assert.equal(beforeBlur(), false);
  stop();
  const afterCleanup = events.editorFocus.capture();
  doc.emit('focusin');
  assert.equal(afterCleanup(), true);
});

test('asset pickers and history controls use instance IDs, including actual paired HistoryReview SSR output', () => {
  const left = events.editorDomIds('left');
  const right = events.editorDomIds('right');
  const ids = [...Object.values(left), ...Object.values(right)];
  assert.equal(new Set(ids).size, 10);
  const clicked = [];
  const origins = new WeakMap();
  const inputs = Object.fromEntries(ids.map(id => [id, { click: () => clicked.push(id) }]));
  for (const [id, origin] of [[left.image, 'left'], [right.image, 'right']]) {
    editorFunction('openAssetPicker', {
      insertDropdown: true, document: { getElementById: id => inputs[id] },
      assetPickerOrigins: origins, captureOrigin: () => origin,
    }).handler(id);
    assert.equal(origins.get(inputs[id]), origin);
  }
  assert.deepEqual(clicked, [left.image, right.image]);
  assert.ok(editorSource.includes('for={domIds.history}'));
  assert.ok(editorSource.includes('id={domIds.history}'));
  const component = execute(compile(historySource, { generate: 'server', filename: 'HistoryReview.svelte' }).js.code, {}, {
    'svelte/internal/server': server, svelte,
    '$lib/utils/editor-events': events,
    '$lib/utils/history-comparison': history,
    './SnapshotDocument.svelte': { default: () => {} },
  }).exports.default;
  const props = { raw: 'Body', current: 'Body', selected: 'version', loading: false, error: null, canRestore: false,
    onRetry() {}, onRestore() {}, renderSnapshot: () => '' };
  const html = render(renderer => { component(renderer, props); component(renderer, props); }).body;
  const renderedIds = [...html.matchAll(/\bid="([^"]+)"/g)].map(match => match[1]);
  assert.equal(renderedIds.length, 6);
  assert.equal(new Set(renderedIds).size, 6);
  for (const id of renderedIds.filter(id => id.endsWith('history-changes'))) {
    const prefix = id.slice(0, -'history-changes'.length);
    assert.ok(html.includes(`aria-controls="${id}"`));
    assert.ok(html.includes(`aria-labelledby="${prefix}history-tab-changes"`));
  }
  assert.ok(historySource.includes('aria-controls={ids.snapshotPanel}'));
  assert.ok(historySource.includes('aria-labelledby={ids.snapshotTab}'));
});
