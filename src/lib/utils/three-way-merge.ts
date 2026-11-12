/** Exact UTF-8 Markdown comparison. No renderer, normalization, storage or automatic apply. */
export type Side = 'base' | 'local' | 'remote';
export type HunkKind = 'clean-local' | 'clean-remote' | 'overlapping';
export interface Span { from: number; to: number; byteFrom: number; byteTo: number; lineFrom: number; lineTo: number }
export interface ExactLine extends Span { text: string }
export interface ExactDocument { text: string; lines: readonly ExactLine[]; bytes: number }
export interface Patch { base: Span; target: Span; insert: string; side: 'local' | 'remote' }
export interface MergeHunk {
  id: string; kind: HunkKind; base: Span; local: Span; remote: Span;
  baseText: string; localText: string; remoteText: string; reasons: readonly string[];
}
export interface MergePlan { base: ExactDocument; local: ExactDocument; remote: ExactDocument; localDiff: readonly Patch[]; remoteDiff: readonly Patch[]; hunks: readonly MergeHunk[] }
export interface Mapping { source: Side; sourceSpan: Span; result: Span; hunk: string | null }
export interface Candidate { text: string; mapping: readonly Mapping[] }
export class MergeLimitError extends Error {}
const encoder = new TextEncoder();
const byteLength = (text: string) => encoder.encode(text).length;

