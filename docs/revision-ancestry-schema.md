# Durable revision ancestry — Stage 4 proposal

Status: proposed, awaiting human approval. This document implements no migration
or application behavior. It follows [the conflict design](same-note-conflict-design.md),
especially “What three-way merging requires” and Stage 4, and extends the
[authoritative writer contract](writer-contract.md). Fork first, review second,
automatic merge last remains binding. Durable ancestry does not approve automatic
merge or change the same-note pane policy.

## Recommendation and scope

Use schema **3**, with a separate immutable revision graph and exact full-state
payloads. Keep graph headers, commit/operation identities, incarnation tombstones,
and counters indefinitely. Keep payloads for **at least 90 days and the newest
100 revisions per note incarnation**, plus every current head and every explicitly
pinned base/local-committed-parent/remote revision. The retention predicates are
additive: prune only when older than 90 days, outside the newest 100, not a head,
and not pinned. An unresolved recovery or review pin never expires by age.

Do not reinterpret `versions`: it remains the curated body-history view with its
existing settings and five-minute snapshot cooldown. Revision capture runs on
**every semantic commit**, without cooldown, even for metadata and lifecycle.
No-op writes return the existing receipt and do not create fake revisions.
Retention is a proposal requiring approval, not a feature cut performed here.

The proposal covers DB vaults and every official DB writer. Filesystem vaults
retain their current contract; this does not invent durable FS ancestry. Direct
SQL and clients that ignore the format contract cannot be granted revision-safe
writes. Dirty uncommitted text stays in the private recovery journal, outside the
vault; it is never falsely recorded as a committed parent.

## Identities, restore, and monotonicity

| Identity | Meaning and lifetime |
| --- | --- |
| `vault_uuid` | Random UUID assigned once on new creation or v2 migration, stored inside the vault. Survives renaming, backup creation, and restoring that vault's backup. A foreign replacement retains its own UUID; never rewrite it to match the target path. |
| `storage_epoch` | Random persistent UUID for a restore branch. A cooperating backup restore rotates it before admitting writers, even if the restored bytes equal the previous state. It disambiguates rolled-back histories, not the logical vault. |
| Runtime generation | Native process boot UUID plus `AppState.vault_db_epoch`; connection/file identity remains checked. Close, restore, replacement, or reopen invalidates outstanding reads, saves, uploads, and review generations. Never persist a process counter as the sole restore authority. |
| `incarnation_id` | Fresh UUID on first creation and each creation after permanent deletion, even if the public note ID and all fields are reused. Trash/restore and ordinary imports updating an existing live identity retain it. |
| `revision_id` / `commit_token` | Independent random UUIDs per successful semantic commit, retained forever. A token also binds vault UUID, storage epoch, incarnation, revision number, state digest and current native connection capability. Hash equality alone is no longer write authority. |
| `revision_no` | Strictly increasing integer per public note ID on the current managed lineage, including across incarnation changes. Never reset on deletion. Gaps are allowed; timestamps do not order commits. |

A vault-local counter alone cannot prevent rollback when its backup is restored.
Require an app-private, owner-only **high-water ledger outside the vault**, shared
by GUI and CLI on that host, keyed by vault UUID and public note ID. Before a DB
commit, reserve and durably sync a number above both the ledger and DB high water.
Reservations are serialized by an interprocess ledger lock, followed by the DB
write lock, in that order everywhere. A failed transaction leaves a harmless gap.
Never hold either lock across UI, IPC to a webview, or user interaction. Ledger
failure refuses the commit without clearing any dirty buffer. An implementation
must measure this durability cost rather than silently batching away guarantees.

The ledger also records the last observed storage epoch and head identities.
Restore uses the same cross-process maintenance lock; all official opens, reads
and writes must participate in its shared/exclusive protocol. Restore retires
connections, stages and validates the restored file, creates a new storage epoch,
reconciles DB counters against the ledger, fsyncs and swaps it, then reopens and
broadcasts invalidation. Each live restored note receives a full-state
`restore_baseline` revision above the high-water mark, with the restored head as
its predecessor. The discarded future remains external recovery/provenance, not
an invented parent in the restored graph. Original backup bytes are untouched.

