# Revision ancestry: Stage 4C all-writer transactions

Stage 4C implements the writer portion of the approved
[ancestry schema](revision-ancestry-schema.md) on top of
[Stage 4B](revision-ancestry-stage4b.md). It does not enable production v3 vault
creation/opening, recovery pins/GC, portable ancestry export, or automatic merge.
Those remain the separately gated later stages. No Editor source is changed.

## Entry points and transaction boundary

Default `VaultDb::open` still creates/accepts v2 and rejects v3. Explicit native
`open_v3(path, ledger_root)` is compiled for unit fixtures or the
`revision-ancestry` Cargo feature. It validates a coherent read snapshot, checks
the captured vault/ledger epoch and path, holds the established shared maintenance
lease and installs TEMP DML guards. Feature-enabled CLI tools/tests may explicitly
select the same session using `RYUK_V3_LEDGER`; the default binary cannot.

`revision_writer.rs::WriteTransaction` owns the external ledger lock and one SQL
transaction. Order: maintenance lease → ledger lock → durable ticket reservation
→ BEGIN IMMEDIATE → predecessor capture → explicit route policy/projection writes
→ revision/full-state/parents/asset links/counter/head → exact receipt → commit.
The SQL transaction ends before the ledger unlocks, including error paths.

One ticket above the global high-water floor can serve every changed note in a
batch, including note IDs generated inside the transaction. Per-note revisions
are strictly increasing, not contiguous. Writer receipts encode i64 revision
numbers as decimal strings so JavaScript/journal round trips stay exact beyond
the safe-integer range. External ledger format 2 makes older
4B tools fail closed on the new global floor; new tools read format 1 migration
ledgers and upgrade only when reserving a global ticket. Missing/corrupt ledgers,
pending restores, path/epoch mismatch, overflow and disk failures refuse writes.

Full-state comparison includes exact body bytes, title, tag set, pinned,
trash/deletion, creation/modified fields, notebook membership, session binding,
legacy generated provenance and attachment ownership. A fresh incarnation records
its deleted predecessor's identity as provenance without claiming a cross-life
merge parent. Existing immutable creation fields cannot be changed. True no-ops
retain the current revision and modified time; semantically changed non-import
writes receive commit-time modification timestamps.

The helper captures all affected states, so multi-note notebook repaths and
empty-trash cannot leave an unchecked note behind. Failed SQL, validation or
commit rolls back projections and graph together; a reservation gap is retained.
Receipt construction prepares the graph first, under the write lock. Semantic
writes after preparation refuse commit. Returned read/write receipts never obtain
a baseline by rereading after commit.

## Writer-contract audit

| Official route | Wiring in 4C |
| --- | --- |
| Editor save, title rename, history restore | `writer.rs::write_policy` → shared helper; `api-db.ts` retains full v3 accepted descriptor and pending operation ID. History restore is a new child of the current stored head. |
| Task done/priority/due | Existing adapter read-and-CAS path uses `writerSource: task` for v3; stale body or metadata rejects. |
| GUI new, daily, duplicate; CLI new | Absent-only create. Same public ID after hard deletion gets a fresh incarnation; a daily create race selects the existing note. CLI keyed creation derives a stable fresh note ID for replay. |
| CLI content update | Requires accepted v3 token; reconstructs metadata from that exact revision, then CAS. `--operation-id` replays its original committed receipt even after a later writer. |
| CLI/native atomic tags | Fresh atomic field intent preserves body/title/pinned; supplied expected tokens are checked inside the helper. V3 CLI requires `--expected-token`. |
| Single-note import | `import_single_receipt` finalizes rewritten body, BLOB references, notebook membership and receipt in one helper transaction. Scoped retry returns the original copy. |
| Plain bulk import | Per-note explicit copy/replace policy under the helper; v3 trash state finalizes in that same transaction. Scoped ordered item replay does not overwrite a later edit. |
| Indexed v2 export import | One helper transaction includes notes, lifecycle, assets, history, notebook layout, tags and session metadata. Protected internal operation/recovery keys cannot forge v3 authority. Generated v1 digests remain unproven legacy provenance. |
| Generated summaries | Exact last-generated revision binding; repeated identical generated input is a no-op. Manual/metadata/lifecycle changes, including A→B→A, preserve incoming content/JSON in pending artifacts and report typed conflict. |
| Trash/untrash, delete, empty-trash | Helper lifecycle transactions retain tombstones, incarnation identity and counters. Empty-trash commits all items atomically, with per-note operation items; same-key retry never deletes newly trashed notes. V2's existing behavior is preserved. |
| Notebook creation/move/repath | Shared helper captures every changed note assignment. Layout-only changes with no affected note are auxiliary and create no fictitious note revisions. |
| Attachment upload/reference changes | Standalone upload has durable staging holds; body/reference acceptance attaches ownership and graph atomically. Import and recovery finalize assets inside their own note transactions. Shared bytes survive deletion through revision links. |
| Session metadata; generic state | Session bindings are semantic revisions; repeated identical binding is a no-op. Preferences are auxiliary. Generic v3 metadata writes cannot replace authoritative operation, summary, recovery or staging records. |
| Recovery fork and explicit review apply | Both use the helper; existing retry/payload fencing remains. Apply verifies reviewed CAS and records current R as predecessor plus proven committed B as draft base; L remains an uncommitted artifact. |
| Curated history checkpoints | Helper protects rights/links. Imported/deduplicated legacy rows remain ancestry unknown. Only newly captured native current/predecessor snapshots may bind a verified revision. No cooldown throttles graph commits. |
| Legacy upserts | Explicitly unavailable for v3. TEMP guards reject forgotten direct projection writes outside the helper. Read-only export transactions, migration and restore are separate native boundaries, not authoring backdoors. |

