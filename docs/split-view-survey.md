# Split view: T4 Phase A state-store survey

Status: survey only. Phase B requires explicit human approval of this document.
No application source or pane implementation was changed. References are to the
source as surveyed in this session; line numbers are starting locations, not a
promise that later edits will preserve offsets.

## Finding and proposed boundary

Two independent notes cannot be displayed safely by adding a second `<Editor>`
today. Both instances subscribe to the same module-level stores and reactively
load the same note. Their independent save queues would then operate against
shared metadata, dirty flags, modes, and navigation intent. This is a hard
blocker requiring bounded changes **inside Editor.svelte** before Phase B can
instantiate that same component twice. Do not fork Editor or duplicate its code.
This document proposes that boundary; it does not authorize implementation.

Keep one vault, sidebar, note list, search overlay, settings overlay, theme and
application configuration per webview. Give each mounted editor a stable pane
identity, its own note stores and navigation history. Route shared controls to
the selected pane; do not switch Editor's store bindings when keyboard focus
moves. Focus mode (the distraction-free layout) is distinct from pane focus and
can remain a window-level preference.

## Exact current shapes

The relevant module-level exports in `src/lib/stores/app.ts:30`, `:64`, `:79`,
and `:85` are reproduced below. Boolean/string inference is part of their
current shape. These are singleton stores per JS/webview runtime, not a native
process-wide shared memory object.

```ts
// UI state
export const viewMode = writable<ViewMode>("all");
export const sortMode = writable<SortMode>("modified");
// Group the notes list under relative-date headers (All Notes, date sorts only). Persisted per vault.
export const groupNotesByDate = writable<boolean>(false);
export const tasksLayout = writable<"list" | "calendar">("list");
export const tasksHideCompleted = writable<boolean>(true);
export const tasksOnlyFlagged = writable<boolean>(false);
export const tasksSort = writable<"due" | "priority" | "note">("due");
export const sidebarCollapsed = writable(false);
export const notelistCollapsed = writable(false);
export const sidebarWidth = writable(220);
export const notelistWidth = writable(280);
export const outlineWidth = writable(220);
export const searchQuery = writable("");
export const showCommandPalette = writable(false);
export const showSearch = writable(false);
export const showSettings = writable(false);
export const settingsTab = writable<string | null>(null);
export const showInfo = writable(false);
export const notebookIcons = writable<Record<string, string>>({});
export const tagStyles = writable<Record<string, TagStyle>>({});
export const quickAccessPaths = writable<string[]>([]);
export const collapsedNotebooks = writable<string[]>([]);

// Data
export const notebooks = writable<NotebookEntry[]>([]);
export const rootNoteCount = writable<number>(0);
export const notes = writable<NoteEntry[]>([]);
export const tags = writable<[string, number][]>([]);
export const activeNote = writable<NoteContent | null>(null);
export const activeNotePath = writable<string | null>(null);
export const activeNotebook = writable<NotebookEntry | null>(null);
export const activeTag = writable<string | null>(null);

// External-file viewer mode: set when an .md file outside the active vault is opened.
// While set, the editor is forced read-only, autosave is suppressed, and a banner
// offering "Import to vault" / "Close" is shown.
export const viewerNote = writable<{ path: string; content: string } | null>(
  null,
);

// Mobile state
export const mobileView = writable<"sidebar" | "notelist" | "editor">(
  "sidebar",
);

// Editor state
export const editorDirty = writable(false);
export const sourceMode = writable(false);
export const focusMode = writable(false);
export const readOnly = writable(false);

```

Payload definitions (`src/lib/types.ts:1`, `:10`, `:29`, `:47`):

```ts
export interface NoteMeta {
  id: string;
  title: string;
  tags: string[];
  pinned: boolean;
  created: string;
  modified: string;
}

export interface NoteEntry {
  path: string;
  relative_path: string;
  meta: NoteMeta;
  preview: string;
}

export interface NotebookEntry {
  name: string;
  path: string;
  relative_path: string;
  children: NotebookEntry[];
  note_count: number;
}

export interface NoteContent {
  path: string;
  meta: NoteMeta;
  content: string;
  raw: string;
}
```