On another host, after ledger loss, or after an unmanaged in-place restore, an
integer cannot be proven greater than a lost future counter. Do not claim global
numeric monotonicity: establish a fresh storage epoch and require recovery review
before rebinding old artifacts. The authoritative identity is the epoch plus
incarnation plus revision/commit UUID, not the integer alone. First-open ledger
enrollment follows the same rule. A migrated vault has no pre-migration numeric
history to preserve. Counter overflow refuses writes; it never wraps.

A byte-identical clone has the same persistent UUID. Opening it as an independent
editable vault must be an explicit clone operation allocating a new vault UUID,
with origin provenance; opening it as a restore uses the restore protocol. Reject
concurrent writable paths claiming the same UUID in the local ledger. There is no
claim of multi-host coordination or detection of every arbitrary in-place SQL
rollback: file identity, ledger mismatch, head verification and polling fail
closed where detectable. Re-enrollment never silently promotes a stale draft.

## Proposed exact DDL

This is the v3 addition to the existing v2 schema, not standalone replacement DDL.
Enable foreign keys on every connection. UUIDs use canonical lowercase text;
canonical serialization and UUID validation are enforced in native code. SQL
checks supplement the transactional writer, not a security boundary against SQL
clients disabling constraints. Bind bootstrap values through parameters.

