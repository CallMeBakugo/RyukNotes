<script lang="ts">
	import OperationFeedback from './OperationFeedback.svelte';
	import { sameAcceptedNote } from '$lib/utils/accepted-note';
	import { invoke } from '@tauri-apps/api/core';
	import RecoveryInbox from './RecoveryInbox.svelte';
	import { createEditorWorkspace, provideEditorWorkspace, splitRatioBounds, type PaneId, type PaneNavigation, type PaneRelease, type WorkspacePane } from '$lib/stores/editor-workspace';
	import { defaultEditorPane, provideEditorPane } from '$lib/stores/editor-pane';
	import EditorPaneHost from './EditorPaneHost.svelte';

	import { onMount, onDestroy, tick, untrack } from 'svelte';
	import { listen } from '@tauri-apps/api/event';
	import Sidebar from './Sidebar.svelte';
	import NoteList from './NoteList.svelte';
	import Editor from './Editor.svelte';
	import SearchPanel from './SearchPanel.svelte';
	import CommandPalette from './CommandPalette.svelte';
	import SettingsPanel from './SettingsPanel.svelte';
	import InfoPanel from './InfoPanel.svelte';
	import TitleBar from './TitleBar.svelte';
	import ResizeHandle from './ResizeHandle.svelte';
	import TagLabel from './TagLabel.svelte';
	import { getCurrentWindow } from '@tauri-apps/api/window';
	import {
		sidebarWidth,
		notelistWidth,
		outlineWidth,
		sidebarCollapsed,
		notelistCollapsed,
		collapsedNotebooks,
		showSearch,
		showCommandPalette,
		theme,
		resolvedTheme,
		customThemes,
		focusMode,
		activeNotebook,
		showInfo,
		showSettings,
		mobileView,
		appConfig,
		quickAccessPaths,
		tags,
		notes,
		notebooks,
		rootNoteCount,
		viewMode,
		sortMode,
		groupNotesByDate,
		tasksLayout,
		tasksHideCompleted,
		tasksOnlyFlagged,
		tasksSort,
		activeTag,
		notebookSortMode,
		notebookOrder,
		noteOrder,
		platformIsMobile,
		activeVaultConfig,
		activateVaultData,
	} from '$lib/stores/app';
	import { keybindings, matchAction } from '$lib/keybindings';

	const appWindow = getCurrentWindow();
	const isMac = navigator.platform.startsWith('Mac');
	const isMobile = $derived($platformIsMobile);
	import { loadVaultState, saveVaultState, readNote, deleteNote, createDailyNote, createBackup, getPendingOpenFile, ackPendingOpenFile, addQuickAccess, removeQuickAccess, getQuickAccess, setTheme, setTaskDone, setTaskPriority, setTaskDue } from '$lib/api';
	import { darkThemes, isAndroid } from '$lib/platform';
	import { openNoteWindow } from '$lib/utils/window';
	import { normalizeStartupView, resolveStartupTarget } from '$lib/utils/startup-view';
	import { isCurrentVaultEvent, sameNoteTags, updateNoteList } from '$lib/utils/note-list-updates';
	import { isPathInDirectory, isTrashedNote } from '$lib/utils/paths';
	import { get } from 'svelte/store';
	import type { VaultState, FileEvent, NotebookEntry, NoteEntry, TaskItem, SearchResult } from '$lib/types';
	import { completeWindowClose, cancelWindowClose } from '$lib/api';
	import type { StartupTarget } from '$lib/utils/startup-view';

	// Keep the original pane's legacy stores for routes awaiting Stage 3 retargeting.
	// Each new pane receives independent stores; this first context never follows focus.
	const workspace = createEditorWorkspace(defaultEditorPane);
	provideEditorWorkspace(workspace);
	provideEditorPane(get(workspace).panes[0].context);
	const activePane = workspace.activePane;
	const anyDirty = workspace.anyDirty;
	const workspaceBusy = workspace.busy;
	const activeNote = $derived($activePane.context.activeNote);
	const activeNotePath = $derived($activePane.context.activeNotePath);
	const viewerNote = $derived($activePane.context.viewerNote);
	const editorDirty = $derived($activePane.context.editorDirty);
	const sourceMode = $derived($activePane.context.sourceMode);
	const readOnly = $derived($activePane.context.readOnly);
	const navHistory = $derived($activePane.context.navHistory);

	function findNotebookByPath(list: NotebookEntry[], relPath: string): NotebookEntry | null {
		for (const nb of list) {
			if (nb.relative_path === relPath) return nb;
			const found = findNotebookByPath(nb.children, relPath);
			if (found) return found;
		}
		return null;
	}

	let sidebar = $state<Sidebar>();
	let noteList = $state<NoteList>();
	let mobileEditor = $state<Editor>();
	let editorInstances = $state<Record<PaneId, Editor | undefined>>({});
	$effect(() => {
		workspace.registerEditor($workspace.panes[0].id, isMobile ? mobileEditor : editorInstances[$workspace.panes[0].id]);
	});
	const editor = $derived(isMobile ? mobileEditor : editorInstances[$workspace.activePaneId]);
	let editorAreaWidth = $state(0);
	let separatorWidth = $state(0);
	const availableEditorWidth = $derived(Math.max(0, editorAreaWidth - separatorWidth));
	const splitBounds = $derived(splitRatioBounds(availableEditorWidth));
	const workspaceId = $props.id();
	let splitBusy = $state(false);
	let splitError = $state('');

	function activatePane(id: PaneId) {
		workspace.activate(id);
	}

	function activateOpenNote(path: string, token?: PaneNavigation): boolean {
		const activated = workspace.activateOpenNote(path, token?.context);
		if (activated) {
			taskNoteOpened = true;
			if (isMobile) $mobileView = 'editor';
		}
		return activated;
	}

	async function toggleSplit() {
		if (splitBusy) return;
		splitError = '';
		if ($workspace.panes.length === 1) {
			workspace.openSplit();
			return;
		}
		splitBusy = true;
		try {
			const closed = await workspace.closeSecond(async (second) => {
				const instance = editorInstances[second.id];
				return (!instance || await instance.flushSave())
					&& !get(second.context.editorDirty) && ownerIsCurrent();
			});
			if (!closed) splitError = 'Save the second pane before closing it.';
		} catch (error) {
			console.error('Failed to close second pane:', error);
			splitError = 'The second pane could not be saved. It remains open.';
		} finally {
			splitBusy = false;
		}
	}

	function resizeSplit(delta: number) {
		const width = availableEditorWidth;
		if (width > 0) workspace.resize($workspace.splitRatio + delta / width, width);
	}

	$effect(() => {
		workspace.resize(untrack(() => $workspace.splitRatio), availableEditorWidth);
	});

	function editorFor(token: PaneNavigation) {
		return isMobile ? mobileEditor : editorInstances[token.paneId];
	}

	async function flushWorkspace(): Promise<boolean> {
		return workspace.flushAll();
	}

	async function flushDestination(token?: PaneNavigation): Promise<boolean> {
		const intent = token ?? beginNavigation();
		const instance = editorFor(intent);
		return navigationIsCurrent(intent) && (!instance || await instance.flushSave())
			&& navigationIsCurrent(intent) && !get(intent.context.editorDirty);
	}

	async function beginPaneMutation(paths?: readonly string[]): Promise<PaneRelease | null> {
		if (backupInFlight) return null;
		return workspace.acquireMutation(paths);
	}
	let outlineOpen = $state(false);
	let outlinePanelId = $state('');

	function handleOutlineChange(open: boolean, panelId: string) {
		outlineOpen = open;
		outlinePanelId = panelId;
	}
	let pendingSearchReveal = $state<{ path: string; token: PaneNavigation } | null>(null);
	let unlistenFileChange: (() => void) | null = null;
	let unlistenClose: (() => void) | null = null;
	let unlistenVaultRestored: (() => void) | null = null;
	let fileRefreshTimer: ReturnType<typeof setTimeout> | null = null;
	const ownerVault = get(appConfig)?.active_vault ?? null;
	activateVaultData(ownerVault);
	let vaultSwitchRelease: PaneRelease | null = null;
	let stateReady = false;
	let restoringNote = false;
	let disposed = false;
	const navigationIntents = new Set<PaneNavigation>();
	let persistTimer: ReturnType<typeof setTimeout> | null = null;
	let removePopstateListener: (() => void) | null = null;
	const backupListeners = new Set<() => void>();
	let backupInFlight = false;
	let pendingOpenDrain: Promise<void> | null = null;
	let unlistenPendingOpen: (() => void) | null = null;
	let pendingOpenNotification = false;
	let removeMobileFlushListeners: (() => void) | null = null;
	const recentChanges = new Map<string, NoteEntry>();
	let initialRefreshPending = true;
	let fileRefreshPending = false;
	let refreshLists = false;
	let fileRefreshRequest = 0;

	function ownerIsCurrent() {
		return !disposed && (get(appConfig)?.active_vault ?? null) === ownerVault;
	}

	function beginNavigation(path?: string) {
		const token = workspace.captureForPath(path);
		for (const older of navigationIntents) {
			if (older.paneId === token.paneId || !older.isCurrent()) navigationIntents.delete(older);
		}
		navigationIntents.add(token);
		return token;
	}

	function navigationIsCurrent(token: PaneNavigation) {
		return ownerIsCurrent() && token.isCurrent();
	}

	function scheduleFileRefresh(lists: boolean) {
		if (!ownerIsCurrent()) return;
		fileRefreshPending = true;
		refreshLists ||= lists;
		// Let the initial reads finish; note updates are replayed by NoteList.
		if (initialRefreshPending) return;
		const request = ++fileRefreshRequest;
		if (fileRefreshTimer) clearTimeout(fileRefreshTimer);
		fileRefreshTimer = setTimeout(async () => {
			fileRefreshTimer = null;
			if (!ownerIsCurrent() || request !== fileRefreshRequest) return;
			fileRefreshPending = false;
			const lists = refreshLists;
			refreshLists = false;
			if (lists) {
				await Promise.all([sidebar?.refresh(), noteList?.refresh(true)]);
				if (!ownerIsCurrent() || request !== fileRefreshRequest) return;
				if (isMobile) await sidebar?.refreshTags();
			} else {
				await sidebar?.refreshTags();
			}
		}, isMobile ? 1000 : 300);
	}

	function handleTagsChanged(path: string, nextTags: string[]) {
		if (!ownerIsCurrent()) return;
		// Record successful saves before patching lists so their delayed watcher
		// event does not schedule a second scan.
		const previous = recentChanges.get(path) ?? get(notes).find((note) => note.path === path);
		if (previous) {
			const updated = { ...previous, meta: { ...previous.meta, tags: nextTags } };
			recentChanges.set(path, updated);
			if (!sameNoteTags(previous, updated)) scheduleFileRefresh(false);
		} else {
			scheduleFileRefresh(false);
		}
	}

	async function applyStartupTarget(target: StartupTarget): Promise<boolean> {
		if (!ownerIsCurrent()) return false;
		if (target.mode === 'notebook') {
			const vault = $appConfig?.active_vault;
			const notebook = target.notebookPath === '' && vault
				? { name: 'Unfiled Notes', path: vault, relative_path: '', children: [], note_count: $rootNoteCount }
				: findNotebookByPath($notebooks, target.notebookPath);
			if (!notebook) return false;
			const changed = $viewMode !== 'notebook' || $activeNotebook?.relative_path !== notebook.relative_path;
			$viewMode = 'notebook';
			$activeNotebook = notebook;
			$activeTag = null;
			if (changed) await noteList?.refresh();
			return true;
		}

		if (target.mode === 'tag') {
			const changed = $viewMode !== 'tag' || $activeTag !== target.tag;
			$viewMode = 'tag';
			$activeTag = target.tag;
			$activeNotebook = null;
			if (changed) await noteList?.refresh();
			return true;
		}

		const changed = $viewMode !== target.mode || $activeNotebook !== null || $activeTag !== null;
		$viewMode = target.mode;
		$activeNotebook = null;
		$activeTag = null;
		if (changed) await noteList?.refresh();
		return true;
	}

	let unlistenOpenFile: (() => void) | null = null;

	// Mobile editor header helpers
	let noteRelativePath = $derived($activeNotePath && $appConfig?.active_vault ? $activeNotePath.replace($appConfig.active_vault + '/', '') : '');
	let isQuickAccess = $derived(noteRelativePath ? $quickAccessPaths.includes(noteRelativePath) : false);
	let backupInterval: ReturnType<typeof setInterval> | null = null;





	function parseFrequencyMs(freq: string): number {
		switch (freq) {
			case '6h': return 6 * 60 * 60 * 1000;
			case '12h': return 12 * 60 * 60 * 1000;
			case '7d': return 7 * 24 * 60 * 60 * 1000;
			case '24h': default: return 24 * 60 * 60 * 1000;
		}
	}

	async function checkScheduledBackup() {
		if (!ownerIsCurrent() || backupInFlight || workspace.isBlocked()) return;
		const config = get(appConfig);
		const vault = activeVaultConfig(config);
		if (!config?.backup_enabled || !vault) return;
		const interval = parseFrequencyMs(config.backup_frequency);
		const last = vault.last_backup_time ? new Date(vault.last_backup_time).getTime() : 0;
		if (Date.now() - last < interval) return;
		backupInFlight = true;
		let unlisten: (() => void) | null = null;
		try {
			unlisten = await listen<{ vault_path: string; bookmark_id?: string | null; success: boolean; entry?: { created: string } }>('backup-done', (event) => {
				if (event.payload.vault_path !== ownerVault || (event.payload.bookmark_id ?? null) !== (vault.bookmark_id ?? null)) return;
				if (ownerIsCurrent() && event.payload.success && event.payload.entry?.created) {
					const current = get(appConfig);
					const active = activeVaultConfig(current);
					if (current && active) appConfig.set({
						...current,
						vaults: current.vaults.map((v) => v === active ? { ...v, last_backup_time: event.payload.entry!.created } : v)
					});
				}
				backupInFlight = false;
				unlisten?.();
				if (unlisten) backupListeners.delete(unlisten);
			});
			if (!ownerIsCurrent()) { unlisten(); backupInFlight = false; return; }
			backupListeners.add(unlisten);
			await createBackup(ownerVault!);
		} catch (error) {
			console.error('Failed to start scheduled backup:', error);
			unlisten?.();
			if (unlisten) backupListeners.delete(unlisten);
			backupInFlight = false;
		}
	}

	// Track note navigation in history stack
	async function handlePathsMoved(oldPath: string, newPath: string) {
		if (!ownerIsCurrent()) return;
		workspace.remapPaths(oldPath, newPath);
	}

	$effect(() => {
		const path = $activeNotePath;
		if (path) {
			navHistory.push(path);
			if (stateReady) queueMicrotask(drainPendingOpenFiles);
		}
	});

	$effect(() => {
		if (!$workspaceBusy && !$anyDirty && stateReady && ownerIsCurrent()) untrack(drainPendingOpenFiles);
	});

	async function navigateHistory(direction: -1 | 1) {
		const token = beginNavigation();
		if (!navigationIsCurrent(token)) return;
		if (editorFor(token) && !(await editorFor(token)!.flushSave())) return;
		if (!navigationIsCurrent(token)) return;
		const history = get(token.context.navHistory);
		const path = history.stack[history.index + direction];
		if (!path || activateOpenNote(path, token)) return;
		token.context.navHistory.go(direction);
		try {
			const content = await readNote(ownerVault!, path);
			if (!navigationIsCurrent(token)) return;
			if (editorFor(token) && !(await editorFor(token)!.flushSave())) return;
			if (!navigationIsCurrent(token)) return;
			if (activateOpenNote(path, token)) return;
			token.context.activeNote.set(content);
			token.context.activeNotePath.set(path);
			token.context.editorDirty.set(false);
			editorFor(token)?.loadNote(path, content.content);
		} catch (e) {
			console.error('Failed to navigate history:', e);
		}
	}

	async function handleOpenFile(filePath: string, token = beginNavigation(filePath)): Promise<boolean> {
		if (!navigationIsCurrent(token) || !filePath || !filePath.endsWith('.md')) return false;
		if (activateOpenNote(filePath, token)) return true; // Never replace an open draft with a stale disk read.
		const isExternal = !isPathInDirectory(filePath, ownerVault);
		if (isExternal && isAndroid) return false;
		if (editorFor(token) && !(await editorFor(token)!.flushSave())) return false;
		if (!navigationIsCurrent(token)) return false;
		try {
			const content = await readNote(ownerVault!, filePath);
			if (!navigationIsCurrent(token)) return false;
			if (activateOpenNote(filePath, token)) return true;
			token.context.viewerNote.set(isExternal ? { path: filePath, content: content.content } : null);
			token.context.activeNote.set(content);
			token.context.activeNotePath.set(filePath);
			token.context.editorDirty.set(false);
			if (isExternal) {
				token.context.readOnly.set(true);
				$focusMode = true;
			}
			editorFor(token)?.loadNote(filePath, content.content);
			return true;
		} catch (error) {
			console.error('Failed to open file:', error);
			return false;
		}
	}

	function drainPendingOpenFiles() {
		if (pendingOpenDrain) { pendingOpenNotification = true; return; }
		if (!stateReady || !ownerIsCurrent() || workspace.isBlocked()) return;
		pendingOpenDrain = (async () => {
			try {
				while (ownerIsCurrent()) {
					const pending = await getPendingOpenFile();
					if (!pending || !ownerIsCurrent()) break;
					if (!(await handleOpenFile(pending.path))) break;
					if (!ownerIsCurrent()) break;
					await ackPendingOpenFile(pending.id);
				}
			} catch (error) {
				console.error('Failed to open pending file:', error);
			} finally {
				pendingOpenDrain = null;
				if (pendingOpenNotification) {
					pendingOpenNotification = false;
					queueMicrotask(drainPendingOpenFiles);
				}
			}
		})();
	}

	async function restoreLastNote(path: string, token: PaneNavigation) {
		const canRestore = () => navigationIsCurrent(token) && !get(token.context.activeNotePath) && !get(token.context.editorDirty) && !get(token.context.viewerNote);
		if (!canRestore() || isTrashedNote(path, ownerVault)) return;
		const isExternal = !isPathInDirectory(path, ownerVault);
		if (isExternal && isAndroid) return;
		restoringNote = true;
		try {
			const content = await readNote(ownerVault!, path);
			if (!canRestore()) return;
			if (activateOpenNote(path, token)) return;
			token.context.activeNote.set(content);
			token.context.activeNotePath.set(path);
			token.context.editorDirty.set(false);
			if (isExternal) {
				token.context.viewerNote.set({ path, content: content.content });
				token.context.readOnly.set(true);
				$focusMode = true;
			}
			editorFor(token)?.loadNote(path, content.content);
		} catch (_) {
		} finally {
			restoringNote = false;
			if (ownerIsCurrent()) persistState();
		}
	}

	async function persistVaultState() {
		if (!ownerIsCurrent() || !ownerVault || !stateReady || (restoringNote && !$activeNotePath)) return;
		const state: VaultState = {
			last_open_note: $activeNotePath,
			sidebar_width: $sidebarWidth,
			notelist_width: $notelistWidth,
			outline_width: $outlineWidth,
			sidebar_collapsed: $sidebarCollapsed,
			notelist_collapsed: $notelistCollapsed,
			collapsed_notebooks: $collapsedNotebooks,
			notebook_sort_mode: $notebookSortMode,
			notebook_order: $notebookOrder,
			note_order: $noteOrder,
			sort_mode: $sortMode,
			group_notes_by_date: $groupNotesByDate,
			last_view_mode: $viewMode,
			last_notebook: $activeNotebook?.relative_path ?? null,
			last_tag: $activeTag,
			tasks_layout: $tasksLayout,
			tasks_hide_completed: $tasksHideCompleted,
			tasks_only_flagged: $tasksOnlyFlagged,
			tasks_sort: $tasksSort
		};
		try {
			await saveVaultState(ownerVault, state);
		} catch (_) {}
	}

	function persistState() {
		if (!ownerIsCurrent() || !stateReady) return;
		if (persistTimer) clearTimeout(persistTimer);
		persistTimer = setTimeout(() => {
			persistTimer = null;
			void persistVaultState();
		}, 1000);
	}

	async function prepareVaultSwitch(): Promise<boolean> {
		const release = await beginPaneMutation();
		if (!release) return false;
		try {
			await persistVaultState();
			if (!ownerIsCurrent()) { await release(); return false; }
			vaultSwitchRelease = release;
			return true;
		} catch (error) {
			await release();
			console.error('Failed to prepare vault switch:', error);
			return false;
		}
	}

	function handleSidebarResize(delta: number) {
		$sidebarWidth = Math.max(160, Math.min(400, $sidebarWidth + delta));
		persistState();
	}

	function handleNotelistResize(delta: number) {
		$notelistWidth = Math.max(200, Math.min(500, $notelistWidth + delta));
		persistState();
	}

	function revealNoteList() {
		$notelistCollapsed = false;
		tick().then(() => { if (ownerIsCurrent()) noteList?.refresh(); });
	}

	function toggleNoteList() {
		const nextCollapsed = !$notelistCollapsed;
		$notelistCollapsed = nextCollapsed;
		if (!nextCollapsed) {
			tick().then(() => { if (ownerIsCurrent()) noteList?.refresh(); });
		}
	}

	// Tasks view: the editor pane shows a placeholder until a task is opened from the list.
	let taskNoteOpened = $state(false);

	function handleNoteSelected(path: string, content: string, task?: TaskItem, token = beginNavigation()) {
		if (!navigationIsCurrent(token)) return;
		token.context.viewerNote.set(null);
		taskNoteOpened = true;
		editorFor(token)?.loadNote(path, content, task);
		if (isMobile) $mobileView = 'editor';
	}

	async function selectNoteFromSwitcher(path: string, token = beginNavigation(path), canSelect?: () => boolean): Promise<boolean> {
		const current = () => navigationIsCurrent(token) && (canSelect?.() ?? true);
		if (!current()) return false;
		if (activateOpenNote(path, token)) return true;
		if (editorFor(token) && !(await editorFor(token)!.flushSave())) return false;
		if (!current()) return false;
		try {
			const content = await readNote(ownerVault!, path);
			if (!current()) return false;
			if (editorFor(token) && !(await editorFor(token)!.flushSave())) return false;
			if (!current()) return false;
			if (activateOpenNote(path, token)) return true;
			token.context.viewerNote.set(null);
			token.context.activeNote.set(content);
			token.context.activeNotePath.set(path);
			token.context.editorDirty.set(false);
			handleNoteSelected(path, content.content, undefined, token);
			return true;
		} catch (e) {
			console.error('Failed to switch note:', e);
			return false;
		}
	}

	async function selectNoteFromSearch(result: SearchResult, notebook: NotebookEntry | null, canSelect: () => boolean): Promise<boolean> {
		const { path } = result;
		const token = beginNavigation(path);
		if (!(await selectNoteFromSwitcher(path, token, canSelect)) || !navigationIsCurrent(token) || !canSelect()) return false;
		if (get(token.context.activeNotePath) !== path) return true;
		const mode = notebook ? 'notebook' : 'all';
		if ($viewMode !== mode || $activeNotebook?.path !== notebook?.path || $activeTag !== null) {
			$activeNotebook = notebook;
			$activeTag = null;
			$viewMode = mode;
			$notes = [];
		}
		$notelistCollapsed = false;
		pendingSearchReveal = { path, token };
		if (isMobile) $mobileView = 'editor';
		await tick();
		if (!navigationIsCurrent(token) || !canSelect()) return false;
		await editorFor(token)?.revealSearchTarget(result.match_target, () => navigationIsCurrent(token) && canSelect() && $workspace.activePaneId === token.paneId);
		return navigationIsCurrent(token) && canSelect();
	}

	$effect(() => {
		const request = pendingSearchReveal;
		const list = noteList;
		if (!request || !list) return;
		// A hidden focus-mode list can mount later; only the originating navigation
		// may refresh and reveal it. Do not subscribe this effect to list internals.
		void untrack(async () => {
			const current = () => pendingSearchReveal === request && noteList === list
				&& navigationIsCurrent(request.token) && get(request.token.context.activeNotePath) === request.path;
			try {
				if (!current()) return;
				await list.refresh();
				if (!current()) return;
				const revealed = await list.revealNote(request.path, request.token);
				if (!revealed && current()) {
					// Search can find a newly indexed note before a cached list sees it.
					await list.refresh(true);
					if (current()) await list.revealNote(request.path, request.token);
				}
			} catch (error) {
				console.error('Failed to reveal search result:', error);
			} finally {
				if (pendingSearchReveal === request && (noteList === list
					|| !navigationIsCurrent(request.token) || get(request.token.context.activeNotePath) !== request.path)) pendingSearchReveal = null;
			}
		});
	});

	/** A tag clicked under a note's title: browse that tag's notes, like picking it in the sidebar. */
	function showTagNotes(tag: string) {
		$viewMode = 'tag';
		$activeTag = tag;
		$activeNotebook = null;
		handleViewChanged();
	}

	function handleViewChanged() {
		if (!ownerIsCurrent()) return;
		beginNavigation();
		taskNoteOpened = false;

		// Picking a notebook/tag/Tasks/etc. in the sidebar is a request to browse that view's
		// notes - so if the notes list was hidden, bring it back rather than navigating blindly
		// into a panel the user can't see. revealNoteList() re-mounts the list and refreshes it.
		if ($notelistCollapsed) {
			revealNoteList();
		} else {
			noteList?.refresh();
		}
		if (isMobile) $mobileView = 'notelist';
	}

	function createAndFocusNote() {
		return noteList?.handleCreateNote();
	}

	async function updateActiveNoteMetadata(patch: Pick<NoteEntry['meta'], 'pinned' | 'tags'>): Promise<boolean> {
		if (!ownerIsCurrent() || !$activeNote || !editor || editor.isMutationPending()) return false;
		$activeNote = { ...$activeNote, meta: { ...$activeNote.meta, ...patch } };
		editor.markEdited();
		return editor.forceSave();
	}

	// An active task mutation is serialized against both editing surfaces. The editor
	// briefly becomes read-only while the native command changes the disk note.
	async function changeTask(task: TaskItem, operation: () => Promise<void>) {
		if (!ownerIsCurrent()) return;
		const release = await beginPaneMutation([task.note_path]);
		if (!release) return;
		try { if (ownerIsCurrent()) await operation(); }
		catch (error) { console.error('Failed to update task:', error); }
		finally { await release(); }
	}

	async function toggleTask(task: TaskItem) {
		await changeTask(task, () => setTaskDone(ownerVault!, task.note_path, task.line, task.raw_line, !task.completed));
	}

	async function changeTaskPriority(task: TaskItem, priority: string | null) {
		await changeTask(task, () => setTaskPriority(ownerVault!, task.note_path, task.line, task.raw_line, priority));
	}

	async function changeTaskDue(task: TaskItem, due: string | null) {
		await changeTask(task, () => setTaskDue(ownerVault!, task.note_path, task.line, task.raw_line, due));
	}

	async function handleDailyNote() {
		const token = beginNavigation();
		if (!navigationIsCurrent(token)) return;
		try {
			if (editorFor(token) && !(await editorFor(token)!.flushSave())) return;
			if (!navigationIsCurrent(token)) return;
			const entry = await createDailyNote(ownerVault!);
			if (!navigationIsCurrent(token)) return;
			if (activateOpenNote(entry.path, token)) return;
			const content = await readNote(ownerVault!, entry.path);
			if (!navigationIsCurrent(token)) return;
			if (editorFor(token) && !(await editorFor(token)!.flushSave())) return;
			if (!navigationIsCurrent(token)) return;
			if (activateOpenNote(entry.path, token)) return;
			token.context.activeNote.set(content);
			token.context.activeNotePath.set(entry.path);
			token.context.editorDirty.set(false);
			editorFor(token)?.loadNote(entry.path, content.content);
			$viewMode = 'daily';
			noteList?.refresh(true);
			sidebar?.refresh();
			if (isMobile) $mobileView = 'editor';
		} catch (e) {
			console.error('Failed to create/open daily note:', e);
		}
	}

	// Android back gesture / hardware back button support
	// We maintain a simple counter of how many views deep we are.
	// sidebar=0, notelist=1, editor=2. Each forward nav pushes, back pops.
	let historyDepth = 0;
	let navFromPopstate = false;

	if (get(platformIsMobile)) {
		history.replaceState({ mobileView: 'sidebar', depth: 0 }, '');

		$effect(() => {
			const view = $mobileView;
			if (navFromPopstate) {
				navFromPopstate = false;
				return;
			}
			const targetDepth = view === 'sidebar' ? 0 : view === 'notelist' ? 1 : 2;
			if (targetDepth > historyDepth) {
				// Forward navigation - push entries for each level skipped
				for (let d = historyDepth + 1; d <= targetDepth; d++) {
					const v = d === 1 ? 'notelist' : 'editor';
					history.pushState({ mobileView: v, depth: d }, '');
				}
				historyDepth = targetDepth;
			} else if (targetDepth < historyDepth) {
				const steps = historyDepth - targetDepth;
				historyDepth = targetDepth;
				navFromPopstate = true; // suppress the popstate that history.go triggers
				history.go(-steps);
			}
		});

		const onPopstate = (e: PopStateEvent) => {
			if (!ownerIsCurrent()) return;
			if (navFromPopstate) {
				navFromPopstate = false;
				return;
			}
			// If a modal is open, close it instead of navigating
			if ($showSettings || $showInfo || $showSearch || $showCommandPalette) {
				$showSettings = false;
				$showInfo = false;
				$showSearch = false;
				$showCommandPalette = false;
				// Re-push the current state so the next back still works
				history.pushState({ mobileView: $mobileView, depth: historyDepth }, '');
				return;
			}
			const state = e.state;
			const targetDepth = state?.depth ?? 0;
			historyDepth = targetDepth;
			navFromPopstate = true;
			if (targetDepth === 0) $mobileView = 'sidebar';
			else if (targetDepth === 1) $mobileView = 'notelist';
			else $mobileView = 'editor';
		};
		window.addEventListener('popstate', onPopstate);
		removePopstateListener = () => window.removeEventListener('popstate', onPopstate);
	}

	function mobileBack() {
		if ($mobileView === 'editor') $mobileView = 'notelist';
		else if ($mobileView === 'notelist') $mobileView = 'sidebar';
	}

	async function trashOpenNote(path: string): Promise<boolean> {
		if (!ownerIsCurrent()) return false;
		const release = await beginPaneMutation([path]);
		if (!release) return false;
		try {
			if (!ownerIsCurrent()) return false;
			await deleteNote(ownerVault!, path);
			$notes = $notes.filter(note => note.path !== path);
			noteList?.refresh(true).catch(error => console.error('Failed to refresh notes:', error));
			return true;
		} catch (error) { console.error('Failed to trash note:', error); return false; }
		finally { await release(); }
	}

	function handleMouseDown(e: MouseEvent) {
		if (e.button === 3) { e.preventDefault(); navigateHistory(-1); }
		if (e.button === 4) { e.preventDefault(); navigateHistory(1); }
	}

	function handleKeydown(e: KeyboardEvent) {
		const mod = e.ctrlKey || e.metaKey;
		const code = e.code;

		// Mod+K (insert link) is an editor formatting action and is not user-customizable.
		if (mod && code === 'KeyK' && !e.shiftKey && $activeNotePath && !$sourceMode) {
			e.preventDefault();
			editor?.addLinkFromToolbar();
		}

		// Ctrl/Cmd+Shift+Delete: move the open note to the trash.
		if (mod && e.shiftKey && code === 'Delete' && $activeNotePath) {
			e.preventDefault();
			editor?.moveOpenNoteToTrash();
			return;
		}

		const action = matchAction(e, $keybindings);
		if (action) {
			e.preventDefault();
			switch (action) {
				case 'nav-back':
					navigateHistory(-1);
					return;
				case 'nav-forward':
					navigateHistory(1);
					return;
				case 'new-note':
					createAndFocusNote();
					return;
				case 'toggle-theme': {
					const customTheme = $customThemes.find(theme => theme.id === $resolvedTheme);
					const isDark = darkThemes.includes($resolvedTheme) || (customTheme?.is_dark ?? false);
					const next = isDark ? 'light' : 'dark';
					$theme = next;
					setTheme(next);
					return;
				}
				case 'search-vault':
					$showSearch = true;
					return;
				case 'find-in-note':
					if ($activeNotePath) {
						editor?.openNoteSearch();
					} else {
						$showSearch = true;
					}
					return;
				case 'replace-in-note':
					if ($activeNotePath) editor?.openNoteSearch(true);
					return;
				case 'quick-open':
					$showCommandPalette = true;
					return;
				case 'save':
					editor?.forceSave();
					return;
				case 'toggle-source':
					$sourceMode = !$sourceMode;
					return;
				case 'open-new-window':
					if ($activeNotePath && $activeNote) {
						openNoteWindow(ownerVault!, $activeNotePath, $activeNote.meta.title);
					}
					return;
				case 'toggle-sidebar':
					$sidebarCollapsed = !$sidebarCollapsed;
					return;
				case 'toggle-notelist':
					toggleNoteList();
					return;
				case 'toggle-focus':
					$focusMode = !$focusMode;
					return;
				case 'toggle-readonly':
					if ($viewerNote) return; // viewer mode is always read-only
					$readOnly = !$readOnly;
					return;
				case 'fullscreen':
					appWindow.isFullscreen().then(fs => appWindow.setFullscreen(!fs));
					return;
			}
		}

		if (e.key === 'Escape') {
			const dismissesAppUi = $showSettings || $showInfo || $focusMode || $showSearch || $showCommandPalette;
			if (!dismissesAppUi) return;

			e.preventDefault();
			e.stopPropagation();
			if ($showSettings) $showSettings = false;
			else if ($showInfo) $showInfo = false;
			else if ($focusMode) $focusMode = false;
			else if ($showSearch) $showSearch = false;
			else if ($showCommandPalette) $showCommandPalette = false;
		}
	}

	$effect(() => {
		$collapsedNotebooks;
		persistState();
	});

	$effect(() => {
		$activeNotePath;
		persistState();
	});

	$effect(() => {
		$sidebarCollapsed;
		$notelistCollapsed;
		$outlineWidth;
		persistState();
	});

	$effect(() => {
		$notebookSortMode;
		$notebookOrder;
		persistState();
	});

	$effect(() => {
		$noteOrder;
		persistState();
	});

	$effect(() => {
		$sortMode;
		$groupNotesByDate;
		$tasksLayout;
		$tasksHideCompleted;
		$tasksOnlyFlagged;
		$tasksSort;
		persistState();
	});

	$effect(() => {
		$viewMode;
		$activeNotebook;
		$activeTag;
		persistState();
	});

	const refreshRequests = new Map<PaneId, number>();
	let recoveryBarrierNotice = $state('');
	workspace.onBarrier(async roots => {
		try {
			const token = await invoke<string>('recovery_barrier', { vault: ownerVault, roots: roots ? [...roots] : null });
			recoveryBarrierNotice = '';
			return async () => { await invoke('recovery_release_barrier', { token }); };
		} catch {
			recoveryBarrierNotice = 'Resolve recovered drafts and close affected detached note windows before continuing.';
			return false;
		}
	});
	workspace.onReconcile(async (owners) => {
		for (const pane of owners) await refreshPane(pane, true, true);
		if (ownerIsCurrent()) scheduleFileRefresh(true);
	});
	async function refreshPane(pane: WorkspacePane, restored = false, locked = false) {
		const context = pane.context;
		const path = get(context.activeNotePath);
		const openingNote = get(context.activeNote);
		if (!path || !openingNote || !ownerIsCurrent() || (workspace.isBlocked() && !locked)) return;
		const request = (refreshRequests.get(pane.id) ?? 0) + 1;
		refreshRequests.set(pane.id, request);
		const current = () => ownerIsCurrent() && refreshRequests.get(pane.id) === request
			&& get(context.activeNotePath) === path;
		const instance = workspace.editor(pane.id);
		try {
			const content = await readNote(ownerVault!, path);
			if (!current()) return;
			if (get(context.editorDirty)) {
				if (restored && content.raw !== instance?.getAcceptedRaw()) instance?.flagExternalConflict();
				return;
			}
			if (get(context.activeNote) !== openingNote) return;
			if (content.meta.accepted?.trashed) {
				context.activeNotePath.set(null);
				context.activeNote.set(null);
				context.viewerNote.set(null);
				return;
			}
			if (sameAcceptedNote(openingNote, content)) return;
			context.activeNote.set(content);
			if (get(context.viewerNote)?.path === path) context.viewerNote.set({ path, content: content.content });
			instance?.loadNote(path, content.content);
		} catch (error) {
			if (!current()) return;
			if (get(context.editorDirty)) { instance?.flagExternalConflict(); return; }
			if (restored && /not found|no such|does not exist|query returned no rows/i.test(String(error))) {
				context.activeNotePath.set(null);
				context.activeNote.set(null);
				context.viewerNote.set(null);
			} else console.error('Failed to refresh pane:', error);
		}
	}

	async function refreshActiveNote(path: string, restored = false) {
		await Promise.all($workspace.panes.filter(p => get(p.context.activeNotePath) === path)
			.map(p => refreshPane(p, restored)));
	}

	onMount(async () => {
		if (!ownerIsCurrent() || !ownerVault) return;
		const startupToken = beginNavigation();
		if (isMobile) {
			const flush = () => {
				if (ownerIsCurrent()) void flushWorkspace();
			};
			const onVisibilityChange = () => {
				if (document.visibilityState === 'hidden') flush();
			};
			document.addEventListener('visibilitychange', onVisibilityChange);
			window.addEventListener('pagehide', flush);
			window.addEventListener('app-lifecycle-flush', flush);
			removeMobileFlushListeners = () => {
				document.removeEventListener('visibilitychange', onVisibilityChange);
				window.removeEventListener('pagehide', flush);
				window.removeEventListener('app-lifecycle-flush', flush);
			};
		}
		if (!isMobile) {
			unlistenClose = await appWindow.onCloseRequested(async (event) => {
				// Always prevent Tauri's implicit JS destroy(), which bypasses the tray policy.
				event.preventDefault();
				if (!ownerIsCurrent()) { await cancelWindowClose(); return; }
				try {
					const release = await workspace.acquireMutation(undefined, true);
					if (!release) { await cancelWindowClose(); return; }
					try {
					if (!ownerIsCurrent()) { await cancelWindowClose(); return; }
					await persistVaultState();
					if (!ownerIsCurrent()) { await cancelWindowClose(); return; }
					await completeWindowClose();
					} finally { await release(); }
				} catch (error) {
					console.error('Failed to close window:', error);
					try { await cancelWindowClose(); } catch (cancelError) { console.error('Failed to cancel window close:', cancelError); }
				}
			});
			if (!ownerIsCurrent()) { unlistenClose(); return; }
		}
		unlistenFileChange = await listen<FileEvent>('file-changed', ({ payload }) => {
			if (!ownerIsCurrent() || !isCurrentVaultEvent(payload, ownerVault)) return;
			if (!payload.note) {
				recentChanges.delete(payload.path);
				scheduleFileRefresh(true);
				return;
			}
			const note = payload.note;
			const visiblePrevious = get(notes).find((entry) => entry.path === note.path);
			const listPrevious = noteList?.updateNote(note) ?? visiblePrevious;
			const previous = recentChanges.get(note.path) ?? listPrevious;
			recentChanges.set(note.path, note);
			if (!noteList) {
				const key = $viewMode === 'notebook' && $activeNotebook ? `nb:${$activeNotebook.path}` :
					$viewMode === 'tag' && $activeTag ? `tag:${$activeTag}` : $viewMode;
				const updated = updateNoteList(get(notes), note, key, get(quickAccessPaths));
				if (updated !== get(notes)) notes.set(updated);
			}
			// Body/modified/preview changes do not change notebook counts or the tag index.
			if (!previous) scheduleFileRefresh(true);
			else if (!sameNoteTags(previous, note)) scheduleFileRefresh(false);
			void refreshActiveNote(payload.path);
		});
		if (!ownerIsCurrent()) { unlistenFileChange(); return; }
		unlistenPendingOpen = await listen('open-file-pending', () => {
			drainPendingOpenFiles();
		});
		if (!ownerIsCurrent()) { unlistenPendingOpen(); return; }
		unlistenVaultRestored = await listen<{ vault_path: string }>('vault-restored', ({ payload }) => {
			if (!ownerIsCurrent() || payload.vault_path !== ownerVault) return;
			// Our restore already owns a lease; its release reconciles all pane buffers.
			if (!workspace.isBlocked()) void (async () => {
				const release = await beginPaneMutation();
				if (release) await release();
			})();
			scheduleFileRefresh(true);
		});
		if (!ownerIsCurrent()) { unlistenVaultRestored(); return; }
		unlistenOpenFile = await listen<string>('open-file', (event) => {
			if (ownerIsCurrent()) void handleOpenFile(event.payload);
		});
		if (!ownerIsCurrent()) { unlistenOpenFile(); return; }

		let lastNotePath: string | null = null;
		let lastViewMode = '';
		let lastNotebook: string | null = null;
		let lastTag: string | null = null;
		try {
			const state = await loadVaultState(ownerVault);
			if (!ownerIsCurrent()) return;
			$sidebarWidth = state.sidebar_width;
			$notelistWidth = state.notelist_width;
			if (typeof state.outline_width === 'number' && Number.isFinite(state.outline_width)) {
				$outlineWidth = Math.max(160, Math.min(500, state.outline_width));
			}
			$sidebarCollapsed = state.sidebar_collapsed;
			$notelistCollapsed = state.notelist_collapsed ?? false;
			$collapsedNotebooks = state.collapsed_notebooks ?? [];
			$notebookSortMode = state.notebook_sort_mode === 'manual' ? 'manual' : 'alphabetical';
			$notebookOrder = state.notebook_order ?? {};
			$noteOrder = state.note_order ?? {};
			if (state.sort_mode === 'created' || state.sort_mode === 'title' || state.sort_mode === 'modified' || state.sort_mode === 'custom') $sortMode = state.sort_mode;
			if (typeof state.group_notes_by_date === 'boolean') $groupNotesByDate = state.group_notes_by_date;
			if (state.tasks_layout === 'calendar' || state.tasks_layout === 'list') $tasksLayout = state.tasks_layout;
			if (typeof state.tasks_hide_completed === 'boolean') $tasksHideCompleted = state.tasks_hide_completed;
			if (typeof state.tasks_only_flagged === 'boolean') $tasksOnlyFlagged = state.tasks_only_flagged;
			if (state.tasks_sort === 'due' || state.tasks_sort === 'priority' || state.tasks_sort === 'note') $tasksSort = state.tasks_sort;
			lastNotePath = state.last_open_note ?? null;
			lastViewMode = state.last_view_mode ?? '';
			lastNotebook = state.last_notebook ?? null;
			lastTag = state.last_tag ?? null;
		} catch (_) {}
		if (!ownerIsCurrent()) return;

		const restoreLastSession = $appConfig?.restore_last_session === true;

		// Display the remembered note independently of the metadata scans.
		if (restoreLastSession && lastNotePath) void restoreLastNote(lastNotePath, startupToken);

		await Promise.allSettled([sidebar?.refresh(), noteList?.refresh()]);
		if (!ownerIsCurrent()) return;

		// Only the all-notes list is a global tag source on mobile.
		if (isMobile) {
			if ($viewMode === 'all') {
				const tagMap = new Map<string, number>();
				for (const note of $notes) {
					for (const tag of note.meta.tags) {
						tagMap.set(tag, (tagMap.get(tag) ?? 0) + 1);
					}
				}
				$tags = Array.from(tagMap.entries()).sort((a, b) => a[0].localeCompare(b[0]));
			} else {
				await sidebar?.refreshTags();
				if (!ownerIsCurrent()) return;
			}
		}

		const startupTarget = resolveStartupTarget({
			startupView: $appConfig?.startup_view,
			restoreLastSession,
			lastViewMode,
			lastNotebook,
			lastTag
		});
		if (navigationIsCurrent(startupToken)) {
			if (!(await applyStartupTarget(startupTarget)) && navigationIsCurrent(startupToken)) {
				await applyStartupTarget({ mode: normalizeStartupView($appConfig?.startup_view) });
			}
		}
		if (!ownerIsCurrent()) return;

		initialRefreshPending = false;
		if (fileRefreshPending) scheduleFileRefresh(refreshLists);

	// Startup state is ready; open any queued OS request.
		stateReady = true;
		drainPendingOpenFiles();

		// Scheduled backup: check on startup and every 5 minutes
		checkScheduledBackup();
		backupInterval = setInterval(checkScheduledBackup, 5 * 60 * 1000);


		persistState();
	});

	onDestroy(() => {
		disposed = true;
		for (const intent of navigationIntents) intent.cancel();
		workspace.dispose();
		void vaultSwitchRelease?.();
		if (persistTimer) clearTimeout(persistTimer);
		removePopstateListener?.();
		backupListeners.forEach((unlisten) => unlisten());
		backupListeners.clear();
		unlistenFileChange?.();
		unlistenVaultRestored?.();
		unlistenPendingOpen?.();
		unlistenClose?.();
		if (fileRefreshTimer) clearTimeout(fileRefreshTimer);
		unlistenOpenFile?.();
		removeMobileFlushListeners?.();
		if (backupInterval) clearInterval(backupInterval);

	});
