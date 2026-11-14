<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { open, save } from '@tauri-apps/plugin-dialog';
  import { dbRestoreAncestryExport, dbReconcileAncestryExport } from '$lib/api-db';
  import { createPortableRecovery, type RecoveryMode, type RecoveryRequest } from '$lib/utils/portable-recovery';
  let { vault = null, begin = async () => async () => {}, onOpenDestination }: {
    vault?: string | null; begin?: () => Promise<null | (() => Promise<void>)>; onOpenDestination?: (path:string) => Promise<void>;
  } = $props();
  let mode = $state<RecoveryMode>('full_restore');
  let busy = $state(false);
  let pending = $state<RecoveryRequest | null>(null);
  let message = $state('');
  let resultPath = $state('');
  const recovery = createPortableRecovery({ begin: () => begin(), restore: async (request: RecoveryRequest) => {
    if (request.mode === 'reconcile') return dbReconcileAncestryExport(request.source, request.destination, request.operationId);
    if (request.mode === 'raw_recovery') return invoke('db_recover_vault_copy', { source: request.source, destination: request.destination, operationId: request.operationId });
    return dbRestoreAncestryExport(request.source, request.destination, null, request.mode, request.operationId);
  } });
  async function run(again = false) {
    if (busy) return;
    busy = true; message = ''; resultPath = '';
    try {
      let input: Omit<RecoveryRequest,'operationId'> | undefined;
      if (!again) {
        const chosenMode = mode, currentVault = vault;
        const source = await open({ directory: chosenMode !== 'raw_recovery', multiple: false, title: chosenMode === 'raw_recovery' ? 'Choose a moved vault or backup (.rn)' : 'Choose an ancestry export folder' });
        if (typeof source !== 'string') return;
        const destination = chosenMode === 'reconcile' ? currentVault : await save({ title: 'Choose a NEW destination vault', filters: [{ name: 'RyukNotes vault', extensions: ['rn'] }] });
        if (!destination) return;
        if (chosenMode !== 'reconcile' && !destination.endsWith('.rn')) throw new Error('Choose a destination filename ending in .rn.');
        if (chosenMode === 'reconcile' && currentVault !== vault) throw new Error('The displayed vault changed. Choose it again.');
        if (!window.confirm(chosenMode === 'reconcile'
          ? 'Add verified ancestry to the displayed vault? Its current notes will stay unchanged; incoming heads remain separate.'
          : 'Create a separate recovered vault? The source stays unchanged. Existing drafts and reviews must be recovered separately; they will not be rebound.')) return;
        input = { mode: chosenMode, source, destination };
      }
      const { request } = await recovery.run(input);
      message = request.mode === 'reconcile' ? 'Ancestry added. Current notes were preserved; incoming heads require review.' : 'Recovery completed with a fresh storage epoch. Open the destination vault and review recovered notes before editing. Existing recovery drafts remain separate.';
      if (request.mode !== 'reconcile') resultPath = request.destination;
    } catch (error) { message = String(error); }
    finally { pending = recovery.pending ? { ...recovery.pending } : null; busy = false; }
  }
</script>
<details class="portable-recovery">
  <summary>Restore or recover a vault</summary>
  <p>Restore an ancestry export, make an independent copy, or recover a moved vault after ledger loss. Destinations must be empty. Existing vaults are never overwritten.</p>
  <label>Recovery mode
    <select bind:value={mode} disabled={busy}>
      <option value="full_restore">Restore export — preserve vault identity</option>
      <option value="independent_copy">Independent copy — new vault identity</option>
      <option value="raw_recovery">Recover moved vault or raw backup</option>
      {#if vault}<option value="reconcile">Add ancestry to this vault — keep current notes</option>{/if}
    </select>
  </label>
  <p>If the original vault still exists at its enrolled location, use an independent copy of its export. Recovery never resets its ledger silently.</p>
  <button disabled={busy} onclick={() => run()}>{busy ? 'Working…' : 'Choose source and destination…'}</button>
  {#if pending}<p>Pending {pending.mode}: {pending.source} → {pending.destination}</p><button disabled={busy} onclick={() => run(true)}>Retry the same recovery</button>{/if}
  {#if message}<p role="status">{message}</p>{/if}
  {#if resultPath}<p>Destination: {resultPath}</p>{#if onOpenDestination}<button disabled={busy} onclick={() => onOpenDestination?.(resultPath)}>Open recovered vault</button>{/if}{/if}
</details>
<style>
  .portable-recovery { margin: 16px 0; padding: 12px; border: 1px solid var(--border-color); border-radius: 8px; text-align: left; }
  summary { cursor: pointer; font-weight: 600; }
  p { font-size: 13px; color: var(--text-secondary); overflow-wrap: anywhere; margin: 10px 0; }
  label { display: grid; gap: 6px; font-size: 13px; }
  select, button { color: var(--text-primary); background: var(--bg-secondary); border: 1px solid var(--border-color); border-radius: 5px; padding: 8px; max-width: 100%; }
  button { margin: 10px 6px 0 0; cursor: pointer; }
</style>