```sql
CREATE TABLE vault_identity (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    vault_uuid TEXT NOT NULL UNIQUE,
    storage_epoch TEXT NOT NULL,
    created TEXT NOT NULL
);

-- Survives notes row deletion. One counter across all incarnations of a note ID.
CREATE TABLE note_revision_counters (
    note_id TEXT PRIMARY KEY,
    high_water INTEGER NOT NULL CHECK (high_water >= 0)
);
CREATE TABLE note_incarnations (
    incarnation_id TEXT PRIMARY KEY,
    note_id TEXT NOT NULL REFERENCES note_revision_counters(note_id),
    born_at TEXT NOT NULL,
    ended_at TEXT
);
CREATE INDEX idx_incarnations_note ON note_incarnations(note_id);

CREATE TABLE revisions (
    revision_id TEXT PRIMARY KEY,
    incarnation_id TEXT NOT NULL REFERENCES note_incarnations(incarnation_id),
    revision_no INTEGER NOT NULL CHECK (revision_no > 0),
    storage_epoch TEXT NOT NULL,
    commit_token TEXT NOT NULL UNIQUE,
    state_sha256 TEXT NOT NULL,
    writer_source TEXT NOT NULL CHECK (writer_source IN
        ('gui','cli','task','import','session_import','recovery','migration','restore')),
    writer_instance TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    operation_item TEXT NOT NULL,
    request_sha256 TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN
        ('create','update','metadata','trash','untrash','delete','import',
         'merge','migration_baseline','restore_baseline')),
    ancestry TEXT NOT NULL CHECK (ancestry IN ('known','unknown_baseline')),
    committed_at TEXT NOT NULL,
    UNIQUE (incarnation_id, revision_no),
    UNIQUE (storage_epoch, operation_id, operation_item)
);
CREATE INDEX idx_revisions_incarnation ON revisions(incarnation_id, revision_no);

-- Self-contained state: pruning an older body never damages a newer payload.
CREATE TABLE revision_states (
    revision_id TEXT PRIMARY KEY REFERENCES revisions(revision_id) ON DELETE RESTRICT,
    body BLOB NOT NULL,
    title TEXT NOT NULL,
    tags_json TEXT NOT NULL CHECK (json_valid(tags_json) AND json_type(tags_json) = 'array'),
    pinned INTEGER NOT NULL CHECK (pinned IN (0,1)),
    lifecycle TEXT NOT NULL CHECK (lifecycle IN ('live','trashed','deleted')),
    note_created TEXT NOT NULL,
    note_modified TEXT NOT NULL,
    extra_json TEXT NOT NULL CHECK (json_valid(extra_json) AND json_type(extra_json) = 'object')
);
CREATE TABLE revision_parents (
    child_id TEXT NOT NULL REFERENCES revisions(revision_id),
    parent_id TEXT NOT NULL REFERENCES revisions(revision_id),
    role TEXT NOT NULL CHECK (role IN ('predecessor','merge_parent','draft_base')),
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    PRIMARY KEY (child_id, role, ordinal),
    CHECK (child_id <> parent_id)
);
CREATE INDEX idx_revision_parents_parent ON revision_parents(parent_id);

-- Includes hard-deleted heads, so absence cannot be mistaken for a fresh identity.
CREATE TABLE note_revision_heads (
    note_id TEXT PRIMARY KEY REFERENCES note_revision_counters(note_id),
    incarnation_id TEXT NOT NULL REFERENCES note_incarnations(incarnation_id),
    revision_id TEXT NOT NULL REFERENCES revision_states(revision_id)
);
CREATE TABLE revision_attachments (
    revision_id TEXT NOT NULL REFERENCES revision_states(revision_id) ON DELETE CASCADE,
    attachment_id TEXT NOT NULL REFERENCES attachments(id) ON DELETE RESTRICT,
    PRIMARY KEY (revision_id, attachment_id)
);
CREATE TABLE revision_pins (
    pin_id TEXT NOT NULL,
    revision_id TEXT NOT NULL REFERENCES revision_states(revision_id) ON DELETE RESTRICT,
    owner_kind TEXT NOT NULL CHECK (owner_kind IN ('recovery','review','export')),
    owner_id TEXT NOT NULL,
    created TEXT NOT NULL,
    PRIMARY KEY (pin_id, revision_id)
);

-- Preserve auditability of deliberate payload pruning; graph headers never go.
CREATE TABLE revision_pruning (
    revision_id TEXT PRIMARY KEY REFERENCES revisions(revision_id),
    pruned_at TEXT NOT NULL,
    policy_version INTEGER NOT NULL CHECK (policy_version > 0)
);
-- Legacy history is explicitly not proof of ancestry. New curated snapshots
-- may bind an exact revision, without turning versions into the commit graph.
ALTER TABLE versions ADD COLUMN ancestry TEXT NOT NULL DEFAULT 'unknown'
    CHECK (ancestry IN ('unknown','verified'));
ALTER TABLE versions ADD COLUMN revision_id TEXT REFERENCES revisions(revision_id);

CREATE TRIGGER revisions_no_update BEFORE UPDATE ON revisions BEGIN
    SELECT RAISE(ABORT, 'revision headers are immutable');
END;
CREATE TRIGGER revisions_no_delete BEFORE DELETE ON revisions BEGIN
    SELECT RAISE(ABORT, 'revision headers are retained');
END;
CREATE TRIGGER revision_states_no_update BEFORE UPDATE ON revision_states BEGIN
    SELECT RAISE(ABORT, 'revision states are immutable');
END;
CREATE TRIGGER revision_parents_no_update BEFORE UPDATE ON revision_parents BEGIN
    SELECT RAISE(ABORT, 'revision parents are immutable');
END;
CREATE TRIGGER revision_parents_no_delete BEFORE DELETE ON revision_parents BEGIN
    SELECT RAISE(ABORT, 'revision parents are retained');
END;
```

Populate `vault_identity` and update `meta.schema_version` to `3` only within the
validated migration transaction. Fresh v3 creation uses the same tables without
running the two ALTER statements twice. No FK points from the graph to `notes`:
hard-delete cascades in the existing schema must not erase ancestry.

Native transactional invariants beyond DDL:

- A revision's incarnation belongs to the head's note ID; numbers increase across
  all its incarnations and never exceed the reserved counter. Validate this also
  on import. New incarnation insertion closes the preceding deleted incarnation.
