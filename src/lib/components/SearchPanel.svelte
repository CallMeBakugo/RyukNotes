<script lang="ts">
	import { tick } from 'svelte';
	import { showSearch, appConfig, mobileView, notebooks, activeNotebook, activeTag, viewMode } from '$lib/stores/app';
	import { searchNotes } from '$lib/api';
	import { relativePath } from '$lib/utils/paths';
	import type { SearchResult, SearchMatch, NotebookEntry } from '$lib/types';
	import { isMobile } from '$lib/platform';

	let { onSelectNote }: { onSelectNote: (path: string) => Promise<boolean> } = $props();

	const PAGE_SIZE = 20;
	const currentVault = $derived($appConfig?.active_vault ?? null);
	let query = $state('');
	let results = $state.raw<SearchResult[]>([]);
	let total = $state(0);
	let offset = $state(0);
	let loading = $state(false);
	let opening = $state(false);
	let error = $state('');
	let composing = $state(false);
	let selectedIndex = $state(0);
	let inputEl = $state<HTMLInputElement>(null!);
	let resultsEl = $state<HTMLDivElement>(null!);
	let requestGeneration = 0;
	let searchTimer: ReturnType<typeof setTimeout> | null = null;
	const rows = $derived(results.map(result => ({
		result,
		title: textParts(result.title, result.title_matches),
		snippet: textParts(result.snippet, result.snippet_matches),
	})));

	function cancelSearch() {
		requestGeneration++;
		if (searchTimer !== null) clearTimeout(searchTimer);
		searchTimer = null;
	}

	function isCurrent(generation: number, vault: string | null) {
		return generation === requestGeneration && $showSearch && vault === currentVault;
	}

	async function runSearch(q: string, pageOffset: number, generation: number, vault: string | null) {
		searchTimer = null;
		if (!isCurrent(generation, vault)) return;
		try {
			const page = await searchNotes(q, PAGE_SIZE, pageOffset);
			if (!isCurrent(generation, vault)) return;
			results = page.results;
			total = page.total;
			selectedIndex = 0;
			await tick();
			if (isCurrent(generation, vault) && resultsEl) resultsEl.scrollTop = 0;
		} catch (cause) {
			if (!isCurrent(generation, vault)) return;
			error = cause instanceof Error ? cause.message : String(cause);
		} finally {
			if (isCurrent(generation, vault)) loading = false;
		}
	}

	function startSearch(value: string, pageOffset = 0, immediate = false) {
		cancelSearch();
		query = value;
		results = [];
		selectedIndex = 0;
		error = '';
		opening = false;
		offset = pageOffset;
		if (pageOffset === 0) total = 0;
		const q = value.trim();
		loading = !!q && !composing;
		if (!loading) return;
		const generation = requestGeneration;
		const vault = currentVault;
		if (immediate) void runSearch(q, pageOffset, generation, vault);
		else searchTimer = setTimeout(() => void runSearch(q, pageOffset, generation, vault), 150);
	}

	$effect(() => {
		const open = $showSearch;
		const vault = currentVault;
		const input = inputEl;
		cancelSearch();
		query = '';
		results = [];
		total = 0;
		offset = 0;
		selectedIndex = 0;
		loading = false;
		opening = false;
		composing = false;
		error = '';
		if (open && input) {
			const generation = requestGeneration;
			void tick().then(() => { if (isCurrent(generation, vault)) input.focus(); });
		}
		return cancelSearch;
	});

	function handleInput(event: Event) {
		if ((event as InputEvent).isComposing) composing = true;
		startSearch((event.currentTarget as HTMLInputElement).value);
	}

	function scrollToSelected() {
		void tick().then(() => {
			const item = resultsEl?.children[selectedIndex] as HTMLElement | undefined;
			item?.scrollIntoView({ block: 'nearest' });
		});
	}

	function handleEscape(e: KeyboardEvent) {
		if (e.key !== 'Escape' || e.isComposing) return;
		e.preventDefault();
		e.stopPropagation();
		close();
	}

	function handleKeydown(e: KeyboardEvent) {
		e.stopPropagation();
		if (e.isComposing || composing || loading || opening) return;
		if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
			e.preventDefault();
			if (!results.length) return;
			selectedIndex = Math.max(0, Math.min(results.length - 1, selectedIndex + (e.key === 'ArrowDown' ? 1 : -1)));
			inputEl?.focus({ preventScroll: true });
			scrollToSelected();
		} else if (e.key === 'Enter' && e.target === inputEl && results[selectedIndex]) {
			e.preventDefault();
			void openResult(results[selectedIndex]);
		}
	}

	function getNotebook(path: string): string {
		if (!currentVault) return '';
		const rel = relativePath(currentVault, path);
		const lastSlash = rel.lastIndexOf('/');
		return lastSlash > 0 ? rel.substring(0, lastSlash) : '';
	}

	function textParts(text: string, matches: SearchMatch[]) {
		const parts: { text: string; matched: boolean }[] = [];
		let cursor = 0;
		for (const { start, end } of matches) {
			if (start > cursor) parts.push({ text: text.slice(cursor, start), matched: false });
			parts.push({ text: text.slice(start, end), matched: true });
			cursor = end;
		}
		if (cursor < text.length) parts.push({ text: text.slice(cursor), matched: false });
		return parts;
	}

	function findNotebookByPath(nbs: NotebookEntry[], target: string): NotebookEntry | null {
		for (const nb of nbs) {
			if (relativePath(nb.path, target) === '.') return nb;
			if (nb.children.length) {
				const found = findNotebookByPath(nb.children, target);
				if (found) return found;
			}
		}
		return null;
	}

	async function openResult(result: SearchResult) {
		if (loading || opening || !$showSearch || !results.includes(result)) return;
		const generation = requestGeneration;
		const vault = currentVault;
		opening = true;
		error = '';
		try {
			const opened = await onSelectNote(result.path);
			if (!isCurrent(generation, vault)) return;
			if (!opened) {
				error = 'Could not open this note.';
				return;
			}
			close();
			// Reveal the note without applying a late result to another search/vault.
			const sep = result.path.includes('\\') ? '\\' : '/';
			const parentDir = result.path.slice(0, result.path.lastIndexOf(sep));
			const nb = parentDir ? findNotebookByPath($notebooks, parentDir) : null;
			$activeNotebook = nb;
			$activeTag = null;
			$viewMode = nb ? 'notebook' : 'all';
			if (isMobile) $mobileView = 'editor';
		} catch (cause) {
			if (isCurrent(generation, vault)) error = cause instanceof Error ? cause.message : String(cause);
		} finally {
			if (isCurrent(generation, vault)) opening = false;
		}
	}

	function previousPage() {
		const lastPage = Math.max(0, Math.floor((total - 1) / PAGE_SIZE) * PAGE_SIZE);
		startSearch(query, Math.max(0, Math.min(offset - PAGE_SIZE, lastPage)), true);
	}

	function close() {
		cancelSearch();
		$showSearch = false;
	}
