import { getContext, setContext } from 'svelte';
import { derived, get, readable, writable } from 'svelte/store';
import { createEditorPane, defaultEditorPane, type EditorPane } from './editor-pane';

export type PaneId = string;
export interface PaneNavigation {
  readonly paneId: PaneId;
  readonly context: EditorPane;
  readonly generation: number;
  isCurrent(): boolean;
  cancel(): void;
}
export interface PaneEditorHandle {
  flushSave(): Promise<boolean>;
  beginPathMutation(): Promise<(() => void) | null>;
  loadNote(path: string, content: string): void;
  getAcceptedRaw(): string;
  flagExternalConflict(): void;
}
export type PaneRelease = () => Promise<void>;

export function pathMatches(path: string, roots: readonly string[]): boolean {
  const normalized = path.replace(/\\/g, '/');
  return roots.some(root => {
    const prefix = root.replace(/\\/g, '/').replace(/\/+$/, '');
    return normalized === prefix || normalized.startsWith(prefix + '/');
  });
}
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
  const handles = new Map<PaneId, PaneEditorHandle>();
  let barrier = false;
  const busy = writable(false);
  let barrierEpoch = 0;
  let live = true;
  let reconcile: (panes: WorkspacePane[]) => Promise<void> = async () => {};
  const state = writable<EditorWorkspaceState>({
    panes: [firstPane], activePaneId: firstPane.id, splitRatio: 0.5,
  });
  return {
    subscribe: state.subscribe,
    busy: { subscribe: busy.subscribe },
    activePane: derived(state, s => s.panes.find(p => p.id === s.activePaneId)!),
    anyDirty: derived(state, (value, set) => derived(
      value.panes.map(p => p.context.editorDirty), dirty => dirty.some(Boolean),
    ).subscribe(set), false),
    isBlocked: () => barrier || !live,
    registerEditor(id: PaneId, handle: PaneEditorHandle | undefined) {
      if (handle) handles.set(id, handle);
      else handles.delete(id);
    },
    editor(id: PaneId) { return handles.get(id); },
    onReconcile(callback: typeof reconcile) { reconcile = callback; },
    remapPaths(oldPath: string, newPath: string) {
      for (const pane of get(state).panes) {
        pane.context.navHistory.remap(oldPath, newPath);
        const path = get(pane.context.activeNotePath);
        if (!path || !pathMatches(path, [oldPath])) continue;
        const migrated = newPath + path.slice(oldPath.length);
        const note = get(pane.context.activeNote);
        if (note) pane.context.activeNote.set({ ...note, path: migrated });
        pane.context.activeNotePath.set(migrated);
      }
    },
    captureForPath(path?: string): PaneNavigation {
      const normalized = path?.replace(/\\/g, '/');
      const owner = normalized ? get(state).panes.find(p => get(p.context.activeNotePath)?.replace(/\\/g, '/') === normalized) : undefined;
      return this.captureNavigation(owner?.context);
    },
    captureNavigation(context?: EditorPane): PaneNavigation {
      context ??= get(state).panes.find(p => p.id === get(state).activePaneId)!.context;
      const destination = context;
      const pane = get(state).panes.find(p => p.context === context);
      const generation = destination.beginNoteNavigation();
      const epoch = barrierEpoch;
      return {
        paneId: pane?.id ?? '', context: destination, generation,
        isCurrent: () => live && !barrier && epoch === barrierEpoch && !!pane
          && get(state).panes.some(p => p.id === pane.id)
          && destination.isCurrentNoteNavigation(generation),
        cancel: () => destination.cancelNoteNavigation(generation),
      };
    },
    /** Resolve owners before the first await; reject overlapping destructive operations. */
    async acquireMutation(roots?: readonly string[]): Promise<PaneRelease | null> {
      if (barrier || !live) return null;
      barrier = true;
      busy.set(true);
      barrierEpoch++;
      const owners = get(state).panes.filter(p => {
        const path = get(p.context.activeNotePath);
        return !roots || (!!path && pathMatches(path, roots));
      });
      const releases: (() => void)[] = [];
      let released = false;
      let finishing: Promise<void> | null = null;
      const unlock = () => {
        if (released) return;
        released = true;
        let failure: unknown;
        for (const release of releases.reverse()) {
          try { release(); } catch (error) { failure ??= error; }
        }
        barrier = false;
        busy.set(false);
        if (failure) throw failure;
      };
      try {
        for (const owner of owners) {
          const handle = handles.get(owner.id);
          if (!handle) {
            if (get(owner.context.activeNotePath) || get(owner.context.editorDirty)) { unlock(); return null; }
            continue;
          }
          const release = await handle.beginPathMutation();
          if (!release) { unlock(); this.activate(owner.id); return null; }
          releases.push(release);
        }
        if (!live || owners.some(p => get(p.context.editorDirty))) { unlock(); return null; }
        return async () => {
          if (released) return;
          finishing ??= (async () => {
            try { if (live) await reconcile(owners); }
            finally { unlock(); }
          })();
          await finishing;
        };
      } catch (error) { unlock(); throw error; }
    },
    async flushAll(): Promise<boolean> {
      const owners = [...get(state).panes];
      const results = await Promise.allSettled(owners.map(async owner => {
        const handle = handles.get(owner.id);
        return (!handle || await handle.flushSave()) && !get(owner.context.editorDirty);
      }));
      // Recheck after all asynchronous saves, not just each pane's earlier completion.
      return live && results.every(result => result.status === 'fulfilled' && result.value)
        && get(state).panes.every(p => !get(p.context.editorDirty));
    },
    dispose() { live = false; barrierEpoch++; },
    activate(id: PaneId) {
      state.update(s => s.activePaneId !== id && s.panes.some(p => p.id === id)
        ? { ...s, activePaneId: id } : s);
    },
    activateOpenNote(path: string, source?: EditorPane): boolean {
      const normalized = path.replace(/\\/g, '/');
      const owner = get(state).panes.find(p => get(p.context.activeNotePath)?.replace(/\\/g, '/') === normalized);
      if (!owner) return false;
      // An explicit selection cancels the owner's older load, not another pane's.
      if (owner.context !== source) owner.context.beginNoteNavigation();
      this.activate(owner.id);
      return true;
    },
    openSplit() {
      if (barrier || !live) return;
      state.update(s => {
        if (s.panes.length === 2) return s;
        const pane = { id: `pane-${nextId++}`, context: createEditorPane() };
        return { ...s, panes: [...s.panes, pane], activePaneId: pane.id };
      });
    },
    /** Do not dispose a pane until its mounted editor has accepted the close. */
    async closeSecond(canClose: (pane: WorkspacePane) => Promise<boolean>): Promise<boolean> {
      if (barrier || !live) return false;
      const second = get(state).panes[1];
      if (!second) return true;
      if (!(await canClose(second))) {
        this.activate(second.id);
        return false;
      }
      // A delayed close must not cross a maintenance barrier or remove a replacement.
      if (barrier || !live || get(state).panes[1]?.id !== second.id) return false;
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
export function getEditorWorkspace(): EditorWorkspace | undefined {
  return getContext<EditorWorkspace | undefined>(workspaceContext);
}

export function getActiveEditorPane() {
  const workspace = getContext<EditorWorkspace | undefined>(workspaceContext);
  return workspace ? derived(workspace.activePane, p => p.context) : readable(defaultEditorPane);
}
