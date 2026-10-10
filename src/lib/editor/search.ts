import type { Node as ProseMirrorNode } from '@tiptap/pm/model';

export interface NoteSearchMatch { from: number; to: number }
export interface NoteSearchOptions { matchCase: boolean; wholeWord: boolean }

const wordCharacter = /[\p{L}\p{N}\p{M}\p{Pc}\u200c\u200d]/u;

function characterBefore(text: string, offset: number): string {
	if (!offset) return '';
	const last = text.charCodeAt(offset - 1);
	return text.slice(offset - (last >= 0xdc00 && last <= 0xdfff && offset > 1 ? 2 : 1), offset);
}

function literalMatcher(query: string, options: NoteSearchOptions) {
	// Unicode regexp case folding preserves original UTF-16 offsets, unlike lowercasing
	// the document (for example, U+0130 lowercases to two code points).
	const pattern = new RegExp(query.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'), options.matchCase ? 'gu' : 'giu');
	return (text: string, base: number, results: NoteSearchMatch[]) => {
		pattern.lastIndex = 0;
		let match: RegExpExecArray | null;
		while ((match = pattern.exec(text))) {
			const from = match.index;
			const to = from + match[0].length;
			if (!options.wholeWord || (
				!wordCharacter.test(characterBefore(text, from)) &&
				!wordCharacter.test(to < text.length ? String.fromCodePoint(text.codePointAt(to)!) : '')
			)) results.push({ from: base + from, to: base + to });
			// Keep overlapping occurrences, but never start inside a surrogate pair.
			pattern.lastIndex = from + (text.codePointAt(from)! > 0xffff ? 2 : 1);
		}
	};
}

export function findSourceMatches(text: string, query: string, options: NoteSearchOptions): NoteSearchMatch[] {
	const results: NoteSearchMatch[] = [];
	if (query) literalMatcher(query, options)(text, 0, results);
	return results;
}

export function findDocumentMatches(doc: ProseMirrorNode, query: string, options: NoteSearchOptions): NoteSearchMatch[] {
	const results: NoteSearchMatch[] = [];
	if (!query) return results;
	const match = literalMatcher(query, options);
	doc.descendants((block, pos) => {
		if (!block.isTextblock) return;
		let text = '';
		let start = 0;
		const flush = () => {
			if (text) match(text, start, results);
			text = '';
		};
		block.forEach((child, offset) => {
			if (!child.isText) {
				// Hard breaks, images and inline atoms are real boundaries, not marks.
				flush();
				return;
			}
			if (!text) start = pos + 1 + offset;
			text += child.text;
		});
		flush();
		return false;
	});
	return results;
}

/** Matches may overlap ("aa" in "aaa"); replacing all of them takes each stretch of text once. */
export function replaceableMatches(results: NoteSearchMatch[]): NoteSearchMatch[] {
	const picked: NoteSearchMatch[] = [];
	let end = -1;
	for (const match of results) {
		if (match.from < end) continue;
		picked.push(match);
		end = match.to;
	}
	return picked;
}

export function closestSearchMatch(results: NoteSearchMatch[], anchor: number): number {
	if (!results.length) return 0;
	let lo = 0;
	let hi = results.length;
	while (lo < hi) {
		const mid = (lo + hi) >>> 1;
		if (results[mid].from < anchor) lo = mid + 1; else hi = mid;
	}
	return Math.min(lo, results.length - 1);
}

