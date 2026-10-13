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

const defaultVaultState: VaultState = {
  last_open_note: null,
  sidebar_width: 260,
  notelist_width: 320,
  sidebar_collapsed: false,
  notelist_collapsed: false,
  collapsed_notebooks: [],
};

let vaultState: VaultState = { ...defaultVaultState };

export async function dbLoadVaultState(): Promise<VaultState> {
  return { ...vaultState };
}

export async function dbSaveVaultState(state: VaultState): Promise<void> {
  vaultState = { ...state };
}
