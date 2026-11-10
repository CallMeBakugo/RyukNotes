/** Each pane owns its navigation intents; legacy callers share the default owner. */
export function createNoteNavigation() {
	let currentNoteNavigation = 0;
	return {
		/** Start before any await, including selections of the already-open note. */
		beginNoteNavigation(): number {
			return ++currentNoteNavigation;
		},
		isCurrentNoteNavigation(token: number): boolean {
			return token === currentNoteNavigation;
		},
		/** A disposed owner must never cancel a newer owner's intent. */
		cancelNoteNavigation(token: number): void {
			if (token === currentNoteNavigation) currentNoteNavigation++;
		},
	};
}

export const { beginNoteNavigation, isCurrentNoteNavigation, cancelNoteNavigation } = createNoteNavigation();