export function exactDocument(text: string): ExactDocument {
  if (text.length > 2_000_000 || byteLength(text) > 4_000_000) throw new MergeLimitError('Document exceeds the review byte limit. Use Keep both.');
  // JS strings can hold invalid UTF-16; replacing an isolated surrogate would
  // lose accepted bytes. Refuse instead of letting TextEncoder silently replace it.
  if (!text.isWellFormed()) throw new MergeLimitError('Invalid Unicode cannot be mapped losslessly. Use Keep both.');
  const lines: ExactLine[] = [];
  let from = 0, bytes = 0;
  for (const match of text.matchAll(/[^\r\n]*(?:\r\n|\r|\n)|[^\r\n]+$/g)) {
    const value = match[0];
    lines.push(Object.freeze({ text: value, from, to: from + value.length, byteFrom: bytes,
      byteTo: bytes + byteLength(value), lineFrom: lines.length, lineTo: lines.length + 1 }));
    from += value.length; bytes += byteLength(value);
  }
  if (lines.length > 12_000) throw new MergeLimitError('Document exceeds the review line limit. Use Keep both.');
  return Object.freeze({ text, lines: Object.freeze(lines), bytes });
}
function span(doc: ExactDocument, start: number, end: number): Span {
  return Object.freeze({ from: doc.lines[start]?.from ?? doc.text.length, to: doc.lines[end]?.from ?? doc.text.length,
    byteFrom: doc.lines[start]?.byteFrom ?? doc.bytes, byteTo: doc.lines[end]?.byteFrom ?? doc.bytes, lineFrom: start, lineTo: end });
}
/** Bounded exact-line LCS. Equal-looking repeated lines are never evidence of ancestry. */
export function exactDiff(base: ExactDocument, target: ExactDocument, side: 'local' | 'remote'): readonly Patch[] {
  const a = base.lines, b = target.lines;
  let prefix = 0, ae = a.length, be = b.length;
  while (prefix < ae && prefix < be && a[prefix].text === b[prefix].text) prefix++;
  while (ae > prefix && be > prefix && a[ae - 1].text === b[be - 1].text) { ae--; be--; }
  const n = ae - prefix, m = be - prefix;
  if ((n + 1) * (m + 1) > 1_000_000) throw new MergeLimitError('Comparison exceeds the review work limit. Use Keep both.');
  const width = m + 1;
  const cells = new Uint32Array((n + 1) * width);
  for (let i = n - 1; i >= 0; i--) for (let j = m - 1; j >= 0; j--) {
    cells[i * width + j] = a[prefix + i].text === b[prefix + j].text
      ? 1 + cells[(i + 1) * width + j + 1] : Math.max(cells[(i + 1) * width + j], cells[i * width + j + 1]);
  }
  const patches: Patch[] = [];
  let i = 0, j = 0, startA = -1, startB = -1;
  const finish = () => {
    if (startA < 0) return;
    const baseSpan = span(base, prefix + startA, prefix + i), targetSpan = span(target, prefix + startB, prefix + j);
    patches.push(Object.freeze({ base: baseSpan, target: targetSpan, side, insert: target.text.slice(targetSpan.from, targetSpan.to) }));
    startA = startB = -1;
  };
  while (i < n || j < m) {
    if (i < n && j < m && a[prefix + i].text === b[prefix + j].text) { finish(); i++; j++; }
    else {
      if (startA < 0) { startA = i; startB = j; }
      if (j < m && (i === n || cells[i * width + j + 1] > cells[(i + 1) * width + j])) j++;
      else i++;
    }
  }
  finish();
  return Object.freeze(patches);
}
interface Protected { start: number; end: number; reason: string }
/** Conservative syntax boundaries, never a Markdown validity or merge-safety proof. */
function protectedSpans(doc: ExactDocument): Protected[] {
  const lines = doc.lines;
  const result: Protected[] = [];
  const starts: number[] = [], ends: number[] = [];
  for (let i = 0; i < lines.length;) {
    const start = i;
    if (!/\S/.test(lines[i].text)) { starts[i] = i; ends[i] = ++i; continue; }
    while (i < lines.length && /\S/.test(lines[i].text)) i++;
    for (let j = start; j < i; j++) { starts[j] = start; ends[j] = i; }
  }
  let fence: { start: number; marker: string; reason: string } | null = null;
  let math = -1;
  let html: { start: number; tag: string; depth: number } | null = null;
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i].text.replace(/\r\n$|[\r\n]$/, '');
    if (!fence && html) {
      const opens = line.match(new RegExp(`<${html.tag}(?:\\s[^>]*|)>`, 'gi'))?.length ?? 0;
      const closes = line.match(new RegExp(`</${html.tag}\\s*>`, 'gi'))?.length ?? 0;
      html.depth += opens - closes;
      if (html.depth <= 0) { result.push({ start: html.start, end: i + 1, reason: 'raw HTML' }); html = null; }
      continue;
    }
    const openingTag: RegExpMatchArray | null = !fence ? line.match(/^\s*<([A-Za-z][\w:-]*)(?:\s[^>]*|)>/) : null;
    if (openingTag && !/\/\s*>\s*$/.test(line) && !/^(?:img|br|hr|input|meta|link|source|wbr|embed|area|base|col|param|track)$/i.test(openingTag[1])
      && !new RegExp(`</${openingTag[1]}\\s*>`, 'i').test(line)) {
      html = { start: i, tag: openingTag[1], depth: line.match(new RegExp(`<${openingTag[1]}(?:\\s[^>]*|)>`, 'gi'))?.length ?? 1 };
      continue;
    }
    const marker: RegExpMatchArray | null = line.match(/^\s{0,3}(`{3,}|~{3,})(.*)$/);
    if (fence) {
      if (marker && marker[1][0] === fence.marker[0] && marker[1].length >= fence.marker.length && !marker[2].trim()) {
        result.push({ start: fence.start, end: i + 1, reason: fence.reason }); fence = null;
      }
      continue;
    }
    if (marker) { fence = { start: i, marker: marker[1], reason: /(?:ryuk|helix)-secret/.test(marker[2]) ? 'secrets' : 'fenced code' }; continue; }
    if (/^\s*(?:\$\$|\\\[)/.test(line)) {
      if (math >= 0) { result.push({ start: math, end: i + 1, reason: 'math' }); math = -1; }
      else if (/\$\$.+\$\$|\\\[.+\\\]/.test(line)) result.push({ start: i, end: i + 1, reason: 'math' });
      else math = i;
    } else if (math >= 0 && /^\s*\\\]/.test(line)) { result.push({ start: math, end: i + 1, reason: 'math' }); math = -1; }
    const reasons: string[] = [];
    if (/^\s*(?:[-+*]|\d+[.)])\s/.test(line)) reasons.push('lists and tasks');
    if (/\|/.test(line)) reasons.push('tables');
    if (/^\s{0,3}\[[^\]]+\]:/.test(line)) reasons.push('reference definitions');
    if (/\$[^$\r\n]+\$|\\\(|\\\)/.test(line)) reasons.push('math');
    if (/<(?:\/?[A-Za-z]|!--|!DOCTYPE)|data-secret-block/i.test(line)) reasons.push('raw HTML');
    if (/\[\[|\]\]/.test(line)) reasons.push('wiki targets');
    if (/\.rn-attachments\/|\.(?:ryuknotes|helixnotes)\/attachments\/|!\[|\]\(/.test(line)) reasons.push('attachment references');
    if (/`/.test(line)) reasons.push('inline code');
    if (reasons.length) {
      for (const reason of reasons) result.push({ start: starts[i], end: ends[i], reason });
    }
  }
  if (html) result.push({ start: html.start, end: lines.length, reason: 'raw HTML' });
  if (fence) result.push({ start: fence.start, end: lines.length, reason: fence.reason });
  if (math >= 0) result.push({ start: math, end: lines.length, reason: 'math' });
  return [...new Map(result.map(boundary => [`${boundary.start}:${boundary.end}:${boundary.reason}`, boundary])).values()];
}
const touches = (a: Span, b: Span) => a.lineFrom === a.lineTo || b.lineFrom === b.lineTo
  ? a.lineFrom <= b.lineTo && b.lineFrom <= a.lineTo : a.lineFrom < b.lineTo && b.lineFrom < a.lineTo;