V2 retains its prior state-token/policy behavior, summary digest policy, direct
metadata/lifecycle behavior and snapshot settings; formerly deferred auxiliary
transactions stay deferred. The helper adds no v3 graph or ledger to v2. No UI or
pane destination/focus routing is changed.

## Retry and recovery details

A v3 `NoteWrite.operationId` or native `with_write_operation(id, request, action)`
scope binds input/policy and source. Each scope has ordered sub-operations and a
durable root claim; each graph row uses the affected note ID as its item key.
Dispatch without a key is a fresh generated intent. Metadata points to revision
IDs for replay, and never substitutes current state if the original payload was
pruned. Full-body receipts already used by the older recovery fork/apply protocol
remain under that protocol; 4D must account for them before enabling cleanup.

The adapter keeps a failed call's operation ID while its exact input/base remains
the same. It refuses stale replies after session invalidation or after another
acknowledgment advanced the lineage. An IO failure or event does not adopt R as B,
clear dirty state, or acknowledge savedRevision. Existing queue and recovery
barriers continue to fence local revisions. Old v2 tokens/journals do not acquire
v3 ancestry by body resemblance, and old restore epochs cannot be rebound silently.

Uploads intentionally do not advance the uploading editor's accepted note: they
persist private staging metadata inside the vault and immutable deduplicated
bytes. The subsequent accepted reference promotes a per-note link and clears
that staging hold in the note transaction. Other staging holds, unresolved drafts
and revision references remain retention roots for 4D. No staging expiry or BLOB
GC is introduced here. Portable v3 export explicitly refuses until 4E rather than
silently emitting a v2 index without the revision graph.

## Verification

- `npm run check`: passed, 0 errors and 3 existing warnings in untouched conflict
  review/Editor code.
- `npm test`: passed, all 48 test files. New adapter regressions cover lost replies,
  late invalidated replies, reordered no-op acknowledgments and failed task writes.
- `git diff --check`: passed. SQLite prepared the extracted revision-writer SQL
  against the actual v3 DDL; this checks SQL syntax, not Rust execution.
- Native fixtures added: both connection orders, ABA, metadata/lifecycle races,
  delete/recreate, body-token rejection, exact historical receipts, tag races,
  summary manual-return-to-same-state, operation reuse/replay, batch deletion,
  notebook/session changes, staging/shared assets, single/plain/indexed import,
  disk-full/ledger refusal, and a subprocess exiting after a durable reservation.
  Mock-app IPC and feature-enabled executable CLI regressions exercise the wire
  contract, including default-mode refusal of v3.
- Not run: Rust check/check --tests/test, feature-enabled check/test, fmt/clippy,
  native fixtures/IPC/subprocess/CLI execution, frontend/release builds, packaging,
  boot and live multi-window/CLI/restore tests. Rust tools are hidden in the cage.
  Native source remains host-unverified; npm gates do not validate it.
- No user vault/config access, Editor changes, network or git writes.

Stop after Stage 4C. Recovery pins/retention (4D), portable ancestry/activation
(4E), and live verification (4F) require their own implementation requests.
