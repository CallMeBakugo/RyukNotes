<script lang="ts">
  import { onMount, tick, untrack } from 'svelte';
  import HistoryReview from './HistoryReview.svelte';
  import SnapshotDocument from './SnapshotDocument.svelte';
  import SourceEditor from './SourceEditor.svelte';
  import { SourceController } from '$lib/editor/source/controller';
  import { editorFocus, restoreEditorFocus, reviewDomIds, handleReviewKey } from '$lib/utils/editor-events';
  import type { ConflictReviewModel, ReviewState, MetadataField, MetadataChoice } from '$lib/utils/conflict-review';
  import type { Side } from '$lib/utils/three-way-merge';

  let { model, ownerId, ownsFocus, focusIntent, onApply, onKeepBoth, renderSnapshot }: {
    model: ConflictReviewModel; ownerId: string; ownsFocus: () => boolean; focusIntent: () => boolean;
    onApply: () => Promise<void>; onKeepBoth: () => Promise<void>; renderSnapshot: (body: string) => string;
  } = $props();
  const id = $props.id();
  const ids = reviewDomIds(id);
  let view = $state.raw<ReviewState>(untrack(() => model.state));
  let root: HTMLElement;
  let query = $state('');
  let searchPosition = -1;
  let tab = $state<'changes' | 'base'>('changes');
  const source: SourceController = new SourceController((changes, edited) => {
    if (!edited) return;
    const edits: { from: number; to: number; insert: string }[] = [];
    changes.iterChangedRanges((from, to, nextFrom, nextTo) => edits.push({ from, to, insert: source.state.doc.sliceString(nextFrom, nextTo) }));
    model.edit(edits);
  });
  $effect(() => model.subscribe(state => { view = state; }));
  $effect(() => {
    const display = view.result.replace(/\r\n|\r/g, '\n');
    if (display !== source.state.doc.toString()) source.load(display);
  });
  const complete = $derived(!!view.plan && view.plan.hunks.every(h => h.kind !== 'overlapping' || !!view.choices[h.id]) && view.metadata.every(m => m.choice !== null));
  const canApply = $derived.by(() => { if (view.status !== 'ready' || !view.built) return false; return model.canApply; });
  let previewUrls = $state.raw<ReadonlyMap<string, string>>(new Map());
  let assetGeneration = 0;
  const snapshotReview = $derived(view.reviewed);
  $effect(() => {
    const review = snapshotReview, generation = ++assetGeneration;
    const urls: string[] = [];
    previewUrls = new Map();
    if (review?.reviewId) void Promise.all((review.previewReferences ?? []).map(async reference => {
      try {
        const image = await model.preview(reference);
        const url = URL.createObjectURL(new Blob([new Uint8Array(image.data)], { type: image.mime }));
        if (generation !== assetGeneration) { URL.revokeObjectURL(url); return null; }
        urls.push(url); return [reference, url] as const;
      } catch { return null; }
    })).then(values => {
      if (generation === assetGeneration) previewUrls = new Map(values.filter((value): value is readonly [string, string] => value !== null));
    });
    return () => { assetGeneration++; urls.forEach(url => URL.revokeObjectURL(url)); };
  });
  function renderOwned(body: string): string {
    let display = body;
    for (const [reference, url] of previewUrls) display = display.replaceAll(reference, url);
    return renderSnapshot(display); // Display only: B/L/R and merge mappings never change.
  }
  const ownedRenderer = $derived.by(() => { const captured = previewUrls; return (body: string) => {
    let display = body; for (const [reference, url] of captured) display = display.replaceAll(reference, url); return renderSnapshot(display);
  }; });
  let alive = true;
  onMount(() => {
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    void tick().then(() => { if (alive && focusIntent() && ownsFocus()) restoreEditorFocus(focusIntent, () => root?.focus()); });
    return () => {
      alive = false;
      // Never restore into a different pane/window after a newer focus intent.
      if (previous?.isConnected && ownsFocus() && root?.contains(document.activeElement)) {
        const current = editorFocus.capture();
        void tick().then(() => { if (current() && ownsFocus()) restoreEditorFocus(current, () => previous.focus()); });
      }
    };
  });
  function keydown(event: KeyboardEvent) {
    handleReviewKey(event, root.contains(event.target as Node), ownsFocus(), {
      advance, find: () => document.getElementById(ids.search)?.focus(),
      close: () => { void model.close(); }, canClose: () => view.status !== 'applying',
    });
  }
  function advance(reverse = false) {
    if (!query || !ownsFocus()) return;
    const text = source.state.doc.toString();
    const matches: number[] = [];
    for (let from = text.indexOf(query); from >= 0; from = text.indexOf(query, from + Math.max(1, query.length))) matches.push(from);
    if (!matches.length) return;
    searchPosition = (searchPosition + (reverse ? -1 : 1) + matches.length) % matches.length;
    source.select(matches[searchPosition], matches[searchPosition] + query.length, true); source.focus();
  }
  function choose(event: Event, hunk: string) { model.choose(hunk, (event.target as HTMLSelectElement).value as Side); }
  function metadata(event: Event, field: MetadataField) { model.chooseMetadata(field, (event.target as HTMLSelectElement).value as MetadataChoice); }
  function build() {
    if (!ownsFocus()) return;
    if (view.manual && !window.confirm('Replace your edited result with a result rebuilt from the selected sides?')) return;
    model.build();
  }
  const renderedBase = $derived.by(() => {
    const reviewed = snapshotReview;
    if (tab !== 'base' || !reviewed?.base) return '';
    try { return renderOwned(reviewed.base.content); } catch { return ''; }
  });
