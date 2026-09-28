let currentNoteNavigation = 0;

/** Start an intent before any await, including selections of the already-open note. */
export function beginNoteNavigation(): number {
	return ++currentNoteNavigation;
}

export function isCurrentNoteNavigation(token: number): boolean {
	return token === currentNoteNavigation;
}

/** A disposed owner may invalidate its own intent, but never a newer owner's intent. */
export function cancelNoteNavigation(token: number): void {
	if (isCurrentNoteNavigation(token)) currentNoteNavigation++;
}
