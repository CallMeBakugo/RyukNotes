interface VaultChanges {
  revision: number;
  pending: number;
  choices: Map<string, number>;
  tails: Map<string, Promise<void>>;
}

/** Keep old refresh snapshots and late choices from replacing committed appearances. */
export class AppearanceMutations {
  private vaults = new Map<string, VaultChanges>();

  private state(vault: string): VaultChanges {
    let state = this.vaults.get(vault);
    if (!state) {
      state = { revision: 0, pending: 0, choices: new Map(), tails: new Map() };
      this.vaults.set(vault, state);
    }
    return state;
  }

  snapshot(vault: string): number {
    return this.state(vault).revision;
  }

  acceptsSnapshot(vault: string, revision: number): boolean {
    const state = this.state(vault);
    return state.pending === 0 && state.revision === revision;
  }

  choose(vault: string, key: string): () => boolean {
    const choices = this.state(vault).choices;
    const choice = (choices.get(key) ?? 0) + 1;
    choices.set(key, choice);
    return () => choices.get(key) === choice;
  }

  async run<T>(
    vault: string,
    key: string,
    canRun: () => boolean,
    operation: () => Promise<T>,
    publish: (result: T) => void,
  ): Promise<void> {
    const state = this.state(vault);
    state.revision++;
    state.pending++;
    const previous = state.tails.get(key) ?? Promise.resolve();
    const work = previous.catch(() => {}).then(async () => {
      if (!canRun()) return;
      const result = await operation();
      // Publish committed state in queue order, including when a later choice
      // fails or a later file dialog is cancelled.
      publish(result);
    });
    state.tails.set(key, work);
    try {
      await work;
    } finally {
      state.pending--;
      state.revision++;
      if (state.tails.get(key) === work) state.tails.delete(key);
    }
  }
}

export const notebookAppearance = new AppearanceMutations();
export const tagAppearance = new AppearanceMutations();