- Every known non-create revision has exactly one actual stored predecessor;
  baseline roots have none. Parent edges point only to previously committed,
  same-note/same-incarnation revisions and form an acyclic graph. Recreating a
  deleted ID starts a new root and records prior incarnation as provenance in
  `extra_json`, not as a same-incarnation merge parent.
- Payload BLOB is the exact UTF-8 accepted Markdown bytes, including CRLF and
  boundary whitespace. Hash a versioned, length-delimited canonical full state:
  body, title, sorted unique tag set, pinned, lifecycle, immutable creation field,
  modified field, notebook membership, attachment identities, and authoritative
  session binding/generated-state provenance in `extra_json`. UI preferences,
  connection state and pending import artifacts are not note state.
- A delete revision retains the last body and metadata with lifecycle `deleted`;
  its predecessor records the live/trashed state. No garbage collection may remove
  the head. Auxiliary attachment upload creates no revision until references or
  semantic ownership change; history-only links remain auxiliary.
- Insert header, full state, parents and attachment links, update materialized
  notes/tags/notebook/session projections, update counter/head, and capture receipt
  in one immediate transaction. Failures roll back all of these. Only return the
  receipt after commit succeeds. The predecessor payload must exist at commit;
  old predecessors may be pruned later under the explicit policy.
- Parent insertion is only permitted while constructing a new revision in that
  transaction; committed headers cannot acquire new parents. Imported immutable
  graph rows are validated before insertion, never amended on ID collision.
- Merge commits name current R as `predecessor` and the verified committed B as
  `draft_base`. `merge_parent` is only for an actually committed branch revision.
  L is an uncommitted journal artifact: record owner/artifact digest, captured local
  revision and review operation in `extra_json`, never fabricate a revision for L.
- Retried `(storage_epoch, operation_id, operation_item)` must match request digest
  and policy/expected token, then return the original receipt. Different input
  under the same key is an error. Retained operation headers prevent retry from
  creating a second commit after payload pruning; a pruned receipt returns an
  explicit historical-result-unavailable status, never a fresh write. Current
  head state must not be substituted for an older operation's acknowledgment.

## Retention, pins, and attachment ownership

Headers and edges remain even when payloads go; missing payload means “pruned”,
not “empty body”. A reviewer can establish graph ancestry but cannot compute a
merge without exact B. Pruned or unknown B routes to Keep both/two-way inspection.
Do not recursively pin all ancestors: pins protect the exact full states needed
by an unresolved workflow, not every predecessor back to creation.

Register an editable owner's accepted base pin in the **same native transaction
that returns its accepted read**, before GC can run. Add pins for reviewed R and
any committed merge parents when opening review. Journal checkpoints must record
vault UUID, storage epoch, incarnation, revision ID, token and local revision as
well as exact B/L. Establish the base pin before acknowledging a durable checkpoint.
Journal disk failure still retains memory and refuses destructive transitions.

Pin removal requires explicit matching-resolution/discard acknowledgment and an
ordered durable journal tombstone. Crashed owners and expired heartbeats do not
release recovery pins. On restart reconcile pin records with private journals;
orphaned pins remain conservatively until the recovery inbox offers explicit
cleanup. A missing local journal (different host/restore) is not permission to
prune. Renewed review pins its new R before releasing obsolete review-only pins.
Lease pins for export last until its coherent artifact is complete or explicit
abandonment is established under the maintenance lock.

GC runs in an immediate transaction with the same coordination lock order. Check
age/count/head/pin predicates again under lock, record `revision_pruning`, then
remove eligible `revision_states` and their attachment links. The FK from heads
and pins makes accidental deletion fail. Headers, parents and counters remain.
Version retention settings govern only `versions`; clearing history must not
release revision pins. A separate ancestry policy controls this 90-day/100 floor;
initially no UI and no implicit coupling to `max_versions = 0` semantics.