Other relevant shared store shapes (`src/lib/stores/app.ts:18`, `:56`):
`appConfig = writable<AppConfig | null>(null)`;
`notebookSortMode = writable<"alphabetical" | "manual">("alphabetical")`;
`notebookOrder = writable<Record<string, number>>({})`;
`noteOrder = writable<Record<string, number>>({})`.
AppConfig remains shared; save/focus policy reads `active_vault`,
`default_view_mode`, `new_notes_in_source_mode`, `show_line_numbers`,
`restore_last_session`, `startup_view`, and appearance preferences rather than
copying configuration into each pane. Its full definition is at `src/lib/types.ts:99`.

Navigation (`src/lib/stores/app.ts:124`–`:169`):
`NavHistoryState = { stack: string[]; index: number; skipping: boolean }`;
initial value `{ stack: [], index: -1, skipping: false }`. The exported
`navHistory` exposes `subscribe`, `clear(): void`, `push(path: string): void`,
`remap(oldPrefix: string, newPrefix: string): void`, and
`go(direction: -1 | 1): string | null`. `canGoBack` and `canGoForward` are
derived booleans. Separately, `src/lib/utils/note-navigation.ts:1` owns ONE
`currentNoteNavigation: number` initially 0, incremented by
`beginNoteNavigation(): number`; `isCurrentNoteNavigation(token): boolean`
and `cancelNoteNavigation(token): void` govern every navigation producer.

Persistence shape (`src/lib/types.ts:158`):

```ts
export interface VaultState {
  last_open_note: string | null;
  sidebar_width: number;
  notelist_width: number;
  outline_width?: number;
  sidebar_collapsed: boolean;
  notelist_collapsed: boolean;
  collapsed_notebooks: string[];
  notebook_sort_mode?: string;
  notebook_order?: Record<string, number>;
  note_order?: Record<string, number>;
  sort_mode?: string;
  group_notes_by_date?: boolean;
  last_view_mode?: string;
  last_notebook?: string | null;
  last_tag?: string | null;
  tasks_layout?: string;
  tasks_hide_completed?: boolean;
  tasks_only_flagged?: boolean;
  tasks_sort?: string;
}
```

`viewMode` is the exact union `"all" | "notebook" | "tag" | "trash" |
"search" | "quickaccess" | "daily" | "tasks" | "hermes"`
(`src/lib/types.ts:259`). `sortMode` is typed `SortMode = "modified" | "title" | "created" | "custom"`
(`src/lib/types.ts:258`), initially `"modified"`; list filtering/sorting and notebook/tag selection are shared
browser state, not the editor's document identity.

`vaultState` (`src/lib/stores/app.ts:233`) derives only `last_open_note`, widths,
collapsed flags/notebooks, notebook sort/order and note order. Actual save logic
in `AppLayout.svelte:384` constructs the larger `VaultState`, adding list/task
preferences and last view/notebook/tag; neither path stores two notes or a split
ratio. Restore is at `AppLayout.svelte:964`. `activateVaultData` at
`app.ts:174` clears the single note/viewer/dirty/history and shared vault data.

Instance-local Editor shapes, already suitable for retaining per pane:

- `loadedPath: string = ''`; `NoteScrollPosition = { rich: number; source: number }`;
  `noteScrollPositions: Map<string, NoteScrollPosition>` capped at 200
  (`Editor.svelte:196`). Rich TipTap instance and SourceController are local
  (`:109`, `:186`). Undo/selection, progressive load, hidden title heading,
  menus, history review, in-note search and outline state are also local.
- `saves = new RevisionSaveQueue()` (`Editor.svelte:3407`). Its private
  `SaveSession = { revision: number; savedRevision: number; raw: string }`,
  initially `{ revision: 0, savedRevision: 0, raw: '' }`;
  `SaveRevision = { readonly session: SaveSession; readonly revision: number }`;
  `tail: Promise<void>`, `idleTimer` and `maxWaitTimer` are local
  (`src/lib/utils/revision-save.ts:1`). `dirty` compares revisions; it is not
  the global `editorDirty` store, even though callers try to keep them aligned.
- `saveError: { message: string; path: string; vault: string; conflict: boolean } | null`,
  `saveAttempt: number`, `retryingSave: boolean` (`Editor.svelte:3409`).
