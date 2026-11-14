# TASKS.md — Ranked backlog for Codex

Maintained by the human and Hermes. Agents: work this list, do not edit it.

## Execution protocol (applies to every task)

1. Sessions run caged in the `codex/ryuk` worktree through `codex-caged`.
   Agents cannot perform git writes of any kind — not staging, not commits.
   The agent edits and runs gates; `codex-collect` gates and commits the
   session; the human splits collected commits into `codex/<NN>-<slug>`
   branches at merge time when desired.
2. Read AGENTS.md before starting. On any conflict, AGENTS.md wins.
3. When a task says STOP, stop and report. The human is responsive and will answer.
4. Verification gates per AGENTS.md: the agent runs the JS gates in-session
   (`npm run check`, `npm test`); Rust gates are run host-side by
   `codex-collect` together with the JS gates before anything is committed.
5. Do not push and do not open pull requests. Finish the work, verify, stop.
   The human (or Hermes, on explicit instruction) handles pushes and PRs.
6. Never touch `~/Notes`, `~/.config/ryuknotes`, or anything outside this
   repository. Never commit `.rn` files or vault data.
7. If a task requires a feature cut or removal, propose the line-item list
   first and wait for approval (AGENTS.md rule 1).
8. Hermes may drive sessions unattended — headlessly via `codex exec`
   overnight, or interactively on the human's behalf. Every rule above
   applies identically, including rule 7: approvals are recorded as
   rulings inside the task sections below, and an agent working a task
   carrying a recorded ruling executes it without waiting.

Tasks are ranked by value, adjusted so the first task doubles as a pipeline
smoke test. Work in this order unless the human says otherwise.

