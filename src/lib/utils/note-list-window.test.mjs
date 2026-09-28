import assert from 'node:assert/strict';
import test from 'node:test';
import { buildNoteListRows, noteListOffsets, noteListWindow, noteListRevealScrollTop } from './note-list-window.ts';

const today = new Date(2026, 8, 26, 12);
function note(index, daysAgo = 0, pinned = false) {
  const date = new Date(2026, 8, 26 - daysAgo, 12).toISOString();
  return {
    path: `/vault/${index}.md`, relative_path: `${index}.md`, preview: 'Preview',
    meta: { id: String(index), title: `Note ${index}`, pinned, tags: [], created: date, modified: date },
  };
}

test('grouped rows retain pinned/date sections and original note indexes', () => {
  const notes = [note(0, 60, true), note(1), note(2, 1), note(3, 1)];
  const rows = buildNoteListRows(notes, 'modified', today);
  assert.deepEqual(rows.filter(row => row.kind === 'section').map(row => row.label), ['Pinned', 'Today', 'Yesterday']);
  assert.deepEqual(rows.filter(row => row.kind === 'note').map(row => [row.note.path, row.noteIndex]),
    notes.map((entry, index) => [entry.path, index]));
  assert.equal(new Set(rows.map(row => row.key)).size, rows.length);
});

test('measured heights select the correct rows across section boundaries', () => {
  const rows = buildNoteListRows([note(0), note(1), note(2, 1)], 'modified', today);
  const heights = new Map(rows.map((row, index) => [row.key, [24, 60, 100, 32, 80][index]]));
  const offsets = noteListOffsets(rows, heights, 62, 32);
  assert.deepEqual(offsets, [0, 24, 84, 184, 216, 296]);
  const window = noteListWindow(offsets, 90, 40, 0);
  assert.deepEqual(rows.slice(window.start, window.end).map(row => row.key), ['note:/vault/1.md']);
  assert.equal(window.top, 84);
  assert.equal(window.bottom, 112);
  const crossing = noteListWindow(offsets, 175, 60, 0);
  assert.deepEqual(rows.slice(crossing.start, crossing.end).map(row => row.kind), ['note', 'section', 'note']);
});

test('thousands of grouped notes keep a bounded viewport and can reach the final note', () => {
  const notes = Array.from({ length: 3000 }, (_, index) => note(index));
  const rows = buildNoteListRows(notes, 'created', today);
  const offsets = noteListOffsets(rows, new Map(), 62, 32);
  for (const scrollTop of [0, 60000, offsets.at(-1) - 900]) {
    const window = noteListWindow(offsets, scrollTop, 900);
    assert.ok(window.end - window.start <= 38, 'only the viewport and overscan should render');
    assert.ok(offsets[window.start] <= scrollTop);
    assert.ok(offsets[window.end] >= scrollTop + 900);
    assert.equal(window.top + offsets[window.end] - offsets[window.start] + window.bottom, offsets.at(-1));
  }
  const last = noteListWindow(offsets, offsets.at(-1) - 900, 900);
  assert.equal(rows[last.end - 1].note.path, '/vault/2999.md');
});

test('empty lists and a viewport above the note rows have safe padding', () => {
  assert.deepEqual(noteListWindow([0], 0, 900), { start: 0, end: 0, anchor: 0, top: 0, bottom: 0 });
  const rows = buildNoteListRows([note(0), note(1)], null, today);
  const offsets = noteListOffsets(rows, new Map(), 62, 32);
  const window = noteListWindow(offsets, -250, 900);
  assert.equal(window.start, 0);
  assert.equal(window.end, 2);
  assert.equal(window.top, 0);
  assert.equal(window.bottom, 0);
  assert.deepEqual(rows.map(row => row.noteIndex), [0, 1]);
});

test('revealing a distant grouped note puts its row inside the virtual window', () => {
  const notes = Array.from({ length: 3000 }, (_, index) => note(index, index, index < 3));
  const rows = buildNoteListRows(notes, 'created', today);
  const offsets = noteListOffsets(rows, new Map(), 62, 32);
  const index = rows.findIndex(row => row.kind === 'note' && row.note.path === notes[2550].path);
  const top = noteListRevealScrollTop(offsets, index, 0, 600);
  const window = noteListWindow(offsets, top, 600);
  assert.ok(index >= window.start && index < window.end);
  assert.ok(offsets[index] >= top);
  assert.ok(offsets[index + 1] <= top + 600);
});

test('revealing keeps visible notes stationary and accounts for content above the list', () => {
  const offsets = [0, 30, 90, 150, 210];
  assert.equal(noteListRevealScrollTop(offsets, 1, 25, 100), 25);
  assert.equal(noteListRevealScrollTop(offsets, 3, 0, 100), 130);
  assert.equal(noteListRevealScrollTop(offsets, 2, 0, 100, 180), 250);
  assert.equal(noteListRevealScrollTop(offsets, 0, 200, 100), 0);
});

test('revealing oversized notes shows their beginning and ignores unavailable rows', () => {
  assert.equal(noteListRevealScrollTop([0, 200, 260], 0, 100, 80, 20), 20);
  assert.equal(noteListRevealScrollTop([0], 0, 42, 600), 42);
  assert.equal(noteListRevealScrollTop([0, 62], -1, 42, 600), 42);
  assert.equal(noteListRevealScrollTop([0, 62], 1, 42, 600), 42);
  assert.equal(noteListRevealScrollTop([0, 62], 0, 42, 0), 42);
});
