import { diffLines, type Change } from 'diff';

export interface HistoryRow {
  kind: 'same' | 'added' | 'removed';
  text: string;
  currentLine: number | null;
  snapshotLine: number | null;
  /** Whether this line has a terminating LF; CRLF is normalized for comparison. */
  newline: boolean;
}

export interface HistoryChunk {
  kind: 'lines' | 'unchanged';
  rows: HistoryRow[];
}

export interface HistoryComparison {
  added: number;
  removed: number;
  chunks: HistoryChunk[];
}

/** Separate only a complete leading frontmatter block, keeping both slices verbatim. */
export function splitSnapshot(raw: string): { body: string; metadata: string } {
  const opening = /^(?:\uFEFF)?---\r?\n/.exec(raw);
  if (!opening) return { body: raw, metadata: '' };

  const closingDelimiter = /\n---(?:\r?\n|$)/g;
  closingDelimiter.lastIndex = opening[0].length - 1;
  const closing = closingDelimiter.exec(raw);
  if (!closing) return { body: raw, metadata: '' };

  const bodyStart = closing.index + closing[0].length;
  return { body: raw.slice(bodyStart), metadata: raw.slice(0, bodyStart) };
}

/**
 * Make two note bodies comparable. The note body the app loads has its leading and trailing
 * blank lines removed, while a stored snapshot keeps the file's exact text, so differences in
 * those lines are not changes the editor can show or restore.
 */
export function comparableBody(body: string): string {
  const trimmed = body.replace(/^(?:\r?\n)+/, '').replace(/(?:\r?\n)+$/, '');
  return trimmed === '' ? '' : trimmed + '\n';
}

/** Diff cost grows with the square of the edit distance; beyond this, a coarse diff is shown instead. */
const MAX_EDIT_LENGTH = 1000;

/** Always-correct fallback: shared leading and trailing lines stay unchanged, the middle is replaced. */
function coarseChanges(current: string, snapshot: string): Change[] {
  const lines = (text: string) => text.replace(/\r\n/g, '\n').match(/[^\n]*\n|[^\n]+$/g) ?? [];
  const before = lines(current);
  const after = lines(snapshot);
  let start = 0;
  while (start < before.length && start < after.length && before[start] === after[start]) start++;
  let end = 0;
  while (end < before.length - start && end < after.length - start
    && before[before.length - 1 - end] === after[after.length - 1 - end]) end++;
  const part = (items: string[], kind: 'same' | 'added' | 'removed'): Change[] => items.length
    ? [{ value: items.join(''), count: items.length, added: kind === 'added', removed: kind === 'removed' }]
    : [];
  return [
    ...part(before.slice(0, start), 'same'),
    ...part(before.slice(start, before.length - end), 'removed'),
    ...part(after.slice(start, after.length - end), 'added'),
    ...part(before.slice(before.length - end), 'same'),
  ];
}

/** Added/removed rows describe restoring the snapshot over the current editor body. */
export async function compareHistory(
  currentBody: string,
  snapshotBody: string,
): Promise<HistoryComparison> {
  // jsdiff's callback mode waits on a timer for every step, which takes seconds on large changes.
  const changes = diffLines(currentBody, snapshotBody, { stripTrailingCr: true, maxEditLength: MAX_EDIT_LENGTH })
    ?? coarseChanges(currentBody, snapshotBody);

  const chunks: HistoryChunk[] = [];
  let added = 0;
  let removed = 0;
  let currentLine = 1;
  let snapshotLine = 1;
  const contextLines = 3;

  for (let changeIndex = 0; changeIndex < changes.length; changeIndex++) {
    const change = changes[changeIndex];
    const kind: HistoryRow['kind'] = change.added ? 'added' : change.removed ? 'removed' : 'same';
    const leadingContext = changeIndex > 0 ? contextLines : 0;
    const trailingContext = changeIndex < changes.length - 1 ? contextLines : 0;
    const collapse = kind === 'same' && change.count > (
      changes.length === 1 ? contextLines * 2 : leadingContext + trailingContext
    );

    let offset = 0;
    let rowIndex = 0;
    while (offset < change.value.length) {
      const newlineIndex = change.value.indexOf('\n', offset);
      const newline = newlineIndex !== -1;
      const end = newline ? newlineIndex : change.value.length;
      const chunkKind: HistoryChunk['kind'] = collapse
        && rowIndex >= leadingContext
        && rowIndex < change.count - trailingContext
        ? 'unchanged'
        : 'lines';
      let chunk = chunks[chunks.length - 1];
      if (!chunk || chunk.kind !== chunkKind) {
        chunk = { kind: chunkKind, rows: [] };
        chunks.push(chunk);
      }
      chunk.rows.push({
        kind,
        text: change.value.slice(offset, end),
        currentLine: kind === 'added' ? null : currentLine++,
        snapshotLine: kind === 'removed' ? null : snapshotLine++,
        newline,
      });
      if (kind === 'added') added++;
      if (kind === 'removed') removed++;
      offset = newline ? end + 1 : end;
      rowIndex++;
    }
  }

  return { added, removed, chunks };
}
