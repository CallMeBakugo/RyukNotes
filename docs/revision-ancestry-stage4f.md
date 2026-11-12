# Production activation and Stage 4F human boot gate

The human reported that collect passed the complete 4B–4E Rust/JS battery on the
pre-activation tree, and explicitly accepted host-side proof outside the cage.
That satisfies the activation precondition. This session activates production
schema 3 and prepares Stage 4F; **it does not claim live acceptance passed**.
The activation delta still needs its own host Rust/build/boot checks.

## Activated wiring

- `VaultDb::open` now uses `open_with_report`, creating schema 3 and transparently
  migrating recognized v1/v2 files with the existing 4B migrator. Preflight still
  rejects malformed/future schemas before mutations. Exclusive cross-process
  maintenance excludes managed live legacy connections; migration preserves its
  0600 `VACUUM INTO` pre-v3 snapshot, immediate transactional T9 transformation,
  orphan sweep, FK diagnostics/validation, rollback and durable ledger enrollment.
  It creates no intermediate v2 commit.
- Ordinary GUI and CLI attach the complete v3 writer session, with WAL, full
  synchronous durability and connection-local write guards. Existing v3 files
  with missing, retired or differently bound ledgers still refuse writes; opening
  is not permission to invent a restore epoch. Old v2 clients still reject 3.
- Desktop GUI and CLI share `dirs::data_local_dir()/ryuknotes/revision-ledger`.
  `RYUK_V3_LEDGER` overrides it for both, including default builds. Mobile startup
  injects the private sandbox ledger root alongside its existing config-root setup.
  The compatibility `revision-ancestry` Cargo feature name remains accepted;
  production migration, restore, writers and portable ancestry need no feature.
- Open commands return a receipt with schema, previous schema, vault UUID, storage
  epoch and optional status. `dbOpenVault` forwards that one line to the existing
  recovery/status surface, retained through startup/picker unmount. Migrating v1/v2
  visibly reports the upgrade and private snapshot. CLI opens report to stderr,
  leaving JSON stdout intact. Later ordinary/directory opens clear old notices.
- Production backup creation uses the durable private snapshot helper. DB backup
  restore uses matching-UUID schema-3 restore, fresh epochs/baselines and the
  existing all-window barriers, rather than copying over a v3 file with v2 logic.
  Foreign or mixed-schema backups reject before closing the displayed connection.
  To recover a pre-v3 backup, copy it to a separate disposable `.rn` file and open
  that copy for migration; do not overwrite a v3 vault with UUID-less backup bytes.
- Restore returns recovery-review requirements and broadcasts `vault-restored`.
  Clean pane/window reconciliation compares accepted tokens as well as raw bytes,
  so unchanged Markdown still adopts a fresh epoch. Dirty buffers are not replaced.
  An old clean **paused** owner can explicitly retire its pin across a same-UUID
  restore only through its matching local-revision/durable-tombstone acknowledgment;
  no dirty owner, save baseline, or next accepted state can cross the epoch there.
- A failed/refused restore does not invalidate a still-valid adapter acceptance
  lineage. Uncertain/pending restores remain closed for explicit recovery.
- Explicit Keep both can preserve a legacy v2 checkpoint after in-place migration
  only with its captured physical origin, or a captured logical UUID for revision
  journals. It creates a fresh note with unknown/source provenance, never a guessed
  merge base or generated-summary authority. Active stale saves remain fenced.

The only component changes are the one-line notice surface and token-aware clean
reconciliation checks in AppLayout/NoteWindow. Editor, pane providers, separator,
focus/shortcut ownership, save timers and the same-note pane ruling are unchanged.
Legacy v2 unit contracts use a test-only frozen opener; production creation tests
and executable CLI tests now exercise v3. In-memory v2 scratch fixtures are not a
production vault creation route.

## Registered inspection and maintenance entry points

