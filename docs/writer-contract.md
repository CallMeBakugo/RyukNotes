# Authoritative DB writers — Stages 1 and 4C

The UX ruling remains fork first, review second, automatic merge last. Editor
and pane ownership are unchanged. The original Stage 1 **schema 2** contract is
preserved below. Stage 4C adds the complete **schema 3** writer contract described
in [the implementation audit](revision-ancestry-stage4c.md); no production format
activation, retention GC or new conflict UI is included.

Schema 3 tools must explicitly opt into the native `revision-ancestry` feature
and `VaultDb::open_v3(path, ledger_root)`. Ordinary GUI/CLI opens still create/use
schema 2 and reject schema 3 until Stage 4E. Feature-enabled CLI tests/tools can
set `RYUK_V3_LEDGER` explicitly; default builds do not honor it. There is no
partial fallback from a v3 writer to the body-digest contract.

## Schema 2 accepted state and acknowledgments

A DB read returns an opaque state token and captured vault identity with the exact
body, title, tags, pinned flag and trash state. Tokens hash these semantic fields
(tags as an unambiguous sorted set), note ID and vault identity. Modification times
are not revisions. `db_save_note` requires an explicit create or update policy.
Updates compare the full accepted token inside `BEGIN IMMEDIATE`, before history,
FTS, attachment-link or note changes. Missing, trashed, changed and replaced-vault
states return distinct typed errors. An update cannot recreate a missing note.

The command reads its resulting note and token before committing the same write
transaction and returns that captured receipt. The adapter never reads after a
save to discover what it acknowledged. Acceptance lineages are private to a read:
a task or another editor's read cannot promote a dirty buffer's baseline. Queued
saves may follow their own successful acknowledgments. Rejected saves do not
advance that lineage or the existing revision queue's saved revision/raw baseline.
Only the four typed conflicts acquire the existing `Save conflict:` display
prefix; I/O and policy errors are not classified by matching arbitrary messages.

Vault identity is canonical path plus filesystem identity (device/inode on Unix;
creation time on other platforms), captured when opening the connection. Read and
write paths validate the originating vault; saves also carry the read identity.
This is replacement detection, not a persistent revision or incarnation scheme.
**ABA remains:** A→B→A, identical delete/recreate, and an in-place restore to the
same semantic state are indistinguishable. Non-Unix creation-time identity has
platform-dependent precision. Arbitrary SQL and non-cooperating file replacement
are not made safe by this contract. Schema-level revisions and restore generations
remain future work; no schema changes are included here.

## Official mutation policies

| Route | Policy |
| --- | --- |
| Editor save / history restore through saveNote | Full-state guarded update; configured existing GUI prior-body history cadence. |
| Task done/priority/due | Captured read then guarded update; a later writer causes rejection rather than replaying stale body or metadata. |
| Title rename | Captured read then guarded update. |
| New note / duplicate / daily note | Absent-only create. Single-note and duplicate copies use fresh IDs; a concurrent daily creation selects the existing note. |
| Single external note import | Copy, allocating a fresh ID on collision. Attachment rewriting and receipt capture occur in the same transaction; finalizing that newly created row is not a second replacement. |
| Bulk Markdown / indexed export import | Explicit replace by imported identity, preserving a differing committed predecessor body in that note's import transaction. This is deliberate import overwrite, not an editor CAS. Re-import equality creates no extra predecessor snapshot. |
| CLI notes new | Fresh-ID absent-only create; existing title/body/stdin behavior preserved. |
| CLI notes update | Required expected token; full-state guarded update, preserving current metadata. Every changed predecessor body is checkpointed without pruning GUI history. |
| CLI notes tag | Atomic field intent: acquire the write lock, read fresh state, edit tags only, preserve current body/title/pinned. Reject missing/trashed notes. |
| Trash / restore / delete / empty trash | Explicit atomic lifecycle intents, not whole-note replay; subsequent guarded editor updates observe missing/trashed/changed state. Existing all-pane barriers remain responsible for resolving local drafts. |
| Notebook moves, state, attachment links, explicit history checkpoints | Explicit field/auxiliary operations; no replay of an accepted whole-note body. Existing vault-job and pane barriers remain. |
| Session-summary import | Generated-state replacement only, described below. |

The low-level legacy `upsert_note*` helpers remain for library fixtures; official
content writers use the explicit policy boundary. Tokens do not guard unrelated
notebook/state metadata and are not a general distributed lock.

## Schema 2 CLI and generated summaries

`ryuk --vault vault.rn notes show ID --json` exposes `accepted.token`.
Pass that exact token to
`ryuk --vault vault.rn notes update ID --expected-token TOKEN --stdin`.
A successful update prints its committed JSON receipt. Conflict errors from
update, tag and summary import are JSON on stderr with exit status **3**; other
errors exit **1**. Tokens are opaque; scripts must reread after a conflict rather
than retry with a guessed baseline.

