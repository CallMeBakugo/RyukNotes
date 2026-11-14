<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import type { PaneRelease } from '$lib/stores/editor-workspace';
  let { vault, begin }: { vault: string; begin: () => Promise<PaneRelease | null> } = $props();
  let stats = $state<Record<string,number> | null>(null);
  let message = $state('');
  let busy = $state(false);
  let generation = 0;
  const rows = [
    ['liveBodyBytes','Live note bodies'], ['trashedBodyBytes','Trashed note bodies'],
    ['attachmentBytes','Attachment bytes'], ['curatedBodyBytes','Curated history bodies'],
    ['revisionPayloadBytes','Retained revision payload (body, title, tags, extras)'],
    ['databaseFileBytes','Database file on disk'], ['walFileBytes','WAL file on disk'],
    ['databasePagesBytes','SQLite logical pages'], ['reusablePageBytes','Reusable space within SQLite'],
    ['privateRecoveryBytesAllVaults','Private recovery files (all vaults, outside database)'],
  ];
  function format(n: number) { return n < 1024 ? `${n} B` : n < 1048576 ? `${(n/1024).toFixed(1)} KiB` : `${(n/1048576).toFixed(1)} MiB`; }
  $effect(() => {
    const path = vault, request = ++generation;
    stats = null; message = '';
    void invoke<Record<string,number>>('db_storage_diagnostics', { vaultPath:path }).then(value => {
      if (generation === request) stats = value;
    }).catch(error => { if (generation === request) message = `Storage inspection failed: ${String(error)}`; });
    return () => { generation++; };
  });
  async function collect() {
    if (busy || !window.confirm('Prune eligible revision payloads and unowned attachment bytes? Only revisions older than 90 days AND outside the newest 100 are eligible. Heads and pinned drafts/reviews are protected. Pruned payloads cannot be recovered from this vault. No VACUUM or private-journal deletion will run.')) return;
    const path = vault, request = generation;
    busy = true; message = '';
    let release: PaneRelease | null = null;
    try {
      release = await begin();
      if (!release) throw new Error('Save or resolve drafts in every window first.');
      if (generation !== request || path !== vault) return;
      const report = await invoke<{statesPruned:number;blobsRemoved:number}>('db_collect_revisions',{ vaultPath:path });
      if (generation !== request) return;
      message = `Pruned ${report.statesPruned} revision payloads and ${report.blobsRemoved} unowned attachments. Headers and protected state remain.`;
      const value = await invoke<Record<string,number>>('db_storage_diagnostics',{vaultPath:path});
      if (generation === request) stats = value;
    } catch (error) { if (generation === request) message = `Maintenance failed: ${String(error)}`; }
    finally { try { await release?.(); } finally { busy = false; } }
  }
</script>
<section aria-label="Database storage and retention">
  <h4>Storage and revision retention</h4>
  {#if stats}
    <dl>{#each rows as [key,label]}{#if stats[key] !== undefined}<div><dt>{label}</dt><dd>{format(stats[key])}</dd></div>{/if}{/each}</dl>
    <p>{stats.headers ?? 0} revision headers · {stats.states ?? 0} retained states · {stats.pins ?? 0} pins · {stats.prunedStates ?? 0} pruned states</p>
  {/if}
  <p>Payload sizes overlap with database allocation; do not add them together. File sizes are observed separately and can change during writes. Cleanup frees reusable SQLite space and may not shrink the file.</p>
  <p>Retention keeps headers forever, payloads for at least 90 days and the newest 100 per incarnation, plus heads and recovery/review pins. Curated history settings do not change these rules. Cleanup runs only when requested.</p>
  <button disabled={busy} onclick={collect}>{busy ? 'Maintaining…' : 'Prune eligible revision payloads…'}</button>
  {#if message}<p role="status">{message}</p>{/if}
</section>
<style>
  section { margin: 16px 0; font-size: 13px; }
  p { color: var(--text-secondary); line-height: 1.5; margin: 10px 0; }
  dl div { display:flex; justify-content: space-between; gap:16px; padding:4px 0; }
  dd { margin:0; white-space:nowrap; }
  button { color:var(--text-primary); background:var(--bg-secondary); border:1px solid var(--border-color); border-radius:6px; padding:8px; cursor:pointer; }
</style>