All existing owner/checkpoint/fork/review/pin commands remain registered. Production
also exposes portable restore/copy/reconciliation, retention GC and ordinary v3
backup restore. New `db_revision_status({ vaultPath })` is metadata-only and returns:

- schema, vault UUID, storage epoch and native connection ID;
- ledger root and that connection's `data_version` hint;
- immutable header, retained state, pruning marker and pin counts;
- live owner IDs, window labels, note IDs, dirty/conflicted/locked flags, sequence
  and accepted revision bindings, with no bodies or private journal bytes.

Diagnostic reads do not advance a baseline, savedRevision, journal frontier or
polling observation. `data_version` is connection-local and is not a global clock.
A new native connection has a new connection ID. In desktop devtools, the existing
bridge can inspect a disposable open vault without importing a browser module:

```js
await window.__TAURI_INTERNALS__.invoke('db_revision_status', {
  vaultPath: '/absolute/disposable/vault.rn'
});
```

## Human-only run, after collecting this activation delta

Run the native/default and compatibility-feature gates plus the Tauri build
host-side first. The user-reported pre-activation pass is not a pass for these new
changes. Release binaries must be built through the Tauri CLI per AGENTS.md;
never substitute a plain cargo release artifact. This agent did not build or boot.

Use a fresh temporary root, isolated app config/data, and one shared ledger for GUI
and CLI. Set these in a dedicated host terminal before launching either binary:

```sh
RYUK_4F_ROOT=$(mktemp -d)
export RYUK_V3_LEDGER="$RYUK_4F_ROOT/ledger"
export XDG_CONFIG_HOME="$RYUK_4F_ROOT/config"
export XDG_DATA_HOME="$RYUK_4F_ROOT/data"
```

Launch the Tauri-built GUI and CLI from that terminal. Use only `.rn` fixtures
under this root, not the real Notes/config paths. Set the GUI backup location to
`$RYUK_4F_ROOT/backups` before making backups. Keep failed artifacts for inspection;
do not clean or resolve an unresolved draft merely to make a gate green.

### 1. Ordinary creation and transparent upgrade

1. Create a disposable vault through ordinary `ryuk --vault PATH notes new TITLE`
   (stdin/body flags as usual), then open it in the GUI. Inspect schema 3, UUID,
   epoch and ledger location; both processes must use the same ledger without a
   feature flag. Creation stderr must not pollute CLI JSON stdout.
2. Use separate synthetic v1 and v2 fixtures from the native migration tests or an
   isolated old binary. Opening each in the GUI must display the single upgrade
   line. Inspect the `.pre-v3-*.bak` permissions (0600), original pre-upgrade rows,
   upgraded byte/BLOB equality, trashed notes and legacy unknown history. Reopening
   must not create another baseline/snapshot or repeat a migration success notice.
3. Hold a managed legacy connection during upgrade: opening must refuse as busy,
   retain the original schema/data and succeed after the old connection closes.
   Check malformed/future metadata refusal on disposable copies only.
4. The real old v2 executable must reject the upgraded file with
   `Unsupported vault schema version 3`; never let it write around that refusal.

### 2. Two real webviews and same-note ownership

1. Open note A in the main window, open the second pane and select A again: activate
   the existing pane; do not create another editable A buffer in that window.
2. Open A in a detached NoteWindow. Inspect two distinct owner IDs/window labels
   with coherent accepted revision bindings. The detached view must read DB data,
   and cannot switch to a different vault. The second main pane remains independent.
3. Edit both windows, deliberately arranging a winning save before the stale save.
   The stale writer must retain its exact unsaved draft and show the owner-scoped
   conflict banner. Check Keep both, review cancellation/renewal, source-mode edits,
   late/reordered hints, and attachments shared by the original and recovered note.
4. Confirm an inactive window/pane never gets a surprise modal or stolen focus.
   F3, find, title focus, drops and file/image controls remain owned by their pane.