</script>

{#if $showSearch}
	<div class="search-overlay" role="presentation" onclick={close} onkeydowncapture={handleEscape}>
		<div class="search-panel" role="dialog" aria-modal="true" aria-label="Search notes" tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={handleKeydown}>
			<div class="search-input-wrapper">
				<svg class="search-icon" width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
					<circle cx="11" cy="11" r="8" />
					<line x1="21" y1="21" x2="16.65" y2="16.65" />
				</svg>
				<input
					bind:this={inputEl}
					type="text"
					placeholder="Search notes..."
					role="combobox"
					aria-label="Search notes"
					aria-controls="global-search-results"
					aria-expanded={results.length > 0}
					aria-autocomplete="list"
					aria-activedescendant={results.length ? `global-search-result-${selectedIndex}` : undefined}
					bind:value={query}
					oninput={handleInput}
					oncompositionstart={() => { composing = true; cancelSearch(); results = []; total = 0; error = ''; loading = false; }}
					oncompositionend={(e) => { composing = false; startSearch(e.currentTarget.value); }}
				/>
				{#if !isMobile}<kbd class="search-esc">Esc</kbd>{/if}
			</div>

			{#if error}<div class="search-error" role="alert">{error}</div>{/if}
			{#if composing}
				<div class="search-empty">Type to search across all notes</div>
			{:else if loading}
				<div class="search-empty" role="status">Searching…</div>
			{:else if results.length > 0}
				<div class="search-results" id="global-search-results" role="listbox" aria-label="Search results" bind:this={resultsEl}>
					{#each rows as row, i (row.result.path)}
						{@const result = row.result}
						<button
							class="result-item"
							id={`global-search-result-${i}`}
							role="option"
							aria-selected={i === selectedIndex}
							tabindex="-1"
							disabled={opening}
							class:selected={i === selectedIndex}
							onclick={() => openResult(result)}
							onmouseenter={() => (selectedIndex = i)}
						>
							<div class="result-header">
								<svg class="result-icon" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
									<path d="M14 2H6a2 2 0 00-2 2v16a2 2 0 002 2h12a2 2 0 002-2V8z" />
									<polyline points="14 2 14 8 20 8" />
									<line x1="16" y1="13" x2="8" y2="13" />
									<line x1="16" y1="17" x2="8" y2="17" />
								</svg>
								<span class="result-title">{#each row.title as part}{#if part.matched}<mark>{part.text}</mark>{:else}{part.text}{/if}{/each}</span>
								{#if getNotebook(result.path)}
									<span class="result-notebook">{getNotebook(result.path)}</span>
								{/if}
							</div>
							{#if result.snippet}
								<span class="result-snippet">{#each row.snippet as part}{#if part.matched}<mark>{part.text}</mark>{:else}{part.text}{/if}{/each}</span>
							{/if}
						</button>
					{/each}
				</div>
			{:else if query.trim() && !error}
				<div class="no-results">
					<svg width="40" height="40" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" opacity="0.3">
						<circle cx="11" cy="11" r="8" />
						<line x1="21" y1="21" x2="16.65" y2="16.65" />
						<line x1="8" y1="8" x2="14" y2="14" />
						<line x1="14" y1="8" x2="8" y2="14" />
					</svg>
					<span>{total > 0 ? 'Results changed. Choose an earlier page.' : `No results for "${query}"`}</span>
				</div>
			{:else if !error}
				<div class="search-empty">
					<span>Type to search across all notes</span>
				</div>
			{/if}
			{#if total > 0}
				<div class="search-footer">
					<span class="search-count" role="status">
						{#if results.length}{offset + 1}–{offset + results.length} of {total}{:else}{total} result{total !== 1 ? 's' : ''}{/if}
					</span>
					{#if total > PAGE_SIZE || offset > 0}
						<div class="search-pages" aria-label="Result pages">
							<button onclick={previousPage} disabled={loading || opening || offset === 0} aria-label="Previous results page">Previous</button>
							<button onclick={() => startSearch(query, offset + PAGE_SIZE, true)} disabled={loading || opening || offset + PAGE_SIZE >= total} aria-label="Next results page">Next</button>
						</div>
					{/if}
					{#if !isMobile}
						<span class="search-hints"><kbd>&uarr;</kbd><kbd>&darr;</kbd> navigate <kbd>&crarr;</kbd> open</span>
					{/if}
				</div>
			{/if}
		</div>
	</div>
{/if}

<style>
	.search-overlay {
		position: fixed;
		inset: 0;
		background: rgba(0, 0, 0, 0.4);
		backdrop-filter: blur(4px);
		-webkit-backdrop-filter: blur(4px);
		display: flex;
		align-items: flex-start;
		justify-content: center;
		padding-top: 15vh;
		z-index: 2000;
		animation: overlay-in 0.15s ease-out;
	}

	@keyframes overlay-in {
		from { opacity: 0; }
		to { opacity: 1; }
	}

	@keyframes panel-in {
		from { opacity: 0; transform: translateY(-8px) scale(0.98); }
		to { opacity: 1; transform: translateY(0) scale(1); }
	}

	.search-panel {
		background: var(--bg-primary);
		border: 1px solid var(--border-color);
		border-radius: 14px;
		box-shadow: 0 16px 48px rgba(0, 0, 0, 0.2), 0 0 0 1px rgba(255, 255, 255, 0.05) inset;
		width: min(620px, calc(100vw - 32px));
		max-height: min(460px, calc(85dvh - 24px));
		overflow: hidden;
		display: flex;
		flex-direction: column;
		animation: panel-in 0.15s ease-out;
	}

	:global(:root.dark) .search-panel {
		box-shadow: 0 16px 48px rgba(0, 0, 0, 0.5), 0 0 0 1px rgba(255, 255, 255, 0.06) inset;
	}

	.search-input-wrapper {
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 16px 18px;
		border-bottom: 1px solid var(--border-light);
	}

	.search-icon {
		color: var(--text-tertiary);
		flex-shrink: 0;
	}

	.search-input-wrapper input {
		flex: 1;
		min-width: 0;
		border: none;
		background: none;
		color: var(--text-primary);
		font-size: 17px;
		outline: none;
		font-weight: 400;
	}

	.search-input-wrapper input::placeholder {
		color: var(--text-tertiary);
	}

	.search-esc {
		font-size: 11px;
		color: var(--text-tertiary);
		background: var(--bg-tertiary);
		border: 1px solid var(--border-color);
		border-radius: 4px;
		padding: 2px 6px;
		flex-shrink: 0;
		font-family: inherit;
		line-height: 1.2;
	}

	.search-results {
		overflow-y: auto;
		padding: 6px;
		flex: 1;
	}

	.result-item {
		display: block;
		width: 100%;
		padding: 10px 12px;
		border: none;
		background: none;
		border-radius: 8px;
		cursor: pointer;
		text-align: left;
		transition: background 0.1s;
	}

	.result-item.selected {
		background: var(--bg-hover);
	}

	.result-header {
		display: flex;
		align-items: center;
		gap: 8px;
	}

	.result-icon {
		color: var(--text-tertiary);
		flex-shrink: 0;
		opacity: 0.6;
	}

	.result-title {
		font-weight: 500;
		font-size: 14px;
		color: var(--text-primary);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.result-notebook {
		margin-left: auto;
		font-size: 11px;
		color: var(--text-tertiary);
		background: var(--bg-tertiary);
		padding: 1px 7px;
		border-radius: 4px;
		flex-shrink: 0;
		max-width: 160px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.result-snippet {
		display: -webkit-box;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		-webkit-box-orient: vertical;
		font-size: 12px;
		color: var(--text-tertiary);
		margin-top: 4px;
		margin-left: 24px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: normal;
		overflow-wrap: anywhere;
		line-height: 1.4;
	}

	.result-title mark,
	.result-snippet mark {
		background: color-mix(in srgb, var(--accent) 25%, transparent);
		color: var(--text-accent);
		border-radius: 2px;
		padding: 0 1px;
	}

	.search-footer {
		display: flex;
		flex-wrap: wrap;
		gap: 8px;
		align-items: center;
		justify-content: space-between;
		padding: 8px 18px;
		border-top: 1px solid var(--border-light);
	}

	.search-count {
		font-size: 12px;
		color: var(--text-tertiary);
	}

	.search-hints {
		font-size: 11px;
		color: var(--text-tertiary);
		display: flex;
		align-items: center;
		gap: 6px;
	}

	.search-hints kbd {
		font-size: 11px;
		color: var(--text-tertiary);
		background: var(--bg-tertiary);
		border: 1px solid var(--border-color);
		border-radius: 4px;
		padding: 0px 5px;
		font-family: inherit;
		line-height: 1.4;
	}

	.no-results {
		padding: 32px 20px;
		text-align: center;
		color: var(--text-tertiary);
		font-size: 14px;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 10px;
	}

	.search-empty {
		padding: 24px 20px;
		text-align: center;
		color: var(--text-tertiary);
		font-size: 13px;
	}

	.search-error {
		padding: 12px 18px;
		color: var(--text-primary);
		font-size: 13px;
		overflow-wrap: anywhere;
	}

	.search-pages {
		display: flex;
		gap: 6px;
	}

	.search-pages button {
		padding: 4px 8px;
		border: 1px solid var(--border-color);
		border-radius: 4px;
		background: var(--bg-secondary);
		color: var(--text-primary);
		font: inherit;
		font-size: 12px;
		cursor: pointer;
	}

	.search-pages button:disabled {
		opacity: 0.45;
		cursor: default;
	}
</style>
