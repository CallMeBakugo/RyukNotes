# Same-note conflicts: Phase A design

Status: design for human review, 2026-11-11. No application implementation is
included or authorized by this document. Evidence baseline: `7c21ae8` on
`codex/ryuk`; references below describe that tree.

## Recommendation and standing rulings

Rank the UX **(1) coordinated conflict-copy recovery, (2) explicit side-by-side
merge review, (3) narrowly bounded automatic merge**. Ship recovery and a common
writer contract before adding merge. Review should become the preferred way to
reconcile changes once the merge engine and commit protocol are verified; Keep
both must remain available even when there is no trustworthy common base.

The split-view survey names this additional blocker in its focus/autosave section,
proposed data model, and hard blocker 5: two independent save queues cannot
coordinate two buffers of the same note. Its original same-note recommendation
has since received a binding ruling: selecting a note already open in the other
main-window pane activates that pane. Preserve that ruling and the single-note
restore policy. This design addresses an editor plus a detached NoteWindow and
independent CLI/import writers; it does not enable duplicate editable main panes,
replace Editor with a fork, or propose real-time collaboration.

Existing protections preserve a dirty GUI buffer on an ordinary rejected save.
They do not provide a universal multi-writer transaction protocol, an exact
revision history, or crash recovery for that buffer. Those are distinct gaps.

## Evidence and current boundaries

| Surface | Current behavior and source |
| --- | --- |
| Save queue | `src/lib/utils/revision-save.ts:13`: one queue per Editor, in-memory `revision`, `savedRevision`, accepted `raw`; 3 s idle debounce and 10 s maximum wait. A failed persistence callback does not advance the baseline or saved revision. These counters identify local edits, not database revisions. |
| Pane ownership | `src/lib/stores/editor-workspace.ts:121`, `:166`, `:182`: window-local mutation leases, aggregate flush, and same-note activation. A detached webview has its own workspace and queues (`NoteWindow.svelte:18`). A main-window lease is not a lock on the detached buffer or CLI. |
| GUI persistence | `src/lib/api.ts:215`, `api-db.ts:99`; `commands.rs:4229`; `vault/db.rs:399`: DB saves compare `expectedRaw` with stored **body only** inside an immediate SQLite write transaction. Changed/missing bodies reject before updating content, tags, FTS or history. Expected data is optional in the native API; unguarded callers remain. |
| Adapter acknowledgment | `api-db.ts:105`: `db_save_note` returns no committed note; the adapter separately calls `dbReadNote`. Another connection can commit between these operations. The returned raw may be another writer's value, not the body just committed by this queue. This must be fixed before baselines can support merging reliably. |
| Existing conflict UI | `Editor.svelte:3505–3581`: string-pattern error classification, Retry, Save a copy and confirmed Reload from disk. The copy captures body/meta, creates a new note, saves it, and changes the originating editor only if the captured local revision still matches. It is not a cross-window resolution session or an atomic fork operation. |
| Refresh | `AppLayout.svelte:947`, `:1026`; `NoteWindow.svelte:214`, `:246`: file notifications can refresh clean buffers. Ordinary refresh preserves dirty buffers; restore-specific paths can flag conflicts. These consumers do not imply that every DB writer emits an event. |
| DB invalidation | `commands.rs:4736`: DB vault opening explicitly installs no filesystem watcher. `db_save_note` and `ryuk` writes do not supply a shared per-note change feed. Do not assume SQLite/WAL file changes automatically produce meaningful `file-changed` events. Detection may wait until a save or explicit read. |
| Other app writers | `api-db.ts:362` task writes omit `expectedRaw`. Window-local task leases cannot serialize against another window or process. Bulk imports retain upsert semantics (`vault/db_import.rs`, `db_export.rs`); single external-note imports allocate a fresh ID on collision. Audit these separately from ordinary editor saves. |
| CLI | `bin/ryuk.rs:204`: `notes tag` reads a complete note, then unconditionally upserts its old body and new tags. A concurrent body change can be overwritten. `notes new` uses a fresh UUID; trash/restore mutate lifecycle state. There is no general `notes edit` command in this tree. |
| Hermes summaries | `bin/ryuk.rs:267`: `session import` regenerates `hermes-session-<session_id>` and unconditionally upserts it; repeated imports can overwrite manual edits. It also supplies generated metadata, including `pinned=false`. The summary note is distinct from the session's linked home note. |
| History | `vault/db.rs:68`, `:434`, `:495`, `:523`: versions contain `id, note_id, content, created`. GUI body-changing saves can snapshot the prior body atomically, with a 5-minute cooldown and retention; forced versions bypass cooldown. Metadata-only saves do not snapshot. Plain library/CLI upserts request no automatic snapshot. |
| Filesystem mode | `vault/operations.rs:856`, `commands.rs:875`, `history.rs:120`: the baseline is complete raw Markdown/frontmatter. App saves share `note_save`; prior raw snapshots are best-effort after publication, throttled/pruned. This mutex does not serialize an unrelated filesystem editor. |
| Review machinery | `HistoryReview.svelte:8`, `:38`; `history-comparison.ts:23`, `:43`, `:78`: asynchronous two-way display diff, collapsed context and inert snapshot rendering. It is not a three-way merge algorithm. Its body normalization, CRLF comparison and coarse fallback are unsuitable as authoritative edit coordinates. |

