// DB-vault adapter.
//
// When the active vault is a `.rn` SQLite database, the path-based api.ts
// functions route here. The UI components are untouched: note ids masquerade
// as note paths, and NoteEntry/NoteContent shapes are synthesized from the
// db_* command results.
//
// Filesystem watchers and filesystem-only attachment tools remain separate.
// Notebook membership is logical metadata; note paths remain stable IDs.

import { invoke } from "@tauri-apps/api/core";
import { recordVaultOpenStatus } from "./utils/vault-open-status";
import type {
  NoteContent,
  DbAcceptedState,
  NoteEntry,
  NoteMeta,
  SearchPage,
  TrashContents,
  VaultState,
  NotebookEntry,
  VersionEntry,
} from "./types";

import type { ExternalNoteReference } from "./editor/externalNote";

let dbMode = false;
let dbSessionGeneration = 0;

export function isDbVault(): boolean {
  return dbMode;
}

export function setDbMode(on: boolean): void {
  dbSessionGeneration++;
  dbMode = on;
  if (!on) recordVaultOpenStatus(null);
}

export function looksLikeDbVault(path: string | null | undefined): boolean {
  return !!path && path.endsWith(".rn");
}

interface DbNote {
  id: string;
  title: string;
  pinned: boolean;
  trashed: boolean;
  created: string;
  modified: string;
  tags: string[];
  preview: string;
}

function toEntry(note: DbNote): NoteEntry {
  return {
    path: note.id,
    relative_path: note.title,
    meta: {
      id: note.id,
      title: note.title,
      tags: note.tags,
      pinned: note.pinned,
      created: note.created,
      modified: note.modified,
    },
    preview: note.preview ?? "",
  };
}

export interface DbOpenReport {
  schema: number; previousSchema: number; status: string | null; vaultUuid: string; storageEpoch: string;
}
export async function dbOpenVault(path: string): Promise<void> {
  recordVaultOpenStatus(null);
  const report = await invoke<DbOpenReport>('db_open_vault', { path });
  setDbMode(true);
  recordVaultOpenStatus(report?.status ?? null);
}

// Detached webviews have their own adapter flag, but share the native connection.
// Attach to the originating vault without reopening it or invalidating live owners.
export async function dbAttachVault(path: string): Promise<DbAcceptedState['vault']> {
  const identity = await invoke<DbAcceptedState['vault']>('db_writer_identity', { vaultPath: path }).catch(writeError);
  setDbMode(true);
  return identity;
}

export async function dbGetNotes(notebookPath: string | null): Promise<NoteEntry[]> {
  const notes: DbNote[] = await invoke("db_get_notes", { notebookPath });
  return notes.map(toEntry);
}

export type DbWriteErrorKind = 'changed' | 'deleted' | 'trashed' | 'vault_replaced' | 'invalid_policy' | 'io';
export class DbWriteError extends Error {
  constructor(public readonly kind: DbWriteErrorKind, message: string, public readonly current?: unknown) {
    // Narrow compatibility boundary: Editor still displays Error.message and
    // recognizes its existing prefix. Native classification never parses text.
    super((['changed', 'deleted', 'trashed', 'vault_replaced'].includes(kind) ? 'Save conflict: ' : '') + message);
    this.name = 'DbWriteError';
  }
}
function writeError(error: unknown): never {
  if (error && typeof error === 'object' && 'kind' in error && 'message' in error) {
    const typed = error as { kind: DbWriteErrorKind; message: string; current?: unknown };
    if (['changed', 'deleted', 'trashed', 'vault_replaced', 'invalid_policy', 'io'].includes(typed.kind)) {
      throw new DbWriteError(typed.kind, typed.message, typed.current);
    }
  }
  throw error;
}
interface AcceptanceChain {
  current: DbAcceptedState;
  generation: number;
  pending?: { key: string; operationId: string };
}
// A read owns a lineage. Unrelated reads (including task/list reads) cannot
// promote a dirty editor's baseline. Weak keys keep inactive live buffers valid.
const acceptedChains = new WeakMap<DbAcceptedState, AcceptanceChain>();

export async function dbReadNote(path: string, vaultPath?: string): Promise<NoteContent> {
  const note = await invoke<Record<string, unknown> | null>('db_get_note', { id: path, vaultPath }).catch(writeError);
  if (!note && vaultPath && /^(?:\/|[A-Za-z]:[\\/])/.test(path) && path.endsWith('.md')) {
    return invoke('read_note', { vaultPath, path });
  }
  if (!note) throw new Error(`No note with id ${path}`);
  return toContent(note);
}

