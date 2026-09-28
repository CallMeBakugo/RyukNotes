import assert from 'node:assert/strict';
import test from 'node:test';
import { Schema } from '@tiptap/pm/model';
import { EditorState, TextSelection } from '@tiptap/pm/state';
import { buildTaskMetaDecorations, focusedPlaceholderDecorations } from '../src/lib/editor/decorations.ts';

const schema = new Schema({
  nodes: {
    doc: { content: 'block+' },
    paragraph: { group: 'block', content: 'inline*' },
    taskList: { group: 'block', content: 'taskItem+' },
    taskItem: { content: 'paragraph block*' },
    text: { group: 'inline' },
  },
  marks: { bold: {} },
});
const p = (value = '') => schema.node('paragraph', null, value ? schema.text(value) : undefined);
const item = (...children) => schema.node('taskItem', null, children);
const list = (...items) => schema.node('taskList', null, items);

test('task decorations include nested tasks once, but never style ordinary prose tokens', () => {
  const doc = schema.node('doc', null, [
    p('ordinary !high due:2026-09-27'),
    list(item(p('parent !low'), list(item(p('nested !high due:2026-09-28')))), item(p('sibling !med'))),
    p('ordinary !low'),
  ]);
  const tokens = buildTaskMetaDecorations(doc).find().map(({ from, to }) => doc.textBetween(from, to));
  assert.deepEqual(tokens, ['!low', '!high', 'due:2026-09-28', '!med']);
  assert.deepEqual(buildTaskMetaDecorations(schema.node('doc', null, [p('!high'), p('due:2026-09-27')])).find(), []);
});

test('placeholder follows an empty nested paragraph and disappears when that paragraph gains content', () => {
  const doc = schema.node('doc', null, [p('before'), list(item(p()))]);
  let emptyPosition;
  doc.descendants((node, pos) => { if (node.type.name === 'paragraph' && !node.content.size) emptyPosition = pos; });
  let state = EditorState.create({ doc, selection: TextSelection.create(doc, emptyPosition + 1) });
  const decorations = focusedPlaceholderDecorations(state).find();
  assert.deepEqual(decorations.map(({ from, to }) => [from, to]), [[emptyPosition, emptyPosition + 2]]);
  state = state.apply(state.tr.insertText('written'));
  assert.deepEqual(focusedPlaceholderDecorations(state).find(), []);
  state = state.apply(state.tr.setSelection(TextSelection.create(state.doc, 1)));
  assert.deepEqual(focusedPlaceholderDecorations(state).find(), []);
});