Git history explains these boundaries rather than superseding them:

- `e1f1ef0` introduced earlier save/lifecycle safety; `14b2a3e` added draft-aware
  review and explicit restore confirmation. Neither established shared editing.
- `5228606` (T8) deliberately guarded GUI saves while preserving unguarded
  creation/import upserts. Its separate-connection regression demonstrates stale
  rejection, not reciprocal protection of a CLI writer.
- `0c405c0` (T3) added atomic prior-body snapshots to `db_save_note`, cooldown,
  retention, forced history and logical moves. Its message explicitly leaves
  metadata-only and direct library/CLI snapshot plumbing outside that subset.
- T4 Stages 1–4 isolated queues, navigation and event ownership; they did not
  make separate windows share a document. `7c21ae8` preserves attachment bytes,
  ownership and exported history; it does not add revision ancestry or merging.

## Conflict-detection timeline

Use `B` for an owner's last accepted state, `L` for its current local draft, and
`R` for the current committed state. An owner is `(vault identity, window ID,
pane ID, editor session)`, never just the globally active note. Two different
owners may have the same `B`; neither owner's dirty buffer is the other's `R`.

| Moment | Pane and detached window | CLI / session import | Detection today; proposed behavior |
| --- | --- | --- | --- |
| Read | Each reads B into independent stores and a local save session. | A CLI operation reads its own state; a summary import can generate a replacement without reading its prior body. | No shared ownership registry. Proposed: capture exact body **and metadata** with a state token and vault generation at read time. |
| Edit | Pane produces L1, detached produces L2; debounce begins independently. | `notes tag` can hold an old complete row while preparing a tag edit. Hermes may hold launch-time grounding while generating a later summary. | Nothing has committed yet. Grounding context is not proof of the summary note's accepted base. |
| First commit | Pane saves against B; if the DB body still equals B it commits R1. | An unguarded CLI/import can commit R1 without proving what it replaced or snapshotting it. | SQLite serializes transactions, not editing intentions. Proposed: every official mutating route states create/update/import policy, writes a prior-state recovery record, and returns the exact committed result. |
| Other owner sees change | A clean editor might eventually refresh; a dirty editor retains L2. | An independent CLI exits without notifying Tauri listeners. | DB notifications are incomplete. Proposed: app-origin events after commit plus connection-local `PRAGMA data_version` polling and focus/resume checks as invalidation hints; re-read each open note on a hint. |
| Second commit | Detached save of L2 against B rejects when the stored body is R1. The draft stays dirty. A simultaneous queued save can already be in flight. | A later unguarded CLI upsert may overwrite R1, even if the GUI save succeeded. A second summary import replaces the same deterministic summary ID. | Proposed: compare-and-swap (CAS) for updates; return a typed conflict and coherent R. Freeze automatic writes for that owner, preserve B/L and show recovery choices. |
| Metadata / ABA | Title, tags or pinned can change while the body stays B; current DB body guard can still pass. Body B→X→B is also invisible to that guard. | `notes tag` is a concrete metadata/body race; trash or delete/recreate requires lifecycle identity, not just body equality. | Full-state equality improves Stage 1, but monotonic revisions/incarnations are needed to distinguish historical identity. Never claim a body hash solves ABA. |
| Apply review | A third write can arrive while a person reviews B/L/R. Typing can also turn L2 into L3. | CLI or another import can commit R2 during the review. | Proposed: apply only against the reviewed stored token **and** captured local revision. A mismatch retains the draft and review result, refreshes R and requires a new review. No force overwrite. |