export function dbContentFromReceipt(note: Record<string, unknown>): NoteContent { return toContent(note); }

function toContent(note: Record<string, unknown>, chain?: AcceptanceChain): NoteContent {
  const path = String(note.id);
  const content = String(note.content ?? '');
  const meta: NoteMeta = { id: path, title: String(note.title), tags: [...((note.tags as string[]) ?? [])],
    pinned: !!note.pinned, created: String(note.created), modified: String(note.modified) };
  const receipt = note.accepted as Pick<DbAcceptedState, 'vault' | 'token' | 'revision' | 'vaultUuid' | 'storageEpoch'> | undefined;
  if (receipt) {
    const accepted: DbAcceptedState = Object.freeze({ vault: Object.freeze({ ...receipt.vault }), token: receipt.token,
      body: content, title: meta.title, tags: Object.freeze([...meta.tags]), pinned: meta.pinned, trashed: !!note.trashed,
      created: meta.created, modified: meta.modified,
      ...(receipt.revision ? { revision: Object.freeze({ ...receipt.revision }), vaultUuid: receipt.vaultUuid, storageEpoch: receipt.storageEpoch } : {}) });
    meta.accepted = accepted;
    chain ??= { current: accepted, generation: dbSessionGeneration };
    chain.current = accepted;
    acceptedChains.set(accepted, chain);
  }
  return { path, meta, content, raw: content };
}

export async function dbSaveNote(path: string, meta: NoteMeta, body: string, expectedRaw: string, vaultPath?: string, writerSource: 'gui' | 'task' = 'gui'): Promise<NoteContent> {
  const chain = meta.accepted && acceptedChains.get(meta.accepted);
  if (!chain || chain.current.body !== expectedRaw || meta.id !== path) {
    throw new DbWriteError('invalid_policy', 'A captured accepted state is required before updating this note.');
  }
  const accepted = chain.current;
  const v3 = accepted.token.startsWith('revision-v3.');
  const generation = dbSessionGeneration;
  if (v3 && chain.generation !== generation) throw new DbWriteError('vault_replaced', 'This accepted state belongs to a prior vault session.');
  let operationId: string | undefined;
  const tags = v3 ? [...new Set(meta.tags ?? [])].sort() : meta.tags ?? [];
  if (v3) {
    const key = JSON.stringify([accepted.token, path, meta.title, body, tags, meta.pinned ?? false, writerSource]);
    if (chain.pending?.key !== key) chain.pending = { key, operationId: crypto.randomUUID() };
    operationId = chain.pending.operationId;
  }
  const full = await invoke<Record<string, unknown>>('db_save_note', { request: {
    vault: accepted.vault, vaultPath, id: path, title: meta.title, content: body,
    pinned: meta.pinned ?? false, tags,
    policy: { kind: 'update', expectedToken: accepted.token },
    ...(v3 ? { operationId, writerSource } : {}),
  } }).catch(writeError);
  if (v3) {
    // A late reply is an acknowledgment of its own transaction, never permission
    // to adopt a baseline after this runtime was invalidated or a newer receipt won.
    if (generation !== dbSessionGeneration || chain.generation !== generation) throw new DbWriteError('vault_replaced', 'The vault session changed while saving.');
    if (chain.current !== accepted) throw new DbWriteError('changed', 'Another acknowledgment already advanced this buffer.');
    const next = full.accepted as { token?: string; vaultUuid?: string; storageEpoch?: string; revision?: DbAcceptedState['revision'] } | undefined;
    if (full.id !== path || !next?.token?.startsWith('revision-v3.') || next.vaultUuid !== accepted.vaultUuid
      || next.storageEpoch !== accepted.storageEpoch || next.revision?.incarnationId !== accepted.revision?.incarnationId) {
      throw new DbWriteError('vault_replaced', 'The acknowledgment does not belong to this note incarnation and vault.');
    }
    chain.pending = undefined;
  }
  // Only this transaction's receipt can advance this lineage. No read-back.
  return toContent(full, chain);
}
async function createExplicit(id: string, title: string, content: string, tags: string[], vaultPath?: string): Promise<NoteEntry> {
  const vault = await invoke<DbAcceptedState['vault']>('db_writer_identity', { vaultPath }).catch(writeError);
  const full = await invoke<Record<string, unknown>>('db_save_note', { request: {
    vault, vaultPath, id, title, content, pinned: false, tags, policy: { kind: 'create' },
  } }).catch(writeError);
  const note = toContent(full);
  return { path: note.path, relative_path: note.meta.title, meta: note.meta, preview: content.slice(0, 120) };
}
export async function dbCreateNote(title: string, notebookRelative: string | null = null, vaultPath?: string): Promise<NoteEntry> {
  if (!vaultPath) return createExplicit(crypto.randomUUID(), title, '', []);
  const full = await invoke<Record<string, unknown>>('db_create_note', { vaultPath, notebookRelative, title }).catch(writeError);
  const note = toContent(full);
  return { path: note.path, relative_path: note.meta.title, meta: note.meta, preview: note.content.slice(0, 120) };
}
export async function dbCreateDailyNote(date?: string, vaultPath?: string): Promise<NoteEntry> {
  const iso = date ?? new Date().toISOString().slice(0, 10);
  const id = `daily-${iso}`;
  const existing = await invoke<Record<string, unknown> | null>('db_get_note', { id, vaultPath }).catch(writeError);
  if (existing) {
    const note = toContent(existing);
    return { path: id, relative_path: note.meta.title, meta: note.meta, preview: note.content.slice(0, 120) };
  }
  try { return await createExplicit(id, iso, '', [], vaultPath); }
  catch (error) {
    if (!(error instanceof DbWriteError) || error.kind !== 'changed') throw error;
    // Another window created today's note first. Select it; never delete/rewrite it.
    const note = await dbReadNote(id, vaultPath);
    return { path: id, relative_path: note.meta.title, meta: note.meta, preview: note.content.slice(0, 120) };
  }
}
export async function dbRenameNote(path: string, newTitle: string, vaultPath?: string): Promise<string> {
  const note = await dbReadNote(path, vaultPath);
  await dbSaveNote(path, { ...note.meta, title: newTitle }, note.content, note.raw, vaultPath);
  return path;
}
export async function dbDuplicateNote(path: string, vaultPath?: string): Promise<NoteEntry> {
  const note = await dbReadNote(path, vaultPath);
  return createExplicit(crypto.randomUUID(), `${note.meta.title} (copy)`, note.content, note.meta.tags, note.meta.accepted?.vault.path);
}

