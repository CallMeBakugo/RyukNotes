<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import type { RecoveryDraft } from '$lib/utils/note-recovery';
  let { notice = '' }: { notice?: string } = $props();
  let drafts = $state<RecoveryDraft[]>([]);
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
    refresh();
    const listener = listen('recovery-available', refresh);
    window.addEventListener('focus', refresh);
    return () => { live = false; window.removeEventListener('focus', refresh); void listener.then(stop => stop()).catch(() => {}); };
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
      await invoke('recovery_resolve', { owner: draft.owner, sequence: draft.sequence });
      await invoke('recovery_unregister', { owner: draft.owner });
      claimed.delete(draft.owner);
      drafts = drafts.filter(d => d.owner !== draft.owner);
      message = discard ? 'Recovered draft discarded.' : 'Recovered draft preserved as a new note. Refresh the note list to open it.';
    } catch { message = 'Recovery could not finish. The draft is preserved. Reopen its originating vault and retry.'; }
    finally { busy = false; }
  }
</script>

{#if drafts.length || notice || message}
  <aside class="recovery-inbox" aria-label="Draft recovery">
    {#if notice}<p role="status">{notice}</p>{/if}
    {#if message}<p role="status">{message}</p>{/if}
    {#each drafts as draft (draft.owner)}
      <div>
        <span>Recovered draft: {draft.title}</span>
        <button disabled={busy} onclick={() => resolve(draft, false)}>Keep both</button>
        <button disabled={busy} onclick={() => resolve(draft, true)}>Discard recovered draft…</button>
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