Concrete body race: pane and detached both read B; pane commits L1; detached
fails saving L2. The merge base is the detached owner's actual B, **not** the
latest history row (which may describe a different prior save). In the reverse
race the pane becomes the conflicted owner. Both windows need independently
addressed status; focus changes do not transfer ownership of L.

There are additional failure windows to close:

1. A GUI commits L1, another process commits R2, and the adapter's follow-up read
   returns R2. Adopting R2 as the queue baseline while its editor still shows L1
   can make a later edit overwrite R2 without rejection. Return L1 and its token
   from the write transaction; treat later R2 as a separate invalidation.
2. A CLI tag operation reads B, a GUI commits L1, then CLI upserts B with tags.
   The stale writer is the CLI; a GUI-only check cannot prevent this loss.
3. A clean buffer that missed a DB change becomes dirty before polling sees it.
   It must retain its old B and L, not adopt R as its baseline to make saving pass.
4. Current clean refresh compares DB `.raw` (body), so metadata-only changes can
   remain invisible too. Compare the proposed full state/token, not only raw.
5. Deleted, trashed, replaced or restored notes need explicit lifecycle outcomes.
   The DB body guard rejects a missing row but does not itself guard trash state.
   Never resurrect or untrash a note as a side effect of resolving body text.

## Writer and coordination contract

Keep `api.ts` as the storage boundary. Proposed internal shapes, not code changes:

```ts
AcceptedState = {
  key: { vaultIdentity, vaultGeneration, noteId },
  body, metadata: { title, tags, pinned, created, trashed, notebook },
  token,                         // state digest initially; revision identity later
};
ConflictSession = {
  owner: { windowId, paneId, editorSession },
  base: AcceptedState, local: { body, metadata, localRevision },
  remote: AcceptedState | { kind: 'deleted' | 'unavailable' },
  generation, recoveryId, phase, // conflicted / reviewing / applying / resolved
};
```

A native save must validate the captured vault, compare expected state while
holding the SQLite write transaction, snapshot/checkpoint the predecessor as
required, and commit note/meta/FTS/attachment links together. Return the exact
committed state. A typed error distinguishes changed, deleted, trashed,
vault-replaced and I/O failure; string matching is only a compatibility adapter.
Events carry identity/token/origin, not entire dirty bodies. Duplicate, stale,
missing and out-of-order events are normal; they only trigger a coherent re-read.
`data_version` is connection-local, not a persisted per-note revision number.

Register editable owners across app windows in the native process for discovery
and status delivery. This registry is advisory: windows can crash, and CLI
processes are not registry members. Never hold SQLite locks while waiting for a
window or a human. Window-local T4 leases remain useful for UI mutations but are
not distributed locks. Every commit still relies on storage-side CAS.

For CLI tag changes, prefer an atomic field operation on fresh state, preserving
the current body, rather than rewriting a previously read complete note. Give
scripted content updates an explicit expected token and useful nonzero conflict
exit/result. Audit task mutations, bulk import, history restore, rename metadata,
trash/restore and deletion along with editor saves; a single unchecked route can
invalidate the guarantee. Bulk import's existing overwrite policy must be made
explicit rather than silently relabeled as guarded editing.

