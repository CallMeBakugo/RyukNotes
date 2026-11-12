import type { NoteContent, NoteMeta } from '../types';
import { ExactResult, planMerge, constructCandidate, type MergePlan, type Side, type Candidate } from './three-way-merge.ts';

export interface ReviewedNote { available: boolean; reason?: string; reviewId?: string; revision?: number; base?: NoteContent; local?: NoteContent; remote?: NoteContent; previewReferences?: readonly string[] }
export interface Proposal { body: string; title: string; tags: string[]; pinned: boolean }
export type MetadataField = 'title' | 'tags' | 'pinned';
export type MetadataChoice = Side | 'union';
export interface MetadataDecision { field: MetadataField; conflict: boolean; choice: MetadataChoice | null }
export interface ReviewState {
  visible: boolean; status: 'closed' | 'loading' | 'ready' | 'stale' | 'applying' | 'unavailable'; message: string;
  reviewed: Readonly<ReviewedNote> | null; plan: MergePlan | null; choices: Readonly<Record<string, Side>>;
  metadata: readonly MetadataDecision[]; result: string; manual: boolean; built: boolean; mapping: Candidate['mapping'] | null;
  journalError: boolean; version: number;
}
export interface ReviewDependencies {
  open(): Promise<ReviewedNote>;
  checkpoint(id: string, sequence: number, proposal: Proposal): Promise<void>;
  cancel(id: string): Promise<void>;
  asset?(id: string, reference: string): Promise<{ mime: string; data: number[] }>;
  apply(id: string, revision: number, sequence: number): Promise<NoteContent>;
  localRevision(): number;
  ownerIsCurrent(): boolean;
  adopt(note: NoteContent, revision: number): Promise<boolean>;
}
function freeze<T>(value: T): T {
  if (value && typeof value === 'object') { Object.values(value).forEach(freeze); Object.freeze(value); }
  return value;
}
function equal(a: unknown, b: unknown): boolean {
  if (Array.isArray(a) && Array.isArray(b)) return JSON.stringify([...new Set(a)].sort()) === JSON.stringify([...new Set(b)].sort());
  return a === b;
}
function decisions(base: NoteMeta, local: NoteMeta, remote: NoteMeta): MetadataDecision[] {
  return (['title', 'tags', 'pinned'] as const).map(field => {
    const l = !equal(base[field], local[field]), r = !equal(base[field], remote[field]);
    const conflict = l && r && !equal(local[field], remote[field]);
    return { field, conflict, choice: conflict ? null : l ? 'local' : 'remote' };
  });
}
/** Window/pane ownership remains in dependencies; no global active note or focus.
 * Opening/canceling/invalidating cannot acknowledge raw or mutate a live document. */
