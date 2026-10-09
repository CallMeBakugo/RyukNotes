import { Node, mergeAttributes } from '@tiptap/core';
import type { Node as ProseMirrorNode } from '@tiptap/pm/model';
import { clampImgGridCols, normalizeImgGridLayout } from '../imageGrid.ts';

export interface ImageGridDrag {
	gridPos: number;
	index: number;
}

export interface ImageGridOptions {
	/** Replace the grid at `gridPos` with one image paragraph per image. */
	onUngroup: (gridPos: number) => void;
	/** Move image `fromIndex` of one grid before slot `toIndex` of the same or another grid. */
	onMoveImage: (fromGridPos: number, fromIndex: number, toGridPos: number, toIndex: number) => void;
}

export interface ImageGridStorage {
	/** The grid image being dragged. Shared by every grid in the editor so images can move between grids. */
	drag: ImageGridDrag | null;
}

declare module '@tiptap/core' {
	interface Storage {
		imageGrid: ImageGridStorage;
	}
}

function controlButton(label: string, title: string, onClick: () => void): HTMLButtonElement {
	const button = document.createElement('button');
	button.type = 'button';
	button.className = 'img-grid-control';
	button.title = title;
	button.textContent = label;
	button.addEventListener('mousedown', (event) => event.preventDefault());
	button.addEventListener('click', (event) => {
		event.preventDefault();
		onClick();
	});
	return button;
}

/**
 * Side-by-side image grid (#254). Saved as a `:::img-grid` block (see imageGrid.ts). The content is
 * the editor's inline image nodes, so size, copy and the image toolbar work the same as elsewhere.
 */
export const ImageGrid = Node.create<ImageGridOptions, ImageGridStorage>({
	name: 'imageGrid',
	group: 'block',
	content: 'image+',
	defining: true,

	addOptions() {
		return { onUngroup: () => {}, onMoveImage: () => {} };
	},

	addStorage() {
		return { drag: null };
	},

	addAttributes() {
		return {
			cols: {
				default: 2,
				parseHTML: (element: HTMLElement) => clampImgGridCols(element.getAttribute('data-cols') ?? ''),
				renderHTML: (attributes: Record<string, unknown>) => ({ 'data-cols': String(clampImgGridCols(attributes.cols)) }),
			},
			layout: {
				default: 'grid',
				parseHTML: (element: HTMLElement) => normalizeImgGridLayout(element.getAttribute('data-layout')),
				renderHTML: (attributes: Record<string, unknown>) => ({ 'data-layout': normalizeImgGridLayout(attributes.layout) }),
			},
		};
	},

	parseHTML() {
		return [{ tag: 'div[data-img-grid]' }];
	},

	renderHTML({ HTMLAttributes }) {
		// `data-img-grid` is what parseHTML matches, so copied grids paste back as grids.
		return ['div', mergeAttributes(HTMLAttributes, { 'data-img-grid': '', class: 'img-grid' }), 0];
	},

	addNodeView() {
		const { onUngroup, onMoveImage } = this.options;
		const storage = this.storage;
		return ({ node, getPos, editor }) => {
			const dom = document.createElement('div');
			const header = document.createElement('div');
			const content = document.createElement('div');
			dom.className = 'img-grid-wrap';
			header.className = 'img-grid-header';
			header.contentEditable = 'false';
			dom.append(header, content);
			const position = (): number | null => (typeof getPos === 'function' ? getPos() ?? null : null);

			const render = (current: ProseMirrorNode) => {
				const cols = clampImgGridCols(current.attrs.cols);
				const layout = normalizeImgGridLayout(current.attrs.layout);
				for (const element of [dom, content]) {
					element.setAttribute('data-img-grid', '');
					element.setAttribute('data-cols', String(cols));
					element.setAttribute('data-layout', layout);
				}
				content.className = 'img-grid';
				header.replaceChildren();
				if (!editor.isEditable) return;
				const setAttrs = (attrs: Record<string, unknown>) => {
					const pos = position();
					if (pos !== null) editor.view.dispatch(editor.state.tr.setNodeMarkup(pos, undefined, { ...current.attrs, ...attrs }));
				};
				header.append(
					controlButton(`${cols} col`, 'Cycle columns (2-4)', () => setAttrs({ cols: cols >= 4 ? 2 : cols + 1 })),
					controlButton(layout, 'Toggle grid / masonry', () => setAttrs({ layout: layout === 'masonry' ? 'grid' : 'masonry' })),
					controlButton('ungroup', 'Ungroup back to single images', () => {
						const pos = position();
						if (pos !== null) onUngroup(pos);
					}),
				);
			};
			render(node);

			const clearDropMark = () => {
				content.classList.remove('img-grid-dropping');
				content.removeAttribute('data-drop-index');
			};
			const dropIndexAt = (x: number, y: number): number => {
				const images = Array.from(content.querySelectorAll('img'));
				for (let i = 0; i < images.length; i++) {
					const rect = images[i].getBoundingClientRect();
					if (y < rect.top + rect.height / 2 - 4) return i;
					if (y <= rect.bottom + 4 && x < rect.left + rect.width / 2) return i;
				}
				return images.length;
			};
			content.addEventListener('dragstart', (event) => {
				if (!editor.isEditable) return;
				const image = (event.target as HTMLElement | null)?.closest?.('img');
				const pos = position();
				if (!image || !content.contains(image) || pos === null) return;
				const index = Array.from(content.querySelectorAll('img')).indexOf(image as HTMLImageElement);
				if (index >= 0) storage.drag = { gridPos: pos, index };
			});
			content.addEventListener('dragover', (event) => {
				if (!storage.drag || !editor.isEditable) return;
				event.preventDefault();
				event.stopPropagation();
				if (event.dataTransfer) event.dataTransfer.dropEffect = 'move';
				content.classList.add('img-grid-dropping');
				content.setAttribute('data-drop-index', String(dropIndexAt(event.clientX, event.clientY)));
			});
			content.addEventListener('dragleave', (event) => {
				const to = event.relatedTarget as globalThis.Node | null;
				if (!to || !content.contains(to)) clearDropMark();
			});
			content.addEventListener('dragend', () => {
				storage.drag = null;
				clearDropMark();
			});
			content.addEventListener('drop', (event) => {
				const drag = storage.drag;
				if (!drag) return;
				event.preventDefault();
				event.stopPropagation();
				const toIndex = dropIndexAt(event.clientX, event.clientY);
				clearDropMark();
				storage.drag = null;
				const pos = position();
				if (pos !== null) onMoveImage(drag.gridPos, drag.index, pos, toIndex);
			});

			return {
				dom,
				contentDOM: content,
				update(updated) {
					if (updated.type.name !== 'imageGrid') return false;
					render(updated);
					return true;
				},
				ignoreMutation(mutation) {
					if (mutation.type === 'selection') return false;
					return !content.contains(mutation.target);
				},
				stopEvent(event) {
					if (header.contains(event.target as globalThis.Node)) return true;
					// Grid reordering handles its own drops; ProseMirror would insert a second copy.
					return (event.type === 'drop' || event.type === 'dragover') && !!storage.drag
						&& content.contains(event.target as globalThis.Node);
				},
				destroy() {
					clearDropMark();
				},
			};
		};
	},
});
