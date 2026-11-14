# AGENTS.md — RyukNotes

Standing rules for AI coding agents (Codex, Claude Code, or similar) working in this
repository. Read all of this before touching anything.

## Project identity

RyukNotes is a fork of HelixNotes (upstream: ArkHost, AGPL-3.0), rebranded and heavily
modified. Display name is "Ryuk's Notebook"; machinery stays apostrophe-free.

- Identifier: `com.callmebakugo.ryuknotes`, binary `ryuknotes`, CLI `ryuk`,
  vault extension `.rn`, repo name `RyukNotes`, config dir `~/.config/ryuknotes`.
- Stack: Rust + Tauri 2 backend, SvelteKit frontend, single-file SQLite vault (`.rn`)
  with FTS5 search. Backend surface: `src-tauri/src/{lib,commands,state,types}.rs`,
  `vault/`, `search/`, `history.rs`, `backup.rs`. Frontend: `src/lib/{api.ts,api-db.ts}`,
  components in `src/lib/components/`.
- Display name "Ryuk's Notebook" on every user-visible surface: productName, window
  title, titlebar, vault picker, info panel, desktop entry.

### Naming rules (permanent user decisions — do not relitigate)

- NEVER reintroduce the name "canari" in any branch, tag, identifier, or artifact.
  The default branch is `main` (renamed 2026-10-15 from the old fork branch name).
  Default-vs-work branch drift once left the GitHub front page showing stale upstream
  code for days.
- Do not brand anything back toward Helix. Exceptions that MUST stay: the LICENSE and
  copyright headers (AGPL), the hidden "Open-source credits" line in the InfoPanel,
  and legacy read-compat code (`.helixnotes` dir fallbacks, `helix-secret` fence
  parsing, `helix-keybindings` localStorage fallback).
- The auto-updater is REMOVED permanently (plugin, Cargo dep, capability, endpoint
  config). Never re-add auto-update. The Settings "Check for Updates" button is dead
  UI by design.

## Remotes and pushing

- `github` (git@github.com:CallMeBakugo/RyukNotes.git) — master remote, branch `main`.
- `codeberg` — mirror only. Push only when the user explicitly calls a milestone pause.
- `fork` (gitlab.com/CallMeBakugo/HelixNotes) — untouched monument. Never push.
- `origin` (gitlab.com/ArkHost/HelixNotes) — upstream, read-only. Never push.
- Never push from a shallow clone; `git fetch --unshallow origin` first.
- Commits are SSH-signed via local config. Author is already configured; do not change
  identity, remotes, or credentials.
- Agents do not push to any remote and do not open pull requests. Finish the
  branch, run the verification gates, and stop; the user (or Hermes, on explicit
  instruction) handles pushes and PR creation.
- Run sandboxed: `workspace-write` with network access disabled (see ~/.codex/config.toml).
  The repo builds fully offline; if a task needs network, say so and wait for the user to
  grant it — never work around the sandbox.

## Hard rules (user's standing preferences)

1. Propose feature cuts as a line-item list and WAIT for explicit approval before
   cutting. "Discuss twice, cut once." Cuts are permanent decisions the user vetoes
   item by item.
2. `src/lib/components/Editor.svelte` is the crown jewel (~13.5k lines). Do not touch
   it during cut phases.
3. Remove a Tauri plugin COMPLETELY or not at all: registration in `lib.rs`, Cargo
   dependency, capabilities permission, AND config section. Partial removal panics at
   boot or fails the build script. Frontend dynamic `import()` of a removed plugin
   fails gracefully inside existing try/catch — that is acceptable dead UI.
4. Do not excise `#[cfg(mobile)]` or other cfg-gated code for LOC. It is
   compile-time excluded from desktop builds; removing it is edits with zero runtime
   reward.
5. DB-vault work branches at the `src/lib/api.ts` boundary (adapter
   `src/lib/api-db.ts` synthesizes existing shapes: `NoteEntry`, `NoteContent`,
   `SearchPage`, `TrashContents`; note ids masquerade as note paths). Components stay
   untouched — 32k lines of Svelte never learn the storage changed.
6. FS-only UI in DB mode is HIDDEN behind `{#if !isDbVault()}` guards, never faked.
7. One commit per logical unit (one removed feature = one commit), with a message
   stating what changed, why, and the verification results.
8. Never commit `.rn` files, vault data, secrets, backups, or `.env`. The live vault
   `~/Notes/ryuknotes.rn` and config `~/.config/ryuknotes` are USER DATA outside this
   repo. Do not read, sync, or modify them unless explicitly instructed.
9. Map a feature's full wiring surface BEFORE cutting it: grep its symbols across the
   backend files, then `api.ts`/stores, then every component including
   `src/routes/+layout.svelte` (startup wiring hides there).
10. The in-app Ryuk fan-art icon ships for personal use only; swap for original
    death-god art before any public-facing release branding. Removing a character's
    NAME does not de-risk the LIKENESS.

## Build and verify (this machine's quirks)

- `pnpm` is NOT on PATH in non-interactive shells. Use `npx pnpm <script>`, or
  `PATH="$PWD/node_modules/.bin:$PATH" tauri <subcommand>` from the repo root. Do not
  use the `~/.local/bin/pnpm` shim for tauri work — it runs a vite build and silently
  swallows the tauri invocation.