### 3. CLI and generated-summary races

1. Capture `accepted.token` from `ryuk --vault PATH notes show ID --json`. Keep the
   GUI buffer dirty while a CLI `notes update ID --expected-token TOKEN --stdin`
   commits. If the GUI wins first, CLI must exit 3 without overwriting it; repeat
   with a fresh token to exercise the other ordering. Neither failure may clear
   dirty memory, advance raw/savedRevision, or redirect a different pane's load.
2. CLI tag mutations require the current v3 expected token and preserve the body.
   Confirm metadata-only races and a body A→B→A still reject stale revisions.
3. Import synthetic session-summary JSON locally; no real Hermes/network session
   is needed. Repeated generated input is a no-op. After a manual edit, including
   return to identical text, incoming summary content must remain a labeled pending
   artifact with a reported conflict. A recovery copy must not be restorable as the
   original `hermes/session` summary.

### 4. Restore, restart and retention roots

1. Save both windows, create a GUI backup in the isolated backup directory, then
   make a later saved edit. While another window has an unresolved dirty/conflicted
   draft, restore, vault switch and close of that dirty owner must refuse disposal.
2. After explicit resolution/save and all-pane pause, restore the backup. If the
   existing coordinator refuses a detached owner rather than pausing it, record
   that safe refusal and close it explicitly after saving; never bypass the
   barrier to force a two-window restore. Reopen it afterward to check its fresh
   binding. When both owners can pause, inspect both throughout. UUID stays;
   epoch changes, counters/baselines advance above the ledger high water and old
   tokens reject. Inspect both main and detached accepted bindings, including a
   backup with **identical Markdown bytes**. Clean old owners must retire, new ones
   must register, and no baseline may be silently rebound for a dirty buffer.
3. A foreign-UUID or legacy backup must refuse before closing the current v3
   connection. Verify a failed restore still permits a valid subsequent save.
4. Restart on disposable checkpoints/reviews. Unresolved pins/artifacts persist;
   missing journals or expired heartbeats do not prune them. Explicit inbox cleanup
   must retain newer typing and require its matching durable acknowledgment.
5. On another disposable host-data/ledger root, a raw v3 copy must refuse ordinary
   writes without enrollment. Use explicit portable full restore into an empty
   destination with that new ledger: require a fresh epoch and recovery review.
   Repeating the same restore operation must not create another baseline.
6. Export/re-export/import mixed image and non-image attachments, retained/pruned
   ancestry and shared bytes. Verify native status/inspection and byte hashes.
   Retention age/count/failure/crash boundaries remain covered by native fixtures;
   do not edit immutable headers or the system clock to simulate them in the GUI.

Record binary/build identity, gate output, fixture paths, UUID/epoch/connection
IDs, token/winner ordering, owner states and failures. Keep both available when
review cannot establish a verified base. Automatic merge still has no approval.

## This session's verification

- `npm run check`: passed, 0 errors and the same 3 existing warnings.
- `npm test`: passed, all 52 test files; new tests cover visible open status,
  failed migration, same-byte/new-token reconciliation and refused-restore lineage.
- `git diff --check`: passed.
- Added/updated native fixtures for production v1/v2/empty opens, private snapshots,
  legacy-client refusal, busy/missing ledger, default CLI ledger resolution,
  v3 tag/summary/Hermes paths, real mock-app open/status/restore IPC, dirty detached
  refusal, clean old-epoch pin retirement and explicit legacy recovery copies.
- NOT RUN here: Rust check/check --tests/test, feature-enabled tests, fmt/clippy,
  current activation native fixtures/CLI binaries, builds, packaging, boot, actual
  two-webview/CLI/Hermes races, real restore or live-vault exercises. Rust tools are
  hidden in the cage. No GUI verification was attempted.
- No live user data/config access, network or git writes. `.session-commit-msg`
  records the changes and verification boundary. Stop for the human boot gate.