export async function dbSearchNotes(query: string, limit = 20, offset = 0): Promise<SearchPage> {
  return invoke("db_search_page", { query, limit, offset });
}

export async function dbGetAllTags(): Promise<[string, number][]> {
  const notes: DbNote[] = await invoke("db_list_notes", { includeTrashed: false });
  const counts = new Map<string, number>();
  for (const note of notes) {
    for (const tag of note.tags) {
      counts.set(tag, (counts.get(tag) ?? 0) + 1);
    }
  }
  return [...counts.entries()];
}

export async function dbGetAllNoteTitles(): Promise<{ title: string; path: string }[]> {
  const notes: DbNote[] = await invoke("db_list_notes", { includeTrashed: false });
  return notes.map((note) => ({ title: note.title, path: note.id }));
}

export async function dbGetTrash(): Promise<TrashContents> {
  const notes: DbNote[] = await invoke("db_list_notes", { includeTrashed: true });
  return {
    notes: notes.filter((note) => note.trashed).map(toEntry),
    notebooks: await invoke("db_notebook_trash"),
  };
}

// ── Persistent vault state (meta table in the DB) ──

const defaultVaultState: VaultState = {
  last_open_note: null,
  sidebar_width: 260,
  notelist_width: 320,
  sidebar_collapsed: false,
  notelist_collapsed: false,
  collapsed_notebooks: [],
};

export async function dbLoadVaultState(): Promise<VaultState> {
  const stored = await invoke<VaultState | null>("db_get_state", { key: "vault_state" });
  return { ...defaultVaultState, ...(stored ?? {}) };
}

export async function dbSaveVaultState(state: VaultState): Promise<void> {
  await invoke("db_set_state", { key: "vault_state", value: state });
}

// ── Quick Access (note ids stored in the meta table) ──

async function readQuickAccessIds(): Promise<string[]> {
  const ids = await invoke<string[] | null>("db_get_state", { key: "quick_access" });
  return Array.isArray(ids) ? ids : [];
}

