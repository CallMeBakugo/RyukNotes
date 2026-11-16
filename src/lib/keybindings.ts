import { writable } from "svelte/store";

// A single key combination. `mod` means Ctrl on Windows/Linux and Cmd on macOS.
// `code` is a KeyboardEvent.code value (layout-independent), e.g. "KeyN",
// "Backslash", "ArrowLeft", "F11".
export interface Binding {
  mod?: boolean;
  shift?: boolean;
  alt?: boolean;
  code: string;
}

export interface ActionDef {
  id: string;
  label: string;
  group: "General" | "Interface" | "Navigation";
  default: Binding;
}

// Every customizable action and its factory-default binding. These mirror the
// handlers in AppLayout.handleKeydown and the rows in InfoPanel's Shortcuts tab.
const isMac = typeof navigator !== "undefined" && navigator.platform.startsWith("Mac");

export const ACTIONS: ActionDef[] = [
  { id: "new-note", label: "New note", group: "General", default: { mod: true, code: "KeyN" } },
  { id: "save", label: "Save", group: "General", default: { mod: true, code: "KeyS" } },
  { id: "quick-open", label: "Command palette", group: "General", default: { mod: true, code: "KeyP" } },
  { id: "find-in-note", label: "Find in note", group: "General", default: { mod: true, code: "KeyF" } },
  // Cmd+H hides the application on macOS.
  { id: "replace-in-note", label: "Replace in note", group: "General", default: isMac ? { mod: true, alt: true, code: "KeyF" } : { mod: true, code: "KeyH" } },
  { id: "search-vault", label: "Search vault", group: "General", default: { mod: true, shift: true, code: "KeyF" } },
  { id: "open-new-window", label: "Open note in new window", group: "General", default: { mod: true, shift: true, code: "KeyW" } },

  { id: "toggle-sidebar", label: "Toggle sidebar", group: "Interface", default: { mod: true, code: "Backslash" } },
  { id: "toggle-notelist", label: "Toggle notes list", group: "Interface", default: { mod: true, shift: true, code: "Backslash" } },
  { id: "toggle-theme", label: "Toggle dark / light theme", group: "Interface", default: { mod: true, shift: true, code: "KeyN" } },
  { id: "toggle-focus", label: "Toggle focus mode", group: "Interface", default: { mod: true, shift: true, code: "KeyE" } },
  { id: "toggle-readonly", label: "Toggle read-only", group: "Interface", default: { mod: true, shift: true, code: "KeyR" } },
  { id: "toggle-source", label: "Toggle source / WYSIWYG", group: "Interface", default: { mod: true, shift: true, code: "KeyM" } },
  { id: "fullscreen", label: "Toggle fullscreen", group: "Interface", default: { code: "F11" } },

  { id: "nav-back", label: "Go back", group: "Navigation", default: { alt: true, code: "ArrowLeft" } },
  { id: "nav-forward", label: "Go forward", group: "Navigation", default: { alt: true, code: "ArrowRight" } },
];

export const DEFAULT_BINDINGS: Record<string, Binding> = Object.fromEntries(
  ACTIONS.map((a) => [a.id, a.default]),
);

const STORAGE_KEY = "ryuk-keybindings";

