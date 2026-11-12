export interface OpenNoteRequest { path: string; paneId?: string }

export function openNoteRequest(detail: unknown): OpenNoteRequest | null {
  if (typeof detail === 'string') return detail ? { path: detail } : null;
  if (!detail || typeof detail !== 'object') return null;
  const value = detail as Partial<OpenNoteRequest>;
  if (typeof value.path !== 'string' || !value.path) return null;
  if (value.paneId !== undefined && (typeof value.paneId !== 'string' || !value.paneId)) return null;
  return { path: value.path, paneId: value.paneId };
}

/** Snapshot the destination before any listener can activate a different pane. */
export function createOpenNoteRouter() {
  const destinations = new WeakMap<object, string>();
  const claimed = new WeakSet<object>();
  return {
    claim(event: object, destination: string, owner: string): boolean {
      if (!destinations.has(event)) destinations.set(event, destination);
      if (destinations.get(event) !== owner || claimed.has(event)) return false;
      claimed.add(event);
      return true;
    },
  };
}
export const editorOpenNotes = createOpenNoteRouter();

export function editorDomIds(componentId: string) {
  return {
    image: `${componentId}-insert-image-input`,
    grid: `${componentId}-insert-grid-input`,
    camera: `${componentId}-insert-camera-input`,
    file: `${componentId}-insert-file-input`,
    history: `${componentId}-history-version-picker`,
  };
}
export function historyDomIds(componentId: string) {
  return {
    changesTab: `${componentId}-history-tab-changes`,
    snapshotTab: `${componentId}-history-tab-snapshot`,
    changesPanel: `${componentId}-history-changes`,
    snapshotPanel: `${componentId}-history-snapshot`,
  };
}

export function createFocusIntents() {
  let generation = 0;
  return {
    invalidate() { generation++; },
    capture() {
      const captured = generation;
      return () => captured === generation;
    },
  };
}
export const editorFocus = createFocusIntents();
let restoringFocus = 0;
export function restoreEditorFocus(current: () => boolean, restore: () => void): boolean {
  if (!current()) return false;
  restoringFocus++;
  try { restore(); } finally { restoringFocus--; }
  return true;
}

/** One tracker per webview, including deliberate moves away and back to one pane. */
export function trackEditorFocus(doc: Document, win: Window): () => void {
  const invalidate = () => editorFocus.invalidate();
  doc.addEventListener('pointerdown', invalidate, true);
  const onFocus = () => { if (!restoringFocus) invalidate(); };
  doc.addEventListener('focusin', onFocus, true);
  // Typing or another shortcut is a newer intent even if DOM focus does not move.
  const onKey = () => invalidate();
  doc.addEventListener('keydown', onKey, true);
  win.addEventListener('blur', invalidate);
  return () => {
    doc.removeEventListener('pointerdown', invalidate, true);
    doc.removeEventListener('focusin', onFocus, true);
    doc.removeEventListener('keydown', onKey, true);
    win.removeEventListener('blur', invalidate);
    invalidate();
  };
}

const overlaySelector = '[data-editor-overlay], [role="dialog"], [role="menu"], [aria-modal="true"], .palette-overlay, .context-menu, .sort-menu';
export function hasEditorOverlay(doc: Document = document): boolean {
  return [...doc.querySelectorAll<HTMLElement>(overlaySelector)].some(element =>
    element.getClientRects().length > 0 && doc.defaultView?.getComputedStyle(element).visibility !== 'hidden');
}

export function ownsEditorSearch(root: HTMLElement | null, active: boolean, doc: Document = document): boolean {
  if (!root || !active || hasEditorOverlay(doc)) return false;
  const focused = doc.activeElement;
  const owner = focused?.closest('[data-editor-id]');
  if (owner && owner !== root) return false;
  // Another input owns text-navigation keys even when it is not in a modal.
  return !focused || root.contains(focused) || !focused.matches('input, textarea, select, [contenteditable="true"]');
}

type DropTarget = { element: () => HTMLElement | null; insert: (paths: string[]) => void };
export function createNativeDropRouter() {
  const targets = new Map<string, DropTarget>();
  return {
    register(id: string, target: DropTarget): () => void {
      targets.set(id, target);
      return () => { if (targets.get(id) === target) targets.delete(id); };
    },
    dispatch(paths: string[], physical: { x: number; y: number }, ratio: number, doc: Document): boolean {
      if (!(ratio > 0) || !Number.isFinite(ratio) || hasEditorOverlay(doc)) return false;
      const x = physical.x / ratio;
      const y = physical.y / ratio;
      if (!Number.isFinite(x) || !Number.isFinite(y)) return false;
      const hit = doc.elementFromPoint(x, y);
      if (!hit) return false;
      for (const target of targets.values()) {
        const root = target.element();
        if (!root || !root.contains(hit)) continue;
        const bounds = root.getBoundingClientRect();
        if (x < bounds.left || x >= bounds.right || y < bounds.top || y >= bounds.bottom) continue;
        // Consume once, including a read-only target; never fall back to another pane.
        target.insert([...paths]);
        return true;
      }
      return false;
    },
  };
}
export const nativeEditorDrops = createNativeDropRouter();

export function reviewDomIds(componentId: string) {
  return { title: `${componentId}-review-title`, result: `${componentId}-review-result`, search: `${componentId}-review-search` };
}
/** Contain dialog keystrokes even if an event arrives for an inactive owner.
 * Only the focused owning pane may advance/search/close its own proposal. */
export function handleReviewKey(event: KeyboardEvent, inside: boolean, owns: boolean, actions: {
  advance(reverse: boolean): void; find(): void; close(): void; canClose(): boolean;
}): boolean {
  if (!inside || event.isComposing) return false;
  const find = (event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'f';
  const editorShortcut = (event.ctrlKey || event.metaKey) && ['s', 'f', 'h'].includes(event.key.toLowerCase());
  if (event.key !== 'F3' && event.key !== 'Escape' && !editorShortcut) return false;
  event.preventDefault(); event.stopPropagation();
  if (!owns) return true;
  if (event.key === 'F3') actions.advance(event.shiftKey);
  else if (find) actions.find();
  else if (event.key === 'Escape' && actions.canClose()) actions.close();
  return true;
}
