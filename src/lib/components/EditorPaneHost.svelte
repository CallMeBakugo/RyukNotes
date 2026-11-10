<script lang="ts">
  import { onDestroy, untrack } from 'svelte';
  import Editor from './Editor.svelte';
  import { provideEditorPane, type EditorPane } from '$lib/stores/editor-pane';

  let { pane, paneId, onEditorChange, onMoveToTrash, onShowTag, showTaskPlaceholder = false }: {
    pane: EditorPane;
    paneId: string;
    showTaskPlaceholder?: boolean;
    onEditorChange: (editor: Editor | undefined) => void;
    onMoveToTrash: (path: string) => Promise<boolean>;
    onShowTag: (tag: string) => void;
  } = $props();
  // Keyed by pane ID in the parent: this context never follows active-pane focus.
  const context = provideEditorPane(untrack(() => pane));
  const activeNotePath = context.activeNotePath;
  let editor = $state<Editor>();
  $effect(() => { const instance = editor; untrack(() => onEditorChange(instance)); });
  onDestroy(() => onEditorChange(undefined));
</script>

<div class="pane-surface" data-pane-id={paneId} data-note-path={$activeNotePath ?? undefined}>
  <Editor bind:this={editor} {onMoveToTrash} {onShowTag} />
  {#if showTaskPlaceholder}
    <div class="tasks-editor-placeholder">
      <svg width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true"><path d="M9 11l3 3L22 4"/><path d="M21 12v7a2 2 0 01-2 2H5a2 2 0 01-2-2V5a2 2 0 012-2h11"/></svg>
      <p>Select a task to open its note</p>
    </div>
  {/if}
</div>

<style>
  .pane-surface { flex: 1; min-height: 0; min-width: 0; overflow: hidden; position: relative; }
  .tasks-editor-placeholder { position: absolute; inset: 0; z-index: 5; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 12px; background: var(--bg-primary); color: var(--text-tertiary); font-size: 14px; }
</style>
