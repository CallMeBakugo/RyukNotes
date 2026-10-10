/*
 * Renaming a note rewrites the wiki links to it in every note that links there (see
 * `update_wikilinks_after_rename` in the backend). A note that is open with unsaved edits has to
 * follow in the editor, or its next save would write the old links back. These rules mirror the
 * backend's.
 */

export interface LinkRename {
	old_ref: string;
	new_ref: string;
	old_title: string;
	new_title: string;
	/** Whether links written with the note's title were rewritten, not only path links. */
	title_rules: boolean;
}

export interface RenamedLink {
	ref: string;
	display: string | null;
}

/** The link `[[ref]]` or `[[ref|display]]` after the rename, or null when it is not affected. */
export function renamedLink(ref: string, display: string | null, rename: LinkRename): RenamedLink | null {
	const followsTitle = display === rename.old_title && rename.old_title !== rename.new_title;
	if (rename.title_rules && ref === rename.old_title) {
		return { ref: rename.new_title, display: followsTitle ? rename.new_title : display };
	}
	if (rename.old_ref !== rename.new_ref && ref === rename.old_ref) {
		return { ref: rename.new_ref, display: followsTitle ? rename.new_title : display };
	}
	return null;
}

export interface TextChange {
	from: number;
	to: number;
	insert: string;
}

/** The edits that bring the wiki links in Markdown text in line with the rename. */
export function renamedLinkChanges(text: string, rename: LinkRename): TextChange[] {
	const changes: TextChange[] = [];
	for (const match of text.matchAll(/\[\[([^\[\]|]+)(?:\|([^\[\]]*))?\]\]/g)) {
		const next = renamedLink(match[1], match[2] ?? null, rename);
		if (!next) continue;
		const insert = next.display === null ? `[[${next.ref}]]` : `[[${next.ref}|${next.display}]]`;
		changes.push({ from: match.index, to: match.index + match[0].length, insert });
	}
	return changes;
}
