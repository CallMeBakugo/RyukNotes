import type { SearchTarget } from '../types';
import type { NoteSearchMatch } from './search';
import { stripTitleHeading } from './titleVisibility.ts';

function isTextBoundary(text: string, offset: number): boolean {
	const previous = text.charCodeAt(offset - 1);
	const next = text.charCodeAt(offset);
	return !(previous >= 0xd800 && previous <= 0xdbff && next >= 0xdc00 && next <= 0xdfff);
}

function isLineBoundary(text: string, offset: number): boolean {
	return !(text.charCodeAt(offset - 1) === 13 && text.charCodeAt(offset) === 10);
}

function validMatch(text: string, match: NoteSearchMatch): boolean {
	return Number.isInteger(match.from) && Number.isInteger(match.to)
		&& match.from >= 0 && match.to > match.from && match.to <= text.length
		&& isTextBoundary(text, match.from) && isTextBoundary(text, match.to);
}

function normalizeLines(text: string): string {
	return text.includes('\r') ? text.replace(/\r\n?/g, '\n') : text;
}

function normalizedOffset(text: string, offset: number): number {
	let removed = 0;
	for (let i = text.indexOf('\r'); i >= 0 && i < offset; i = text.indexOf('\r', i + 1)) {
		if (text.charCodeAt(i + 1) === 10 && i + 1 < offset) removed++;
	}
	return offset - removed;
}

// Compare the bounded anchor against the original string, retaining its coordinates.
function matchAnchorAt(
	text: string, anchor: string, offset: number, start: number, end: number,
): NoteSearchMatch | null {
	if (!isTextBoundary(text, offset) || !isLineBoundary(text, offset)) return null;
	let cursor = offset;
	let from = offset;
	let to = offset;
	for (let i = 0; i < anchor.length; i++) {
		if (i === start) from = cursor;
		if (i === end) to = cursor;
		const character = text.charCodeAt(cursor);
		if ((character === 13 ? 10 : character) !== anchor.charCodeAt(i)) return null;
		cursor += character === 13 && text.charCodeAt(cursor + 1) === 10 ? 2 : 1;
	}
	if (end === anchor.length) to = cursor;
	const match = { from, to };
	return validMatch(text, match) && isTextBoundary(text, cursor) ? match : null;
}

/** Resolve an indexed anchor to UTF-16 offsets in the supplied, unmodified text. */
export function resolveSearchTarget(text: string, target: SearchTarget): NoteSearchMatch | null {
	if (!Number.isInteger(target.start) || !Number.isInteger(target.end)
		|| target.start < target.before.length || target.end <= target.start
		|| target.end - target.start !== target.text.length
		|| [target.before, target.text, target.after].some((part) => /[\ud800-\udfff]/u.test(part))) return null;

	const anchor = target.before + target.text + target.after;
	const anchorStart = target.start - target.before.length;
	const recorded = { from: target.start, to: target.end };
	if (validMatch(text, recorded) && text.startsWith(anchor, anchorStart)
		&& isTextBoundary(text, anchorStart) && isTextBoundary(text, anchorStart + anchor.length)) {
		return recorded;
	}

	// A boundary inside CRLF has no unique equivalent in an LF-only document.
	const start = target.before.length;
	const end = start + target.text.length;
	if (!isLineBoundary(anchor, start) || !isLineBoundary(anchor, end)) return null;
	const normalized = normalizeLines(anchor);
	const matchStart = normalizedOffset(anchor, start);
	const matchEnd = normalizedOffset(anchor, end);
	const first = normalized.charCodeAt(0);
	let found: NoteSearchMatch | null = null;
	for (let offset = 0; offset < text.length; offset++) {
		const character = text.charCodeAt(offset);
		if ((character === 13 ? 10 : character) !== first) continue;
		const candidate = matchAnchorAt(text, normalized, offset, matchStart, matchEnd);
		if (!candidate) continue;
		if (found) return null;
		found = candidate;
	}
	return found;
}

/** Project a body range into the editor's LF-only, optionally title-hidden Markdown. */
export function visibleSearchTarget(
	body: string, title: string | undefined, hideTitle: boolean, match: NoteSearchMatch,
): { markdown: string; match: NoteSearchMatch | null; hiddenTitle: boolean } {
	const { markdown, removedRange } = stripTitleHeading(normalizeLines(body), title, hideTitle);
	if (!validMatch(body, match) || !isLineBoundary(body, match.from) || !isLineBoundary(body, match.to)) {
		return { markdown, match: null, hiddenTitle: false };
	}
	const visible = { from: normalizedOffset(body, match.from), to: normalizedOffset(body, match.to) };
	if (removedRange) {
		if (visible.from < removedRange.to && visible.to > removedRange.from) {
			return { markdown, match: null, hiddenTitle: true };
		}
		if (visible.from >= removedRange.to) {
			const removed = removedRange.to - removedRange.from;
			visible.from -= removed;
			visible.to -= removed;
		}
	}
	return { markdown, match: visible, hiddenTitle: false };
}

