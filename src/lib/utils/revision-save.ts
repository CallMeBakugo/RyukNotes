interface SaveSession {
  revision: number;
  savedRevision: number;
}

export interface SaveRevision {
  readonly session: SaveSession;
  readonly revision: number;
}

/** Serializes immutable save snapshots without letting an old completion mark newer edits clean. */
export class RevisionSaveQueue {
  private session: SaveSession = { revision: 0, savedRevision: 0 };
  private tail: Promise<void> = Promise.resolve();
  private idleTimer: ReturnType<typeof setTimeout> | null = null;
  private maxWaitTimer: ReturnType<typeof setTimeout> | null = null;

  startSession(): void {
    this.cancelScheduled();
    this.session = { revision: 0, savedRevision: 0 };
  }

  markChanged(): void {
    this.session.revision++;
  }

  schedule(save: () => void): void {
    if (this.idleTimer !== null) clearTimeout(this.idleTimer);
    const session = this.session;
    const run = () => {
      this.cancelScheduled();
      if (session === this.session) save();
    };
    this.idleTimer = setTimeout(run, 3_000);
    if (this.maxWaitTimer === null) this.maxWaitTimer = setTimeout(run, 10_000);
  }

  cancelScheduled(): void {
    if (this.idleTimer !== null) clearTimeout(this.idleTimer);
    if (this.maxWaitTimer !== null) clearTimeout(this.maxWaitTimer);
    this.idleTimer = null;
    this.maxWaitTimer = null;
  }

  capture(): SaveRevision {
    return { session: this.session, revision: this.session.revision };
  }

  isCurrentSession(snapshot: SaveRevision): boolean {
    return snapshot.session === this.session;
  }

  isCurrent(snapshot: SaveRevision): boolean {
    return this.isCurrentSession(snapshot) && snapshot.revision === this.session.revision;
  }

  get dirty(): boolean {
    return this.session.revision !== this.session.savedRevision;
  }

  save(snapshot: SaveRevision, persist: () => Promise<void>): Promise<boolean> {
    const queued = this.tail.then(async () => {
      // An explicit flush may capture the same revision as an already queued autosave.
      if (snapshot.revision > snapshot.session.savedRevision) {
        await persist();
        snapshot.session.savedRevision = snapshot.revision;
      }
      return this.isCurrent(snapshot);
    });
    // Recover the queue, not the caller's result: a failed save stays dirty and remains retryable.
    this.tail = queued.then(() => {}, () => {});
    return queued;
  }

  async idle(): Promise<void> {
    await this.tail;
  }
}