For `session import`, keep deterministic IDs and the Hermes JSON shape. Proposed
policy requiring approval: remember the last successfully generated summary
state/provenance on the Ryuk side. Re-import may replace it only if the current
note still matches that generated state (identical imports are no-ops). If manual
changes intervened, preserve the incoming generated summary as a separate pending
artifact and report conflict; never throw away the input or hand-merge silently.
Older summaries with no proven generated base need explicit review/overwrite
choice. The current JSON contains no authoring revision, so a fresh read just
before upsert is not a substitute for a missing authoring base. This is a proposed
compatibility decision, not approval to change existing import behavior now.

Filesystem mode needs its own adapter for raw frontmatter/body and cooperating
writer locks. Atomic rename alone is not cross-process compare-and-swap. Do not
promise protection from arbitrary non-cooperating file writers or direct SQL.
Detect such changes where possible, preserve drafts, and disable automatic merge
when identity/base cannot be established. App-restores and external database
replacement must invalidate generations and reopen/re-read safely; a token from a
pre-restore connection is not authority to write a restored vault.

## What three-way merging requires

Compute `diff(B,L)` and `diff(B,R)` over the exact accepted Markdown bodies,
then construct a candidate M. Read R with its metadata/token as one coherent
state. B must be the precise accepted body of the owner whose L is being merged,
including its accepted title-heading representation; L must be an immutable
capture of that owner's draft. Do not choose B by timestamp proximity or by which
history row happens to be newest. Missing B means two-way inspection and Keep
both, not guessed ancestry.

| Existing evidence | Enough for merge? |
| --- | --- |
| Live queue `raw` | Enough for a body-only three-way computation **if** it is the exact acknowledged commit/read and remains retained with the draft. Current read-after-write acknowledgment needs repair first. Accepted metadata must also be captured separately; mutable `activeNote.meta` is not a base. |
| DB prior-body version | Helpful recovery material when an exact match to B can be proven. Not a guaranteed base: saves can fall inside cooldown; retention can remove B; CLI/direct upserts skip snapshots; failed saves create no new snapshot. |
| DB version identity | `snapshot_on` uses UUIDs; `add_version` dedupes by hash of note ID and content. Neither records a parent revision, writer, metadata or the editor's base. Timestamp is a capture time, not ancestry. Forced snapshots can legitimately contain duplicate bodies. |
| FS prior-raw version | May include frontmatter, unlike DB body snapshots. Still throttled/pruned and best-effort after save; not an atomic revision graph or a durable dirty draft. |
| HistoryReview comparison | Useful visualization, not merge instructions. `comparableBody` trims boundary blank lines; comparison normalizes CRLF and can fall back to coarse changes. Neither may determine authoritative patches or declare a merge safe. |

**Schema conclusion:** no SQLite migration is necessary for a live, explicit
body-review prototype that retains B/L/R in a recovery journal and uses an atomic
state comparison. Existing snapshots alone are insufficient for durable,
metadata-aware, all-writer revision ancestry. Do not reinterpret or backfill
legacy versions as if they contained that information.

Before enabling unattended merge, propose and separately approve a migration
with: a stable vault UUID; note incarnation plus monotonic revision (covering
metadata and lifecycle changes); full predecessor state or an immutable reference
to it; parent revision(s), source/operation and commit identity; and retention
pins for bases referenced by unresolved drafts/reviews. A separate revision table
is preferable to changing the meaning of the current body-history table. The
existing versions UI can remain a curated, throttled history view. A merge commit
records the current stored predecessor and the local draft's base/provenance;
it must not falsely label an uncommitted draft as an earlier DB revision.

Migration needs the T9 standard of backup, transactional validation, rollback,
future-version rejection and export/import compatibility. Unknown legacy ancestry
stays unknown. Backup restore can roll revision counters back: include a runtime
restore generation, reject stale pre-restore tokens, and specify detection of
external replacement before trusting that connection again. Do not use revision
integers, modification timestamps or content hashes alone as vault identity.

