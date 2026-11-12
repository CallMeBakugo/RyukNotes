<script lang="ts">
  import { onMount } from 'svelte';
  import { vaultOpenStatus } from '$lib/utils/vault-open-status';
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import type { RecoveryDraft } from '$lib/utils/note-recovery';
  let { notice = '' }: { notice?: string } = $props();
  let drafts = $state<RecoveryDraft[]>([]);
  type PinGroup = { owner: string; pins: { pinId: string; revisionId: string }[] };
  let orphanPins = $state<PinGroup[]>([]);
  let message = $state('');
  let busy = $state(false);
  let claimed = new Set<string>();
  onMount(() => {
    let live = true;
    const refresh = () => {
      void invoke<RecoveryDraft[]>('recovery_list').then(result => {
        if (live) drafts = [...drafts.filter(d => claimed.has(d.owner)), ...result];
      }).catch(() => { if (live) message = 'Recovery storage could not be checked. Keep any unsaved editors open.'; });
    };
    const refreshPins = () => { void invoke<PinGroup[]>('recovery_orphan_pins').then(result => { if (live) orphanPins = result; }).catch(() => { if (live) message = 'Recovery holds could not be checked; no holds were released.'; }); };
    const refreshAll = () => { refresh(); refreshPins(); };
    refreshAll();
    const listener = listen('recovery-available', refreshAll);
    window.addEventListener('focus', refreshAll);
    return () => { live = false; window.removeEventListener('focus', refreshAll); void listener.then(stop => stop()).catch(() => {}); };
  });

  async function resolve(draft: RecoveryDraft, discard: boolean) {
    if (busy) return;
    if (discard && !window.confirm('Permanently discard this recovered draft? The saved note will remain unchanged.')) return;
    busy = true;
    try {
      if (!claimed.has(draft.owner)) {
        draft = await invoke<RecoveryDraft>('recovery_claim', { owner: draft.owner });
        claimed.add(draft.owner);
      }
      if (!discard) await invoke('recovery_fork', { owner: draft.owner, sequence: draft.sequence });
      if (draft.base.recoveryIdentity) await invoke('recovery_resolve_revision', { owner: draft.owner, sequence: draft.sequence,
        localRevision: draft.revision, identity: draft.base.recoveryIdentity, action: 'discard', nextAccepted: null });
      else await invoke('recovery_resolve', { owner: draft.owner, sequence: draft.sequence });
      await invoke('recovery_unregister', { owner: draft.owner });
      claimed.delete(draft.owner);
      drafts = drafts.filter(d => d.owner !== draft.owner);
      message = discard ? 'Recovered draft discarded.' : 'Recovered draft preserved as a new note. Refresh the note list to open it.';
    } catch { message = 'Recovery could not finish. The draft is preserved. Reopen its originating vault and retry.'; }
    finally { busy = false; }
  }
  async function cleanup(group: PinGroup) {
    if (busy || !window.confirm('Release these orphaned recovery holds? No local draft was found. Old revision data may then be pruned; saved notes stay unchanged.')) return;
    busy = true;
    try { await invoke('recovery_cleanup_pins', { group }); orphanPins = orphanPins.filter(p => p.owner !== group.owner); message = 'Orphaned recovery holds released by explicit acknowledgment.'; }
    catch { message = 'Recovery holds changed or remain active. No cleanup was acknowledged; refresh and retry.'; }
    finally { busy = false; }
  }
</script>

{#if drafts.length || orphanPins.length || notice || message || $vaultOpenStatus}
  <aside class="recovery-inbox" aria-label="Draft recovery">
    {#if $vaultOpenStatus}<p role="status">{$vaultOpenStatus}</p>{/if}
    {#if notice}<p role="status">{notice}</p>{/if}
    {#if message}<p role="status">{message}</p>{/if}
    {#each drafts as draft (draft.owner)}
      <div>
        <span>{draft.base.reviewResult ? 'Review result' : 'Recovered draft'}: {draft.title}</span>
        <button disabled={busy} onclick={() => resolve(draft, false)}>Keep both</button>
        <button disabled={busy} onclick={() => resolve(draft, true)}>Discard recovered draft…</button>
      </div>
    {/each}
    {#each orphanPins as group (group.owner)}
      <div><span>Orphaned recovery holds: {group.pins.length} revision(s). No local journal was found.</span>
        <button disabled={busy} onclick={() => cleanup(group)}>Release holds…</button>
      </div>
    {/each}
  </aside>
{/if}

<style>
  .recovery-inbox { position: fixed; bottom: 12px; left: 16px; right: 16px; z-index: 80; max-height: 35vh; overflow: auto; padding: 10px 14px; border: 1px solid var(--border-color); border-radius: 8px; background: var(--bg-secondary); color: var(--text-primary); font-size: 13px; }
  p { margin: 4px 0; }
  .recovery-inbox div { display: flex; gap: 8px; align-items: center; padding: 4px 0; }
  span { flex: 1; }
  button { background: var(--bg-hover); color: var(--text-primary); border: 1px solid var(--border-color); border-radius: 5px; padding: 4px 8px; cursor: pointer; }
</style>