// Editor and platform commands cannot also be assigned to an application action.
const FIXED_BINDINGS: Binding[] = [
  {code: 'F3'}, {shift: true, code: 'F3'},
  {alt: true, code: 'F4'},
  ...(isMac ? [{mod: true, code: 'KeyH'}, {mod: true, code: 'KeyQ'}] : []),
  ...['KeyB', 'KeyI', 'KeyU', 'KeyE', 'KeyK', 'KeyZ', 'KeyY', 'Period', 'Equal', 'Minus', 'Digit0'].map(code => ({mod: true, code})),
  ...['KeyX', 'KeyZ', 'Digit8', 'Digit9', 'Delete'].map(code => ({mod: true, shift: true, code})),
  ...['Digit1','Digit2','Digit3','Digit4','Digit5','Digit6'].map(code => ({mod: true, code})),
  ...['ArrowUp','ArrowDown'].flatMap(code => [{alt: true, code}, {alt: true, shift: true, code}]),
];
export function validBinding(value: unknown): value is Binding {
  if (!value || typeof value !== 'object') return false;
  const b = value as Binding;
  return typeof b.code === 'string' && /^(?:Key[A-Z]|Digit[0-9]|Numpad(?:[0-9]|Add|Subtract|Multiply|Divide|Decimal|Enter|Equal|Comma)|Intl(?:Backslash|Ro|Yen)|F(?:[1-9]|1[0-9]|2[0-4])|Arrow(?:Left|Right|Up|Down)|Backslash|BracketLeft|BracketRight|Comma|Period|Slash|Semicolon|Quote|Backquote|Minus|Equal|Space|Enter|Tab|Backspace|Delete|Home|End|PageUp|PageDown|Insert)$/.test(b.code)
    && (['mod','shift','alt'] as const).every(key => b[key] === undefined || typeof b[key] === 'boolean')
    && (!!b.mod || !!b.alt || /^F[0-9]+$/.test(b.code));
}
export function bindingConflict(id: string, binding: Binding, map: Record<string, Binding>): string | null {
  if (!(Object.hasOwn(DEFAULT_BINDINGS,id)) || !validBinding(binding)) return 'Use a modifier or function key';
  if (FIXED_BINDINGS.some(fixed => bindingsEqual(fixed, binding))) return 'Reserved editor shortcut';
  return ACTIONS.some(action => action.id !== id && bindingsEqual(map[action.id], binding)) ? 'Already in use' : null;
}
export function normalizeBindings(stored: unknown): Record<string, Binding> {
  const map = {...DEFAULT_BINDINGS};
  if (stored && typeof stored === 'object') for (const action of ACTIONS) {
    const value = (stored as Record<string, unknown>)[action.id];
    if (validBinding(value) && !FIXED_BINDINGS.some(fixed => bindingsEqual(fixed,value))) map[action.id] = {...value};
  }
  // Reset ambiguous assignments as a group; a restored default can expose another
  // collision, so repeat until every action has one distinct binding.
  for (let pass=0; pass<ACTIONS.length; pass++) {
    const collisions = new Set<string>();
    for (let i=0;i<ACTIONS.length;i++) for (let j=i+1;j<ACTIONS.length;j++) {
      if (bindingsEqual(map[ACTIONS[i].id],map[ACTIONS[j].id])) {
        collisions.add(ACTIONS[i].id); collisions.add(ACTIONS[j].id);
      }
    }
    if (!collisions.size) break;
    for (const id of collisions) map[id] = DEFAULT_BINDINGS[id];
  }
  return map;
}
function loadBindings(): Record<string, Binding> {
  try {
    const raw = typeof localStorage !== "undefined" && (localStorage.getItem(STORAGE_KEY) ?? localStorage.getItem("helix-keybindings"));
    return normalizeBindings(raw ? JSON.parse(raw) : null);
  } catch { return {...DEFAULT_BINDINGS}; }
}
export const keybindings = writable<Record<string, Binding>>(loadBindings());
keybindings.subscribe(map => {
  try { if (typeof localStorage !== 'undefined') localStorage.setItem(STORAGE_KEY, JSON.stringify(map)); }
  catch { /* Storage may be unavailable. */ }
});
if (typeof window !== 'undefined') window.addEventListener('storage', event => {
  if (event.key === STORAGE_KEY || event.key === 'helix-keybindings' || event.key === null) keybindings.set(loadBindings());
});
export function setBinding(id: string, binding: Binding): boolean {
  let changed = false;
  keybindings.update(map => {
    if (bindingConflict(id,binding,map)) return map;
    changed = true; return {...map,[id]:{...binding}};
  });
  return changed;
}
export function resetBinding(id: string): void {
  if (!(Object.hasOwn(DEFAULT_BINDINGS,id))) return;
  keybindings.update(map => {
    const restored = DEFAULT_BINDINGS[id];
    const occupied = ACTIONS.find(action => action.id !== id && bindingsEqual(map[action.id],restored));
    return normalizeBindings({...map, ...(occupied ? {[occupied.id]:map[id]} : {}), [id]:restored});
  });
}
export function shouldHandleShortcut(event: KeyboardEvent, allowRepeat = false): boolean {
  const target = event.target as Element | null;
  return !event.defaultPrevented && !event.isComposing && event.keyCode !== 229 && (allowRepeat || !event.repeat) && event.key !== 'Process' && event.key !== 'Dead'
    && !target?.closest?.('[role="dialog"], [data-editor-overlay], .settings-overlay, .info-overlay, .palette-overlay, .search-overlay');
}