- `EditorOrigin = { snapshot: SaveRevision; path: string | null;
  vault: string | null | undefined; noteId: string | undefined;
  sourceMode: boolean; surface: Editor | null }` (`Editor.svelte:3413`).
  Its fields currently capture global note/mode identity.
- `externalOperationBusy`, `pathMigrationPending`, `trashingNote`, and
  `renamingNote: SaveRevision | null` serialize mutations (`:203`).

## Component and route inventory

References below are relative to `src/lib/components/` unless prefixed otherwise.
The list covers both direct store consumers and components reached through
callbacks or DOM routing; absence of a direct store import is not sufficient.

| Component / route | Single-note assumption and Phase B consequence |
| --- | --- |
| `AppLayout.svelte:86` | One `editor` reference, outline-open/ID pair, pending search reveal, task-note-opened flag (`:448`), navigation token and active refresh request. Replace target-dependent values with pane-owned records and stable component refs. |
| `AppLayout.svelte:263` | Path move remaps one history/note. History/open-file/startup restore (`:289`, `:307`, `:354`), switcher/search (`:464`, `:489`) and daily note (`:597`) replace one note pair. Each asynchronous intent must retain its destination pane across awaits. |
| `AppLayout.svelte:564` | Metadata edits and native task changes (`:571`) inspect only the active path. A task in an inactive pane would be mutated on disk without its editor's flush/lock; resolve owners by path across all panes. |
| `AppLayout.svelte:686` | Trash clears only one selection. Shortcuts and mouse back/forward (`:709`, `:714`) target one editor; active pane should receive save/search/replace/link/AI/history/source/outline/detach actions. App Escape and focus mode stay window-level. |
| `AppLayout.svelte:421` | Vault switch flushes one editor. Close (`:904`), mobile lifecycle (`:888`), watcher refresh (`:858`, `:943`), vault restore (`:950`) and dirty subscription/pending OS opens (`:1042`) all assume one draft. Flush all before destructive/window/vault operations, refresh all matching paths, and aggregate dirty state. |
| `AppLayout.svelte:1190` | Sidebar/NoteList callbacks bind directly to one editor; mobile Editor at `:1196`, desktop Editor at `:1273`. Introduce keyed pane hosts in the desktop editor-panel, not two sidebars/lists. Tasks placeholder (`:1274`) and mobile outline reporting are currently single-target. |
| `Editor.svelte:38` | Direct singleton imports are the principal hard blocker. Props at `:83` contain only trash/outline/tag callbacks, no note or pane context. Full internal map follows below. |
| `Sidebar.svelte:48` | Before-note-change/path-mutation callbacks are singular. Notebook rename/move/delete (`:389`, `:428`, `:448`, `:483`, `:588`) may affect both documents. Remap/flush/lock every affected pane, while keeping one shared browser filter (`:216`–`:291`). |
| `Sidebar.svelte:775` | Empty-trash remembers and clears one active note after deletion. Check all open panes for removed notes; do not clear an unrelated live document. Its current `''` deselection differs from other callers' `null` and should be normalized at the navigation boundary. |
| `NoteList.svelte:55` | Callbacks for select, save-before-switch/duplicate, path mutation, metadata, create-focus and task mutations all target one editor. Calendar select (`:119`), ordinary select (`:566`), task select (`:586`) and create (`:614`) write global note/path and clear dirty. |
| `NoteList.svelte:659` | Duplicate only flushes a dirty active note. Rename/move (`:693`, `:995`), delete/restore (`:719`, `:757`), metadata (`:796`), batch operations (`:1115`, `:1132`, `:1170`) only protect/clear the active path. Must discover inactive owners too. |
| `NoteList.svelte:175` | Range/multiselect anchor and reveal checks (`:294`, `:1078`, `:1092`), calendar highlight (`:1396`) and row active class (`:1469`) use one active path. Keep list multiselection independent; distinguish active-pane note from the other open note. Navigation token generation at `:241` must become destination-scoped. |
| `NoteSwitcher.svelte:15` | Callback already accepts a path; recent/current sections use global active note/history (`:82`, `:101`, `:126`). Its own popover focus is scoped (`:64`, `:164`). Capture destination when opening/selecting so delayed fetch does not follow a later pane focus. |
| `TitleBar.svelte:89` | Focus mode is window-wide; edit/view button at `:94` reads/toggles global readOnly and derives trash state from one path. Route edit/view to active pane and keep focus layout policy explicit. |
| `CommandPalette.svelte:12` | Shared view navigation resets browser filters; toggle-source at `:139` changes all subscribed editors today. Route note actions to captured active pane; theme/search remain shared. |
| `SearchPanel.svelte:9` | No direct note-store dependency; `onSelectNote` at `:176` delegates to AppLayout and supplies search generation guard. Combine that guard with captured pane identity/navigation generation; one shared overlay is sufficient. |
| `TasksView.svelte:13` | Task-open/toggle/priority/due callbacks (used at `:166`, `:176`, `:182`, `:307`) identify a note, not a pane. Parent must resolve displayed ownership for mutations and active destination for open. Keep shared task filters/list. |
| `SettingsPanel.svelte:15` | `onBeforeVaultSwitch` also guards attachment scans/cleanup and backup creation/restore (`:96`, `:118`, `:180`, `:226`), not just switching vaults (`:1322`). Its callback must protect every draft. |
| `NoteWindow.svelte:23` | Detached window has one immutable originating vault and one editor (`:29`, `:318`); initial load (`:173`), close (`:129`), dirty refresh/conflict (`:190`), title/controls (`:53`, `:254`) use singleton stores. Separate webview runtime makes this safe today, but a new Editor context API needs a default one-pane provider here too. No assumption that detached windows share JS stores with main window. |
| `src/routes/+layout.svelte:87` | Capture-phase link handler (`:119`, `:141`; touch handling `:145` onward) resolves relative links against global activeNotePath (`:93`) then broadcasts a string-only `ryuknotes-open-note` (`:98`). Inactive-pane clicks can resolve against the wrong directory and both Editor listeners receive it. Must derive origin from the link's pane before resolution and target one destination. Startup platform/theme and global drop-navigation prevention (`:196`) can stay shared. |
| `src/routes/+page.svelte:49` | Zoom viewport preservation takes the first source scroller/editor body in the document. Choose the event-origin/active pane, or preserve both scroll ratios when global font size changes (`:89`). Root chooses one main layout or detached window (`:328`); both need compatible context providers. |
| `SourceEditor.svelte:5` | Controller passed as prop; attach/configure/measure (`:14`, `:25`) are instance-local. No singleton note ownership blocker. Keep one controller per Editor, never share it between panes. |
| `HistoryReview.svelte:60` | Global `getElementById` for tab focus and fixed IDs/ARIA targets (`:82`, `:83`, `:94`, `:120`) collide if both histories open. Instance-prefix IDs or scope refs. No note-store dependency. |
| `SnapshotDocument.svelte:14` | HTML/label props and instance preview root; no active-note store. Preview sanitization/rendering is local. Global theme/Mermaid behavior should be checked in two-history validation, not replaced with pane note state. |
| `ImageViewer.svelte:3` | Image/close props, local viewport state; mount autofocus (`:203`) and dialog-local keyboard handling (`:216`). No broadcast key handler blocker; simultaneous modal dialogs still need an overlay/focus ownership policy so background openings do not steal focus. |
| `InfoPanel.svelte:50` | App-level modal captures window shortcuts (`:74`); keep a single window overlay and prevent commands leaking into panes. It is distinct from Editor's per-note Info state. |
| `ResizeHandle.svelte:4` | Callback-based resize control, local drag bookkeeping and keyboard resizing (`:51`); reusable for split separator with min/max/controls. Clamp split based on available editor width and remeasure both views. No note ownership state. |
| `ResizeHandles.svelte:3` | Native window edge resize controls; unrelated to editor split ratio. Keep window-level. |
| `TagSuggestInput.svelte:5`, `TagStylePicker.svelte`, `TagLabel.svelte`, `NotebookGlyph.svelte` | Input/presentation/style helpers do not import active-note/dirty/navigation stores. Tags and glyph preferences remain vault-wide; callbacks must keep originating pane identity when changing a document. |
| `VaultPicker.svelte` | No active-note store consumption; transitions vault readiness/configuration. New vault activation must clear all pane sessions through the existing reset boundary, after switch guard succeeds. |

