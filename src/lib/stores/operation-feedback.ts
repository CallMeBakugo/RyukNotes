import { writable } from 'svelte/store';
export interface OperationMessage { id: number; text: string; kind: 'success' | 'error' }
export const operationMessage = writable<OperationMessage | null>(null);
let sequence = 0;
export function reportOperationError(action: string, error: unknown): void {
  const detail = error instanceof Error ? error.message
    : error && typeof error === 'object' && 'message' in error ? String(error.message) : String(error);
  operationMessage.set({ id: ++sequence, kind: 'error', text: `${action}: ${detail}` });
}
export function reportOperationSuccess(text: string): void {
  operationMessage.set({ id: ++sequence, kind: 'success', text });
}
export function dismissOperation(id: number): void {
  operationMessage.update(current => current?.id === id ? null : current);
}