- Rust gate: `cargo check` → `cargo check --tests` → `cargo test` (run in `src-tauri/`).
- Frontend gate: `npx pnpm check` (svelte-check), `npx pnpm test` (node:test),
  `npx pnpm build`.
- Release builds MUST go through the tauri CLI:
  `node node_modules/@tauri-apps/cli/tauri.js build --no-bundle` from the repo root,
  then copy `target/release/{ryuknotes,ryuk}` to `~/.local/bin/`. Plain
  `cargo build --release` can produce a dev-mode artifact that shows "Could not
  connect to localhost" on every serverless boot.
- Boot verification is a separate gate from compiling: run `npx pnpm tauri dev` in the
  background, wait ~40s, then `pgrep -f target/debug/ryuknotes`. A clean build proves
  nothing about boot. Kill the dev process afterward by exact binary name.
- CI runs on every push to `main` and every PR into `main`
  (`.github/workflows/ci.yml`): svelte-check, JS tests, cargo test, clippy with
  `-D warnings`, fmt, vite build. Do not push red.
- Before tagging a release: bump `package.json` + `src-tauri/tauri.conf.json` +
  `src-tauri/Cargo.toml` (and `Cargo.lock`). Tags must be annotated with a message
  (`git tag -a vX.Y.Z -m "..."`) — bare `git tag vX` fails because the user's git
  signs tags. Artifact filenames carry the APP version, not the tag; bump first.

## Known pitfalls (each of these has caused real damage)

- Kill processes by exact binary name (`pkill -x ryuknotes`), never `pkill -f` with a
  substring that appears in your own command line — it kills the shell running the
  cleanup and silently swallows the rest of a chained command.
- `cp` over a running binary fails with "Text file busy". Kill the app by exact name
  first; the ~3s autosave means nothing typed is at risk during the swap.
- Chained `&&` surveys die at the first missing path and silently truncate output —
  use `;` separators for repo surveys.
- Patch Svelte/TS import lines as whole lines; splicing a name out of a long comma
  list with minimal context can merge two identifiers onto one line.
- Guarding a template section needs div-depth counting from the start line until
  depth returns to zero — the first `</div>` is usually an inner close. If a splice
  goes bad anyway, `git checkout -- <file>` and redo; do not hand-balance braces.
- After scripted template edits, grep the region for duplicated insertions — a
  re-run can stack two identical guard lines that parse until the close tag.
- In ProseMirror code, "graph" and "paragraph"-adjacent matches are document-model
  vocabulary, not the removed GraphView feature. Confirm each hit references the
  actual component before cutting.
- Tests that must touch real vault data use env-var-gated `#[ignore]` dry-run tests
  (e.g. `RYUK_REAL_VAULT=<dir> cargo test real_vault_dry_run -- --ignored`); they
  import into an in-memory DB and never write to the source vault.
- Contentless FTS5 tables refuse DELETE unless declared with
  `contentless_delete=1`.
- Tauri commands are tested through the mock app
  (`tauri::test::mock_builder().manage(AppState::new(...))`), not just the module —
  command-level tests catch registration bugs that module tests cannot.
- Importer tests: damaged-frontmatter files are imported with fallback metadata by
  design (rescued, not skipped) — expect the rescued note in report counts.
- Re-import dedupe relies on content-hash upserts; test idempotency explicitly
  (import twice, count once), never assume.

## Public-facing docs

The GitHub front stays SIMPLE: short plain-grammar README (what it is, CLI examples,
build-from-source, AGPL credit to upstream HelixNotes), no marketing, no badge
walls, and an honest status line until the project stabilizes.

## Agent branch protocol (codex/ryuk worktree)

- Codex sessions run from the worktree at `~/Public/HelixNotes-codex` on branch
  `codex/ryuk`, launched through `codex-caged`. Work only on this branch; never
  touch `main` from a session.
- In the worktree, commits are authored as "Codex (RyukNotes agent)" with signing
  disabled (worktree-scoped config). That is deliberate — do not change identity
  or signing settings. The main checkout stays SSH-signed; the two never mix.
- Git writes are IMPOSSIBLE in-session: the inner sandbox re-binds .git
  read-only even for approved escalations, so staging and commits fail no
  matter what. Do not attempt git writes from a session.
- End every work session by writing the commit message to
  `.session-commit-msg` in the worktree root: what changed, why, and the
  verification results, including which gates were NOT run. The human then
  runs `codex-collect`, which runs every gate and commits using exactly
  that message. Until the message file exists, the session is not finished.
- The cage shares the main checkout's node_modules read-only, so the JS gates
  run in-session through the repo's own npm scripts: `npm run check` and
  `npm test`. Never type file globs into a command — the `*` characters get
  eaten by message rendering and the command arrives broken. The npm scripts
  carry the globs safely inside package.json.
- Rust gates CANNOT run in-session: the cage hides `~/.cargo` and `~/.rustup`.
  Do not attempt cargo commands; state plainly in the final report which gates
  were not run. The human runs the Rust gates outside the cage before merge.
- The permission profile in `~/.codex/config.toml` governs sessions; do not pass
  `--sandbox`, and do not ask for network unless a task truly needs it.