## Editor.svelte internal map: required reuse changes

| Source location | Current assumption / collision |
| --- | --- |
| `:38`, `:83`, `:164`–`:196` | Global stores, callback-only props, trash/session actions and SourceController callback all read global note/mode. Add an instance-scoped store boundary; keep app configuration and vault metadata shared. |
| `:281`, `:530` | Search target and outline origin checks compare loadedPath with global path. Instance-scoped note/path is required even when that pane is inactive. Outline IDs already use `$props.id()` (`:81`); preserve that good pattern. Outline width remains global at `:8228`. |
| `:945`, `:954`, `:2770`, `:3199`, `:3298`, `:3878` | Viewer import/close, wiki-link open/create and history navigation begin a module-global navigation intent and replace global note stores. Independent navigation needs a per-pane generation; ctrl/middle-click detached-window behavior must remain intact. |
| `:3407`–`:3581` | Save queue is local but captureOrigin, current-session checks, body/meta capture and clean flag publication all use global stores. Focusing a different document must never change an inactive pane's save origin or baseline. |
| `:3583`, `:3611`, `:3646`, `:3901`, `:3927` | Trash, conflict flagging, read-only reaction, native task mutation and path-mutation locks refer to the single current note. Parent must target all affected instances; per-pane lock state stays internal. |
| `:3941`–`:4050` | Scroll map and load sessions are local already. loadNote reads global accepted raw/title/viewer, resets global dirty, and writes readOnly/sourceMode. Those must become local pane stores. load failure clears dirty (`:3970`) only for its owner. |
| `:2423`, `:2431`, `:6331` | Slash/wiki selected-item scrolling uses document-wide first match; opening a callout menu removes all callout menus. Scope selectors/cleanup to owned DOM or a deliberately shared overlay manager. |
| `:5181`, `:7743` | Every instance handles F3 on window when its in-note search is open; both can advance and steal focus. Gate by pane/overlay ownership, not merely noteSearchOpen. |
| `:5438`, `:5775` | Reactive load follows global path/note; TipTap transaction/update callbacks eventually mark the shared dirty flag. Two instances will load together today. |
| `:7127` | Source toggle converts local documents but both react to one global boolean and asynchronously focus their own rich/source view (`:7161`, `:7179`). One toggle can steal focus twice. Make mode local and focus restoration conditional on current focus intent. |
| `:7211` | Each instance registers native window drops and inserts every dropped file when canInsert passes. No hit testing of drop coordinates against the pane. Route once by drop location, not simply the last active pane. |
| `:7237`, `:7250`, `:7343` | Wiki title file events can be shared/duplicated safely as cache updates, but path moved and rewritten links inspect one global path/session. Remap/rebase all affected pane owners without dropping drafts; preserve stale-write checks. |
| `:7374`–`:7393` | Each instance subscribes to untargeted ryuknotes-open-note and flushes on destroy without an awaited parent barrier. One broadcast starts two competing global intents. Parent must route once and flush before pane removal; onDestroy is only a fallback. |
| `:7078`, `:8202`, `:8477`–`:8482` | getElementById opens fixed insert-image/grid/camera/file input IDs; both panes would open the first input. Prefix all IDs and associated lookup/label/ARIA references with componentId. History picker fixed ID also needs uniqueness. |
| `:7784`, `:7873`, `:8033`, `:8152` | Title, metadata, dirty/errors, viewer/banner and source/rich rendering all follow global state. History/AI/paste/attachment async operations already use EditorOrigin; make that origin pane-local and do not replace it with active-pane lookup after awaits. |