function repeated(doc: ExactDocument): boolean {
  const seen = new Set<string>();
  for (const line of doc.lines) if (/\S/.test(line.text)) {
    if (seen.has(line.text)) return true;
    seen.add(line.text);
  }
  return false;
}
function targetRange(doc: ExactDocument, patches: readonly Patch[], from: number, to: number): Span {
  let offset = 0;
  for (const p of patches) if (p.base.lineTo <= from && !(p.base.lineFrom === from && p.base.lineTo === from)) offset += p.target.lineTo - p.target.lineFrom - (p.base.lineTo - p.base.lineFrom);
  const start = from + offset;
  let end = to + offset;
  for (const p of patches) if (p.base.lineFrom >= from && p.base.lineTo <= to) end += p.target.lineTo - p.target.lineFrom - (p.base.lineTo - p.base.lineFrom);
  return span(doc, start, end);
}
function baseBoundary(patches: readonly Patch[], line: number, end: boolean): number {
  let delta = 0;
  for (const patch of patches) {
    if (line < patch.target.lineFrom) break;
    if (line <= patch.target.lineTo) return end ? patch.base.lineTo : patch.base.lineFrom;
    delta += patch.target.lineTo - patch.target.lineFrom - (patch.base.lineTo - patch.base.lineFrom);
  }
  return line - delta;
}
export function planMerge(baseText: string, localText: string, remoteText: string): MergePlan {
  const base = exactDocument(baseText), local = exactDocument(localText), remote = exactDocument(remoteText);
  const localDiff = exactDiff(base, local, 'local'), remoteDiff = exactDiff(base, remote, 'remote');
  const all = [...localDiff, ...remoteDiff].sort((a, b) => a.base.lineFrom - b.base.lineFrom || a.base.lineTo - b.base.lineTo);
  if (!all.length) return Object.freeze({ base, local, remote, localDiff, remoteDiff, hunks: Object.freeze([]) });
  const protectedBase = protectedSpans(base);
  const project = (doc: ExactDocument, patches: readonly Patch[]) => protectedSpans(doc).map(boundary => ({ ...boundary,
    baseStart: Math.max(0, baseBoundary(patches, boundary.start, false)), baseEnd: Math.min(base.lines.length, baseBoundary(patches, boundary.end, true)) }));
  const protectedLocal = project(local, localDiff), protectedRemote = project(remote, remoteDiff);
  // A changed structural delimiter/reference definition can affect distant lines.
  // Moves and repeated text make alignment uncertain: present whole exact sides,
  // never silently pick an LCS alignment and call it a clean merge.
  const structural = all.some(p => /(?:^|\r\n|\r|\n)\s*(?:`{3,}|~{3,}|\$\$|\\\[|\\\]|\[[^\]]+\]:)/.test(p.insert + base.text.slice(p.base.from, p.base.to)));
  const removed = all.map(p => base.text.slice(p.base.from, p.base.to)).filter(Boolean);
  const possibleMove = [localDiff, remoteDiff].some(patches => patches.some(p => p.base.lineFrom === p.base.lineTo && p.insert) && patches.some(p => p.base.lineFrom !== p.base.lineTo && !p.insert));
  const moved = possibleMove || all.some(p => p.insert && removed.some(text => p.insert.includes(text)));
  const ambiguous = repeated(base) || repeated(local) || repeated(remote) || moved || structural;
  let groups: { start: number; end: number; patches: Patch[]; reasons: string[] }[] = [];
  for (const patch of all) {
    let start = patch.base.lineFrom, end = patch.base.lineTo;
    const reasons: string[] = [];
    for (const boundary of protectedBase) if (touches(patch.base, span(base, boundary.start, boundary.end))) {
      start = Math.min(start, boundary.start); end = Math.max(end, boundary.end); reasons.push(boundary.reason);
    }
    const target = patch.side === 'local' ? local : remote;
    for (const boundary of patch.side === 'local' ? protectedLocal : protectedRemote) if (touches(patch.target, span(target, boundary.start, boundary.end))) {
      start = Math.min(start, boundary.baseStart); end = Math.max(end, boundary.baseEnd); reasons.push(boundary.reason);
    }
    groups.push({ start, end, patches: [patch], reasons });
  }
  if (ambiguous) groups = [{ start: 0, end: base.lines.length, patches: all, reasons: [structural ? 'structural boundary changed' : moved ? 'move or ambiguous alignment' : 'repeated lines: ancestry is ambiguous'] }];
  else {
    groups.sort((a, b) => a.start - b.start || a.end - b.end);
    for (let i = 1; i < groups.length;) {
      const a = groups[i - 1], b = groups[i];
      if (b.start < a.end || (b.start === a.end && (a.start === a.end || b.start === b.end))) {
        a.end = Math.max(a.end, b.end); a.patches.push(...b.patches); a.reasons.push(...b.reasons); groups.splice(i, 1);
      } else i++;
    }
  }
  const hunks = groups.map((group, i): MergeHunk => {
    const b = span(base, group.start, group.end), l = targetRange(local, localDiff, group.start, group.end), r = targetRange(remote, remoteDiff, group.start, group.end);
    const sides = new Set(group.patches.map(p => p.side));
    return Object.freeze({ id: `hunk-${i}`, kind: group.reasons.length || sides.size > 1 ? 'overlapping' : sides.has('local') ? 'clean-local' : 'clean-remote',
      base: b, local: l, remote: r, baseText: base.text.slice(b.from, b.to), localText: local.text.slice(l.from, l.to), remoteText: remote.text.slice(r.from, r.to), reasons: Object.freeze([...new Set(group.reasons)]) });
  });
  return Object.freeze({ base, local, remote, localDiff, remoteDiff, hunks: Object.freeze(hunks) });
}
export function constructCandidate(plan: MergePlan, choices: Readonly<Record<string, Side>>): Candidate {
  let position = 0, length = 0, bytes = 0, line = 0;
  const parts: string[] = [];
  const mapping: Mapping[] = [];
  const append = (source: Side, sourceSpan: Span, value: string, hunk: string | null) => {
    const from = length, byteFrom = bytes, lineFrom = line;
    parts.push(value); length += value.length; bytes += byteLength(value); line += exactDocument(value).lines.length;
    mapping.push(Object.freeze({ source, sourceSpan, hunk, result: Object.freeze({ from, to: length, byteFrom, byteTo: bytes, lineFrom, lineTo: line }) }));
  };
  for (const hunk of plan.hunks) {
    append('base', span(plan.base, position, hunk.base.lineFrom), plan.base.text.slice(plan.base.lines[position]?.from ?? plan.base.text.length, hunk.base.from), null);
    const side: Side | undefined = hunk.kind === 'clean-local' ? 'local' : hunk.kind === 'clean-remote' ? 'remote' : choices[hunk.id];
    if (!side || !['base', 'local', 'remote'].includes(side)) throw new Error(`Choose an exact side for ${hunk.id} before constructing a result.`);
    append(side, hunk[side], side === 'base' ? hunk.baseText : side === 'local' ? hunk.localText : hunk.remoteText, hunk.id);
    position = hunk.base.lineTo;
  }
  append('base', span(plan.base, position, plan.base.lines.length), plan.base.text.slice(plan.base.lines[position]?.from ?? plan.base.text.length), null);
  const text = parts.join(''); exactDocument(text);
  return Object.freeze({ text, mapping: Object.freeze(mapping) });
}

/** CodeMirror displays LF. This artifact preserves original terminators outside
 * edited display ranges; new lines use the explicitly reported insertion EOL. */
export class ExactResult {
  raw: string;
  readonly insertionEol: string;
  constructor(raw: string) { exactDocument(raw); this.raw = raw; this.insertionEol = raw.match(/\r\n|\r|\n/)?.[0] ?? '\n'; }
  get display(): string { return this.raw.replace(/\r\n|\r/g, '\n'); }
  edit(changes: readonly { from: number; to: number; insert: string }[]): void {
    const offsets: number[] = [0];
    for (let i = 0; i < this.raw.length;) { i += this.raw[i] === '\r' && this.raw[i + 1] === '\n' ? 2 : 1; offsets.push(i); }
    let next = this.raw;
    for (const change of [...changes].sort((a, b) => b.from - a.from)) {
      if (offsets[change.from] === undefined || offsets[change.to] === undefined || change.from > change.to) throw new Error('Invalid exact result edit range.');
      next = next.slice(0, offsets[change.from]) + change.insert.replace(/\r\n|\r|\n/g, this.insertionEol) + next.slice(offsets[change.to]);
    }
    // Retain even unsupported input in memory. Validation fences publication;
    // it must not make an isolated editor silently revert a user's keystroke.
    this.raw = next; exactDocument(next);
  }
}