</script>

<svelte:window onkeydown={handleKeydown} onmousedown={isMobile ? undefined : handleMouseDown} />

{#if isMobile}
	<!-- ═══ MOBILE LAYOUT ═══ -->
	<div class="mobile-shell">
		<!-- Mobile Header -->
		<div class="mobile-header">
			{#if $mobileView !== 'sidebar'}
				<button class="mobile-header-btn" onclick={mobileBack} aria-label="Go back">
					<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
						<path d="M15 18l-6-6 6-6"/>
					</svg>
				</button>
			{:else}
				<div class="mobile-header-brand">
					<img src="/ryuk-logo.png" alt="Ryuk's Notebook" width="22" height="22" />
				</div>
			{/if}
			{#if $mobileView !== 'editor'}
				<span class="mobile-header-title">
					{#if $mobileView === 'sidebar'}
						Ryuk's Notebook
					{:else}
						{#if $viewMode === 'notebook'}{$activeNotebook?.name ?? 'Notebook'}{:else if $viewMode === 'tag' && $activeTag}<TagLabel name={$activeTag} size={16} />{:else if $viewMode === 'quickaccess'}Quick Access{:else if $viewMode === 'daily'}Daily Notes{:else if $viewMode === 'tasks'}Tasks{:else if $viewMode === 'hermes'}Hermes{:else if $viewMode === 'trash'}Trash{:else}All Notes{/if}
					{/if}
				</span>
			{/if}
			<div class="mobile-header-actions">
				{#if $mobileView === 'editor'}
					<button class="mobile-header-btn" class:active={$readOnly} disabled={isTrashedNote($activeNotePath, ownerVault)} onclick={() => ($readOnly = !$readOnly)} title={$readOnly ? 'Edit' : 'View'}>
						<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
							{#if $readOnly}
								<path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z" />
								<circle cx="12" cy="12" r="3" />
							{:else}
								<path d="M11 4H4a2 2 0 00-2 2v14a2 2 0 002 2h14a2 2 0 002-2v-7" />
								<path d="M18.5 2.5a2.121 2.121 0 013 3L12 15l-4 1 1-4 9.5-9.5z" />
							{/if}
						</svg>
					</button>
					<button class="mobile-header-btn" onclick={() => editor?.openNoteSearch()} title="Find in note">
						<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
							<circle cx="11" cy="11" r="8"/><line x1="21" y1="21" x2="16.65" y2="16.65"/>
						</svg>
					</button>
					<button class="mobile-header-btn" class:active={$activeNote?.meta.pinned} onclick={() => { if ($activeNote) void updateActiveNoteMetadata({ pinned: !$activeNote.meta.pinned, tags: [...$activeNote.meta.tags] }); }} title="Pin">
						<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
							<path d="M12 17v5"/><path d="M9 2h6l-1 7h4l-2 4H8l-2-4h4L9 2z"/>
						</svg>
					</button>
					<button class="mobile-header-btn" class:active={isQuickAccess} onclick={async () => {
						if (!noteRelativePath) return;
						try {
							if (isQuickAccess) { await removeQuickAccess(ownerVault!, noteRelativePath); } else { await addQuickAccess(ownerVault!, noteRelativePath); }
							if (!ownerIsCurrent()) return;
							const qaNotes = await getQuickAccess(ownerVault!);
							if (ownerIsCurrent()) $quickAccessPaths = qaNotes.map(n => n.relative_path);
						} catch (e) { console.error('Quick access toggle failed:', e); }
					}} title="Quick Access">
						<svg width="18" height="18" viewBox="0 0 24 24" fill={isQuickAccess ? 'currentColor' : 'none'} stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
							<polygon points="12 2 15.09 8.26 22 9.27 17 14.14 18.18 21.02 12 17.77 5.82 21.02 7 14.14 2 9.27 8.91 8.26 12 2" />
						</svg>
					</button>
					<button class="mobile-header-btn" class:active={outlineOpen} onclick={() => editor?.toggleOutlinePanel()}
						title="Outline" aria-label="Outline" aria-expanded={outlineOpen} aria-controls={outlinePanelId || undefined}>
						<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
							<line x1="4" y1="6" x2="20" y2="6"/><line x1="8" y1="12" x2="20" y2="12"/><line x1="8" y1="18" x2="20" y2="18"/><circle cx="4" cy="12" r="1" fill="currentColor"/><circle cx="4" cy="18" r="1" fill="currentColor"/>
						</svg>
					</button>
					<button class="mobile-header-btn" onclick={() => editor?.toggleHistoryPanel()} title="History">
						<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
							<circle cx="12" cy="12" r="10"/><polyline points="12 6 12 12 16 14"/>
						</svg>
					</button>
					{#if $appConfig?.ai_provider}
					<button class="mobile-header-btn" onclick={() => editor?.triggerAiMenu()} title="AI Actions">
						<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
							<path d="M12 8V4l-2-2"/><rect x="4" y="8" width="16" height="12" rx="2"/><path d="M2 14h2"/><path d="M20 14h2"/><path d="M9 13v2"/><path d="M15 13v2"/>
						</svg>
					</button>
					{/if}
					<button class="mobile-header-btn" class:active={$sourceMode} onclick={() => ($sourceMode = !$sourceMode)} title={$sourceMode ? 'Rich Editor' : 'Source Mode'}>
						<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
							<polyline points="16 18 22 12 16 6" /><polyline points="8 6 2 12 8 18" />
						</svg>
					</button>
				{:else}
					<button class="mobile-header-btn" onclick={() => ($showSearch = true)} title="Search">
						<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
							<circle cx="11" cy="11" r="8" />
							<line x1="21" y1="21" x2="16.65" y2="16.65" />
						</svg>
					</button>
						<button class="mobile-header-btn" onclick={handleDailyNote} title="Daily Note">
						<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
							<rect x="3" y="4" width="18" height="18" rx="2" ry="2" />
							<line x1="16" y1="2" x2="16" y2="6" />
							<line x1="8" y1="2" x2="8" y2="6" />
							<line x1="3" y1="10" x2="21" y2="10" />
						</svg>
					</button>
					<button class="mobile-header-btn" onclick={() => ($showInfo = true)} title="Info">
						<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
							<circle cx="12" cy="12" r="10" />
							<line x1="12" y1="16" x2="12" y2="12" />
							<line x1="12" y1="8" x2="12.01" y2="8" />
						</svg>
					</button>
					<button class="mobile-header-btn" onclick={() => ($showSettings = true)} title="Settings">
						<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
							<circle cx="12" cy="12" r="3" />
							<path d="M19.4 15a1.65 1.65 0 00.33 1.82l.06.06a2 2 0 01-2.83 2.83l-.06-.06a1.65 1.65 0 00-1.82-.33 1.65 1.65 0 00-1 1.51V21a2 2 0 01-4 0v-.09A1.65 1.65 0 009 19.4a1.65 1.65 0 00-1.82.33l-.06.06a2 2 0 01-2.83-2.83l.06-.06A1.65 1.65 0 004.68 15a1.65 1.65 0 00-1.51-1H3a2 2 0 010-4h.09A1.65 1.65 0 004.6 9a1.65 1.65 0 00-.33-1.82l-.06-.06a2 2 0 012.83-2.83l.06.06A1.65 1.65 0 009 4.68a1.65 1.65 0 001-1.51V3a2 2 0 014 0v.09a1.65 1.65 0 001 1.51 1.65 1.65 0 001.82-.33l.06-.06a2 2 0 012.83 2.83l-.06.06A1.65 1.65 0 0019.4 9a1.65 1.65 0 001.51 1H21a2 2 0 010 4h-.09a1.65 1.65 0 00-1.51 1z" />
						</svg>
					</button>
				{/if}
			</div>
		</div>

		<!-- Mobile Content -->
		<div class="mobile-content">
			<div class="mobile-panel" class:active={$mobileView === 'sidebar'}>
				<Sidebar bind:this={sidebar} onViewChanged={handleViewChanged} onBeforeNoteChange={flushWorkspace} onBeforePathMutation={beginPaneMutation} onPathsMoved={handlePathsMoved} />
			</div>
			<div class="mobile-panel" class:active={$mobileView === 'notelist'}>
				<NoteList bind:this={noteList} onNoteSelected={handleNoteSelected} onActivateOpenNote={activateOpenNote} onBeforeNoteSwitch={flushDestination} onBeforeNoteDuplicate={flushWorkspace} onBeforePathMutation={beginPaneMutation} onPathsMoved={handlePathsMoved} onUpdateActiveNote={updateActiveNoteMetadata} onTagsChanged={handleTagsChanged} onRefreshTags={() => { if (!isMobile) void sidebar?.refreshTags(); }} onNoteMoved={() => sidebar?.refresh()} onNoteCreated={(token) => { if (navigationIsCurrent(token) && $workspace.activePaneId === token.paneId) editorFor(token)?.focusTitle(token.generation, token.focusIsCurrent); }} onToggleTask={toggleTask} onSetTaskPriority={changeTaskPriority} onSetTaskDue={changeTaskDue} />
			</div>
			<div class="mobile-panel" class:active={$mobileView === 'editor'}>
				<Editor bind:this={mobileEditor} onMoveToTrash={trashOpenNote} onOutlineChange={handleOutlineChange} onShowTag={showTagNotes} />
			</div>
		</div>

	</div>
{:else}
	<!-- ═══ DESKTOP LAYOUT ═══ -->
	<div class="app-shell">
		{#if $focusMode}
			<!-- svelte-ignore a11y_no_static_element_interactions -->
			<div class="focus-topbar" class:macos={isMac} onmousedown={(e) => { if (!(e.target as HTMLElement).closest('button')) appWindow.startDragging(); }}>
				<span class="focus-title">{$activeNote?.meta.title || 'Untitled'}</span>
				<div class="focus-controls">
					<button class="focus-btn focus-active" onclick={() => ($focusMode = false)} title="Exit focus mode (Escape)">
						<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
							<path d="M8 3v3a2 2 0 01-2 2H3m18 0h-3a2 2 0 01-2-2V3m0 18v-3a2 2 0 012-2h3M3 16h3a2 2 0 012 2v3"/>
						</svg>
					</button>
					<button class="focus-btn" class:focus-active={$readOnly} onclick={() => ($readOnly = !$readOnly)} title={$readOnly ? 'Switch to Edit Mode' : 'Switch to View Mode'}>
						<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
							{#if $readOnly}
								<path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z" />
								<circle cx="12" cy="12" r="3" />
							{:else}
								<path d="M17.94 17.94A10.07 10.07 0 0112 20c-7 0-11-8-11-8a18.45 18.45 0 015.06-5.94" />
								<path d="M9.9 4.24A9.12 9.12 0 0112 4c7 0 11 8 11 8a18.5 18.5 0 01-2.16 3.19" />
								<line x1="1" y1="1" x2="23" y2="23" />
							{/if}
						</svg>
					</button>
					{#if !isMac}
					<button class="focus-btn" onmousedown={(e) => e.stopPropagation()} onclick={() => appWindow.minimize()} title="Minimize">
						<svg width="10" height="10" viewBox="0 0 10 10"><line x1="1" y1="5" x2="9" y2="5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/></svg>
					</button>
					<button class="focus-btn" onmousedown={(e) => e.stopPropagation()} onclick={() => appWindow.toggleMaximize()} title="Maximize">
						<svg width="10" height="10" viewBox="0 0 10 10"><rect x="1" y="1" width="8" height="8" rx="1" fill="none" stroke="currentColor" stroke-width="1.2"/></svg>
					</button>
					<button class="focus-btn focus-close" onmousedown={(e) => e.stopPropagation()} onclick={() => appWindow.close()} title="Close">
						<svg width="10" height="10" viewBox="0 0 10 10"><line x1="1.5" y1="1.5" x2="8.5" y2="8.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/><line x1="8.5" y1="1.5" x2="1.5" y2="8.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/></svg>
					</button>
					{/if}
				</div>
			</div>
		{:else}
			<TitleBar onNewNote={createAndFocusNote} onDailyNote={handleDailyNote} onSelectNote={selectNoteFromSwitcher} onBeforeVaultSwitch={prepareVaultSwitch} />
		{/if}
		<div class="app-layout">
			{#if !$focusMode}
				<div class="sidebar-panel" style="width: {$sidebarCollapsed ? 44 : $sidebarWidth}px">
					<Sidebar bind:this={sidebar} onViewChanged={handleViewChanged} onBeforeNoteChange={flushWorkspace} onBeforePathMutation={beginPaneMutation} onPathsMoved={handlePathsMoved} />
				</div>

				{#if !$sidebarCollapsed}
					<ResizeHandle onResize={handleSidebarResize} />
				{/if}

				{#if !$notelistCollapsed}
					<div class="notelist-panel" style="width: {$notelistWidth}px">
						<NoteList bind:this={noteList} onNoteSelected={handleNoteSelected} onActivateOpenNote={activateOpenNote} onBeforeNoteSwitch={flushDestination} onBeforeNoteDuplicate={flushWorkspace} onBeforePathMutation={beginPaneMutation} onPathsMoved={handlePathsMoved} onUpdateActiveNote={updateActiveNoteMetadata} onTagsChanged={handleTagsChanged} onRefreshTags={() => { if (!isMobile) void sidebar?.refreshTags(); }} onNoteMoved={() => sidebar?.refresh()} onNoteCreated={(token) => { if (navigationIsCurrent(token) && $workspace.activePaneId === token.paneId) editorFor(token)?.focusTitle(token.generation, token.focusIsCurrent); }} onToggleTask={toggleTask} onSetTaskPriority={changeTaskPriority} onSetTaskDue={changeTaskDue} />
					</div>

					<ResizeHandle onResize={handleNotelistResize} />
				{:else}
					<div class="notelist-restore-panel">
						<button class="notelist-restore-btn" onclick={revealNoteList} title={`Show notes list (${isMac ? '⌘' : 'Ctrl'}+Shift+\\)`} aria-label="Show notes list">
							<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
								<line x1="4" y1="6" x2="16" y2="6" />
								<line x1="4" y1="12" x2="16" y2="12" />
								<line x1="4" y1="18" x2="16" y2="18" />
								<polyline points="19 8 21 12 19 16" />
							</svg>
						</button>
					</div>
				{/if}
			{/if}

			<div class="editor-panel">
				<div class="workspace-toolbar">
					<span>{$workspace.panes.length === 2 ? 'Split view' : 'Editor'}</span>
					<button class:active={$workspace.panes.length === 2} disabled={splitBusy || $workspaceBusy}
						onclick={toggleSplit} aria-pressed={$workspace.panes.length === 2}
						title={$workspace.panes.length === 2 ? 'Close second pane' : 'Open second pane'}>
						<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" aria-hidden="true"><rect x="3" y="4" width="18" height="16" rx="2" /><path d="M12 4v16" /></svg>
						{$workspace.panes.length === 2 ? 'Close second pane' : 'Split view'}
					</button>
				</div>
				{#if splitError}<div class="split-error" role="alert">{splitError}</div>{/if}
				<div class="editor-workspace" bind:clientWidth={editorAreaWidth}>
					{#each $workspace.panes as pane, index (pane.id)}
						{#if index === 1}
							<div class="split-separator" bind:clientWidth={separatorWidth}>
							<ResizeHandle onResize={resizeSplit} label="Resize editor panes"
								value={Math.round($workspace.splitRatio * 100)} min={Math.round(splitBounds[0] * 100)} max={Math.round(splitBounds[1] * 100)}
								controls={`${workspaceId}-${$workspace.panes[0].id} ${workspaceId}-${pane.id}`} />
							</div>
						{/if}
						<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
						<section class="editor-pane" class:active={$workspace.activePaneId === pane.id}
							id={`${workspaceId}-${pane.id}`} aria-label={`Editor pane ${index + 1}`}
							style:flex={$workspace.panes.length === 1 ? '1 1 0' : `${index === 0 ? $workspace.splitRatio : 1 - $workspace.splitRatio} 1 0`}
							onpointerdown={() => activatePane(pane.id)} onfocusin={() => activatePane(pane.id)}>
							{#if $workspace.panes.length === 2}
								<button class="pane-label" aria-pressed={$workspace.activePaneId === pane.id} onclick={() => activatePane(pane.id)}>Pane {index + 1}</button>
							{/if}
							<EditorPaneHost pane={pane.context} paneId={pane.id} onEditorChange={(instance) => { editorInstances = { ...editorInstances, [pane.id]: instance }; workspace.registerEditor(pane.id, instance); }}
								onMoveToTrash={trashOpenNote} onShowTag={showTagNotes} showTaskPlaceholder={$viewMode === 'tasks' && !taskNoteOpened} />
						</section>
					{/each}
				</div>
			</div>
		</div>
	</div>
{/if}

<SearchPanel onSelectNote={selectNoteFromSearch} />
<CommandPalette onNavigate={handleViewChanged} />
<OperationFeedback />
<RecoveryInbox notice={recoveryBarrierNotice} />
<SettingsPanel onBeforeVaultSwitch={prepareVaultSwitch} onBeginMaintenance={() => beginPaneMutation()} />
<InfoPanel />

<style>
	/* ═══ DESKTOP STYLES ═══ */
	.app-shell {
		display: flex;
		flex-direction: column;
		height: 100vh;
		overflow: hidden;
	}

	.app-layout {
		display: flex;
		flex: 1;
		overflow: hidden;
	}

	.sidebar-panel {
		flex-shrink: 0;
		height: 100%;
		overflow: hidden;
	}

	.notelist-panel {
		flex-shrink: 0;
		height: 100%;
		overflow: hidden;
	}

	.notelist-restore-panel {
		flex-shrink: 0;
		width: 36px;
		height: 100%;
		padding-top: 8px;
		display: flex;
		justify-content: center;
		background: var(--bg-primary);
		border-right: 1px solid var(--border-color);
	}

	.notelist-restore-btn {
		width: 24px;
		height: 24px;
		display: flex;
		align-items: center;
		justify-content: center;
		padding: 0;
		border: none;
		border-radius: 4px;
		background: transparent;
		color: var(--text-tertiary);
		cursor: pointer;
	}

	.notelist-restore-btn:hover {
		background: var(--bg-hover);
		color: var(--text-primary);
	}

	.editor-panel {
		flex: 1;
		height: 100%;
		overflow: hidden;
		min-width: 0;
		position: relative;
	}

	.editor-panel { display: flex; flex-direction: column; min-width: 0; }
	.workspace-toolbar { display: flex; align-items: center; justify-content: space-between; gap: 8px; padding: 5px 10px; border-bottom: 1px solid var(--border-color); background: var(--bg-secondary); color: var(--text-tertiary); font-size: 11px; }
	.workspace-toolbar button { display: flex; align-items: center; gap: 6px; padding: 4px 8px; border: none; border-radius: 6px; background: var(--bg-hover); color: var(--text-secondary); cursor: pointer; }
	.workspace-toolbar button:hover, .workspace-toolbar button.active { color: var(--accent); background: var(--accent-light); }
	.workspace-toolbar button:focus-visible, .pane-label:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
	.workspace-toolbar button:disabled { opacity: 0.5; cursor: default; }
	.split-error { padding: 6px 10px; color: var(--text-primary); background: var(--bg-secondary); font-size: 12px; }
	.editor-workspace { display: flex; flex: 1; min-width: 0; min-height: 0; overflow: hidden; }
	.split-separator { display: flex; flex-shrink: 0; }
	.editor-pane { display: flex; flex-direction: column; min-width: 0; min-height: 0; overflow: hidden; position: relative; }
	.editor-pane.active { box-shadow: inset 0 2px var(--accent); }
	.pane-label { flex-shrink: 0; border: none; border-bottom: 1px solid var(--border-color); background: var(--bg-secondary); color: var(--text-tertiary); padding: 4px 8px; text-align: left; font-size: 11px; cursor: pointer; }
	.editor-pane.active .pane-label { color: var(--accent); }

	.focus-topbar {
		display: flex;
		align-items: center;
		height: 34px;
		background: var(--bg-secondary);
		border-bottom: 1px solid var(--border-light);
		user-select: none;
		flex-shrink: 0;
		padding: 0 8px 0 16px;
	}

	.focus-title {
		flex: 1;
		font-size: 12px;
		color: var(--text-tertiary);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		pointer-events: none;
	}

	.focus-controls {
		display: flex;
		align-items: center;
		gap: 2px;
	}

	.focus-btn {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 32px;
		height: 26px;
		border: none;
		background: none;
		color: var(--text-tertiary);
		cursor: pointer;
		border-radius: 4px;
		transition: background 0.1s, color 0.1s;
	}

	.focus-btn.focus-active {
		color: var(--text-accent);
		background: var(--accent-light);
	}

	.focus-btn:hover {
		background: var(--bg-hover);
		color: var(--text-primary);
	}

	.focus-close:hover {
		background: #e81123;
		color: white;
	}

	.focus-topbar.macos {
		padding-left: 78px;
	}

	/* ═══ MOBILE STYLES ═══ */
	.mobile-shell {
		display: flex;
		flex-direction: column;
		height: 100dvh;
		box-sizing: border-box;
		padding:
			env(safe-area-inset-top, 0px)
			env(safe-area-inset-right, 0px)
			env(safe-area-inset-bottom, 0px)
			env(safe-area-inset-left, 0px);
		overflow: hidden;
		background: var(--bg-primary);
	}

	.mobile-header {
		display: flex;
		align-items: center;
		height: 52px;
		padding: 0 8px;
		background: var(--bg-secondary);
		border-bottom: 1px solid var(--border-color);
		flex-shrink: 0;
		gap: 4px;
	}

	.mobile-header-brand {
		display: flex;
		align-items: center;
		padding: 0 8px;
	}

	.mobile-header-title {
		flex: 1;
		display: flex;
		align-items: center;
		min-width: 0;
		font-size: 17px;
		font-weight: 600;
		color: var(--text-primary);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		padding: 0 4px;
	}

	.mobile-header-actions {
		display: flex;
		align-items: center;
		gap: 2px;
		margin-left: auto;
	}

	.mobile-header-btn {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 34px;
		height: 34px;
		border: none;
		background: none;
		color: var(--text-secondary);
		border-radius: 8px;
		cursor: pointer;
	}

	.mobile-header-btn svg {
		width: 16px;
		height: 16px;
	}

	.mobile-header-btn:active {
		background: var(--bg-hover);
	}

	.mobile-header-btn.active {
		color: var(--accent);
	}

	.mobile-content {
		flex: 1;
		overflow: hidden;
		position: relative;
	}

	.mobile-panel {
		position: absolute;
		inset: 0;
		overflow: hidden;
		visibility: hidden;
		pointer-events: none;
		display: flex;
		flex-direction: column;
	}

	.mobile-panel.active {
		visibility: visible;
		pointer-events: auto;
	}

</style>
