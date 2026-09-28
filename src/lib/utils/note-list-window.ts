import type { NoteEntry } from '../types';
import { dateBucketLabel } from './time.ts';

export type NoteListRow =
  | { kind: 'section'; key: string; label: string }
  | { kind: 'note'; key: string; note: NoteEntry; noteIndex: number };

export function buildNoteListRows(
  notes: NoteEntry[],
  groupBy: 'created' | 'modified' | null,
  now = new Date(),
): NoteListRow[] {
  const rows: NoteListRow[] = [];
  let previousLabel: string | undefined;
  let section = 0;
  for (let noteIndex = 0; noteIndex < notes.length; noteIndex++) {
    const note = notes[noteIndex];
    if (groupBy) {
      const label = note.meta.pinned ? 'Pinned' : dateBucketLabel(note.meta[groupBy], now);
      if (label !== previousLabel) {
        rows.push({ kind: 'section', key: `section:${section++}:${label}`, label });
        previousLabel = label;
      }
    }
    rows.push({ kind: 'note', key: `note:${note.path}`, note, noteIndex });
  }
  return rows;
}

/** Prefix offsets are rebuilt when rows or measured heights change, not on scroll. */
export function noteListOffsets(
  rows: NoteListRow[],
  heights: ReadonlyMap<string, number>,
  noteHeight: number,
  sectionHeight: number,
): number[] {
  const offsets = new Array<number>(rows.length + 1);
  offsets[0] = 0;
  for (let index = 0; index < rows.length; index++) {
    const row = rows[index];
    offsets[index + 1] = offsets[index] +
      (heights.get(row.key) ?? (row.kind === 'note' ? noteHeight : sectionHeight));
  }
  return offsets;
}

function rowAt(offsets: number[], position: number): number {
  let low = 0;
  let high = offsets.length - 1;
  while (low < high) {
    const middle = Math.floor((low + high + 1) / 2);
    if (offsets[middle] <= position) low = middle;
    else high = middle - 1;
  }
  return Math.min(low, Math.max(0, offsets.length - 2));
}

export function noteListWindow(offsets: number[], scrollTop: number, height: number, overscan = 10) {
  const count = offsets.length - 1;
  if (count === 0) return { start: 0, end: 0, anchor: 0, top: 0, bottom: 0 };
  const anchor = rowAt(offsets, Math.max(0, scrollTop));
  const start = Math.max(0, anchor - overscan);
  const end = Math.min(count, rowAt(offsets, Math.max(0, scrollTop + height)) + 1 + overscan);
  return { start, end, anchor, top: offsets[start], bottom: offsets[count] - offsets[end] };
}
