import { Annotation, Compartment, EditorSelection, EditorState, StateEffect, StateField, Transaction, type ChangeDesc, type Extension, type StateCommand, type TransactionSpec } from '@codemirror/state';
import { Decoration, EditorView, drawSelection, keymap, lineNumbers, placeholder, type DecorationSet } from '@codemirror/view';
import { defaultKeymap, history, historyKeymap, indentWithTab, redo, undo } from '@codemirror/commands';
import { HighlightStyle, indentUnit, syntaxHighlighting } from '@codemirror/language';
import { markdown } from '@codemirror/lang-markdown';
import { tags } from '@lezer/highlight';
import { getSelectionPair } from '../selectionPairs.ts';
import type { NoteSearchMatch } from '../search';

const representationChange = Annotation.define<boolean>();
const activeMatch = StateEffect.define<NoteSearchMatch | null>();
export const sourceSearchDecoration = StateField.define<DecorationSet>({
	create: () => Decoration.none,
	update(value, transaction) {
		value = value.map(transaction.changes);
		for (const effect of transaction.effects) {
			if (effect.is(activeMatch)) {
				const match = effect.value;
				value = match && match.to > match.from
					? Decoration.set([Decoration.mark({ class: 'source-search-current' }).range(match.from, match.to)])
					: Decoration.none;
			}
		}
		return value;
	},
	provide: (field) => EditorView.decorations.from(field),
});

/** Insert just the wrappers, preserving the selected immutable text and its direction. */
export function wrapSourceSelection(state: EditorState, key: string): TransactionSpec | null {
	const close = getSelectionPair(key);
	if (state.readOnly || !close || state.selection.main.empty) return null;
	return {
		...state.changeByRange((range) => range.empty ? { range } : ({
			changes: [{ from: range.from, insert: key }, { from: range.to, insert: close }],
			range: EditorSelection.range(range.anchor + key.length, range.head + key.length),
		})),
		userEvent: 'input.type',
		scrollIntoView: true,
	};
}

export const sourceHardBreak: StateCommand = ({ state, dispatch }) => {
	if (state.readOnly) return false;
	dispatch(state.update(state.replaceSelection('  \n'), { userEvent: 'input.type', scrollIntoView: true }));
	return true;
};

const sourceTheme = EditorView.theme({
	'&': { height: '100%', color: 'var(--text-primary)', backgroundColor: 'transparent', fontSize: 'var(--editor-font-size, 14px)' },
	'&.cm-focused': { outline: 'none' },
	'.cm-scroller': { overflow: 'auto', fontFamily: "var(--editor-font-family, 'JetBrains Mono', 'Fira Code', 'Cascadia Code', monospace)", lineHeight: 'var(--editor-line-height, 1.3)' },
	'.cm-content': { padding: '0 0 var(--editor-scroll-past-end, 65vh)', caretColor: 'var(--text-primary)' },
	'.cm-line': { padding: '0 4px' },
	'.cm-gutters': { backgroundColor: 'var(--bg-primary)', color: 'var(--text-secondary)', border: 'none' },
	'.cm-cursor': { borderLeftColor: 'var(--text-primary)' },
	'&.cm-focused .cm-selectionBackground, .cm-selectionBackground, ::selection': { backgroundColor: 'color-mix(in srgb, var(--accent) 25%, transparent)' },
	'.source-search-current': { backgroundColor: 'color-mix(in srgb, var(--accent) 40%, transparent)', outline: '1px solid var(--accent)' },
	'.cm-placeholder': { color: 'var(--text-secondary)' },
});
const sourceSyntax = HighlightStyle.define([
	{ tag: [tags.heading, tags.strong], color: 'var(--accent)' },
	{ tag: [tags.link, tags.url, tags.emphasis], color: 'color-mix(in srgb, var(--accent) 78%, var(--text-primary))' },
	{ tag: [tags.processingInstruction, tags.list, tags.meta], color: 'var(--success)' },
	{ tag: [tags.quote, tags.monospace, tags.comment], color: 'var(--text-secondary)' },
]);

export class SourceController {
	private configuration = new Compartment();
	private readOnly = false;
	private numbers = false;
	private extensions: Extension[];
	private onChange: (changes: ChangeDesc, edited: boolean) => void;
	state: EditorState;
	view: EditorView | null = null;

