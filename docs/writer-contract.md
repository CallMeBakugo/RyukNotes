# Authoritative DB writers — Stage 1

The UX ruling remains fork first, review second, automatic merge last. This stage
adds no conflict interface or merge engine. Editor and pane ownership are unchanged.

## Accepted state and acknowledgments

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

## CLI and generated summaries

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
