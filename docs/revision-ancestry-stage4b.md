# Revision ancestry: Stage 4B foundation

Stage 4B implements the identity/migration foundation from the approved
[proposal](revision-ancestry-schema.md). Stage 4C and later remain separate work.
No Editor, frontend, conflict-resolution UI, autosave, or official writer rollout
is included here. The 90-day/newest-100 retention policy is represented by the
schema; no payload GC is enabled in this stage.

## Activation boundary

- `VaultDb::open` and `open_in_memory` still create/use **schema 2**. They refuse
  schema 3 even in a feature-enabled build. The ordinary GUI/CLI cannot admit a
  v3 vault to unfinished v2 writers.
- `vault::ancestry::Inspection` provides a read-only v3 identity, note/head and
  revision/parent/payload inspection API. Its connection is private, opened with
  SQLite READ_ONLY and query_only; each instance has a fresh runtime session
  generation. Live inspection leases exclude replacement/migration.
- `vault::ancestry::{migrate, restore}` are compiled only for unit fixtures or the
  explicit native Cargo feature `revision-ancestry`. No Tauri command or CLI
  migration flag activates them. They take an explicit private external ledger
  directory. Stage 4C must integrate accepted identities/capabilities; 4D integrates
  durable recovery pins; 4E owns production activation and portable export/import.
- The existing restore command rejects v3 sources and targets before closing the
  current v2 connection. A mock-app IPC regression covers that guard. The v2
  restore path also takes exclusive target/shared backup maintenance leases and
  rechecks schema under them, preventing a race with explicit v3 migration. The existing
  runtime `vault_db_epoch` remains in the production v2 lifecycle; v3 has no
  admitted application writer/owner to rebind in this stage.

## Maintenance and migration

File-backed v2 connections hold a shared OS lock for their lifetime, in an
owner-only `<canonical-vault-path>.maintenance.lock` sidecar outside the database.
V1 upgrade, explicit v3 migration and v3 restore take exclusive leases. They
refuse busy locks rather than wait on a pane/window or upgrade a live shared
lease. File close precedes lease release. Never delete or replace a live lock
sidecar. This coordinates cooperating processes; pre-4B already-open clients and
arbitrary SQL are not made safe by an advisory lock.

Canonical schema metadata is probed before creating a maintenance sidecar,
ledger, snapshot, or changing journal mode. SQLite read-only mode can itself
create WAL/SHM sidecars, so preflight uses immutable mode only without WAL frames;
with frames it reads a stable private temporary copy of main+WAL. It checks for
concurrent file changes and refuses/retries boundedly. Actual schema metadata is
checked again under maintenance and inside the immediate transaction. No caller
should use an unlocked probe result as write authority.

Explicit migration targets v3 from empty/v1/v2. A recognized nonempty source gets
a private 0600 `.pre-v3-<UUID>.bak` through VACUUM INTO and file/directory fsync.
An empty source has no prior data requiring a snapshot. Partial snapshot files
are removed on failure. The original remains untouched by future/malformed
version rejection. V1's T9 attachment transformation and safe orphan-note_tags
sweep run in the same transaction as the graph creation, with no v2 commit.

Each existing note, including trash, gets a fresh incarnation and exactly one
full unknown-ancestry migration baseline. Existing curated versions retain exact
IDs, bodies and timestamps, with unknown ancestry and no revision reference.
Validation includes preserved source field/BLOB digests, full-state/head equality,
BLOB hash/size, graph ordering/counters, SQLite quick_check and full FK diagnostics
with table, rowid, parent, FK index and count. Only then is schema=3 committed.

Injected snapshot, DDL and validation failures roll back without changing source
state. The commit fixture introduces a deferred FK violation after validation so
SQLite COMMIT itself fails and the transaction must roll back all schema/data
changes. Valid pre-upgrade snapshots remain available for explicit recovery.
Ledger reservations are deliberately not rolled back; abandoned fresh migration
UUID ledgers contain only private metadata, not note bodies. A failure after a
successful commit's final durability sync is reported as an uncertain successful
migration requiring read-only inspection, not falsely described as rollback.

## Ledger and restore

The private ledger root must have 0700 permissions on Unix; lock/data files are
0600. Ledger UUID names are validated; unsafe file types/permissions are rejected.
An OS interprocess lock protects each vault UUID. Atomic temporary-file replacement
plus file fsync and directory fsync precedes every acknowledged reservation.
Reserve above both DB and ledger high water, with checked overflow and gaps after
failed commits. Lock order is maintenance, then ledger, then the SQLite write
transaction. No UI or webview IPC occurs while these are held.

A restore validates matching source/target vault UUIDs, stages a consistent copy,
rotates the storage epoch and creates restore-baseline children above reserved
high water, retaining the restored predecessor and exact full state. It preserves
UUID/incarnation identities and attachment references. The ledger records a
pending restore epoch before replacement and finalizes it afterward, so interrupted
replacement cannot masquerade as an observed ordinary write. Failures after
replacement/sync/finalization explicitly require recovery inspection.

Live managed connections block restore. Target WAL is checkpointed before atomic
replacement, and remaining sidecars refuse replacement rather than replaying old
WAL. Old backups themselves are unchanged. Reopening creates a fresh inspection
session generation; the returned `RestoreOutcome` exposes ledger loss and always
requires recovery review before any draft rebinding. No v3 application binding is
silently restored here.

With an intact host ledger, rolling back a backup cannot reuse previously reserved
numbers. After ledger loss or movement to another host, numeric order against a
lost future cannot be proven: the new epoch distinguishes histories. This does
not solve arbitrary direct SQL, external ledger rollback or multi-host concurrent
writing. Epochs and UUIDs are identity, not timestamps/content-hash guesses.

Strict v3 durability currently requires a Unix directory-fsync backend; other
platforms fail closed rather than acknowledge a reservation without it. The
existing v2 migration path remains available without that new platform contract.

## Verification and host gates

Added native fixtures cover empty/v1/v2 migration, independent live WAL
connections, exact note/metadata/history/BLOB preservation, orphan cleanup/FK
failure diagnostics, snapshot/DDL/validation/actual-COMMIT rollback, malformed and
future versions, read-only enforcement, reservation persistence/failure/overflow,
UUID rename/restore, counter rollback, ledger loss, foreign backups, and child
process restore exclusion/order. Tests use only disposable synthetic vaults.

The frozen pre-4B v2 version gate (audited against af52dd5 with read-only git
show) has a regression refusing schema 3. An optional
ignored `actual_v2_cli_refuses_v3` test accepts `RYUK_V2_BINARY` pointing to a real
pre-4B CLI and uses a disposable vault; it must be run host-side to establish
actual old-binary compatibility. A copied gate test is not that proof.

In-session results:

- `npm run check`: passed, 0 errors, 3 warnings in untouched conflict-review code.
- `npm test`: passed, all 47 files.
- `git diff --check`: passed.
- Supplemental Python/SQLite smoke: extracted production DDL/migration/validation
  SQL passed four real-WAL v1/v2 upgrade/rollback cases with private snapshots and
  exact note/history checks. This does not compile or execute the Rust APIs.

Not run: Rust check/check --tests/test, feature-enabled check/test, fmt/clippy,
actual old CLI compatibility, native child-process/IPC fixtures, frontend/release
builds, packaging, boot and live GUI/CLI/restore tests. Rust tools are unavailable
in this cage. Host verification must include default and `revision-ancestry`
feature builds and execute the native suite; the source is not yet Rust-verified.
No live vault/config access or git writes occurred. Stop after 4B.