T9 ownership extends to `revision_attachments`: BLOB GC must check live notes,
curated versions, revision payloads and durable recovery artifacts. Deleting a
note never deletes shared bytes. Recovery artifacts outside the vault must either
keep an in-vault pin/link or own durable private bytes before a checkpoint counts
as durable. Plain-text revision payloads carry the same sensitivity as the vault;
there is no new encryption guarantee. Storage growth and disk-full refusal are
visible limits; pins never silently expire to recover space.

## Writer integration points

The implementation must audit the complete route list in `writer-contract.md`.
No official content route may bypass the revision transaction helper.

| Existing boundary | Proposed integration |
| --- | --- |
| `vault/writer.rs`: `accepted_note`, `state_token`, `check_policy`, `NoteWrite`, `VaultIdentity` | Return captured durable identity/incarnation/revision plus runtime capability; compare those and full state in the same immediate transaction. Retain typed changed/deleted/trashed/vault_replaced errors. Old body-digest-only tokens are rejected for v3 writes. |
| `write_authoritative`, `write_scripted`, `write_policy`; `commands.rs::db_save_note` | Shared commit helper creates revision and exact receipt inside the transaction. Existing curated snapshot policy remains independent. No post-commit read to obtain acknowledgment. |
| `api-db.ts` acceptance chains / `api.ts` | Carry opaque v3 accepted identity without letting unrelated reads advance a queue baseline. Detached attach captures native session identity. Failed or invalidated writes do not advance raw/savedRevision. |
| Task mutations, title rename, history restore | Guarded update using accepted revision. History restore creates a new child of current head; a legacy history body is content selection, not ancestry. |
| New/duplicate/daily, CLI new; `create_on` | Absent-only create, fresh incarnation. Copies start independent graphs with origin provenance. Daily creation race returns existing head; no phantom revision. |
| CLI update / `tag_atomic` | Expected v3 token for content; atomic fresh-state tag intent preserves body and records metadata revision. Both use ledger reservations and shared maintenance coordination. |
| Single-note and bulk import; `write_import_on` | Retain copy vs explicit replace policies; replacement records current predecessor and all metadata/lifecycle changes. Attachment finalization is part of one commit, not an intermediate revision. Equality is a no-op. |
| `import_generated_summary` and session links | Replacement only while proven generated state remains current. Track generated revision identity in provenance, not digest alone; manual changes then return-to-same-state still count as intervening edits. Pending summary artifacts do not replace the head; repeated input remains idempotent. Recovery copies strip authoritative session binding as already ruled. |
| Trash/untrash, hard delete, empty trash | Explicit lifecycle child; retain tombstone/counter after `notes` deletion. Each item in a batch has an operation item key. All affected-owner barriers still precede mutation. |
| Notebook moves, attachment-reference changes, session metadata | Semantic field changes produce revisions even if body is unchanged. State/preferences and standalone upload/history checkpoint operations do not pretend to be note commits. |
| `recovery.rs`: fork and review apply | Fork is a fresh incarnation/note commit under existing idempotent operation ID. Review verifies both remote accepted revision and captured local revision, pins exact B/R, then commits through the same helper. Keep all immutable artifacts until acknowledgment. |
| `state.rs`, `recovery.rs`, backup open/restore commands | Keep connection-local data_version polling as invalidation only; add durable identity/epoch checks and runtime generation fencing. Owner registry is advisory; CLI safety comes from storage CAS and maintenance locks. |

Low-level fixture upserts must be test-only or route through an explicitly named
migration/import helper before v3 ships. Inventory backend commands, importer,
CLI, and lifecycle SQL again at implementation time; a partial writer rollout
must not permit writable v3 vaults.

## Migration and compatibility — T9 standard

1. Reject unknown/future `meta.schema_version` before mutation, snapshot creation
   or schema interpretation. Accept only recognized empty/v1/v2/v3 layouts. Do
   not downgrade, and do not treat malformed metadata as a new empty vault.
