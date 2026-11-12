import type { NoteContent } from '../types';

/** Equal Markdown is not equal accepted state after metadata/epoch changes. */
export function sameAcceptedNote(a: NoteContent | null | undefined, b: NoteContent): boolean {
  return a?.path === b.path && a.raw === b.raw && a.meta.accepted?.token === b.meta.accepted?.token;
}
