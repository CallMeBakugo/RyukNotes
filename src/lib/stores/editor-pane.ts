import { getContext, setContext } from 'svelte';
import { derived, writable, type Readable, type Writable } from 'svelte/store';
import type { NoteContent } from '$lib/types';
import {
  activeNote, activeNotePath, viewerNote, editorDirty, sourceMode, readOnly,
  navHistory, canGoBack, canGoForward, createNavHistory,
} from './app';
import {
  beginNoteNavigation, isCurrentNoteNavigation, cancelNoteNavigation, createNoteNavigation,
} from '../utils/note-navigation';

export interface EditorPane {
  activeNote: Writable<NoteContent | null>;
  activeNotePath: Writable<string | null>;
  viewerNote: Writable<{ path: string; content: string } | null>;
  editorDirty: Writable<boolean>;
  sourceMode: Writable<boolean>;
  readOnly: Writable<boolean>;
  navHistory: ReturnType<typeof createNavHistory>;
  canGoBack: Readable<boolean>;
  canGoForward: Readable<boolean>;
  beginNoteNavigation: () => number;
  isCurrentNoteNavigation: (token: number) => boolean;
  cancelNoteNavigation: (token: number) => void;
}

const paneContext = Symbol('editor-pane');

// Preserve store identity and navigation ordering with all existing single-editor
// parents. This is an adapter, not a mirrored copy or the future active-pane facade.
export const defaultEditorPane: EditorPane = {
  activeNote, activeNotePath, viewerNote, editorDirty, sourceMode, readOnly,
  navHistory, canGoBack, canGoForward,
  beginNoteNavigation, isCurrentNoteNavigation, cancelNoteNavigation,
};

/** Allocate stable dependencies for a pane; never rebind them on focus changes. */
export function createEditorPane(): EditorPane {
  const history = createNavHistory();
  return {
    activeNote: writable<NoteContent | null>(null),
    activeNotePath: writable<string | null>(null),
    viewerNote: writable<{ path: string; content: string } | null>(null),
    editorDirty: writable(false),
    sourceMode: writable(false),
    readOnly: writable(false),
    navHistory: history,
    canGoBack: derived(history, h => h.index > 0),
    canGoForward: derived(history, h => h.index < h.stack.length - 1),
    ...createNoteNavigation(),
  };
}

/** Call during parent initialization; detached windows retain the one-pane adapter. */
export function provideEditorPane(pane: EditorPane = defaultEditorPane): EditorPane {
  return setContext(paneContext, pane);
}

/** Resolve once during Editor initialization, not reactively from active pane focus. */
export function getEditorPane(): EditorPane {
  return getContext<EditorPane | undefined>(paneContext) ?? defaultEditorPane;
}
