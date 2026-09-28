import type { ToolbarLayout } from '../types';

export const toolbarLabels = {
	insert: 'Insert',
	heading: 'Heading',
	bold: 'Bold',
	italic: 'Italic',
	underline: 'Underline',
	strike: 'Strikethrough',
	'text-color': 'Text color',
	link: 'Link',
	'bullet-list': 'Bullet list',
	'ordered-list': 'Numbered list',
	'task-list': 'Task list',
	undo: 'Undo',
	redo: 'Redo',
	'inline-code': 'Inline code',
	'code-block': 'Code block',
	quote: 'Quote',
	details: 'Collapsible section',
	callout: 'Callout',
	table: 'Table',
	'horizontal-rule': 'Horizontal rule',
	highlight: 'Highlight',
	subscript: 'Subscript',
	superscript: 'Superscript',
	alignment: 'Text alignment',
	indent: 'Indent',
	outdent: 'Outdent',
	'date-time': 'Date and time',
	'ai-actions': 'AI actions',
} as const;

export type ToolbarButtonId = keyof typeof toolbarLabels;
export type ToolbarArea = 'main' | 'more';
export type ResolvedToolbarLayout = Record<ToolbarArea, ToolbarButtonId[]>;

const desktopGroups: readonly (readonly ToolbarButtonId[])[] = [
	['insert'], ['heading'], ['bold', 'italic', 'underline', 'strike', 'text-color'],
	['link'], ['bullet-list', 'ordered-list', 'task-list'], ['undo', 'redo'],
	['inline-code', 'code-block', 'quote', 'details', 'callout', 'table', 'horizontal-rule'],
	['highlight', 'subscript', 'superscript'], ['alignment'], ['indent', 'outdent'],
];
const mobileGroups: readonly (readonly ToolbarButtonId[])[] = [
	['insert'], ['heading'], ['bold', 'italic', 'underline', 'strike'], ['link'],
	['bullet-list', 'ordered-list', 'task-list', 'indent', 'outdent'],
	['highlight'], ['date-time'], ['undo', 'redo'], ['ai-actions'],
];

export function toolbarGroups(mobile: boolean, hasAi: boolean): ToolbarButtonId[][] {
	return (mobile ? mobileGroups : desktopGroups)
		.map((group) => group.filter((id) => id !== 'ai-actions' || hasAi))
		.filter((group) => group.length > 0);
}

export function resolveToolbarLayout(
	layout: ToolbarLayout | null | undefined, mobile: boolean, hasAi: boolean,
): ResolvedToolbarLayout {
	const available = toolbarGroups(mobile, hasAi).flat();
	if (mobile || !layout) return { main: available, more: [] };
	const allowed = new Set<string>(available);
	const seen = new Set<string>();
	const take = (ids: readonly string[]) => ids.filter((id): id is ToolbarButtonId => {
		if (!allowed.has(id) || seen.has(id)) return false;
		seen.add(id);
		return true;
	});
	const main = take(layout.main);
	const more = take(layout.more);
	// Newly added controls stay reachable without changing the user's saved order.
	main.push(...available.filter((id) => !seen.has(id)));
	return { main, more };
}

export function groupToolbarButtons(ids: readonly ToolbarButtonId[], mobile: boolean): ToolbarButtonId[][] {
	const defaults = mobile ? mobileGroups : desktopGroups;
	const groups: ToolbarButtonId[][] = [];
	let previous = -1;
	for (const id of ids) {
		const group = defaults.findIndex((items) => items.includes(id));
		if (group !== previous || groups.length === 0) groups.push([]);
		groups[groups.length - 1].push(id);
		previous = group;
	}
	return groups;
}

/** Index is an insertion gap in the destination list before removing the dragged item. */
export function moveToolbarButton(
	layout: ResolvedToolbarLayout, id: ToolbarButtonId, area: ToolbarArea, index: number,
): ResolvedToolbarLayout {
	const from = layout.main.includes(id) ? 'main' : 'more';
	const oldIndex = layout[from].indexOf(id);
	if (oldIndex < 0) return layout;
	const gap = Math.max(0, Math.min(index, layout[area].length));
	const target = gap - (from === area && oldIndex < gap ? 1 : 0);
	if (from === area && target === oldIndex) return layout;
	const next = { main: layout.main.filter((item) => item !== id), more: layout.more.filter((item) => item !== id) };
	next[area].splice(target, 0, id);
	return next;
}

export function toolbarLayoutPreference(layout: ResolvedToolbarLayout): ToolbarLayout | null {
	const defaults = desktopGroups.flat();
	return layout.more.length === 0 && layout.main.length === defaults.length
		&& layout.main.every((id, index) => id === defaults[index]) ? null : layout;
}
