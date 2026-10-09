/**
 * Turns what the async clipboard API (`navigator.clipboard.read()`) returns into the data a keyboard
 * paste would carry, so the editor's context-menu Paste can replay it as a real paste event.
 *
 * The clipboard holds many representations of one copy: plain text, HTML, URI lists, bitmaps, and
 * browser/OS bookkeeping formats. Only actual attachments may become files. Text formats such as
 * `text/uri-list` stay text, and unknown metadata is dropped. Turning them into files would insert
 * junk attachments like `pasted-0.uri-list`.
 */

/** How one clipboard MIME type is pasted. */
export type ClipboardTypeRole = 'plain' | 'html' | 'string' | 'file' | 'ignore';

/** Attachment types a keyboard paste would deliver as files (see `insertFile` in Editor.svelte). */
const FILE_TYPES = /^(?:image\/[\w.+-]+|application\/pdf)$/;

/** Strips parameters (`text/plain;charset=utf-8`) and case. */
export function normalizeClipboardType(type: string): string {
	return type.split(';')[0].trim().toLowerCase();
}

export function classifyClipboardType(type: string): ClipboardTypeRole {
	const mime = normalizeClipboardType(type);
	if (mime === 'text/plain') return 'plain';
	if (mime === 'text/html') return 'html';
	// Other text formats (uri-list, rtf, x-moz-url, ...) are string data on the paste event, never files.
	if (/^text\/[\w.+-]+$/.test(mime)) return 'string';
	if (FILE_TYPES.test(mime)) return 'file';
	// `web ...` custom formats, chromium/..., application/x-..., and anything else unrecognized.
	return 'ignore';
}

/** First URI of an RFC 2483 `text/uri-list`. Lines starting with `#` are comments. */
export function plainTextFromUriList(list: string): string {
	for (const line of list.split(/\r?\n/)) {
		const uri = line.trim();
		if (uri && !uri.startsWith('#')) return uri;
	}
	return '';
}

const EXTENSIONS: Record<string, string> = { 'image/jpeg': 'jpg', 'image/svg+xml': 'svg', 'application/pdf': 'pdf' };

/** File name for a pasted attachment, matching the `pasted-image.png` name of the native image fallback. */
export function pastedFileName(type: string, index: number): string {
	const mime = normalizeClipboardType(type);
	const extension = EXTENSIONS[mime] ?? (mime.split('/')[1]?.replace(/[^\w]/g, '') || 'bin');
	const base = mime.startsWith('image/') ? 'pasted-image' : 'pasted-file';
	return `${base}${index > 0 ? `-${index + 1}` : ''}.${extension}`;
}

export interface ClipboardItemLike {
	readonly types: readonly string[];
	getType(type: string): Promise<Blob>;
}

export interface ClipboardPastePayload {
	text: string;
	html: string;
	/** Other text formats, keyed by normalized MIME type. */
	strings: [type: string, value: string][];
	files: File[];
}

/** Web clipboard data still missing formats a keyboard paste would carry, so the native clipboard is needed. */
export function needsNativeClipboard(payload: ClipboardPastePayload | null): boolean {
	return !payload || (!payload.html && payload.files.length === 0);
}

/**
 * Fill in what the web clipboard didn't provide from the native clipboard. WebKitGTK often returns
 * nothing, or only `text/plain`, for content copied from other applications; Ctrl+V would still
 * have received the HTML (formatting, links), so a paste without it is not the same paste.
 */
export function withNativeClipboard(
	payload: ClipboardPastePayload | null,
	native: { text?: string | null; html?: string | null },
): ClipboardPastePayload {
	return {
		text: payload?.text || native.text || '',
		html: payload?.html || native.html || '',
		strings: payload?.strings ?? [],
		files: payload?.files ?? [],
	};
}

export function isEmptyPayload(payload: ClipboardPastePayload): boolean {
	return !payload.text && !payload.html && payload.files.length === 0;
}

/** Reads every clipboard item into paste data, classifying each representation. */
export async function collectClipboardPayload(items: readonly ClipboardItemLike[]): Promise<ClipboardPastePayload> {
	const payload: ClipboardPastePayload = { text: '', html: '', strings: [], files: [] };
	for (const item of items) {
		for (const type of item.types) {
			const role = classifyClipboardType(type);
			if (role === 'ignore') continue;
			const blob = await item.getType(type);
			if (role === 'file') {
				const mime = normalizeClipboardType(type);
				payload.files.push(new File([blob], pastedFileName(mime, payload.files.length), { type: mime }));
				continue;
			}
			const value = await blob.text();
			if (!value) continue;
			if (role === 'plain') payload.text ||= value;
			else if (role === 'html') payload.html ||= value;
			else payload.strings.push([normalizeClipboardType(type), value]);
		}
	}
	// Some apps offer a copied link only as a URI list. Paste the link as text instead of nothing.
	if (!payload.text) {
		const uriList = payload.strings.find(([type]) => type === 'text/uri-list');
		if (uriList) payload.text = plainTextFromUriList(uriList[1]);
	}
	return payload;
}