Update 2026-11-04: the codex audit (current-state report, see session
history) added T8 through T11. T8 outranks T1–T7 — it is a data-integrity
bug. The pipeline smoke test already happened with the completed backup
lifecycle fix (audit finding #2, committed and fully gated on codex/ryuk).
The human re-ranks on review.

---

## T8 — DB-save staleness guard (data integrity)

Collected on codex/ryuk; split later as `codex/08-db-staleness`.

Concurrent GUI/CLI edits overwrite each other silently on DB vaults:
`saveNote` (src/lib/api.ts:195) accepts `expectedRaw` but drops it on the DB
route, and `db_save_note` (src-tauri/src/commands.rs:4228) upserts
unconditionally. The filesystem path rejects stale content; the DB path
does not.

Scope:
- Wire `expectedRaw` through `dbSaveNote` into the DB command.
- Reject the save with a clear error when stored content no longer
  matches, mirroring the filesystem path's stale-content rejection.
- Adapter boundary only: api.ts, api-db.ts, and the Rust command/DB layer.
  Components untouched (AGENTS.md rule 5).

Acceptance:
- `npm run check` and `npm test` green, with a test covering the
  stale-reject path.
- The final report states that Rust gates run host-side via collect.

Why it outranks everything: silent data loss beats any cosmetic or feature
work.

## T1 — Remove dead updater UI

Branch: `codex/01-updater-ui-sweep`

The auto-updater is fully removed at the platform level, but the Settings
"Check for Updates" button survives as dead UI that fails into a try/catch.

Scope:
- Remove the button and its handler from the settings panel.
- Grep the tree for updater leftovers (`createUpdaterArtifacts`,
  `tauri-plugin-updater`, "Check for Updates" strings) and remove what is
  found. If any removal looks risky, report it instead of forcing it.

Acceptance:
- `npm run check`, `npm test`, and the build all green.
- No updater references remain in frontend source.

Why high: small, bounded, and it keeps the pipeline exercised.

### Approved cut list (ruling recorded 2026-11-04 — binding, no further approval needed)

Removals approved: the updater exports in `src/lib/stores/app.ts`
(`updateAvailable`, `updateObj`, `checkForUpdate`, `isManagedInstall`) and the
dynamic plugin-updater import inside `checkForUpdate`; the startup update block
and its symbols in `src/routes/+layout.svelte` (around lines 105–115); the
desktop update badge in `TitleBar.svelte` (around lines 77–80 plus its import);
in `SettingsPanel.svelte` the update state (lines ~25, 29), the check and
`downloadAndInstall` handlers (lines ~49–93), the isManagedInstall/updateAvailable
template block (lines ~2443–2474), and their now-unused styles; the
`updateAvailable` import in `AppLayout.svelte` (line 51) and any prop it feeds
that becomes dead; stale updater comments. Preserved: the version display and
the package-manager guidance. `installType` itself stays unless provably
updater-only — ambiguous means report, not cut.

Deferred to Hermes host-side after collect: the `@tauri-apps/plugin-updater`
line in package.json (line 48) and its three pnpm-lock entries — pnpm cannot
run inside the cage, and agents never edit lockfiles. Further remnants found
during implementation are reported, not forced (AGENTS.md rule 3).

## T2 — DB-mode attachment wiring

Branch: `codex/02-db-attachments`

Pasting or dragging images into a DB-mode note still expects a filesystem
home. The `.rn` schema already stores attachments as SHA-256-deduplicated
BLOBs with a 25 MiB cap; the editor path is not wired to it.

Scope:
- Branch at the `src/lib/api.ts` boundary (adapter `src/lib/api-db.ts`),
  per AGENTS.md rule 5. If a `db_*` command for attachment insert/fetch is
  missing, add it mirroring the fs counterpart one-to-one.
- Wire image paste and drop in DB mode through the adapter. Components stay
  untouched wherever possible; Editor.svelte gets surgical edits only if
  unavoidable, with the reason documented in the commit.
- T9's ownership hazard must be resolved before or with this task's
  paste-path landing — see T9.

Acceptance:
- Paste an image into a DB-vault note, save, close, reopen; it renders.
- Unit tests for the new command through the mock app.
- Idempotency: pasting the same image twice stores one BLOB (SHA-256 dedupe).

## T3 — DB-mode importNote and moveNote

Branch: `codex/03-db-import-move`

The two remaining adapter gaps from the storage switch.

Scope: implement `importNote` and `moveNote` in `src/lib/api-db.ts` with
fs-equivalent semantics; register any missing `db_*` commands. Also from the
audit: `dbMode` never resets when opening a directory vault, and ordinary DB
saves never create version snapshots — cover both here or report them as
separate follow-up tickets.

Acceptance:
- Import a `.md` file into a DB vault from the UI.
- Move a note without data loss; trash and history survive the move.
- Tests and the build green.

## T4 — Split view (two editor panes)

Branch: `codex/04-split-view`

Two Editor panes in one window with independent scroll.

Phase A (mandatory first): state-store survey. Audit `src/lib/stores/app.ts`,
the sidebar, focus, and autosave state, and every single-active-note
assumption across components including `src/routes/+layout.svelte`. Deliver
the survey as a short markdown doc committed to the branch, then STOP for
human review. Never start with the code.

Phase B (only after Phase A is approved): an `{#each panes}` layout with a
resize handle between panes; each pane owns its note, scroll, and focus.

Constraint: Editor.svelte is instantiated twice, not forked. No edits inside
it unless a hard blocker is documented in the Phase A survey.

## T5 — Dark Shinigami theme

Branch: `codex/05-shinigami-theme`

The deferred cosmetic pass: dark theme across the app.

Constraints: pure styling — no storage, logic, or schema changes; CSS and
theme tokens only; component structure untouched. The human hides and
dislikes the right sidebar; if the restyle makes it redundant, propose
absorbing it as a cut list and STOP — that is a removal decision.

## T6 — Fix AppImage bundling

Branch: `codex/06-appimage`

linuxdeploy-based AppImage bundling currently fails on the dev box. Goal: a
working AppImage artifact for generic distribution. AUR is ON HOLD
(registration closed) — do not pursue AUR.

This is the one task that may need network access (tooling downloads). Ask
the human to launch the session with network enabled; do not work around it.

## T7 — Icon regeneration for original art (BLOCKED)

Branch: `codex/07-icon-art`

Not startable: the human must supply original death-god artwork first. The
current Ryuk fan-art is personal-use only; likeness is the copyright surface,
so it cannot ship as public release branding. Do not source or generate the
art yourself.

When art lands: swap `icons-src/`, regenerate the set
(`PATH="$PWD/node_modules/.bin:$PATH" tauri icon icons-src/<new>.png`),
verify `git status src-tauri/icons` shows real changes, and sweep all
six-plus logo surfaces per the AGENTS.md rebrand procedure.

## T9 — Attachment BLOB single-ownership hazard (extends T2)

Branch: `codex/09-attachment-dedup`

The `attachments` schema is single-owner (`note_id` with ON DELETE CASCADE)
while BLOBs are SHA-256 deduplicated: permanently deleting the first note
can cascade-delete bytes that another note still references. The code
comment says duplicate hashes get "(re)linked to the note"; the audit read it
as "returns the first row" — first READ `add_attachment`
(src-tauri/src/vault/db.rs:478) and state which behavior is real, then make
ownership safe (refcount, or a note-to-attachment link table) so no note's
delete ever removes bytes another note references. Migration must handle
existing vaults safely; propose the schema change and STOP for approval
before touching any vault schema.

## T10 — Hermes launch context hardening

Branch: `codex/10-hermes-context`

GUI session buttons spawn `ryuk` without `--vault`
(src/lib/components/Editor.svelte:173 area), so the CLI may open a different
vault than the app displays; resuming a linked session drops extra grounding
notes and discards a newly supplied prompt via the early resume branch; and
session discovery parses human-readable Hermes output heuristically.

Scope: pass the active vault path into every spawned `ryuk` invocation
first, then make resume honor supplied grounding notes and prompts. If the
fix requires surgery inside Editor.svelte, STOP and report with the
proposed diff — that is the crown jewel and a removal-adjacent decision.

## T11 — Release hygiene (mechanical)

Branch: `codex/11-release-hygiene`

All four version files still say 1.4.1 while a bare `v1.4.2` tag already
exists — the tag was made before bumping (the audit's release discrepancy).
The README upstream attribution says RyukNotes where upstream is actually
HelixNotes (gitlab.com/ArkHost/HelixNotes), leaving a broken attribution
link.

Scope: fix the README attribution lines; bump `package.json`,
`src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml`, and `Cargo.lock`
together per the AGENTS.md tagging procedure. Propose the target version
number and STOP — the version choice is the human's.

---

## Do NOT build (explicitly rejected)

- Vector RAG, sqlite-vec, or embeddings search. The human IS the retrieval
  engine; this was deliberately declined.
- Any previously removed feature: WebDAV/Nextcloud sync, mobile scaffolding
  and APK updater, GraphView, the trimmed font herd, the auto-updater.
- CalDAV calendar integration or in-app file-push. Scheduling and file-push
  stay agent-side.
- Any branch, tag, or identifier named "canari", and any branding toward
  Helix beyond AGPL-required attribution and legacy read-compat.

## Completed

- Backup lifecycle fix (audit finding #2): emits `backup-done`, persists
  `last_backup_time`, prunes with retention, plus a regression test.
  Committed 0dc44b7 on codex/ryuk, fully gated, awaiting review and merge.
- T8, DB-save staleness guard (audit finding #1): expectedRaw reaches the
  DB layer under the SQLite write lock; stale and deleted-note saves reject
  with a conflict error; adapters only, components untouched. Committed
  5228606, fully gated.
- T1, dead updater UI (audit confirmation plus the standing removal order):
  agent pass 45c1c0f removed 313 lines of controls, handlers, badges, and
  stores; Hermes phase 2 (e736ccb) pruned the @tauri-apps/plugin-updater
  dependency and lockfile entries host-side. flake.nix's updater-disabling
  injection is deliberately KEPT — build-time enforcement of the
  never-re-add rule, not a remnant.