2. Acquire exclusive cross-process maintenance coordination; resolve all panes,
   detached owners and durable unresolved recovery or refuse. No SQLite lock is
   held while awaiting owners. Reserve a unique private `0600` pre-v3 backup and
   run **VACUUM INTO** before the upgrade transaction, including committed WAL
   data. Fsync snapshot and parent directory; failure aborts opening writable.
   Exclude external cooperating writers between snapshot and migration. Recheck
   schema/file identity after taking the immediate transaction lock.
3. Upgrade v1 through its existing T9 attachment transformation and safe orphaned
   note_tags sweep inside the same transaction as v3. Snapshot the original
   source once. Do not commit an intermediate v2. For v2 apply the DDL above;
   create one fresh vault UUID/epoch and one unknown-ancestry baseline revision
   per existing note, including trashed notes, with exact current full state,
   tags, memberships, links and generated provenance. Historical hard-deleted
   notes cannot be recovered or assigned invented incarnations.
4. Keep every legacy `versions` row/body/ID/timestamp, mark `ancestry='unknown'`
   and leave `revision_id=NULL`. A body match or close timestamp does not upgrade
   its ancestry. Existing journal bases stay live-capture evidence; migration
   cannot retrospectively assign them revision IDs. Rebind through explicit
   recovery review, never hash matching alone.
5. Validate counts, exact body/metadata and attachment byte digests, projections,
   graph/head/counter constraints, parent acyclicity and all FKs. Final
   `PRAGMA foreign_key_check` reports offending table, rowids (or null for
   WITHOUT ROWID), parent, FK index, and total count. Only then write schema=3
   and commit. Injected failures roll back cleanup, DDL and data. Preserve the
   private snapshot for explicit recovery; never automatically overwrite the
   failed source with it. A post-commit open failure refuses writes; restore is
   an explicit coordinated operation.
6. v2 official clients already reject future versions; verify old GUI and CLI
   binaries do so before touching a v3 fixture. No writable v2 compatibility
   shim: unversioned writes cannot honor ancestry. Offer a separate lossy legacy
   Markdown export if requested; never downgrade the live vault. Arbitrary older
   SQL clients ignoring version checks remain unsupported. New read-only tools
   must still reject future formats rather than assume the v3 layout.

Snapshot creation is outside the SQL transaction because SQLite cannot VACUUM
inside it; the exclusive maintenance protocol closes that gap for cooperating
clients. It does not claim to stop non-cooperating SQL. Migration tests must use
independent connections, real WAL fixtures, mock-app command paths and injected
snapshot/DDL/validation/commit failures, not only an in-memory happy path.

## Export, import, and restoration

Extend `vault/db_export.rs`'s `.ryuknotes/export.json` to a new explicit format
version with required ancestry capability. Include vault UUID, storage epochs,
incarnations/counters/heads, all headers and edges, retained full states, pruning
markers and attachment links/bytes. Serialize bodies as exact byte sidecars with
hashes; Markdown's portable attachment rewriting must not alter authoritative
revision bytes. Preserve all revision-only assets, shared-byte dedup, curated
versions with unknown/verified markers, and authoritative generated provenance.
Export pin descriptions but no private dirty drafts, owner IDs or live leases;
import them as conservative recovery holds pending reconciliation, never as live
owners. Export is a coherent snapshot, not several unlocked table reads.

Default full-vault restore import into an empty destination preserves graph IDs
and vault UUID and establishes a fresh storage epoch/runtime generation under
restore rules. Import-as-independent-copy allocates a new vault UUID, translates
all graph IDs/tokens consistently and records source identities as provenance.
Ordinary import into a populated vault remains explicit replace/copy: do not
splice foreign ancestry into an existing incarnation merely because IDs match.
A same-vault graph reconciliation mode is separate, explicit and validates all
immutable collision payloads before adding rows. Unknown/newer index capability
must fail before any writes; silent dropping of ancestry is forbidden.

