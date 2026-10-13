// DB-vault adapter.
//
// When the active vault is a `.rn` SQLite database, the path-based api.ts
// functions route here. The UI components are untouched: note ids masquerade
// as note paths, and NoteEntry/NoteContent shapes are synthesized from the
// db_* command results.
//
// Not supported in DB mode (by design or deferred): notebooks (the vault is
// flat/tag-first), importNote, moveNote, fs watcher events, attachments by
// relative path (deferred to the attachment rewiring slice).

import { invoke } from "@tauri-apps/api/core";
import type {
  NoteContent,
  NoteEntry,
  NoteMeta,
  SearchPage,
  TrashContents,
  VaultState,
} from "./types";

let dbMode = false;

export function isDbVault(): boolean {
  return dbMode;
}

export function setDbMode(on: boolean): void {
  dbMode = on;
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

export async function dbOpenVault(path: string): Promise<void> {
  await invoke("db_open_vault", { path });
  setDbMode(true);
}

export async function dbGetNotes(notebookPath: string | null): Promise<NoteEntry[]> {
  if (notebookPath) return []; // DB vaults are flat; there are no notebooks.
  const notes: DbNote[] = await invoke("db_list_notes", { includeTrashed: false });
  return notes.map(toEntry);
}

export async function dbReadNote(path: string): Promise<NoteContent> {
  const note = await invoke<Record<string, unknown> | null>("db_get_note", { id: path });
  if (!note) throw new Error(`No note with id ${path}`);
  const meta: NoteMeta = {
    id: String(note.id),
    title: String(note.title),
    tags: (note.tags as string[]) ?? [],
    pinned: !!note.pinned,
    created: String(note.created),
    modified: String(note.modified),
  };
  const content = String(note.content ?? "");
  return { path, meta, content, raw: content };
}

export async function dbSaveNote(
  path: string,
  meta: NoteMeta,
  body: string,
): Promise<NoteContent> {
  await invoke("db_save_note", {
    id: meta.id ?? path,
    title: meta.title,
    content: body,
    pinned: meta.pinned ?? false,
    tags: meta.tags ?? [],
  });
  return dbReadNote(path);
}

export async function dbCreateNote(title: string): Promise<NoteEntry> {
  const id = crypto.randomUUID();
  const now = new Date().toISOString();
  await invoke("db_save_note", {
    id,
    title,
    content: "",
    pinned: false,
    tags: [],
  });
  return {
    path: id,
    relative_path: title,
    meta: {
      id,
      title,
      tags: [],
      pinned: false,
      created: now,
      modified: now,
    },
    preview: "",
  };
}

export async function dbCreateDailyNote(date?: string): Promise<NoteEntry> {
  const iso = date ?? new Date().toISOString().slice(0, 10);
  const id = `daily-${iso}`;
  const existing = await invoke<Record<string, unknown> | null>("db_get_note", { id });
  if (existing) {
    return {
      path: id,
      relative_path: String(existing.title),
      meta: {
        id,
        title: String(existing.title),
        tags: (existing.tags as string[]) ?? [],
        pinned: !!existing.pinned,
        created: String(existing.created),
        modified: String(existing.modified),
      },
      preview: String(existing.content ?? "").slice(0, 120),
    };
  }
  const entry = await dbCreateNote(iso);
  // Rewrite the id to the deterministic daily id.
  await invoke("db_delete_note", { id: entry.path });
  await invoke("db_save_note", {
    id,
    title: iso,
    content: "",
    pinned: false,
    tags: [],
  });
  return { ...entry, path: id, relative_path: iso, meta: { ...entry.meta, id, title: iso } };
}

export async function dbRenameNote(path: string, newTitle: string): Promise<string> {
  const note = await dbReadNote(path);
  await invoke("db_save_note", {
    id: path,
    title: newTitle,
    content: note.content,
    pinned: note.meta.pinned,
    tags: note.meta.tags,
  });
  return path;
}

export async function dbDuplicateNote(path: string): Promise<NoteEntry> {
  const note = await dbReadNote(path);
  const id = crypto.randomUUID();
  const title = `${note.meta.title} (copy)`;
  await invoke("db_save_note", {
    id,
    title,
    content: note.content,
    pinned: false,
    tags: note.meta.tags,
  });
  const now = new Date().toISOString();
  return {
    path: id,
    relative_path: title,
    meta: { id, title, tags: note.meta.tags, pinned: false, created: now, modified: now },
    preview: note.content.slice(0, 120),
  };
}

export async function dbSearchNotes(query: string): Promise<SearchPage> {
  const notes: DbNote[] = await invoke("db_search_notes", { query });
  return {
    results: notes.map((note) => ({
      path: note.id,
      title: note.title,
      snippet: note.preview ?? "",
      score: 0,
      title_matches: [],
      snippet_matches: [],
      match_target: null,
    })),
    total: notes.length,
  };
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
    notebooks: [],
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
  return invoke("db_restore_backup", { backupPath, vaultPath });
}

export async function dbExportMd(outDir: string): Promise<number> {
  return invoke("db_export_md", { outDir });
}


// ── Task line edits (checkbox / priority / due) in DB mode ──
// Line numbers are 0-based indices into the note body, as produced by the
// task parser; the raw line guards against drift.

async function readNoteLines(id: string): Promise<{ title: string; lines: string[]; pinned: boolean; tags: string[] }> {
  const note = await invoke<Record<string, unknown> | null>("db_get_note", { id });
  if (!note) throw new Error(`No note with id ${id}`);
  return {
    title: String(note.title),
    lines: String(note.content ?? "").split("\n"),
    pinned: !!note.pinned,
    tags: (note.tags as string[]) ?? [],
  };
}

async function writeNoteLines(id: string, note: { title: string; lines: string[]; pinned: boolean; tags: string[] }): Promise<void> {
  await invoke("db_save_note", {
    id,
    title: note.title,
    content: note.lines.join("\n"),
    pinned: note.pinned,
    tags: note.tags,
  });
}

function resolveLine(lines: string[], line: number, rawLine: string): number {
  if (lines[line] === rawLine) return line;
  const found = lines.indexOf(rawLine);
  if (found >= 0) return found;
  throw new Error("Task line not found; the note changed since the task list was built");
}

export async function dbSetTaskDone(noteId: string, line: number, rawLine: string, done: boolean): Promise<void> {
  const note = await readNoteLines(noteId);
  const idx = resolveLine(note.lines, line, rawLine);
  const target = done ? "[x]" : "[ ]";
  note.lines[idx] = note.lines[idx].replace(/\[([ xX])\]/, target);
  await writeNoteLines(noteId, note);
}

export async function dbSetTaskPriority(noteId: string, line: number, rawLine: string, priority: string | null): Promise<void> {
  const note = await readNoteLines(noteId);
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

export async function dbSetTaskDue(noteId: string, line: number, rawLine: string, due: string | null): Promise<void> {
  const note = await readNoteLines(noteId);
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
