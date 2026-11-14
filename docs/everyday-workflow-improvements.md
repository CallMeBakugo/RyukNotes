# Everyday workflow improvements and acceptance status

The user approved recon items 1–7 together. Theme/artwork/packaging and automatic
merge remain separate work; no network or live vault access is part of this pass.

## Delivered boundaries

- **Acceptance preparation:** `node scripts/stage4f-host.mjs /absolute/path/to/ryuk`
  runs an actual Tauri-built CLI against a new temporary vault with isolated
  config/data and ledger roots. It checks competing processes, stale tokens,
  metadata/body preservation and ABA refusal. It retains its report and prints
  the fixture location for the GUI exercises. It never substitutes for those
  exercises or selects the user's existing vault. It needs a host-built CLI.
- **Portable recovery:** the vault picker and DB Maintenance expose explicit
  ancestry-export full restore, independent copy, and moved/raw-vault recovery.
  Maintenance additionally offers same-vault reconciliation (which retains current
  heads). Source/destination/mode/operation ID survive retry. Native validation,
  immutable collisions, fresh epochs, missing-ledger enrollment and owner barriers
  remain authoritative. Default production ledger selection is native; the UI does
  not ask users to locate private ledger directories. The source is never edited.
  Raw recovery preserves identity and refuses duplicate enrollment when that UUID
  is already bound to a different existing file. Use an independent export copy
  when keeping the original. Old drafts/reviews are never silently rebound.
- **DB search:** new `db_search_page` returns the common SearchPage contract with
  bounded limit/offset, exact total, stable rank/ID ties, query-aware highlights,
  nearby body snippets and UTF-16 navigation targets. Count and page share a read
  snapshot. Because the persistent FTS index is contentless, only the bounded
  result page is hydrated into a private scratch FTS index for SQLite's own
  phrase/prefix/Boolean matching. The legacy CLI search command stays compatible.
- **Operation feedback:** failed sidebar/note-list actions appear in the invoking
  webview, with no focus transfer. Success expires; errors remain dismissible.
  Dismissal cannot erase a newer message. Batch delete/restore keeps failed rows
  visible/selected and removes only acknowledged successes. Migration and barrier notices are
  dismissible text, not authority to release holds. Unresolved recovery actions
  stay visible. The preceding successful-cleanup banner fix remains included.
- **Notebook trash:** deleting a DB notebook trashes the full subtree atomically
  through the revision helper. Metadata records the subtree and only notes newly
  trashed by that operation. Older trashed notes are not restored accidentally.
  Restore recreates the subtree; occupied paths refuse with guidance. Individually
  restored or deleted notes stay that way. Individually restoring a note whose
  notebook is still in Trash moves it to root. Other intervening semantic edits
  refuse group restore/delete. Only unchanged restore-baseline ancestry chains
  may bridge a backup/portable restore; ABA and reincarnation never qualify.
  Independent copies translate the recorded revision IDs with the graph. Group
  permanent deletion and empty trash remove group bookkeeping through the helper;
  shared bytes and revision history keep their established protections. All-pane
  and native owner barriers must pass. Clean main panes close trashed notes during
  reconciliation; dirty panes remain protected. No schema change was needed.
- **Conflict review:** correct the dialog element semantics and make the two
  Editor callback props reactive. These are the only Editor changes: no queue,
  merge algorithm or pane-policy changes. The reason for touching Editor is the
  approved warning investigation: updated focus/render callbacks must reach an
  already-mounted review.
- **Storage diagnostics:** separate live/trashed body bytes, attachment bytes,
  curated history, retained revision payload, physical DB/WAL bytes, logical and
  reusable SQLite pages, and private recovery files (clearly labeled all-vault).
  Categories overlap and must not be summed. SQL accounting uses a coherent
  snapshot and UTF-8 byte lengths; file observations are separately labeled.
  Explicit confirmed retention invokes the existing Stage 4D collector under
  maintenance/owner barriers and captured-vault validation. No new collector,
  scheduler, VACUUM, journal deletion, pin expiry or retention-policy change.

## Mounted UI harness

`npm run test:ui` serves a fixture-only page at
`http://127.0.0.1:5189/tests/ui/index.html`. Open it in a focused host browser.
It mounts the real ConflictReview, nested HistoryReview/SnapshotDocument,
SourceEditor, RecoveryInbox and OperationFeedback components. It checks distinct
IDs/ARIA references, renderer callback updates, inactive-pane F3 focus, typing
invalidation, cancel/reopen isolation, notice dismissal and success expiry.
Only inbox IPC is mocked. It does not simulate a native window or prove native
multi-window focus. The page reports PASS/FAIL explicitly; compilation is not a
pass for its browser assertions. `node scripts/ui-acceptance.mjs --build` verifies
that its real component/module graph bundles without adding dependencies.
`npm run test:ui:browser` runs those assertions headlessly using an already installed
Chrome (`CHROME_BIN` selects Chromium/another Chrome path). It uses a fresh private
profile, writes failure reports/screenshots to a temporary fixture directory, and
never downloads browser tooling. CI registers this gate against the runner's
installed Chrome. `npm run test:host -- /absolute/path/to/ryuk` is the CLI process
gate; neither browser mocks nor CLI-vs-CLI races replace native GUI acceptance.

## Host acceptance still required

This cage has neither the Rust toolchain nor a native app runner. Serving the
mounted harness on localhost was attempted and refused with `listen EPERM`,
including the approved escalation retry. Do not
mark item 1 or mounted-browser execution complete from frontend gate output.
Run the Rust gates and Tauri build on the host, the CLI process script above,
then the complete disposable-vault checklist in `revision-ancestry-stage4f.md`.
In particular record migration snapshots, actual two-webview ownership/focus,
CLI-versus-dirty-GUI winner order, Keep both/review, dirty-window close refusal,
restart recovery, backup restore and missing-ledger recovery. Exercise the new
notebook subtree restore after backup/full/independent restore, occupied paths,
individual note restore, shared attachments, native failure rollback, and the
explicit retention control with unresolved recovery/review pins.

The native fixtures added here cover IPC registration/wire arguments, recursive
trash/restore, occupied paths, ABA, rollback, graph-ID translation, raw recovery
retry/source preservation, UTF-8 accounting, and search page/snippet/target rules.
They are authored for host execution, not claimed as executed in-session.