Merge body text independently from metadata. Scalars use unchanged-from-base
rules; divergent title/pinned changes need a choice. Tags require explicit set
add/remove semantics and conflict rules, not a blind union. Preserve immutable
identity/creation fields; generate modification time at commit. Notebook moves,
trash/deletion and vault replacement are separate decisions, not text hunks.
Treat attachments as stable references and immutable bytes; retain/link every
chosen asset using T9 ownership rules. Do not merge or rewrite BLOB bytes, secret
payloads or generated HTML as prose.

## Ranked UX choices

### 1. Coordinated conflict-copy fork: first delivery

Use a persistent, owner-scoped banner: **“This note changed elsewhere. Your draft
is still here.”** Offer Keep both / Review changes (when available) / Reload saved
version with an explicit discard confirmation. A normal Retry cannot clear a
conflict by adopting R as B. Do not open a surprise modal in an inactive pane or
move focus between windows.

Build on the existing Save a copy behavior, but make the fork a native atomic,
retry-idempotent operation keyed by a recovery/operation ID. Create a fresh note
ID, preserve the captured L and chosen metadata, and attach its references before
acknowledging success. Leave the original R untouched. Resolve DB notebook
membership through its adapter; the current UI handler's filesystem-style parent
arithmetic is not a DB membership contract. If the user typed after the captured
revision, keep those newer edits dirty and retain the journal; a successful older
copy does not authorize replacing the live buffer or closing it.

No trusted base is needed to keep both. The cost is an extra note and later manual
reconciliation. Show provenance and the original note target without copying a
session binding. Summary forks need an explicit reserved-tag/provenance policy:
copying `hermes/session` blindly to a fresh ID can make the current Editor regard
it as a summary with no restorable session ID (`Editor.svelte:189`). Propose a
clearly labeled recovery copy, not a second authoritative deterministic summary.
Cross-vault copying is a separate operation; initially keep recovery in its
originating vault and never retarget it using current global selection.

### 2. Side-by-side review: preferred reconciliation after foundation

Reuse HistoryReview's presentation, collapsed unchanged context, async-generation
cancellation, source preview and SnapshotDocument's inert rendering. Add a
conflict-specific wrapper with explicit labels **Your draft**, **Saved version**,
and optional **Common base**, plus a separate candidate result. Two read-only
base→local/base→remote comparisons can reuse the machinery; change directional
labels such as “Added by restoring,” which are wrong for merge review.

Introduce a pure three-way engine with exact byte/line mapping; do not derive
patches from normalized `HistoryComparison` rows or rendered HTML. Use a local
SourceEditor/controller for the result if editing is needed, not another live
Editor bound to the same note. Keep base/local/remote immutable in that review.
Every overlap gets explicit choices; the result remains an artifact until CAS
apply succeeds. “Saved version changed again” invalidates the proposal, preserves
manual result edits, and offers renewed review/Keep both. Never replace the original
dirty document merely to show a comparison. Keep raw source available for large
or unrenderable notes; reuse display fallbacks without trusting them for apply.

Bind dialog IDs, focus, F3, shortcuts, assets and asynchronous results to the
owning window/pane/review generation, following T4 Stage 4. The other normal pane
may remain usable. Other dirty windows get their own conflict status and must not
be marked clean by this owner's resolution. Preserve undo history until a verified
apply/explicit discard; define a deliberate undo boundary for accepting M.

### 3. Automatic merge: optional and last

Only consider automatically applying disjoint edits with a proven B, current
identity, unchanged local revision and exact reviewed stored token. Initially
restrict this to well-separated plain-prose blocks. Stop at overlapping edits,
same-position insertions, ambiguous repeated lines, moves, metadata changes,
missing base, deletion/trash/restore, or any parser/size/time-limit failure.
Also stop around lists/tasks, tables, reference definitions, fenced code,
math, secrets, raw HTML/PDF nodes, wiki targets and attachment reference changes:
different lines can still be the same semantic structure. Parser validation is
necessary, not proof of semantic independence. A bounded diff failure goes to
review/Keep both; it never means “no conflicts.”