## Focus and autosave state machine

Current navigation flow: begin a global token, flush outgoing editor, read target,
recheck token and flush again where implemented, set activeNote/path and dirty=false,
then loadNote. loadNote starts a revision session from accepted raw, resets transient
UI/undo and restores scroll; defaults determine source/readOnly. NoteList sometimes
writes stores before invoking parent's load callback. The reactive load effect also
loads when path changes. This is one logical owner expressed in multiple layers,
not a clean prop-driven component contract.

Current save flow:

1. Rich document update / source edit / metadata edit calls markEdited. Revision
   increases, editorDirty=true, debounce resets to 3 seconds; maximum wait is
   10 seconds (`revision-save.ts:27`). Read-only loading is not a user edit.
2. Autosave or explicit flush calls forceSave. It cancels timers, rejects busy,
   rename/trash or stale/uninitialized surfaces, and skips viewer/no-note/clean
   cases (`Editor.svelte:3523`). Viewer skip is suppression, not proof a draft
   was persisted.
3. Immutable body/meta/path/vault/session are captured before awaiting. The queue
   serializes saves, uses accepted raw as expectedRaw, and advances savedRevision
   only after persistence succeeds (`revision-save.ts:74`). Newer edits remain
   dirty. Save completion publishes NoteContent and clears editorDirty only when
   attempt/session/revision/origin still match.