</script>

<!-- This dialog belongs to one editor, not a global active-note surface. -->
<div class="conflict-review" role="dialog" aria-modal="false" aria-labelledby={ids.title}
  data-editor-overlay={ownerId} bind:this={root} tabindex="-1" onkeydown={keydown}>
  <header>
    <h2 id={ids.title}>Review changes</h2>
    <button onclick={() => void model.close()} disabled={view.status === 'applying'}>Cancel review</button>
  </header>
  <p class="notice">Inspection and result edits do not change the live note. Keep both remains available.</p>
  {#if view.message}<p class="notice" role="status">{view.message}</p>{/if}
  {#if view.status === 'loading'}<p role="status">Capturing verified base and saved version…</p>{/if}
  {#if !view.reviewed?.base && view.reviewed?.local}
    <details><summary>Your draft — exact source</summary><pre>{view.reviewed.local.content}</pre></details>
    {#if view.reviewed.remote}<details><summary>Saved version — exact source</summary><pre>{view.reviewed.remote.content}</pre></details>{/if}
  {/if}
  {#if !view.plan && view.manual}<details><summary>Preserved result — exact source</summary><pre>{view.result}</pre></details>{/if}
  {#if view.reviewed?.base && view.reviewed.local && view.reviewed.remote}
    <nav aria-label="Immutable review snapshots">
      <button aria-pressed={tab === 'changes'} onclick={() => tab = 'changes'}>Your draft / Saved version</button>
      <button aria-pressed={tab === 'base'} onclick={() => tab = 'base'}>Common base</button>
    </nav>
    <div class="comparison">
      {#if tab === 'base'}
        <details><summary>Exact Common base Markdown</summary><pre>{view.reviewed.base.content}</pre></details>
        <SnapshotDocument html={renderedBase} label="Common base, read-only" />
      {:else}
        <div class="comparison-columns">
          <HistoryReview mode="conflict" beforeLabel="Common base" afterLabel="Your draft"
            raw={view.reviewed.local.content} current={view.reviewed.base.content} selected="Your draft"
            loading={false} error={null} canRestore={false} onRetry={() => {}} onRestore={() => {}} renderSnapshot={ownedRenderer} />
          <HistoryReview mode="conflict" beforeLabel="Common base" afterLabel="Saved version"
            raw={view.reviewed.remote.content} current={view.reviewed.base.content} selected="Saved version"
            loading={false} error={null} canRestore={false} onRetry={() => {}} onRestore={() => {}} renderSnapshot={ownedRenderer} />
        </div>
      {/if}
    </div>
    <p class="notice">Display previews may normalize whitespace. Exact source and byte mappings determine the result.</p>
    <details><summary>Exact immutable Markdown sources</summary>
      <h3>Your draft</h3><pre>{view.reviewed.local.content}</pre>
      <h3>Saved version</h3><pre>{view.reviewed.remote.content}</pre>
      <h3>Common base</h3><pre>{view.reviewed.base.content}</pre>
    </details>
    {#if view.plan}
    <div class="choices">
      {#each view.plan.hunks as hunk (hunk.id)}
        <details>
          <summary>{hunk.kind} — base lines {hunk.base.lineFrom + 1}–{hunk.base.lineTo}, bytes {hunk.base.byteFrom}–{hunk.base.byteTo}</summary>
          <p>{hunk.reasons.join(', ')}</p>
          <pre aria-label="Exact Common base hunk">{hunk.baseText}</pre>
          <pre aria-label="Exact Your draft hunk">{hunk.localText}</pre>
          <pre aria-label="Exact Saved version hunk">{hunk.remoteText}</pre>
          {#if hunk.kind === 'overlapping'}
            <label for={`${id}-${hunk.id}`}>Explicit overlap choice</label>
            <select id={`${id}-${hunk.id}`} value={view.choices[hunk.id] ?? ''} disabled={view.status === 'applying' || view.status === 'loading'} onchange={event => choose(event, hunk.id)}>
              <option value="" disabled>Choose a side…</option><option value="local">Your draft</option><option value="remote">Saved version</option><option value="base">Common base</option>
            </select>
          {/if}
        </details>
      {/each}
      {#each view.metadata as field (field.field)}
        <label for={`${id}-meta-${field.field}`}>{field.field}{field.conflict ? ' — conflicting changes' : ''}</label>
        <select id={`${id}-meta-${field.field}`} value={field.choice ?? ''} disabled={view.status === 'applying' || view.status === 'loading'} onchange={event => metadata(event, field.field)}>
          <option value="" disabled>Choose metadata…</option>
          <option value="local">Your draft: {JSON.stringify(view.reviewed.local.meta[field.field])}</option>
          <option value="remote">Saved version: {JSON.stringify(view.reviewed.remote.meta[field.field])}</option>
          <option value="base">Common base: {JSON.stringify(view.reviewed.base.meta[field.field])}</option>
          {#if field.field === 'tags'}<option value="union">Keep tags from either side (removed tags may return)</option>{/if}
        </select>
      {/each}
      <button disabled={!complete || view.status !== 'ready'} onclick={build}>Build chosen result</button>
      {#if view.manual}<button disabled={!complete || view.status !== 'ready'} onclick={() => model.build(true)}>Use this edited result</button>{/if}
    </div>
    <h3 id={ids.result}>Result artifact</h3>
    <p class="notice">Existing line endings remain exact. New lines use {view.result.match(/\r\n|\r|\n/)?.[0] === '\r\n' ? 'CRLF' : view.result.match(/\r\n|\r|\n/)?.[0] === '\r' ? 'CR' : 'LF'}. Manual edits stay in this result until you apply.</p>
    <label for={ids.search}>Find in result</label><input id={ids.search} bind:value={query} oninput={() => searchPosition = -1} />
    <div class="result-source" aria-labelledby={ids.result}><SourceEditor controller={source} readonly={view.status === 'applying' || view.status === 'loading'} numbers /></div>
    {#if view.journalError}<button onclick={() => void model.preserve()}>Retry result preservation</button>{/if}
    {/if}
  {/if}
  <footer>
    <button disabled={view.status === 'applying'} onclick={() => void onKeepBoth()}>Keep both</button>
    {#if view.status !== 'loading' && view.status !== 'applying'}<button onclick={() => void model.open(true)}>Renew review</button>{/if}
    <button disabled={!canApply} onclick={() => void onApply()}>{view.status === 'applying' ? 'Applying…' : 'Apply reviewed result'}</button>
  </footer>
</div>

<style>
  .conflict-review { position: absolute; inset: 0; z-index: 60; display: flex; flex-direction: column; gap: 8px; overflow: auto; padding: 12px; background: var(--bg-primary); color: var(--text-primary); border: 1px solid var(--border-color); outline-offset: -3px; }
  header, nav, footer { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  header h2 { flex: 1; font-size: 16px; margin: 0; }
  h3 { font-size: 14px; margin: 4px 0; }
  button, select, input { color: var(--text-primary); background: var(--bg-secondary); border: 1px solid var(--border-color); border-radius: 5px; padding: 6px 8px; }
  button { cursor: pointer; } button:disabled { opacity: .5; cursor: default; }
  .notice, label { font-size: 12px; margin: 0; color: var(--text-secondary); }
  .comparison { min-height: 240px; height: 30vh; flex-shrink: 0; overflow: auto; display: flex; flex-direction: column; border: 1px solid var(--border-color); }
  .comparison-columns { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); min-height: 0; flex: 1; }
  @media (max-width: 700px) { .comparison-columns { grid-template-columns: 1fr; overflow: auto; } }
  .choices { display: flex; flex-direction: column; gap: 6px; }
  details { border: 1px solid var(--border-color); padding: 6px; font-size: 12px; }
  summary { cursor: pointer; }
  pre { white-space: pre-wrap; overflow-wrap: anywhere; max-height: 160px; overflow: auto; }
  .result-source { height: 35vh; min-height: 220px; flex-shrink: 0; overflow: auto; border: 1px solid var(--border-color); }
  footer { position: sticky; bottom: 0; padding: 8px 0; background: var(--bg-primary); }
</style>