	constructor(onChange: (changes: ChangeDesc, edited: boolean) => void = () => {}, events: {
		paste?: (event: ClipboardEvent) => boolean;
		drop?: (event: DragEvent) => boolean;
	} = {}) {
		this.onChange = onChange;
		this.extensions = [
			markdown(), history(), drawSelection(), sourceSearchDecoration, sourceTheme,
			syntaxHighlighting(sourceSyntax), indentUnit.of('\t'), EditorState.tabSize.of(4),
			placeholder('Start writing...'),
			EditorView.contentAttributes.of({ 'aria-label': 'Markdown source', tabindex: '0', spellcheck: 'false', autocapitalize: 'off' }),
			keymap.of([{ key: 'Shift-Enter', run: sourceHardBreak }, indentWithTab, ...historyKeymap, ...defaultKeymap]),
			EditorView.domEventHandlers({
				keydown: (event, view) => {
					if (event.ctrlKey || event.metaKey || event.altKey || event.isComposing) return false;
					const transaction = wrapSourceSelection(view.state, event.key);
					if (!transaction) return false;
					view.dispatch(transaction);
					return true;
				},
				paste: (event, view) => !view.state.readOnly && (events.paste?.(event) ?? false),
				drop: (event, view) => !view.state.readOnly && (events.drop?.(event) ?? false),
			}),
			this.configuration.of(this.settings()),
		];
		this.state = EditorState.create({ extensions: this.extensions });
	}

	private settings(): Extension[] {
		return [EditorState.readOnly.of(this.readOnly), EditorView.editable.of(!this.readOnly),
			this.numbers ? lineNumbers() : EditorView.lineWrapping];
	}

	configure(readOnly: boolean, numbers: boolean): void {
		if (readOnly === this.readOnly && numbers === this.numbers) return;
		this.readOnly = readOnly;
		this.numbers = numbers;
		this.dispatch({ effects: this.configuration.reconfigure(this.settings()) });
	}

	attach(parent: HTMLElement): () => void {
		const view = new EditorView({
			parent, state: this.state,
			dispatchTransactions: (transactions, current) => {
				current.update(transactions);
				this.state = current.state;
				for (const transaction of transactions) this.changed(transaction);
			},
		});
		this.view = view;
		return () => {
			if (this.view === view) this.view = null;
			view.destroy();
		};
	}

	private changed(transaction: Transaction): void {
		if (transaction.docChanged) this.onChange(transaction.changes, !transaction.annotation(representationChange));
	}

	dispatch(spec: TransactionSpec | Transaction): void {
		const transaction = spec instanceof Transaction ? spec : this.state.update(spec);
		if (this.view) this.view.dispatch(transaction);
		else {
			this.state = transaction.state;
			this.changed(transaction);
		}
	}

	/** New note/mode conversion: drop undo history, never mark dirty or focus. */
	load(content: string): void {
		this.state = EditorState.create({ doc: content, extensions: this.extensions });
		// A fresh state starts with the original compartment; retain live settings.
		this.state = this.state.update({ effects: this.configuration.reconfigure(this.settings()) }).state;
		this.view?.setState(this.state);
	}

	replace(content: string, from = 0, to = this.state.doc.length, representationOnly = false): void {
		this.dispatch({ changes: { from, to, insert: content },
			annotations: representationOnly ? [representationChange.of(true), Transaction.addToHistory.of(false)] : undefined,
			userEvent: representationOnly ? undefined : 'input' });
	}

	select(from: number, to = from, scroll = false): void {
		const length = this.state.doc.length;
		this.dispatch({ selection: { anchor: Math.min(from, length), head: Math.min(to, length) },
			effects: scroll ? EditorView.scrollIntoView(Math.min(from, length), { y: 'center' }) : undefined });
	}

	highlight(match: NoteSearchMatch | null, reveal = false): void {
		this.dispatch({ effects: activeMatch.of(match),
			...(match && reveal ? { selection: { anchor: match.from, head: match.to },
				effects: [activeMatch.of(match), EditorView.scrollIntoView(match.from, { y: 'center' })] } : {}) });
	}

	focus(): void { this.view?.focus(); }
	undo(): boolean { return undo({ state: this.state, dispatch: (tr) => this.dispatch(tr) }); }
	redo(): boolean { return redo({ state: this.state, dispatch: (tr) => this.dispatch(tr) }); }
}