Even a safe candidate uses CAS, predecessor history, retained recovery artifacts
and a visible “Merged changes from elsewhere” notice. No fuzzy patching, repeated
blind retries or automatic inserted conflict markers in the live note. One
bounded attempt per captured generation; a racing writer routes back to review.
This option requires explicit later approval and the all-writer revision work.

## Autosave, preservation and lifecycle state machine

Proposed owner states:

```text
clean(B) -> dirty(B,L,n) -> saving(B,Ln,n)
  successful exact acknowledgment -> clean only if local revision is still n
  newer local edit               -> dirty(new accepted B, newer L, n+1)
  stale / deleted / replaced      -> conflicted(B,L,current R,n)
conflicted -> reviewing(B,Ln,R,reviewGeneration)
reviewing  -> applying(M, expected R token, expected local n)
  success with matching n        -> clean(committed M)
  local/remote change or failure -> retain artifacts; conflicted/review-stale
conflicted -> fork pending -> preserved copy; resolve only the captured revision
```

On a conflict, cancel debounce/max-wait timers and fence already queued save
callbacks for that editor session. Merely cancelling timers does not remove
callbacks already on `RevisionSaveQueue.tail`. Do not advance `raw` or
`savedRevision` on failure, event receipt, review opening, or merge calculation.
Typing may continue in the conflicted owner, but its new drafts update recovery
and invalidate stale proposals rather than repeatedly writing to the original.
Other notes continue autosaving. Error Retry and conflict resolution are distinct.

Persist exact B/L/accepted metadata, note/vault identity, local generation and
pending result to a private, atomic recovery journal. Use one record stream per
owner with ordered writes so an older completion cannot overwrite a newer draft;
include BLOB references and resolve transient `blob:` data before calling a
checkpoint durable. Proposed location: app-private per-vault recovery storage,
with restrictive permissions and no body data in ordinary logs. Keep it outside
a vault file that backup restore can replace. Path/identity rebinding and stale
journal cleanup need tests and explicit ownership validation. This is a proposed
runtime location; no user data was accessed for this design.

Memory preservation remains mandatory even if journaling fails: keep the buffer,
show the failure, and refuse any transition that would dispose it. A successful
journal write is not a successful note save. Retain artifacts until the matching
revision was saved/forked or the user explicitly discarded it. Restart recovery
must open a review/copy flow, not silently overwrite the current note. Absolute
zero-loss on abrupt process/power failure before a checkpoint cannot be promised;
never imply an in-memory dirty flag already provides that guarantee.

Close, pane removal, note navigation, vault switch, restore, deletion and attachment
cleanup must refuse unresolved/disposable dirty state unless it has been explicitly
resolved or safely preserved under the approved workflow. Flush returning false
continues to block destruction; do not fake clean to make a T4 lease succeed.
A failed checkpoint/fork/apply leaves the original editor mounted. Detached-window
close uses its own acknowledged barrier; the backend coordinates affected windows
or refuses the operation, never assuming the main-window aggregate covers them.
Existing detached-window vault-switch restrictions are not loosened implicitly.
On mobile visibility/pagehide, checkpoint best-effort but keep the unresolved state;
these events do not guarantee enough time for disk I/O. Runtime restore generations
also invalidate pending uploads, review computations and old save completions.

## Staged implementation and blast radius

Each stage is separately reviewable and must stop at its gate. This document
approves none of them. Stage 1 is the first recommended implementation request;
the schema proposal requires a further explicit approval before migration code.

