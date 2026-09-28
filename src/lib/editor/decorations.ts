import type { Node as ProseMirrorNode } from '@tiptap/pm/model';
import type { EditorState } from '@tiptap/pm/state';
import { Decoration, DecorationSet } from '@tiptap/pm/view';

const COPY_ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>';
const CHECK_ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg>';

export function buildCodeCopyDecorations(doc: ProseMirrorNode): DecorationSet {
	const decorations: Decoration[] = [];
	doc.descendants((node, pos) => {
		if (node.type.name !== 'codeBlock') return !node.isTextblock;
		decorations.push(Decoration.widget(pos + 1, (view, getPos) => {
			const button = document.createElement('button');
			button.type = 'button';
			button.className = 'code-copy-btn';
			button.title = 'Copy code';
			button.innerHTML = COPY_ICON;
			button.addEventListener('click', (event) => {
				event.preventDefault();
				event.stopPropagation();
				const position = getPos();
				if (position === undefined || view.isDestroyed) return;
				const current = view.state.doc.resolve(position).parent;
				if (current.type.name !== 'codeBlock') return;
				void navigator.clipboard.writeText(current.textContent).then(() => {
					if (!button.isConnected) return;
					button.innerHTML = CHECK_ICON;
					button.classList.add('copied');
					setTimeout(() => { button.innerHTML = COPY_ICON; button.classList.remove('copied'); }, 1500);
				}).catch((error) => console.error('Failed to copy code:', error));
			});
			return button;
		}, { side: -1, key: `copy-btn:${pos}` }));
		return false;
	});
	return DecorationSet.create(doc, decorations);
}

const priority = /(^|\s)(!(?:high|medium|med|low))\b/gi;
const due = /\bdue:\d{4}-\d{2}-\d{2}\b/gi;

export function buildTaskMetaDecorations(doc: ProseMirrorNode): DecorationSet {
	const decorations: Decoration[] = [];
	doc.descendants((node, pos) => {
		// A prose-only note never traverses inline text or resolves a position.
		if (node.type.name !== 'taskItem') return !node.isTextblock;
		// One traversal covers the whole task subtree, including nested tasks.
		node.descendants((child, offset) => {
			if (!child.isText || !child.text) return;
			const start = pos + 1 + offset;
			for (const match of child.text.matchAll(priority)) {
				const from = start + match.index! + match[1].length;
				decorations.push(Decoration.inline(from, from + match[2].length, { class: 'task-meta-dim' }));
			}
			for (const match of child.text.matchAll(due)) {
				const from = start + match.index!;
				decorations.push(Decoration.inline(from, from + match[0].length, { class: 'task-meta-dim' }));
			}
		});
		return false;
	});
	return DecorationSet.create(doc, decorations);
}

/** Placeholder only needs the caret's empty textblock, not a recursive document emptiness scan. */
export function focusedPlaceholderDecorations(state: EditorState): DecorationSet {
	const { $anchor } = state.selection;
	const node = $anchor.parent;
	if (!$anchor.depth || !node.isTextblock || node.content.size) return DecorationSet.empty;
	const pos = $anchor.before();
	const placeholder = node.type.name === 'detailsSummary' ? 'Section title...' : 'Start writing...';
	return DecorationSet.create(state.doc, [Decoration.node(pos, pos + node.nodeSize, {
		class: 'is-empty', 'data-placeholder': placeholder,
	})]);
}
