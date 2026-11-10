import { getContext, setContext } from 'svelte';
import { derived, get, readable, writable } from 'svelte/store';
import { createEditorPane, defaultEditorPane, type EditorPane } from './editor-pane';

export type PaneId = string;
export interface WorkspacePane {
  id: PaneId;
  context: EditorPane;
}
export interface EditorWorkspaceState {
  panes: WorkspacePane[];
  activePaneId: PaneId;
  splitRatio: number;
}

export const MIN_SPLIT_RATIO = 0.2;
export const MAX_SPLIT_RATIO = 0.8;
export const MIN_PANE_WIDTH = 240;

export function splitRatioBounds(width: number): [number, number] {
  // On narrow windows share the available space instead of overflowing either pane.
  const min = width > 0 ? Math.min(0.5, Math.max(MIN_SPLIT_RATIO, MIN_PANE_WIDTH / width)) : 0.5;
  return [min, 1 - min];
}

export function clampSplitRatio(ratio: number, width: number): number {
  const [min, max] = splitRatioBounds(width);
  return Math.max(min, Math.min(max, Number.isFinite(ratio) ? ratio : 0.5));
}

export function createEditorWorkspace(first: EditorPane = createEditorPane()) {
  let nextId = 1;
  const firstPane = { id: `pane-${nextId++}`, context: first };
  const state = writable<EditorWorkspaceState>({
    panes: [firstPane], activePaneId: firstPane.id, splitRatio: 0.5,
  });
  return {
    subscribe: state.subscribe,
    activePane: derived(state, s => s.panes.find(p => p.id === s.activePaneId)!),
    activate(id: PaneId) {
      state.update(s => s.activePaneId !== id && s.panes.some(p => p.id === id)
        ? { ...s, activePaneId: id } : s);
    },
    activateOpenNote(path: string): boolean {
      const normalized = path.replace(/\\/g, '/');
      const owner = get(state).panes.find(p => get(p.context.activeNotePath)?.replace(/\\/g, '/') === normalized);
      if (!owner) return false;
      this.activate(owner.id);
      return true;
    },
    openSplit() {
      state.update(s => {
        if (s.panes.length === 2) return s;
        const pane = { id: `pane-${nextId++}`, context: createEditorPane() };
        return { ...s, panes: [...s.panes, pane], activePaneId: pane.id };
      });
    },
    /** Do not dispose a pane until its mounted editor has accepted the close. */
    async closeSecond(canClose: (pane: WorkspacePane) => Promise<boolean>): Promise<boolean> {
      const second = get(state).panes[1];
      if (!second) return true;
      if (!(await canClose(second))) {
        this.activate(second.id);
        return false;
      }
      // A delayed close must not remove a replacement pane.
      if (get(state).panes[1]?.id !== second.id) return false;
      state.update(s => ({ ...s, panes: [s.panes[0]], activePaneId: s.panes[0].id }));
      return true;
    },
    resize(ratio: number, width: number) {
      state.update(s => ({ ...s, splitRatio: clampSplitRatio(ratio, width) }));
    },
  };
}

export type EditorWorkspace = ReturnType<typeof createEditorWorkspace>;
const workspaceContext = Symbol('editor-workspace');

export function provideEditorWorkspace(workspace: EditorWorkspace): void {
  setContext(workspaceContext, workspace);
}

/** Shared controls follow focus; Editor hosts instead provide their fixed pane context. */
export function getActiveEditorPane() {
  const workspace = getContext<EditorWorkspace | undefined>(workspaceContext);
  return workspace ? derived(workspace.activePane, p => p.context) : readable(defaultEditorPane);
}
