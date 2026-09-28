export type HiddenTitleHeading = {
	headingPrefix: string;
	title: string;
};

type TitleHeadingResult = {
	markdown: string;
	hiddenTitle: HiddenTitleHeading | null;
	removedRange?: { from: number; to: number };
};

function normalizeTitle(value: string): string {
	return value.trim().toLowerCase().replace(/[\s\-_\u2014]+/g, ' ');
}

export function stripTitleHeading(
	markdown: string,
	title: string | undefined,
	hideTitle: boolean,
): TitleHeadingResult {
	if (!hideTitle || !title) return { markdown, hiddenTitle: null };

	const lines = markdown.split('\n');
	let lineStart = 0;
	for (let i = 0; i < lines.length; i++) {
		const line = lines[i].trim();
		if (line === '') {
			lineStart += lines[i].length + 1;
			continue;
		}

		const match = line.match(/^(#{1,6})\s+(.+)$/);
		if (match && normalizeTitle(match[2]) === normalizeTitle(title)) {
			const hiddenTitle = { headingPrefix: match[1], title: title.trim() };
			const count = i + 1 < lines.length && lines[i + 1].trim() === '' ? 2 : 1;
			const reachesEnd = i + count === lines.length;
			const removedRange = {
				from: reachesEnd ? Math.max(0, lineStart - 1) : lineStart,
				to: reachesEnd ? markdown.length
					: lineStart + lines[i].length + 1 + (count === 2 ? lines[i + 1].length + 1 : 0),
			};
			return {
				markdown: markdown.slice(0, removedRange.from) + markdown.slice(removedRange.to),
				hiddenTitle,
				removedRange,
			};
		}
		break;
	}

	return { markdown, hiddenTitle: null };
}

export function restoreTitleHeading(
	markdown: string,
	hiddenTitle: HiddenTitleHeading | null,
): string {
	if (!hiddenTitle) return markdown;
	return `${hiddenTitle.headingPrefix} ${hiddenTitle.title}\n\n${markdown}`;
}
