import { writable } from 'svelte/store';

// One line from the native open receipt, retained across startup/picker unmount.
// It carries no pane focus or note state and is never restored as a draft.
export const vaultOpenStatus = writable('');
export function recordVaultOpenStatus(status: string | null): void {
  vaultOpenStatus.set(status ?? '');
}