export class ConflictReviewModel {
  state: ReviewState = { visible: false, status: 'closed', message: '', reviewed: null, plan: null, choices: {}, metadata: [], result: '', manual: false, built: false, mapping: null, journalError: false, version: 0 };
  private listeners = new Set<(state: ReviewState) => void>();
  private generation = 0;
  private artifact = new ExactResult('');
  private sequence = 0;
  private durable = 0;
  private durableSequence = 0;
  private deps: ReviewDependencies;
  private edited = false;
  private tail: Promise<void> = Promise.resolve();
  private timer: ReturnType<typeof setTimeout> | undefined;
  private pending = new Map<string, Promise<boolean>>();
  private closing: Promise<boolean> | null = null;
  constructor(deps: ReviewDependencies) { this.deps = deps; }
  subscribe(run: (state: ReviewState) => void): () => void { this.listeners.add(run); run(this.state); return () => this.listeners.delete(run); }
  private emit(patch: Partial<ReviewState> = {}): void { this.state = { ...this.state, ...patch }; for (const run of this.listeners) run(this.state); }
  get canApply(): boolean { return this.state.status === 'ready' && this.state.built && this.complete && this.deps.ownerIsCurrent() && this.deps.localRevision() === this.state.reviewed?.revision; }
  get complete(): boolean { return !!this.state.plan && this.state.plan.hunks.every(h => h.kind !== 'overlapping' || !!this.state.choices[h.id]) && this.state.metadata.every(m => m.choice !== null); }
  async open(renew = false): Promise<void> {
    if (this.closing) await this.closing;
    if (this.state.status === 'applying') return;
    if (this.state.reviewed?.reviewId && !renew) { this.emit({ visible: true }); return; }
    let previous = this.edited ? this.artifact.raw : null;
    if (this.state.reviewed?.reviewId) {
      if (!(await this.preserve())) return;
      try { await this.deps.cancel(this.state.reviewed.reviewId); }
      catch { this.emit({ journalError: true, message: 'The previous result remains preserved; retry closing it before renewing review.' }); return; }
      this.emit({ reviewed: null, plan: null });
    }
    const generation = ++this.generation;
    this.emit({ visible: true, status: 'loading', message: '', built: false });
    try {
      const reviewed = freeze(structuredClone(await this.deps.open()));
      if (generation !== this.generation || !this.deps.ownerIsCurrent()) {
        if (reviewed.reviewId) await this.deps.cancel(reviewed.reviewId);
        return;
      }
      if (!reviewed.available || !reviewed.base || !reviewed.local || !reviewed.remote || !reviewed.reviewId) {
        this.emit({ reviewed, status: 'unavailable', plan: null, message: reviewed.reason ?? 'No verified common base. Use Keep both.' }); return;
      }
      this.emit({ reviewed });
      const plan = planMerge(reviewed.base.content, reviewed.local.content, reviewed.remote.content);
      // Result edits that arrived during cancel/open awaits are still ours.
      // Capture the latest artifact, never the value from before an await.
      previous = this.edited ? this.artifact.raw : previous;
      this.artifact = new ExactResult(previous ?? reviewed.local.content);
      this.sequence = 0; this.durable = 0; this.durableSequence = 0; this.edited = previous !== null;
      this.emit({ reviewed, plan, choices: {}, metadata: decisions(reviewed.base.meta, reviewed.local.meta, reviewed.remote.meta),
        result: this.artifact.raw, manual: previous !== null, mapping: null, version: 0, journalError: false,
        status: this.deps.localRevision() === reviewed.revision ? 'ready' : 'stale',
        message: previous !== null ? 'Your previous result is preserved. Review the new choices before using it.' : '' });
      if (previous !== null) { this.changed(); await this.preserve(); }
    } catch {
      if (generation === this.generation) this.emit({ status: 'unavailable', message: 'Review cannot establish exact bounded comparisons. Your draft is unchanged; use Keep both.' });
    }
  }
  invalidate(message = 'The saved version changed again. Your result is preserved. Renew review or Keep both.'): void {
    if (['ready', 'stale', 'applying'].includes(this.state.status)) { this.generation++; this.emit({ status: this.state.status === 'applying' ? 'applying' : 'stale', message, built: false }); }
  }
  async preview(reference: string): Promise<{ mime: string; data: number[] }> {
    const id = this.state.reviewed?.reviewId;
    if (!id || !this.deps.asset || !this.deps.ownerIsCurrent()) throw new Error('No owned review asset');
    const image = await this.deps.asset(id, reference);
    if (id !== this.state.reviewed?.reviewId || !this.state.visible || !this.deps.ownerIsCurrent()) throw new Error('Stale review asset');
    return image;
  }
  observe(note: NoteContent): void {
    const remote = this.state.reviewed?.remote;
    if (remote && (note.meta.accepted?.token !== remote.meta.accepted?.token || note.path !== remote.path)) this.invalidate();
  }
  choose(id: string, side: Side): void {
    if (this.state.status === 'applying') return;
    this.emit({ choices: { ...this.state.choices, [id]: side }, built: false });
  }
  chooseMetadata(field: MetadataField, choice: MetadataChoice): void {
    if (this.state.status === 'applying') return;
    if (choice === 'union' && field !== 'tags') throw new Error('Union is only available for tags.');
    this.emit({ metadata: this.state.metadata.map(m => m.field === field ? { ...m, choice } : m), built: false });
  }
  private proposal(): Proposal {
    const reviewed = this.state.reviewed!;
    const meta = { ...reviewed.local!.meta };
    for (const decision of this.state.metadata) {
      const choice = decision.choice;
      if (!choice) continue; // An unready artifact may be recovered, never applied.
      if (decision.field === 'tags') meta.tags = choice === 'union'
        ? [...new Set([...reviewed.local!.meta.tags, ...reviewed.remote!.meta.tags])].sort()
        : [...reviewed[choice]!.meta.tags];
      else if (decision.field === 'title') meta.title = reviewed[choice as Side]!.meta.title;
      else meta.pinned = reviewed[choice as Side]!.meta.pinned;
    }
    return { body: this.artifact.raw, title: meta.title, tags: meta.tags, pinned: meta.pinned };
  }
  build(useEdited = false): void {
    if (!this.complete || this.state.status !== 'ready') return;
    let mapping: Candidate['mapping'] | null = null;
    if (!useEdited) {
      const candidate = constructCandidate(this.state.plan!, this.state.choices);
      this.artifact = new ExactResult(candidate.text); mapping = candidate.mapping;
    }
    this.emit({ result: this.artifact.raw, manual: useEdited, mapping, built: true }); this.changed();
  }
  edit(changes: readonly { from: number; to: number; insert: string }[]): void {
    if (!this.state.visible || this.state.status === 'applying') return;
    try { this.artifact.edit(changes); }
    catch {
      this.emit({ result: this.artifact.raw, manual: true, mapping: null, built: false, status: 'stale',
        message: 'This result exceeds exact comparison limits or contains unsupported Unicode. It remains in memory; correct it and renew review, or preserve a recovery copy.' });
      this.changed(); return;
    }
    this.emit({ result: this.artifact.raw, manual: true, mapping: null }); this.changed();
  }
  private changed(): void {
    this.edited = true;
    this.emit({ version: this.state.version + 1 });
    clearTimeout(this.timer);
    this.timer = setTimeout(() => { void this.preserve(); }, 250);
  }
  preserve(): Promise<boolean> {
    clearTimeout(this.timer);
    if (!this.edited) return Promise.resolve(true);
    if (!this.state.reviewed?.reviewId) return Promise.resolve(this.durable >= this.state.version);
    const version = this.state.version, id = this.state.reviewed.reviewId;
    if (this.durable >= version) return Promise.resolve(true);
    const key = `${id}:${version}`;
    const pending = this.pending.get(key);
    if (pending) return pending;
    const proposal = this.proposal(), sequence = ++this.sequence;
    const job = this.tail.then(() => this.deps.checkpoint(id, sequence, proposal));
    this.tail = job.catch(() => {});
    const result = job.then(() => {
      if (this.state.reviewed?.reviewId === id) {
        this.durable = Math.max(this.durable, version); this.durableSequence = Math.max(this.durableSequence, sequence);
        this.emit({ journalError: false });
      }
      return this.state.reviewed?.reviewId === id && this.state.version === version;
    }).catch(() => {
      if (this.state.reviewed?.reviewId === id) this.emit({ journalError: true, message: 'Result recovery failed. Your result remains in memory; keep this editor open and retry preservation.' });
      return false;
    }).finally(() => { this.pending.delete(key); });
    this.pending.set(key, result); return result;
  }
  close(): Promise<boolean> {
    if (this.closing) return this.closing;
    const closing = this.closeNow().finally(() => { this.closing = null; });
    this.closing = closing; return closing;
  }
  private async closeNow(): Promise<boolean> {
    if (this.state.status === 'applying') return false;
    this.generation++; // Fences an open response arriving after cancellation.
    const version = this.state.version, id = this.state.reviewed?.reviewId;
    const durable = await this.preserve();
    const closed = durable && version === this.state.version;
    // No additional await between the final result-version check and hiding
    // its editor. Ticket cleanup cannot race newer result typing into disposal.
    if (closed && id) void this.deps.cancel(id).catch(() => {});
    this.emit({ visible: false });
    if (closed) this.emit({ reviewed: null, plan: null, status: 'closed' });
    return closed;
  }
  private async retainResult(): Promise<boolean> {
    // Apply may have committed before a reply/retirement failed. Publish a new
    // unresolved artifact generation even if the old generation was tombstoned.
    this.changed(); return this.preserve();
  }
  async apply(): Promise<boolean> {
    if (!this.canApply) return false;
    const reviewed = this.state.reviewed!, generation = this.generation, version = this.state.version;
    this.emit({ status: 'applying', message: '' });
    if (!(await this.preserve())) { this.emit({ status: 'stale' }); return false; }
    if (generation !== this.generation || version !== this.state.version || !this.deps.ownerIsCurrent() || this.deps.localRevision() !== reviewed.revision) {
      this.emit({ status: 'stale', message: 'Your draft changed. The result is preserved; renew review.' }); return false;
    }
    try {
      const receipt = await this.deps.apply(reviewed.reviewId!, reviewed.revision!, this.durableSequence);
      if (generation !== this.generation || !this.deps.ownerIsCurrent() || this.deps.localRevision() !== reviewed.revision) {
        await this.retainResult();
        this.emit({ status: 'stale', message: 'The captured result was committed, but a newer draft remains here.' }); return false;
      }
      if (!(await this.deps.adopt(receipt, reviewed.revision!)) || generation !== this.generation || !this.deps.ownerIsCurrent() || this.deps.localRevision() !== reviewed.revision) {
        await this.retainResult();
        this.emit({ status: 'stale', message: 'The result committed. Recovery retirement needs retry; your draft remains here.' }); return false;
      }
      // Ticket cleanup is advisory after exact commit + local retirement. It
      // must not introduce another await between the final revision check and
      // adoption; owner unregister also removes abandoned ticket memory.
      void this.deps.cancel(reviewed.reviewId!).catch(() => {});
      this.edited = false;
      this.emit({ visible: false, status: 'closed', reviewed: null, plan: null }); return true;
    } catch {
      const durable = await this.retainResult();
      this.emit({ status: 'stale', built: false, message: durable
        ? 'Apply could not accept the reviewed state. Your edited result is preserved. Renew review or Keep both.'
        : 'Apply was not adopted and result recovery failed. Your result is still in memory; keep this editor open and retry preservation.' }); return false;
    }
  }
}
