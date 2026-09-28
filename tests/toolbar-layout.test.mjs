import assert from 'node:assert/strict';
import test from 'node:test';
import { moveToolbarButton, resolveToolbarLayout } from '../src/lib/editor/toolbar.ts';

const layout = () => Object.freeze({
  main: Object.freeze(['bold', 'italic', 'link']),
  more: Object.freeze(['table', 'quote']),
});

test('moving a button between toolbar and More preserves the other buttons and their order', () => {
  const original = layout();
  assert.deepEqual(moveToolbarButton(original, 'italic', 'more', 1), {
    main: ['bold', 'link'], more: ['table', 'italic', 'quote'],
  });
  assert.deepEqual(moveToolbarButton(original, 'quote', 'main', 1), {
    main: ['bold', 'quote', 'italic', 'link'], more: ['table'],
  });
});

test('reordering uses insertion gaps before removing the dragged button', () => {
  const original = layout();
  assert.deepEqual(moveToolbarButton(original, 'bold', 'main', 3).main, ['italic', 'link', 'bold']);
  assert.deepEqual(moveToolbarButton(original, 'link', 'main', 0).main, ['link', 'bold', 'italic']);
  assert.deepEqual(moveToolbarButton(original, 'table', 'more', 2).more, ['quote', 'table']);
});

test('self drops, boundary moves and stale dragged buttons leave the layout unchanged', () => {
  const original = layout();
  assert.equal(moveToolbarButton(original, 'italic', 'main', 1), original);
  assert.equal(moveToolbarButton(original, 'italic', 'main', 2), original);
  assert.equal(moveToolbarButton(original, 'bold', 'main', -1), original);
  assert.equal(moveToolbarButton(original, 'link', 'main', 4), original);
  assert.equal(moveToolbarButton(original, 'heading', 'more', 0), original);
});

test('saved layouts keep user order without duplicate or unavailable buttons and expose newly available controls', () => {
  const saved = Object.freeze({
    main: Object.freeze(['table', 'bold', 'bold', 'removed-command']),
    more: Object.freeze(['bold', 'italic', 'italic']),
  });
  const resolved = resolveToolbarLayout(saved, false, false);
  assert.deepEqual(resolved.main.slice(0, 2), ['table', 'bold']);
  assert.deepEqual(resolved.more, ['italic']);
  assert.ok(resolved.main.includes('link'));
  assert.equal(resolved.main.includes('removed-command'), false);
  const buttons = [...resolved.main, ...resolved.more];
  assert.equal(new Set(buttons).size, buttons.length);
});
