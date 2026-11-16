import { invoke } from '@tauri-apps/api/core';
import { emitTo, listen } from '@tauri-apps/api/event';
import type { ActionDef } from '../keybindings';

export const MAIN_WINDOW_ACTIONS = new Set(['new-note','quick-open','search-vault','toggle-sidebar','toggle-notelist','toggle-focus','nav-back','nav-forward']);
export interface ShortcutRequest { id: string; vault: string; action: string; replyWindow: string; origin?: {path: string; file: string} }
export async function requestMainShortcut(action: string, vault: string, replyWindow: string, origin?: {path: string; file: string}): Promise<void> {
  if (!MAIN_WINDOW_ACTIONS.has(action) || !replyWindow.startsWith('note-')) throw new Error('Unsupported main-window shortcut');
  const id = crypto.randomUUID();
  let timer: ReturnType<typeof setTimeout> | undefined;
  let finish: (error?: Error) => void = () => {};
  const acknowledgment = new Promise<void>((resolve,reject) => { finish = error => error ? reject(error) : resolve(); });
  const unlisten = await listen<{id: string; error?: string}>('main-shortcut-result', ({payload}) => {
    if (payload.id === id) finish(payload.error ? new Error(payload.error) : undefined);
  });
  try {
    timer = setTimeout(() => finish(new Error('The main window did not acknowledge this shortcut.')),10_000);
    await Promise.all([emitTo('main','main-shortcut-request',{id,vault,action,replyWindow,origin} satisfies ShortcutRequest), acknowledgment]);
  } finally { clearTimeout(timer); unlisten(); }
}
export async function receiveMainShortcut(
  request: ShortcutRequest, vault: string | null, ready: () => boolean,
  focus: () => Promise<void>, execute: (action: ActionDef['id']) => unknown,
): Promise<void> {
  if (!request || typeof request.id !== 'string' || typeof request.replyWindow !== 'string' || !request.replyWindow.startsWith('note-')) return;
  let error: string | undefined;
  try {
    if (!ready() || request.vault !== vault || !MAIN_WINDOW_ACTIONS.has(request.action)) throw new Error('Open the matching vault in the main window before using this shortcut.');
    const checkOrigin = async () => {
      if (!request.origin) return;
      const current = await invoke<{path: string; file: string}>('db_writer_identity',{vaultPath:vault});
      if (current.path !== request.origin.path || current.file !== request.origin.file) throw new Error('The originating vault was replaced. Reopen this note window.');
    };
    await checkOrigin();
    await focus();
    if (!ready()) throw new Error('The main window changed while the shortcut was opening.');
    await checkOrigin();
    if (!ready()) throw new Error('The main window changed while checking the originating vault.');
    await execute(request.action);
  } catch (failure) { error = failure instanceof Error ? failure.message : String(failure); }
  await emitTo(request.replyWindow,'main-shortcut-result',{id:request.id,...(error ? {error} : {})});
}