// True when the keyboard event matches the given binding exactly. Modifier
// state must match precisely so e.g. Mod+Shift+F never also triggers Mod+F.
export function matchBinding(e: KeyboardEvent, b: Binding | undefined): boolean {
  if (!b) return false;
  const mod = e.ctrlKey || e.metaKey;
  return (
    !!b.mod === mod &&
    !!b.shift === e.shiftKey &&
    !!b.alt === e.altKey &&
    e.code === b.code
  );
}

// Returns the id of the first action whose binding matches the event, or null.
export function matchAction(e: KeyboardEvent, map: Record<string, Binding>): string | null {
  for (const {id} of ACTIONS) {
    if (matchBinding(e, map[id])) return id;
  }
  return null;
}

// Build a Binding from a captured keydown event. Returns null for events that
// are only modifier keys (no real key pressed yet).
const MODIFIER_CODES = new Set([
  "ControlLeft", "ControlRight", "MetaLeft", "MetaRight",
  "ShiftLeft", "ShiftRight", "AltLeft", "AltRight",
]);

export function bindingFromEvent(e: KeyboardEvent): Binding | null {
  if (MODIFIER_CODES.has(e.code) || !e.code) return null;
  return {
    mod: e.ctrlKey || e.metaKey,
    shift: e.shiftKey,
    alt: e.altKey,
    code: e.code,
  };
}

export function bindingsEqual(a: Binding | undefined, b: Binding | undefined): boolean {
  if (!a || !b) return false;
  return !!a.mod === !!b.mod && !!a.shift === !!b.shift && !!a.alt === !!b.alt && a.code === b.code;
}

const CODE_LABELS: Record<string, string> = {
  Backslash: "\\",
  BracketLeft: "[",
  BracketRight: "]",
  Comma: ",",
  Period: ".",
  Slash: "/",
  Semicolon: ";",
  Quote: "'",
  Backquote: "`",
  Minus: "-",
  Equal: "=",
  Space: "Space",
  Enter: "Enter",
  Tab: "Tab",
  ArrowLeft: "←",
  ArrowRight: "→",
  ArrowUp: "↑",
  ArrowDown: "↓",
};

function codeToLabel(code: string): string {
  if (code.startsWith("Key")) return code.slice(3);
  if (code.startsWith("Digit")) return code.slice(5);
  if (code.startsWith("Numpad")) return code.slice(6);
  return CODE_LABELS[code] ?? code;
}

// Ordered list of key labels for rendering, e.g. ["Ctrl", "Shift", "N"].
export function bindingToKeys(b: Binding): string[] {
  const keys: string[] = [];
  if (b.mod) keys.push(isMac ? "⌘" : "Ctrl");
  if (b.alt) keys.push(isMac ? "⌥" : "Alt");
  if (b.shift) keys.push(isMac ? "⇧" : "Shift");
  keys.push(codeToLabel(b.code));
  return keys;
}
