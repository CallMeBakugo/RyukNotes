# Explicit three-way review — Stage 3

The approved order remains Keep both, explicit review, automatic merge last.
This stage has no automatic merge, live Editor fork, vault schema change or
revision ancestry. Main-window same-note activation and detached owners remain
unchanged.

## Exact engine

`three-way-merge.ts` is pure. It compares accepted Markdown strings as exact UTF-8
bytes with UTF-16 offsets for source-controller integration. Lines retain LF,
CRLF, lone CR, whitespace, Unicode composition, BOMs and final-newline state.
It never calls the history display diff, trims frontmatter/body, renders Markdown,
or normalizes text. Exact base→local and base→remote patches have line, character
and byte ranges. Candidate construction returns source/result segment mappings.

Hunks are clean-local, clean-remote or overlapping. Every overlap requires an
explicit local, saved or base choice. Same-position insertions, even identical
ones, never concatenate themselves. Repeated lines, possible moves and changed
structural delimiters/definitions are conservative whole-side decisions rather
than guessed alignment. Protected boundaries cover lists/tasks, tables, reference
definitions, fenced code (including legacy/current secrets), math, raw HTML/PDF,
wiki targets and attachment references. Boundaries introduced in either target
project back to base coordinates before grouping other edits. Protected changes
require explicit choices; adjacent line numbers do not prove independence.

Work is bounded: 2 million UTF-16 units / 4 million UTF-8 bytes per document,
12,000 lines, and 1 million LCS cells after exact shared-edge trimming. A resource
limit or unsupported encoding routes to read-only exact sources and Keep both,
never a guessed/coarse authoritative patch. Source previews may normalize for
display; they do not supply merge coordinates.

## Review and result artifacts

Each native ticket binds its window, owner, vault identity, journal sequence,
local revision, exact base/local snapshots and coherent saved-state receipt.
Common base is shown only when the journal's body, metadata and origin match its
captured accepted token. No version timestamp, nearest snapshot or matching-looking
body establishes a base. This verifies the captured live accepted state, not
schema-level ancestry: Stage 1's ABA and file-identity limitations remain.
Unknown legacy bases, filesystem buffers without this verified DB contract, and
missing/trashed saved notes route to Keep both without inventing ancestry.

The conflict wrapper reuses two HistoryReview presentations side by side, labeled
Common base→Your draft and Common base→Saved version, plus inert SnapshotDocument
and exact-source views. History restore language/actions stay in history mode.
Snapshots remain immutable until explicit renewal. Raster previews read captured
review BLOBs through an owner/ticket-scoped command, not the current global vault.
Object URLs are generation-checked and revoked when their dialog changes/closes;
non-images remain inert attachment references. The result uses its own
SourceController/SourceEditor with no note-store binding. CodeMirror's LF display
has an exact artifact adapter: unchanged mixed line terminators remain exact;
new lines use the stated insertion separator. Manual edits invalidate inferred
source provenance, retain their raw artifact and cannot silently revert on a
comparison/Unicode limit.

Opening review does not build or apply a result. Build chosen result is an explicit
user action. Every overlap must have a choice, and divergent metadata needs its
own choice. Scalars follow unchanged-from-base rules; tag sets offer local,
saved, base or an explicitly chosen union (which can retain tags removed on the
other side). Creation/identity and trash/deletion are not merge fields. Rebuilding
an edited result requires confirmation; Use this edited result keeps the manual
artifact after the new decisions have been inspected.

Edited/built results use independent private recovery streams, with original B/L,
reviewed R, chosen metadata and immutable attachment bytes. They never overwrite
the original owner's dirty L. Cancel preserves a result for the recovery inbox;
its label is Review result and its Keep both uses the existing summary-copy policy.
Failure to journal retains memory and refuses disposal. A late close/open response,
renewal, typing or another saved writer cannot replace newer result edits.

## Apply and lifecycle

Apply freezes only the owning editor through preservation, CAS, journal retirement
and adoption. It checks the captured local revision/session and exact result
version before calling native code. Native code holds that owner's registry
state stable through an immediate SQLite transaction, compares the full reviewed
saved token, resolves/links result references, forces a prior-R body checkpoint,
writes chosen body/title/tags/pin, and captures its receipt before commit. Creation
fields remain immutable and modification time is generated at commit. There is
no text rewrite through the renderer and no merge of BLOB bytes or secret payloads.
An operation payload/receipt in existing meta storage makes reply-loss retries
idempotent without replaying over a later writer.

A writer before/during CAS rejects with a typed conflict. Local typing, source
surface changes and stale owner generations invalidate adoption. Manual results
remain recoverable; renewed review carries the latest result forward without
using it as a new base. Failed/post-commit adoption or retirement republishes an
unresolved result generation so an old tombstone cannot hide it. Exact successful
commit plus matching local retirement is the only route to acceptReviewed:
raw/savedRevision are not advanced by opening, diffing, choosing, events or failure.
Only then does Editor load the committed body and deliberately reset live undo
history. Until that boundary the live document and its undo history stay intact.

Dialog/search/metadata/hunk IDs are instance-scoped. F3, Escape and application
shortcuts are contained at the owning dialog and execute only in its focused
pane/window. Focus restoration requires the current focus intent; no asynchronous
result focuses another pane/window. Other owners remain independent, and the
Stage 2 close/restore/switch barriers still refuse unresolved drafts/results.

## Verification and limits

Dependency-free Node tests cover exact diff replay and mappings, 500 seeded
adversarial reconstructions and 300 independent mixed-EOL edits, all protected
syntax cases, moves/repeated lines/insertions, Unicode/whitespace, exact manual
source edits, open/cancel/renew generations, typing during review, successful and
stale apply, metadata choices, journal failures, post-commit result preservation,
keyboard/window isolation and queue acknowledgment fencing.

Native tests were added for verified-base refusal, atomic exact/idempotent apply,
body/metadata/lifecycle races, a separate writer holding the transaction while
apply waits, result recovery streams, and registered mock-app IPC including local
revision and window ownership checks. Rust tests and real two-window GUI, focus,
undo, assets and restart exercises must run host-side.

In this cage the shared node_modules directory is empty. npm run check was
attempted but cannot start svelte-kit; npm test was attempted but dependency-bound
files cannot load packages such as diff, typescript and Svelte. All 50 focused tests
pass directly under Node. The full npm suite reports 21 passing files and 25
dependency-loading failures, not a green gate. No dependencies were installed and no network was used.
