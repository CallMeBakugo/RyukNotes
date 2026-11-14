// Retain the exact request on failure: retry must never mint another operation.
export type RecoveryMode = 'full_restore' | 'independent_copy' | 'reconcile' | 'raw_recovery';
export interface RecoveryRequest { mode: RecoveryMode; source: string; destination: string; operationId: string }
export function createPortableRecovery(deps: {
  begin: () => Promise<null | (() => Promise<void>)>;
  restore: (request: RecoveryRequest) => Promise<unknown>;
}) {
  let pending: RecoveryRequest | null = null;
  let busy = false;
  return {
    get pending() { return pending; },
    async run(input?: Omit<RecoveryRequest, 'operationId'>) {
      if (busy) throw new Error('Recovery is already running.');
      if (input) pending = { ...input, operationId: crypto.randomUUID() };
      if (!pending) throw new Error('Choose a recovery source and destination.');
      busy = true;
      let release: null | (() => Promise<void>) = null;
      try {
        release = await deps.begin();
        if (!release) throw new Error('Save or resolve drafts in every window before recovery.');
        const request = { ...pending };
        const result = await deps.restore(request);
        const finish = release;
        release = null;
        await finish();
        pending = null;
        return { request, result };
      } finally { try { await release?.(); } finally { busy = false; } }
    },
  };
}