Summary imports keep deterministic IDs and the existing Hermes JSON shape.
Provenance uses the existing `meta` table, without a migration: a digest records
the last generated semantic state. Exact repeats are no-ops. Only a note still
matching that generated state can be replaced; the previous body is checkpointed.
Manual body or metadata changes, trash, and legacy summaries without proven
provenance prevent replacement. The incoming rendered note and original JSON are
committed as a labeled pending artifact in `meta`, and the command reports a typed
conflict with its artifact key. Repeating identical input against identical saved
state preserves one artifact. `ryuk session pending SESSION-ID` retrieves these
artifacts as JSON. No GUI, automatic promotion, or resolution workflow is added.
An unsaved manual GUI buffer is not yet visible to this policy: its next guarded
save rejects, keeping that dirty draft in the existing save queue.

## Verification boundaries

Adapter tests exercise exact acknowledgments, independent acceptance lineages,
metadata/lifecycle conflicts, guarded tasks and daily creation races. Native tests
cover separate-connection schedules in both orders, exact receipts, full state,
missing/trash/replacement, tags, creation, summary provenance and ABA. Mock-app IPC
and executable CLI tests cover the real wire contracts and exit/artifact behavior.
Rust tests and formatting must be run host-side; npm gates do not compile Rust.

## Schema 3 accepted state and writes

A coherent read returns the full current projection plus an opaque `revision-v3`
token binding vault UUID, restore epoch, note incarnation, revision UUID, commit
UUID and full-state digest. The adapter retains the immutable revision descriptor
with accepted metadata. Old `state-v1` tokens are invalid policies for v3 writes.
Metadata/lifecycle ABA and identical delete/recreate no longer compare equal.
The runtime still validates canonical path/file identity and its captured epoch.

`WriteTransaction` acquires the private ledger before BEGIN IMMEDIATE, then
captures predecessor state under that same SQLite transaction. It records every
semantic change, full state, known parent edges, attachment references, counter,
head, projections and exact receipt before commit. No-op semantic writes keep the
existing revision/modified time. Failure never returns a successful receipt. A
second prepare refuses mutations after receipt construction. Metadata, lifecycle,
notebook/session changes and imports cannot bypass this helper; connection-local
TEMP guards reject forgotten direct DML, and legacy upserts explicitly refuse v3.
The guards are a backstop for official routes, not protection from arbitrary SQL.

Use `NoteWrite.operationId` for editor/scripted writes, CLI `--operation-id`, or a
native `with_write_operation(id, request, action)` scope for multi-item/field
routes. Without an explicit key, a dispatch is a fresh intent with a generated
operation ID. Scope claims bind root input and ordered child operations; each
revision records its note ID as the operation item. Reusing a committed key with
another input/policy rejects; retrying the same key returns its own prior receipt
without changing a later writer's state. Retained operation metadata references
revision IDs, not duplicate full-body receipts. A pruned historical result refuses
replay instead of becoming a fresh write.

A global ledger ticket may be shared by all affected notes in one transaction.
It is reserved/fsynced above every known ledger/DB counter before the DB write
lock, so even IDs allocated inside imports receive strictly increasing per-note
numbers. This deliberately permits gaps. The external ledger upgrades to format
2 when global tickets are first used; older 4B tooling rejects that format rather
than silently ignoring its floor. Existing format-1 migration ledgers remain
readable. Restore uses the global floor too; ledger loss still requires explicit
epoch-changing recovery and cannot prove order against a lost future.

Generated-summary replacement requires the exact last generated revision ID,
not a body digest. A manual edit followed by a return to identical text still
preserves incoming summary JSON/rendered content as a labeled pending artifact
and reports conflict. Imported legacy digests do not prove generated ancestry.
Exact generated repeats are no-ops; authoritative deletion/trash is respected.
CLI v3 tag intents require `--expected-token` and preserve the body under the same
atomic lock; native fresh-field tag intents can use freshly read state. Scripted
content retries reconstruct metadata from the supplied accepted revision, never
from a racing writer's current metadata.

Standalone v3 attachment uploads stage bytes in durable `attachment_stage:v3`
metadata without advancing the note. Accepting the body reference commits its
note ownership and revision together; this prevents self-conflicts during paste
and drop. Import/fork asset finalization remains within their note transaction.
Stage 4D cleanup must honor staging holds and revision references as well as
recovery/review pins. No GC is enabled here.

V3 adapter pending operation IDs survive a lost reply for identical input. Late
or reordered acknowledgments, another vault generation, or another incarnation
cannot advance the lineage. Failed writes do not change raw/savedRevision; the
existing save queue remains responsible for acknowledging only current local
revisions. Recovery review binds only verified committed B as a draft-base edge;
uncommitted L is never invented as a parent. Pre-restore journal tokens cannot
silently rebind to a new epoch.
