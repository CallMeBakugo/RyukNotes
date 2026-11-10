export interface VaultJobResult {
  vault_path: string;
  success: boolean;
  error?: string;
}

/** Await completion of native maintenance that returns before its worker finishes.
 * The operation owns this listener: hiding a panel must not release its pane locks. */
export async function waitForVaultJob<T extends VaultJobResult>(
  vault: string,
  subscribe: (handler: (result: T) => void) => Promise<() => void>,
  start: () => Promise<void>,
): Promise<T> {
  let stop: (() => void) | undefined;
  let started = false;
  try {
    return await new Promise<T>((resolve, reject) => {
      subscribe(result => {
        if (!started || result.vault_path !== vault) return;
        if (result.success) resolve(result);
        else reject(new Error(result.error ?? 'Vault maintenance failed.'));
      }).then(unlisten => {
        stop = unlisten;
        started = true;
        void start().catch(reject);
      }).catch(reject);
    });
  } finally { stop?.(); }
}