async function writeQuickAccessIds(ids: string[]): Promise<void> {
  await invoke("db_set_state", { key: "quick_access", value: ids });
}

export async function dbGetQuickAccess(): Promise<NoteEntry[]> {
  const ids = await readQuickAccessIds();
  const out: NoteEntry[] = [];
  for (const id of ids) {
    const note = await invoke<Record<string, unknown> | null>("db_get_note", { id });
    if (!note) continue;
    out.push({
      path: id,
      relative_path: String(note.title),
      meta: {
        id,
        title: String(note.title),
        tags: (note.tags as string[]) ?? [],
        pinned: !!note.pinned,
        created: String(note.created),
        modified: String(note.modified),
      },
      preview: String(note.content ?? "").slice(0, 120),
    });
  }
  return out;
}

export async function dbAddQuickAccess(id: string): Promise<void> {
  const ids = await readQuickAccessIds();
  if (!ids.includes(id)) {
    ids.push(id);
    await writeQuickAccessIds(ids);
  }
}

export async function dbRemoveQuickAccess(id: string): Promise<void> {
  const ids = await readQuickAccessIds();
  await writeQuickAccessIds(ids.filter((x) => x !== id));
}

export async function dbReorderQuickAccess(paths: string[]): Promise<void> {
  await writeQuickAccessIds(paths);
}

// ── Stats / trash / tasks / backup / export ──

export async function dbGetVaultStats(): Promise<import("./types").VaultStats> {
  return invoke("db_vault_stats");
}

export async function dbEmptyTrash(): Promise<number> {
  return invoke("db_empty_trash");
}

export async function dbGetTasks(noteId?: string): Promise<import("./types").TaskItem[]> {
  return invoke("db_get_tasks", { noteId: noteId ?? null });
}

export async function dbCreateBackup(): Promise<import("./types").BackupEntry> {
  return invoke("db_create_backup");
}

export async function dbRestoreBackup(backupPath: string, vaultPath: string): Promise<void> {
  const outcome = await invoke<AncestryRestoreOutcome | null>('db_restore_backup', { backupPath, vaultPath });
  dbSessionGeneration++;
  if (outcome?.requires_recovery_review) recordVaultOpenStatus('Vault restored with a new storage epoch. Review recovered drafts before rebinding.');
}

export async function dbExportMd(outDir: string, vaultPath: string): Promise<number> {
  return invoke("db_export_md", { outDir, vaultPath });
}


// ── Task line edits (checkbox / priority / due) in DB mode ──
// Line numbers are 0-based indices into the note body, as produced by the
// task parser; the raw line guards against drift.

async function readNoteLines(id: string, vaultPath?: string): Promise<{ note: NoteContent; lines: string[] }> {
  const note = await dbReadNote(id, vaultPath);
  return { note, lines: note.content.split('\n') };
}
async function writeNoteLines(id: string, change: { note: NoteContent; lines: string[] }): Promise<void> {
  await dbSaveNote(id, change.note.meta, change.lines.join('\n'), change.note.raw, undefined, 'task');
}

function resolveLine(lines: string[], line: number, rawLine: string): number {
  if (lines[line] === rawLine) return line;
  const found = lines.indexOf(rawLine);
  if (found >= 0) return found;
  throw new Error("Task line not found; the note changed since the task list was built");
}

export async function dbSetTaskDone(noteId: string, line: number, rawLine: string, done: boolean, vaultPath?: string): Promise<void> {
  const note = await readNoteLines(noteId, vaultPath);
  const idx = resolveLine(note.lines, line, rawLine);
  const target = done ? "[x]" : "[ ]";
  note.lines[idx] = note.lines[idx].replace(/\[([ xX])\]/, target);
  await writeNoteLines(noteId, note);
}

export async function dbSetTaskPriority(noteId: string, line: number, rawLine: string, priority: string | null, vaultPath?: string): Promise<void> {
  const note = await readNoteLines(noteId, vaultPath);
  const idx = resolveLine(note.lines, line, rawLine);
  let text = note.lines[idx].replace(/\s*!(high|medium|med|low)\b/gi, "");
  if (priority) {
    const match = text.match(/^\s*([-*])\s\[([ xX])\]\s*(.*)$/);
    if (match) {
      text = `${match[1]} [${match[2]}] ${match[3].trimEnd()} !${priority}`;
    }
  }
  note.lines[idx] = text.replace(/\s{2,}/g, " ").replace(/\s+$/, "");
  await writeNoteLines(noteId, note);
}

