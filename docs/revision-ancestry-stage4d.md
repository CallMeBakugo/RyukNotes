# Revision ancestry: Stage 4D recovery pins and retention

Stage 4D builds on the approved [schema](revision-ancestry-schema.md),
[writer transactions](revision-ancestry-stage4c.md), and
[conflict design](same-note-conflict-design.md). Production opens and creation
remain schema 2. Native v3 still requires the explicit ancestry feature/fixture
entry point. No migration, schema alteration, automatic merge, portable export,
or production activation is introduced.

## Accepted reads and owner identity

`revision_retention.rs` binds owners to logical vault UUID, persistent storage
epoch, note incarnation, revision ID and commit token. The private checkpoint also
records its local editor revision. The accepted snapshot retains the opaque token
and exact body/metadata; revision numbers remain decimal strings across JavaScript.
A different vault, epoch or incarnation cannot silently become an owner's base.
Clean release uses the departing owner's last acknowledged local revision, even
if the editor capture callback already exposes the next note's session.

`recovery_register_revision` uses `pinned_owner_read`: it verifies the captured
accepted token against immutable stored state, installs the owner base pin and
returns that exact accepted state in one immediate transaction. Registry insertion
happens while the recovery mutex remains held. A failed registration never grants
permission to dispose of the buffer. Legacy registration cannot enroll v3 owners.

There is a separate handoff before registration: the initial adapter read has no
editable owner yet. V3 accepted reads therefore acquire a conservative session
read hold before returning. A previously held revision can be read from a coherent
read snapshot; a new hold requires a fresh coherent read under the immediate write
helper. The receipt itself is unchanged. This closes the read-to-owner gap without
adding an editor-specific lease nonce to a writer receipt.

These provisional read holds deliberately over-retain. They are not automatically
released by successful owner registration, clean unregister, connection closure,
expired heartbeat, or missing journals. Each native connection holds a private
0600 OS-lock lease in the ledger directory for its lifetime. The inbox refuses
cleanup of another live connection's holds, including live CLI processes. After
that process ends, unclaimed read holds can be released only by the explicit
inbox acknowledgment. Reader lease files are not pruning authority: even a
missing lease/journal offers a cleanup choice, never automatic deletion. This
conservative policy can retain many old payloads until the user cleans up holds.

## Journal and resolution ordering

Checkpoint ordering is registry mutex → DB handle → ledger lock → SQL immediate
transaction. Base pins and the union of referenced attachment IDs are committed
first (`recovery_assets:v3:OWNER` in the existing protected metadata namespace).
Existing BLOBs and transient bytes must then be available as checksum-verified,
private durable bytes before the JSON checkpoint is acknowledged. Blob files,
JSON files and directories retain the existing atomic-write/fsync protocol and
0600/0700 permissions. A failure leaves native holds and unsafe owner state;
the renderer keeps its memory snapshot synchronously, including newer typing.

Checkpoint sequence fences, local revisions, vault/epoch/incarnation matching and
owner/window checks remain authoritative. A failed or lost checkpoint reply can
be reconciled against published records; it never authorizes pruning from an
absent local file. Event receipt and `data_version` polling do not change any
accepted baseline, saved revision, journal frontier or pin.

Resolution requires an explicit action (successful save, confirmed discard,
clean release, applied review, or read-only review cancellation) and matching
owner/base/local-revision acknowledgment. The service scans the ordered published
stream, refuses newer local revisions, and pins a successful save's next exact
accepted base before retiring its predecessor holds. A lost save reply can replay
only the matching durable resolution proof, and cannot resolve later typing.

Before SQL removes any pins, native recovery atomically publishes an immutable
ordered `tombstone-N.json`, then its `resolved-SEQUENCE` frontier, syncing each.
The tombstone records identities, local revision, covered checkpoint sequence,
exact pin set, retained next base and action. Only successful publication creates
an internal `DurableResolution` capability. Pin deletion requires this capability
and rechecks the exact captured pin set under the ledger/SQL lock. A newer hold,
journal failure or SQL/commit failure refuses acknowledgment and retains memory.
A crash between proof, marker and SQL can leave extra holds; it cannot authorize
an inferred release. Published records/private bytes are not erased by retention.

Crashed owners leave their pins. The recovery inbox first offers existing draft
and result artifacts through Keep both or explicit discard. Orphan groups with no
local journal get a separate confirmed “Release holds” action; native code checks
again for live owners, reviews, records, process leases and a changed pin set.
Old-epoch recovered artifacts may be copied or explicitly discarded only within
the same logical vault UUID. Copying uses their private bytes and preserves the
old ancestry as provenance; it does not rebase a stale editor or invent a merge
parent. Active editor forks keep the strict originating connection/epoch checks.

