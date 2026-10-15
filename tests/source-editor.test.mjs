import assert from 'node:assert/strict';
import test from 'node:test';
import { EditorSelection } from '@codemirror/state';
import { indentMore, indentLess, moveLineDown } from '@codemirror/commands';
import { SourceController, sourceHardBreak, sourceSearchDecoration, wrapSourceSelection } from '../src/lib/editor/source/controller.ts';
import { closestSearchMatch, findSourceMatches } from '../src/lib/editor/search.ts';

const options = { matchCase: false, wholeWord: false };
const run = (editor, command) => command({ state: editor.state, dispatch: (tr) => editor.dispatch(tr) });

test('source edits preserve immutable save snapshots and isolate undo when another note loads', () => {
  let dirty = 0;
  const editor = new SourceController((_, edited) => { if (edited) dirty++; });
  editor.load('# First\r\n日本語 😀\r\n');
  const saved = editor.state.doc;
  editor.select(saved.length);
  editor.replace('tail', saved.length);
  assert.equal(saved.toString(), '# First\n日本語 😀\n');
  assert.equal(editor.state.doc.toString(), '# First\n日本語 😀\ntail');
  assert.equal(dirty, 1);
  assert.equal(editor.undo(), true);
  assert.equal(editor.state.doc.toString(), saved.toString());
  assert.equal(editor.redo(), true);
  assert.equal(editor.state.doc.toString(), '# First\n日本語 😀\ntail');
  editor.load('Second note');
  assert.equal(editor.undo(), false);
  assert.equal(editor.state.doc.toString(), 'Second note');
});

test('source pair wrapping keeps backwards Unicode selection and undo restores it', () => {
  const editor = new SourceController();
  editor.load('before 日本語😀 after');
  editor.dispatch({ selection: EditorSelection.single(12, 7) });
  editor.dispatch(wrapSourceSelection(editor.state, '('));
  assert.equal(editor.state.doc.toString(), 'before (日本語😀) after');
  assert.equal(editor.state.selection.main.anchor, 13);
  assert.equal(editor.state.selection.main.head, 8);
  assert.equal(editor.state.sliceDoc(8, 13), '日本語😀');
  editor.undo();
  assert.equal(editor.state.doc.toString(), 'before 日本語😀 after');
  assert.equal(editor.state.selection.main.anchor, 12);
  assert.equal(editor.state.selection.main.head, 7);
});

test('hard breaks, indentation and line moves are undoable and respect read-only mode', () => {
  const editor = new SourceController();
  editor.load('first\nsecond');
  editor.select(5);
  assert.equal(run(editor, sourceHardBreak), true);
  assert.equal(editor.state.doc.toString(), 'first  \n\nsecond');
  editor.undo();
  editor.select(1);
  run(editor, indentMore);
  assert.equal(editor.state.doc.toString(), '\tfirst\nsecond');
  run(editor, indentLess);
  run(editor, moveLineDown);
  assert.equal(editor.state.doc.toString(), 'second\nfirst');
  editor.configure(true, true);
  const before = editor.state.doc;
  assert.equal(run(editor, sourceHardBreak), false);
  assert.equal(run(editor, indentMore), false);
  assert.equal(run(editor, moveLineDown), false);
  assert.equal(wrapSourceSelection(editor.state, '['), null);
  assert.equal(editor.state.doc, before);
});

test('live source search maps the current occurrence without resetting caret or dirty state', () => {
  let changes = 0;
  let anchor = 0;
  const editor = new SourceController((mapping) => { changes++; anchor = mapping.mapPos(anchor); });
  editor.load('İ 😀 中文 cat cat');
  let matches = findSourceMatches(editor.state.doc.toString(), 'cat', options);
  anchor = matches[1].from;
  editor.highlight(matches[1], true);
  assert.equal(changes, 0);
  editor.replace('prefix ', 0, 0);
  const selection = editor.state.selection;
  matches = findSourceMatches(editor.state.doc.toString(), 'cat', options);
  const index = closestSearchMatch(matches, anchor);
  assert.equal(index, 1);
  editor.highlight(matches[index]);
  assert.equal(editor.state.selection, selection);
  assert.equal(changes, 1);
  const ranges = [];
  editor.state.field(sourceSearchDecoration).between(0, editor.state.doc.length, (from, to) => ranges.push({ from, to }));
  assert.deepEqual(ranges, [matches[1]]);
  editor.undo();
  matches = findSourceMatches(editor.state.doc.toString(), 'cat', options);
  assert.equal(closestSearchMatch(matches, anchor), 1);
});

test('asset representation updates do not clear edits or become their own undo steps', () => {
  let dirty = 0;
  const editor = new SourceController((_, edited) => { if (edited) dirty++; });
  editor.load('![image](blob:image)');
  editor.replace(' user edit', editor.state.doc.length);
  editor.replace('.ryuknotes/attachments/image.png', 9, 19, true);
  assert.equal(dirty, 1);
  assert.equal(editor.state.doc.toString(), '![image](.ryuknotes/attachments/image.png) user edit');
  editor.undo();
  assert.equal(editor.state.doc.toString(), '![image](.ryuknotes/attachments/image.png)');
});
