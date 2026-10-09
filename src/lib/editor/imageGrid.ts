/*
 * Side-by-side / grid / masonry layout for images (#254).
 *
 * Portable syntax: a `:::` container whose lines are ordinary Markdown images. Other renderers
 * (GitHub, Typora, ...) show the images stacked, plus the literal header and closer lines.
 *
 *     :::img-grid cols=3 masonry
 *     ![](a.jpg)
 *     ![Beach|size=small](b.jpg)
 *     :::
 *
 * Header: `:::img-grid`, optionally followed by `cols=2|3|4` and `masonry` / `grid` (also written
 *   `layout=masonry` / `layout=grid`), in any order.
 * Body: one image per line. The closer is `:::` on its own line, or trailing the last image
 *   (`![](b.jpg) :::`).
 *
 * Content is never partially converted. If any part of the block isn't understood (an unknown
 * option, a caption, a linked image, a blank line, a missing closer), the block is left as
 * ordinary Markdown. The parsing itself is a markdown-it block rule (`markdownParser.ts`), so a
 * grid example inside a code block is never converted.
 */

export type ImgGridLayout = 'grid' | 'masonry';

export interface ImgGridAttrs {
	cols: number;
	layout: ImgGridLayout;
}

export const IMG_GRID_MIN_COLS = 2;
export const IMG_GRID_MAX_COLS = 4;
export const IMG_GRID_DEFAULT_COLS = 2;
export const IMG_GRID_CLOSER = ':::';

/** Clamp a column count from node attributes or the DOM into the supported range. */
export function clampImgGridCols(value: unknown): number {
	const n = typeof value === 'string' ? parseInt(value, 10) : (value as number);
	if (!Number.isFinite(n)) return IMG_GRID_DEFAULT_COLS;
	return Math.min(IMG_GRID_MAX_COLS, Math.max(IMG_GRID_MIN_COLS, Math.floor(n)));
}

export function normalizeImgGridLayout(value: unknown): ImgGridLayout {
	return value === 'masonry' ? 'masonry' : 'grid';
}

/**
 * Parse a `:::img-grid ...` header line (already stripped of container indentation), or null.
 * Any unknown or out-of-range option makes the whole line not a header, so it stays literal text
 * instead of being silently rewritten.
 */
export function parseImgGridHeader(line: string): ImgGridAttrs | null {
	const match = /^:::img-grid(?:[ \t]+(.*?))?[ \t]*$/.exec(line);
	if (!match) return null;
	let cols: number | null = null;
	let layout: ImgGridLayout | null = null;
	for (const token of (match[1] ?? '').split(/[ \t]+/).filter(Boolean)) {
		const colsMatch = /^cols=(\d)$/.exec(token);
		const layoutMatch = /^(?:layout=)?(grid|masonry)$/.exec(token);
		if (colsMatch && cols === null) {
			const n = Number(colsMatch[1]);
			if (n < IMG_GRID_MIN_COLS || n > IMG_GRID_MAX_COLS) return null;
			cols = n;
		} else if (layoutMatch && layout === null) {
			layout = layoutMatch[1] as ImgGridLayout;
		} else {
			return null;
		}
	}
	return { cols: cols ?? IMG_GRID_DEFAULT_COLS, layout: layout ?? 'grid' };
}

/** Canonical header for saving. Defaults are omitted: `:::img-grid`, `:::img-grid cols=3 masonry`. */
export function serializeImgGridHeader(attrs: ImgGridAttrs): string {
	const cols = clampImgGridCols(attrs.cols);
	const options = [
		cols !== IMG_GRID_DEFAULT_COLS ? `cols=${cols}` : '',
		normalizeImgGridLayout(attrs.layout) === 'masonry' ? 'masonry' : '',
	].filter(Boolean);
	return [':::img-grid', ...options].join(' ');
}

/**
 * Whole grid block for saving. `images` is the Markdown of the grid's images, one per line, as the
 * editor serializes inline images. Returns '' when there is nothing to save, so a grid whose images
 * all failed to resolve doesn't leave behind a header that would no longer parse.
 */
export function serializeImgGrid(attrs: ImgGridAttrs, images: string): string {
	const body = images.replace(/\n+$/, '');
	if (!body.trim()) return '';
	return `${serializeImgGridHeader(attrs)}\n${body}\n${IMG_GRID_CLOSER}\n`;
}

/** Split a trailing ` :::` closer off the last image line: `![](b.jpg) :::` gives `![](b.jpg)`. */
export function splitTrailingCloser(line: string): string | null {
	const match = /^(.*\S)[ \t]+:::[ \t]*$/.exec(line);
	return match ? match[1] : null;
}

/** New grids get one column per image, within 2 to 4. */
export function defaultGridCols(imageCount: number): number {
	return clampImgGridCols(imageCount);
}

/** One top-level block, for grouping neighboring images into a grid. */
export interface ImageBlockInfo {
	isImage: boolean;
	imageCount: number;
}

/**
 * Grow from `target` across adjacent image-only blocks (image paragraphs and existing grids).
 * Returns the block range and its image total, or null unless it holds at least two images.
 */
export function expandImageRange(
	blocks: readonly ImageBlockInfo[],
	target: number,
): { start: number; end: number; total: number } | null {
	if (target < 0 || target >= blocks.length || !blocks[target].isImage) return null;
	let start = target;
	let end = target;
	while (start - 1 >= 0 && blocks[start - 1].isImage) start -= 1;
	while (end + 1 < blocks.length && blocks[end + 1].isImage) end += 1;
	let total = 0;
	for (let i = start; i <= end; i++) total += blocks[i].imageCount;
	return total < 2 ? null : { start, end, total };
}

/**
 * Index an item ends up at when it is dragged from `from` and dropped before slot `to` of the same
 * list (`to` counts slots in the list before removal, 0..length). Returns null for a no-op drop.
 */
export function reorderTarget(from: number, to: number, length: number): number | null {
	if (from < 0 || from >= length) return null;
	const target = Math.max(0, Math.min(length - 1, to > from ? to - 1 : to));
	return target === from ? null : target;
}