export async function dbSetTaskDue(noteId: string, line: number, rawLine: string, due: string | null, vaultPath?: string): Promise<void> {
  const note = await readNoteLines(noteId, vaultPath);
  const idx = resolveLine(note.lines, line, rawLine);
  let text = note.lines[idx].replace(/\s*due:\d{4}-\d{2}-\d{2}/g, "");
  if (due) {
    const match = text.match(/^\s*([-*])\s\[([ xX])\]\s*(.*)$/);
    if (match) {
      text = `${match[1]} [${match[2]}] ${match[3].trimEnd()} due:${due}`;
    }
  }
  note.lines[idx] = text.replace(/\s{2,}/g, " ").replace(/\s+$/, "");
  await writeNoteLines(noteId, note);
}

/** The owner is captured before clipboard/file reads; never infer it from current UI state. */
export async function dbSaveImage(vaultPath: string, name: string, data: number[], noteId?: string | null): Promise<string> {
  if (!noteId) throw new Error("Select a note before inserting an image.");
  return invoke("db_save_image", { vaultPath, noteId, name, data });
}

export async function dbSaveAttachment(vaultPath: string, name: string, data: number[], noteId?: string | null): Promise<string> {
  if (!noteId) throw new Error("Select a note before inserting an attachment.");
  return invoke("db_save_attachment", { vaultPath, noteId, name, data });
}

/** Keep the original vault across the save dialog; native code revalidates it. */
export async function dbDownloadAttachment(vaultPath: string, id: string, choose: (filename: string) => Promise<string | null>): Promise<void> {
  const meta = await invoke<{ filename: string }>("db_attachment_info", { vaultPath, id });
  const destination = await choose(meta.filename);
  if (destination) await invoke("db_download_attachment", { vaultPath, id, destination });
}


export async function dbImportNote(vaultPath: string, sourcePath: string, notebookRelative: string | null, expectedRaw: string, references: ExternalNoteReference[]): Promise<NoteContent> {
  return toContent(await invoke<Record<string, unknown>>("db_import_note", { vaultPath, sourcePath, notebookRelative, expectedRaw, references }));
}

export async function dbMoveNote(vaultPath: string, notePath: string, destNotebook: string): Promise<string> {
  return invoke("db_move_note", { vaultPath, notePath, destNotebook });
}

export async function dbGetNotebooks(): Promise<NotebookEntry[]> {
  return invoke("db_get_notebooks");
}

export async function dbCreateNotebook(vaultPath: string, parentRelative: string | null, name: string): Promise<NotebookEntry> {
  return invoke("db_create_notebook", { vaultPath, parentRelative, name });
}

export async function dbGetNoteVersions(vaultPath: string, noteId: string): Promise<VersionEntry[]> {
  return invoke("db_get_note_versions", { vaultPath, noteId });
}

export async function dbGetNoteVersionContent(vaultPath: string, noteId: string, timestamp: string): Promise<string> {
  return invoke("db_get_note_version_content", { vaultPath, noteId, timestamp });
}

export async function dbCreateVersion(vaultPath: string, path: string, noteId: string): Promise<void> {
  return invoke("db_create_version", { vaultPath, path, noteId });
}

export interface AncestryRestoreOutcome {
  identity: { vault_uuid: string; storage_epoch: string };
  ledger_was_missing: boolean;
  requires_recovery_review: boolean;
}
/** Explicit destination creation, never a displayed-vault switch. The caller
 * retains operationId for retry and uses the existing all-pane pause barrier.
 * Production activation remains gated; native code refuses unresolved owners. */
export async function dbRestoreAncestryExport(sourceDir: string, destination: string, ledgerRoot: string | null,
  mode: 'full_restore' | 'independent_copy', operationId: string): Promise<AncestryRestoreOutcome> {
  return invoke('db_restore_ancestry_export', { sourceDir, destination, ledgerRoot, mode, operationId });
}
/** Append validated same-vault ancestry without promoting incoming heads. */
export async function dbReconcileAncestryExport(sourceDir: string, vaultPath: string, operationId: string): Promise<{
  archiveHash: string; headersAdded: number; requiresRecoveryReview: boolean;
}> {
  return invoke('db_reconcile_ancestry_export', { sourceDir, vaultPath, operationId });
}
