<script lang="ts">
  import { operationMessage, dismissOperation } from '$lib/stores/operation-feedback';
  $effect(() => {
    const current = $operationMessage;
    if (current?.kind !== 'success') return;
    const timer = window.setTimeout(() => dismissOperation(current.id), 5000);
    return () => window.clearTimeout(timer);
  });
</script>
{#if $operationMessage}
  <div class="operation-feedback" role={$operationMessage.kind === 'error' ? 'alert' : 'status'}>
    <span>{$operationMessage.text}</span>
    <button onclick={() => { if ($operationMessage) dismissOperation($operationMessage.id); }}>Dismiss</button>
  </div>
{/if}
<style>
  .operation-feedback { position: fixed; top: 48px; left: 16px; right: 16px; z-index: 110; display: flex; gap: 12px; align-items: center; padding: 12px; border: 1px solid var(--border-color); border-radius: 8px; background: var(--bg-secondary); color: var(--text-primary); box-shadow: 0 4px 16px #0003; }
  span { flex: 1; overflow-wrap: anywhere; }
  button { color: var(--text-primary); background: var(--bg-hover); border: 1px solid var(--border-color); border-radius: 4px; padding: 6px 10px; cursor: pointer; }
</style>