4. Failure retains dirty draft and surfaces retry/conflict-copy/reload controls.
   Navigation/window close should remain blocked on false. External refresh keeps
   dirty buffers; restore compares accepted raw and flags conflict rather than
   replacing them (`AppLayout.svelte:858`). Task/path mutations flush then lock
   editing until disk work/reload/rebase completes.
5. Pane disposal must await flush; current onDestroy fire-and-forget cannot cancel
   unmount. Vault change clears the global data set; it must follow all-pane flush.

Scheduled backup is different from the Settings barrier: `AppLayout.svelte:224`
backs up persisted vault state without an editor flush. That remains one shared
scheduler; it does not guarantee capture of unsaved buffers in either pane.

Current focus flow: browser native focus identifies a title/input/TipTap/CodeMirror
surface, but no store records its owning editor. Parent keyboard shortcuts dispatch
to its single editor ref. Search/switcher/outline/toolbars deliberately move focus;
source conversion restores caret/focus after tick, guarded by note/session, not a
pane focus generation. `focusMode` hides navigation chrome; it is not keyboard focus.

With two panes: A typing followed by B load can reset the shared dirty flag; A's
save may capture B metadata/path or fail loadedPath checks; a clean B can release
pending-open work while A is dirty. Switching global activeNote to represent focus
causes A to load B and cancels A's save timers. A navigation cancels B's global token.
Closing/swapping vault flushes only the last-bound editor; task/rename/delete ignores
inactive owners. Source toggle and F3 can focus both panes. Native drops and broadcast
links can run twice. Same-note independent queues do not serialize each other;
expectedRaw conflict protection prevents silent stale overwrite, but does not merge
two live buffers or provide coordinated UI.

Proposed focus flow: pointer/focusin within pane selects stable pane ID; toolbar and
portal interactions retain their originating ID; opening shared overlays captures a
destination. Moving focus to sidebar/settings does not clear last editor pane. Every
async action captures destination and a pane navigation/focus generation. Background
save/refresh is valid without keyboard focus and may never take it. Source conversion,
search reveal and title autofocus require an explicit still-current focus intent.

## Proposed data model and hook points (design only)

- Window workspace: ordered one-or-two stable PaneIds, activePaneId, splitRatio,
  vault identity/epoch and a registry of mounted editor handles. Do not key an
  Editor by path; that would throw away its local history/scroll/save state.
- Pane state: id; writable note: NoteContent|null; writable path: string|null;
  writable viewer: {path:string; content:string}|null; writable dirty:boolean;
  writable sourceMode:boolean; writable readOnly:boolean; its own NavHistoryState
  controller; navigationGeneration:number; focusGeneration:number; taskNoteOpened;
  outline open/panel ID/width. Keep draft body, RevisionSaveQueue, accepted raw,
  selection, rich/source scroll maps and async-operation locks inside each Editor.
  Store refs must stay bound to this pane even while inactive.
- Derived window state: anyDirty; activePane (for shared controls); canGoBack/Forward
  of active pane. An active-pane compatibility facade can serve shared UI temporarily,
  but **Editor must never subscribe to that facade**. No two-way mirror loop between
  old globals and per-pane stores. Migrate direct writers to explicit pane actions.
- Introduce a pane provider/host using Svelte context (or explicit stable store props).
  Editor retrieves local note/mode/history/navigation dependencies there, retaining
  global config/vault/list preferences. A one-pane provider in NoteWindow preserves
  detached-window behavior. This is the minimal structural Editor change required;
  adding a wrapper alone cannot override imported singleton stores.