Re-export/re-import must be idempotent: immutable graph identities plus matching
hashes deduplicate; mismatched immutable IDs reject. Restoring a previously
imported export must not rotate/create a baseline twice for one acknowledged
restore operation. Preserve pruned markers and never resurrect invented payloads
from the curated versions table. A plain Markdown import starts unknown ancestry;
it cannot prove a parent. Test mixed images/files, revision-only references and
shared BLOB bytes through export, import, and pruning.

Raw `.rn` backups preserve UUID, graph, counters and pins as of the snapshot.
Restoring them cannot revive old owner leases or make pre-restore tokens current.
The ledger reservation floor, fresh storage epoch, restore baselines and runtime
invalidation described above handle counter rollback. Reconciliation of external
journals with older backup contents is required before GC; unresolved drafts
remain preserved even if the restored graph lacks their former base revision.

## Implementation order and blast radius

Each row requires its own reviewed implementation request and stop. No automatic
merge is authorized by completing these stages.

| Stage | Deliverable / blast radius | Required gate |
| --- | --- | --- |
| 4A — approve this contract | Document only; decide retention, ledger durability cost and clone/restore policy. | Human approval before code. |
| 4B — identity and migration foundation | Native vault schema/migrator, v3 read-only inspection, ledger and maintenance protocol, backup/restore. Keep writable v3 feature gated until all writer routes are ready; production creation stays v2 meanwhile. No Editor changes. | v1/v2 byte-preserving upgrade/rollback, FK diagnostics, future rejection, old-client refusal, UUID survival, two-process restore, counter rollback/ledger loss. |
| 4C — all-writer transactions | Writer helper, command/CLI/import/task/lifecycle routes and adapter accepted receipt. No partial production activation. | ABA and delete/recreate, metadata-only and lifecycle races, exact receipts, tag races, summary policy, retry identity, disk-full, reservation crash gaps, both connection orders. |
| 4D — recovery pins and retention | Native registry/journal binding, GC/BLOB ownership, thin review controller and surgical owner hooks only if necessary; preserve T4 providers and all-pane barriers. | Kill/restart around every pin/journal boundary, unresolved reviews beyond retention, lost events, failed journal/GC, deleted originals with shared assets, other window dirty. |
| 4E — portable ancestry and activation | Export/import format, coherent snapshot, clone/reconciliation paths, then enable writable v3 only after 4B–4E gates pass together. | Exact graph/body/metadata/BLOB round trip, repeated export/import, collision rejection, unknown format rejection, restore on fresh host and missing ledger. |
| 4F — live verification | No new semantics; command registration, two real webviews, GUI/CLI/Hermes races, boot and restore exercises on disposable fixtures. | Full npm/native/build gates plus human acceptance; automatic merge still requires Stage 5 approval. |

All fixtures use disposable synthetic vaults. No live user vault/config is read or
modified for this proposal. Compile/tests cannot alone verify focus isolation,
restore coordination or fsync durability; the live and fault-injection gates are
part of implementation acceptance.

## Proposal verification and approval gate

- All 280 pre-existing tracked files matched their pre-authoring SHA-256 hashes
  after the requested gates: no application source changes.
- Proposed DDL executed successfully against disposable in-memory parent-table
  stubs, with an empty FK check. This checks syntax, not migration correctness or
  the native transactional invariants; those require implementation fixtures.
- `npm run check` attempted: blocked by missing `svelte-kit`.
- `npm test` attempted: 21 passed, 26 failed; dependencies including TypeScript,
  Svelte, Vite and CodeMirror are unavailable. These are not passing gates.
- `git diff --check` passed. Rust gates, formatting/clippy, frontend/release
  builds, packaging, boot and live restore/multi-window tests were not run.
- Only this proposal and `.session-commit-msg` are authored by this task; existing
  untracked AGENTS.md and TASKS.md remain untouched. No git writes or user-data
  access occurred.

Stop here for human approval of schema, retention and restore/ledger policy.
