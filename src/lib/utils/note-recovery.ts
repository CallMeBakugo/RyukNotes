import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { NoteContent, NoteMeta, DbAcceptedState } from '../types';

export function isRecoveryConflict(error: unknown): boolean {
  if (error && typeof error === 'object' && 'kind' in error) {
    return ['changed', 'deleted', 'trashed', 'vault_replaced'].includes(String(error.kind));
  }
  const message = typeof error === 'string' ? error
    : error && typeof error === 'object' && 'message' in error ? String(error.message) : '';
  // Compatibility with the existing filesystem writer only, not arbitrary I/O
  // messages containing the word conflict.
  return /^Save conflict:/i.test(message);
}

export interface RecoveryBlob { reference: string; name: string; mime: string; data: number[] }
export interface RecoveryDraft {
  owner: string; sequence: number; revision: number; capturedAt: string; vault: DbAcceptedState['vault']; noteId: string;
  base: NoteContent & { reviewResult?: { reviewId: string; sourceOwner: string } }; body: string; title: string; tags: string[]; pinned: boolean;
  blobs: RecoveryBlob[]; conflicted: boolean;
}
export interface DraftCapture { revision: number; base: NoteContent; body: string; meta: NoteMeta }
// Retain immutable memory snapshots even if the journal fails or a renderer
// component is unexpectedly unmounted. Never mistake persistence for a note save.
export const recoveryMemory = new Map<string, RecoveryDraft>();
export function createRecoveryOwner(vault: DbAcceptedState['vault'], noteId: string,
  capture: () => DraftCapture, onError: (message: string) => void) {
  const owner = crypto.randomUUID();
  let sequence = 0;
  let conflicted = false;
  let dirty = false;
  let locked = false;
  let stopped = false;
  let latestRevision = -1;
  let durableSequence = 0;
  let journalIdle: ReturnType<typeof setTimeout> | undefined;
  let journalMax: ReturnType<typeof setTimeout> | undefined;
  const cancelJournalTimers = () => { clearTimeout(journalIdle); clearTimeout(journalMax); journalIdle = undefined; journalMax = undefined; };
  let pendingFork: RecoveryDraft | null = null;
  let registered = false;
  const register = async () => {
    if (!registered) { await invoke('recovery_register', { owner, vault, noteId }); registered = true; }
  };
  let tail: Promise<unknown> = register().catch(() => onError('Recovery registration failed. Keep this editor open.'));
  const ordered = <T>(work: () => Promise<T>): Promise<T> => {
    const result = tail.then(work);
    tail = result.catch(() => {});
    return result;
  };
  async function status() {
    await register();
    return invoke('recovery_status', { owner, dirty, conflicted, locked });
  }
  function snapshot(): RecoveryDraft {
    const current = capture();
    return { owner, sequence: ++sequence, revision: current.revision, capturedAt: new Date().toISOString(), vault: { ...vault }, noteId,
      base: structuredClone(current.base), body: current.body, title: current.meta.title, tags: [...current.meta.tags],
      pinned: current.meta.pinned, blobs: [], conflicted };
  }
  async function durable(record: RecoveryDraft): Promise<void> {
    // Resolve object URL bytes into the checkpoint, without changing the live
    // document or attaching uploads to the original note. No checkpoint is
    // acknowledged if fetch/serialization/native fsync fails.
    const urls = new Set(Array.from(record.body.matchAll(/(?:\]\(|(?:src|href)=["']|<)(blob:[^)\s"'<>]+)/g), match => match[1]));
    for (const url of urls) {
      const response = await fetch(url);
      if (!response.ok) throw new Error('A transient attachment could not be preserved.');
      const blob = await response.blob();
      if (blob.size > 25 * 1024 * 1024) throw new Error('Recovery attachment exceeds 25 MiB.');
      const reference = `.rn-recovery/${record.owner}-${record.sequence}-${record.blobs.length}`;
      record.blobs.push({ reference, name: 'recovered-attachment', mime: blob.type || 'application/octet-stream',
        data: Array.from(new Uint8Array(await blob.arrayBuffer())) });
      record.body = record.body.replaceAll(url, reference);
    }
    await invoke('recovery_checkpoint', { draft: record });
    durableSequence = Math.max(durableSequence, record.sequence);
  }
  function checkpoint(): Promise<RecoveryDraft> {
    cancelJournalTimers();
    const record = snapshot();
    latestRevision = record.revision;
    recoveryMemory.set(owner, record);
    return ordered(async () => {
      await status();
      await durable(record);
      return record;
    }).catch(error => {
      onError('Recovery checkpoint failed. Your draft remains in memory; keep this editor open.');
      throw error;
    });
  }
  return {
    owner,
    async ready(): Promise<boolean> {
      try { await ordered(register); return registered; }
      catch { onError('Recovery registration failed. Your draft is still here; retry after the vault operation.'); return false; }
    },
    get unresolved() { return dirty || conflicted; },
    edit() {
      dirty = true;
      pendingFork = null; // Typing invalidates proposals, never their stored copies.
      const record = snapshot();
      latestRevision = record.revision;
      recoveryMemory.set(owner, record); // Memory updates synchronously on every edit.
      void ordered(status).catch(() => onError('Recovery status failed. Keep this editor open.'));
      const flush = () => { void checkpoint().catch(() => {}); };
      clearTimeout(journalIdle);
      journalIdle = setTimeout(flush, 250);
      journalMax ??= setTimeout(flush, 1_000);
    },
    conflict() {
      conflicted = true; dirty = true;
      void checkpoint().catch(() => {});
    },
    checkpoint,
    async saved(revision: number): Promise<void> {
      // A save that acknowledged an older capture cannot resolve newer typing.
      if (revision !== capture().revision || conflicted) return;
      cancelJournalTimers();
      const record = recoveryMemory.get(owner);
      if (!record) { dirty = false; return; }
      await ordered(async () => {
        if (revision !== capture().revision || conflicted) return;
        // The exact note commit already made this captured revision durable.
        // Retire the last journal checkpoint, without refetching object URLs
        // that a successful upload may legitimately have retired. Tombstone
        // I/O failure still retains memory/owner state and refuses disposal.
        await invoke('recovery_resolve', { owner, sequence: durableSequence });
        if (revision === capture().revision) { dirty = false; recoveryMemory.delete(owner); }
      });
    },
    async discard(revision: number): Promise<boolean> {
      await ordered(async () => {});
      if (revision !== capture().revision) return false;
      cancelJournalTimers();
      let record = recoveryMemory.get(owner);
      // A failed checkpoint must not prevent explicit, confirmed discard, but
      // must first establish an ordered tombstone in native recovery storage.
      if (!record || record.revision !== revision) record = await checkpoint();
      await ordered(async () => {
        if (durableSequence < record!.sequence) await durable(record!);
        await invoke('recovery_resolve', { owner, sequence: record!.sequence });
      });
      if (revision !== capture().revision) return false;
      dirty = false; conflicted = false; recoveryMemory.delete(owner); return true;
    },
    async fork(): Promise<{ note: Record<string, unknown>; revision: number; sequence: number }> {
      const revision = capture().revision;
      pendingFork ??= await checkpoint();
      const record = pendingFork;
      if (record.revision !== revision) { pendingFork = null; throw new Error('The draft changed; Keep both again to preserve the new revision.'); }
      const note = await ordered(() => invoke<Record<string, unknown>>('recovery_fork', { owner, sequence: record.sequence }));
      return { note, revision: record.revision, sequence: record.sequence };
    },
    async openReview(): Promise<Record<string, unknown>> {
      const record = await checkpoint();
      return ordered(() => invoke<Record<string, unknown>>('recovery_review_open', { owner, sequence: record.sequence, revision: record.revision }));
    },
    checkpointReview(reviewId: string, sequence: number, proposal: { body: string; title: string; tags: string[]; pinned: boolean }): Promise<void> {
      return ordered(() => invoke('recovery_review_checkpoint', { owner, reviewId, sequence, proposal }));
    },
    reviewAsset(reviewId: string, reference: string): Promise<{ mime: string; data: number[] }> {
      return invoke('recovery_review_asset', { owner, reviewId, reference });
    },
    cancelReview(reviewId: string): Promise<void> { return ordered(() => invoke('recovery_review_cancel', { owner, reviewId })); },
    applyReview(reviewId: string, revision: number, sequence: number): Promise<Record<string, unknown>> {
      return ordered(() => invoke<Record<string, unknown>>('recovery_review_apply', { owner, reviewId, revision, sequence }));
    },
    async pause(): Promise<boolean> {
      await tail;
      if (dirty || conflicted) return false;
      locked = true;
      await status(); return true;
    },
    resume() { locked = false; void ordered(status).catch(() => onError('Recovery owner could not resume. Keep this editor open.')); },
    async release() {
      if (stopped) return;
      stopped = true;
      cancelJournalTimers();
      await tail;
      if (dirty || conflicted) return; // Native registry and memory still own it.
      await invoke('recovery_unregister', { owner });
    },
    watch(reread: () => Promise<void>) {
      let reading = false;
      const hint = async () => {
        if (stopped || reading) return;
        reading = true;
        try {
          await tail;
          await register();
          await invoke('recovery_poll', { owner });
          // Re-read even after a missed/reordered event or a same-connection
          // write (which data_version deliberately does not count).
          if (!stopped) await reread();
        } catch { /* A hint is best effort; save guards and barriers stay authoritative. */ }
        finally { reading = false; }
      };
      const visible = () => { if (document.visibilityState === 'visible') void hint(); };
      const timer = setInterval(() => { if (document.visibilityState === 'visible') void hint(); }, 2_000);
      window.addEventListener('focus', hint);
      window.addEventListener('app-lifecycle-flush', hint);
      document.addEventListener('visibilitychange', visible);
      const listener = listen('note-invalidated', () => void hint());
      void hint();
      return () => {
        clearInterval(timer); window.removeEventListener('focus', hint);
        window.removeEventListener('app-lifecycle-flush', hint);
        document.removeEventListener('visibilitychange', visible);
        void listener.then(stop => stop()).catch(() => {});
      };
    },
    // A failed journal leaves this false and no transition may infer clean.
    get revision() { return latestRevision; },
  };
}