## Review lifetime and UI boundary

Review opening atomically pins verified committed B and reviewed current R before
returning either as review authority. The native response carries both revision
bindings. The existing review controller checks UUID, epoch, incarnation, revision
and commit identities against the immutable snapshots before constructing a plan.
The committed draft base and current predecessor are thus held through review and
possible apply. Uncommitted local text remains a private artifact.

Closing a read-only review writes its ordered cancellation tombstone before
releasing the review pins. Closing an edited review preserves its separate durable
result stream and holds for the recovery inbox. Successful CAS apply retires that
result only after the note commit and durable resolution proof. The source editor
owner still requires its own existing adoption/discard acknowledgment. Failed
publication/apply and newer typing retain artifacts and invalidate stale proposals.

The surgical changes are in the recovery utility, native owner/review service,
inbox and revision verification in the controller. `Editor.svelte`, NoteWindow,
AppLayout, pane providers, layout, focus/DOM/F3 ownership and all-pane job/barrier
implementations are unchanged. A clean paused window can still close while a
detached dirty window remains alive; it does not close or forget that other owner.
Vault-wide maintenance continues to refuse unresolved owners or recovered drafts.

## Payload and BLOB retention

`db_collect_revisions` is a native callable maintenance operation for explicitly
opened v3 vaults. There is no new startup scheduler or production activation.
The immediate transaction uses the established maintenance lease → ledger → SQL
lock order. Maintenance neither consumes a scoped note-write operation index nor
creates semantic note revisions; attempting a semantic mutation fails. Durable
ledger reservations may leave gaps, as with failed authoring transactions.

A state is eligible only when all predicates hold under the write lock:

- Its commit is older than 90 days.
- At least 100 newer revision headers exist for that incarnation.
- It is not any note's current head, including a hard-deleted head.
- No recovery, review, export or other valid pin references it.

The destructive statements recheck the predicates, insert a policy-versioned
pruning marker and delete only the eligible state. Its attachment links cascade;
headers, parent edges, incarnation identities and counters remain forever. Full
foreign-key and graph/payload validation precede commit. Failures roll back the
entire GC, including markers. Curated `versions` settings, snapshot cooldown and
history clearing never control revision retention or remove pins.

BLOB deletion additionally excludes every ownership domain: per-note links,
retained revision payload links, curated history bodies, durable staging holds,
private recovery asset holds, and persistent fork/apply receipts or pending-summary
artifacts that still reference the bytes. Receipt checks are deliberately
conservative. Deleting a note cannot cascade through shared BLOB rows. Private
recovery files remain outside this GC; an acknowledged checkpoint already has
private bytes as well as its in-vault holds. Explicit resolution is required to
retire recovery asset holds.

## Verification and limits

- `npm run check`: passed, zero errors, three existing warnings in untouched
  ConflictReview/Editor code.
- `npm test`: passed, all 49 test files.
- New controller tests cover v3 identity enrollment, local revision persistence,
  next-base handoff, failed journals/tombstones, unchanged dirty memory, reordered
  hints and refusing unproven review ancestry while preserving manual result edits.
- New Node SQLite fixtures execute the actual native retention SQL against
  disposable on-disk WAL files and independent connections: age/count/head/pin
  predicates, locking, rollback of pruning markers, all BLOB ownership domains,
  deleted note ownership, unrelated non-JSON metadata and malformed hold refusal.
  These test SQL behavior, not Rust or ledger orchestration.
- Native fixtures added for pin/review/checkpoint/proof/marker/release crash
  boundaries and restart; retention beyond 90 days with unresolved reviews;
  failed GC rollback; missing journals/live process cleanup refusal; exact/lost
  save acknowledgments and newer typing; shared attachment survival/private bytes;
  and mock-app IPC ownership, read-only review cancellation, dirty detached-window
  close/maintenance refusal and renderer loss. These require host execution.
- Not run: Rust check, check --tests, cargo test, feature-enabled native gates,
  fmt/clippy, native crash/IPC fixtures, builds, packaging, GUI boot, live
  multi-window, real restore or user-vault testing. Rust tools are hidden by the
  cage. Npm/SQLite success does not establish Rust compilation or crash durability.
- No Editor changes, user-vault/config access, network or git writes.

Stop after 4D. Portable ancestry export/import and activation (4E), then live
verification (4F), remain separate stages.