- AppLayout owns keyed pane hosts and split separator, editor-ref lookup and active
  action dispatch. Sidebar/NoteList/search/switcher capture destination at intent
  start. Native task/path/delete operations resolve ALL matching paths first and
  acquire flush/locks for affected panes; window/vault/restore/cleanup barriers flush
  all. Abort whole operation on any failure and preserve both drafts.
- Parent refresh routes file events by vault+path to all open owners. Shared list
  caches remain shared; per-pane refresh generations prevent crossed completion.
- Route layout-level relative links using closest pane origin; native drops use pane
  bounds/coordinates; broadcast events become targeted or are consumed once by the
  parent. Centralize window commands while keeping editor-local keybindings local.
- Initial same-note policy recommendation: selecting a note already open in the
  other pane activates that pane rather than creating two editable buffers. This
  is a proposed behavior requiring review, not an approved feature cut. If duplicate
  views of one note are required, a shared document/session owner or explicit
  synchronization policy is an additional blocker; two independent queues are not
  sufficient. Detached-window external conflicts still use expectedRaw safeguards.
- Preserve current single-note persistence initially by saving active-pane path to
  last_open_note. If restoring two panes is desired, add optional pane descriptors,
  active pane and ratio with backward-compatible defaults. Update BOTH actual
  AppLayout save/restore and derived vaultState; fs serialization also needs Rust
  VaultState changes (`src-tauri/src/types.rs:392`), whereas DB state is JSON via
  `src/lib/api-db.ts:252`/`:266`; routing stays at `src/lib/api.ts:341`. Do not assume
  adding TS fields alone will round-trip through the filesystem backend.
- Desktop split with narrow-width clamping; mobile and detached windows keep one
  pane unless separately approved. Focus mode should retain both mounted editors
  and their buffers/scroll, hiding shared chrome rather than unmounting an inactive
  dirty pane. Global font-size changes must preserve both viewports and remeasure
  CodeMirror/TipTap after resize.

## Hard blockers and approval boundary

1. Editor singleton note/mode/dirty/history imports and local/global save-origin
   coupling: context/store injection inside Editor is unavoidable for independent
   notes. Parent-only layout work cannot satisfy the requirement.
2. Global navigation generation and scattered direct store writers: scope all
   producers together, including Editor wiki/viewer/history and create-title focus.
3. Lifecycle and mutation barriers protect one editor: enumerate all relevant owners
   before close, switch, restore, attachment cleanup, deletion and path mutations.
4. Event/DOM collisions inside Editor: targeted links/drops/F3; scoped selectors,
   asset input IDs and asynchronous focus. HistoryReview IDs also require repair.
5. Same-note editing policy must be settled before exposing independent buffers.
   Existing optimistic concurrency is a last defense, not a shared-document design.
6. Split session persistence requires a cross-language shape extension if requested;
   preserving the active-note-only restore is a bounded alternative for review.

No backend storage-schema change is needed merely to show two different notes from
one vault. No evidence here requires forking Editor or replacing its editing engine.
The hard blockers above are work prerequisites, not authorization to edit Editor.

After approval, Phase B verification should cover different-note typing/save in
both panes, independent scroll/caret/source/readOnly/history, rapid crossed navigation,
shared sidebar/search destinations, all-pane close/vault guards, inactive-note native
mutations, watcher/restore conflicts, asset picker/drop/link targeting, duplicate IDs,
same-note policy, focus-mode/resize and detached-window regressions. Keep expectedRaw
and immutable save-snapshot protections intact.

## Phase A verification

- `npm run check`: passed, 0 errors and 0 warnings.
- `npm test`: passed, all 37 tests.
- Application-source SHA-256 comparison (`src/` and `src-tauri/src/`): unchanged
  across this survey session. Gate success alone does not establish source integrity.
- Rust gates, frontend/release build, packaging and GUI boot: not run for this
  documentation-only task; Rust toolchains are unavailable in the cage.
- No git operations and no user vault/config access. Only this survey and
  `.session-commit-msg` were intentionally written in the worktree.

Phase B remains unstarted and requires explicit approval of this survey under T4.