| Stage | Deliverable and blast radius | Gate before proceeding |
| --- | --- | --- |
| 1 — authoritative writer contract | `api.ts`/`api-db.ts`, native commands, `vault/db.rs`, CLI and task/import writers: exact commit acknowledgments, typed conflict states, captured vault identity, accepted metadata and explicit create/update/import policies. Full-state CAS without migration improves present-state protection; document its ABA limit. Reuse existing history on guarded body changes and add predecessor capture for official CLI updates. No merge UI or duplicate-pane policy change. Editor adapter error compatibility stays narrow. | Separate-connection races in both orders; save/read-back interleaving; metadata-only races; missing/trashed notes; CLI tag updates; repeated and manually edited summaries; task/import conflicts; unchanged existing creation semantics. Approve the summary replacement compatibility policy first. |
| 2 — coordinated recovery and Keep both | Native owner/invalidation service plus polling, private recovery journal, atomic fork command, window-specific conflict status. Surgical Editor save-error/queue fencing, copy/close hooks; AppLayout, NoteWindow and workspace barriers. No text merge. This is the first unavoidable Editor work beyond adapters: each instance owns its unsaved document and queue. | Two real webviews, event loss/reordering, unguarded external connection changes, journal/disk failures, fork retries, edits during fork, close/restart/restore barriers and shared attachment survival. Verify same-note pane activation and detached behavior remain intact. |
| 3 — explicit three-way review | Pure merge module/tests, conflict wrapper around HistoryReview/SnapshotDocument, isolated result SourceEditor and exact CAS apply. Surgical Editor draft-capture/apply/undo hooks; no Editor fork. Use the journal's verified live B; legacy history is optional evidence, never guessed ancestry. No SQLite schema migration required for this bounded interactive stage. | Exact whitespace/Unicode/CRLF tests; repeated lines; insertion overlap; code/math/secrets/tables; metadata choices; unresolved base; another writer during review/apply; typing during review; distinct IDs/focus across windows. Manual review required for uncertain lifecycle/ABA identity. |
| 4 — durable revision ancestry, separate approval | Proposed vault schema extension for incarnation/revision, full revision states and provenance/parents, pinned bases, all official writers, backup/restore, export/import and recovery journal binding. Keep old versions readable and explicitly ancestry-unknown. No automatic merging yet. | Approve schema and retention policy before coding; transactional migration/rollback fixtures, legacy/future schema handling, restore/counter rollback, deletion/recreation, exported revision data, old-client behavior and retention under unresolved reviews. |
| 5 — optional bounded automation | Opt-in safe subset in the already tested engine/coordinator; preserve review fallback and visible recovery. Small Editor surface if orchestration remains outside it. No new shared editor engine. | Adversarial and property-based merge tests, resource bounds, every protected syntax boundary, ongoing CLI/window races and crash recovery. Explicit approval of the automatic-merge rules. |

Acceptance across all implementation stages must include: pane A plus detached
window on the same note; separate CLI connections before/during/after save; a
session summary manually edited in a GUI and re-imported; missed notifications;
body-identical metadata conflicts; delete/recreate and restore epochs; pending
attachments; and a dirty other pane during every close/maintenance route. Run JS
and native mock-app/CLI gates, then real GUI boot and two-window interaction tests.
Unit tests alone cannot establish correct focus, close interception or durability.

## Phase A verification and stop

This session changes only this design and `.session-commit-msg`. A SHA-256 baseline
of all tracked files was taken before authoring; compare it after the gates to
establish application-source integrity independently of test success. The final
comparison checked all 262 baseline tracked files: 0 changed. Final git status
shows the new design and `.session-commit-msg`, alongside the pre-existing
untracked instruction files; no tracked application changes are present.

- `npm run check`: passed, 0 errors and 0 warnings.
- `npm test`: passed, all 42 test files, 0 failures.
- Rust gates, formatter/clippy, frontend/release builds, packaging, GUI boot and
  live multi-window/CLI exercises: not run for this design-only task.
- No user vault/config access, application edits, schema changes or git writes.

STOP after Phase A. Human approval is required before implementing the writer
contract, coordinated conflict UI, merge engine or any schema proposal.
