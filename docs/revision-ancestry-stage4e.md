# Stage 4E — portable ancestry (pre-activation audit)

> Follow-up: the human reported a complete host collect pass for 4B–4E and
> authorized production activation using that proof. The current activation and
> outstanding human gate are documented in [Stage 4F](revision-ancestry-stage4f.md).
> The blocked status below records this earlier session, not current behavior.

Portable ancestry is implemented for the explicit native v3/tooling boundary.
**Production activation was not performed.** The request makes activation
conditional on every 4B–4E gate passing together in this session. AGENTS.md
prohibits attempting Rust gates in the cage; the native migration, writer, pin,
retention, portable restore and executable old-client fixtures could not run.
Passing npm and SQLite wire tests is insufficient evidence to flip ordinary
creation/opening. `SCHEMA_VERSION` remains 2, ordinary opens still refuse v3,
and the explicit `revision-ancestry` feature/fixture entry point remains required.
This is an unmet activation precondition, not a waiver or a completed activation.

## Format 2 contract

V2 vault exports keep existing format 1. Writable v3 exports use format **2** in
`.ryuknotes/export.json` with a required `ancestry` manifest. Its exact capability
list is:

- `immutable-revision-graph-v1`
- `sha256-byte-sidecars-v1`
- `anonymous-recovery-holds-v1`

Unknown versions, capabilities, envelope fields, tables or columns refuse import
before destination, ledger, staging or maintenance writes. The legacy Markdown
importer refuses format 2 with an explicit restore/copy/reconciliation message;
it cannot fall through to plain Markdown or drop ancestry. A v2 export cannot
replace a format-2 archive in place.

The manifest records vault UUID, current storage epoch and all retained header
epochs. Its fixed ordered table/column contract is in
`src-tauri/src/vault/portable_ancestry.rs::TABLES`: identity, incarnations,
counters, heads, headers, parents, retained full states, pruning markers,
revision attachment links, note projections, tags/links, BLOBs, sessions,
curated versions (including unknown/verified ancestry markers), metadata and
anonymous pin descriptions. Derived FTS rows are rebuilt, not exported as truth.

Every row is an immutable SHA-256-addressed JSON sidecar. Cells explicitly encode
null, exact UTF-8 text, canonical decimal i64 strings, or a byte-sidecar descriptor.
Full-state body BLOBs and attachment bytes use exact binary sidecars; there is no
Markdown parsing, trimming, line-ending normalization or portable-link rewriting
on authoritative state. Each descriptor names
`.ryuknotes/ancestry/sha256/HASH.bin` and carries hash and byte size. Equal bytes
share one sidecar. Declared/referenced sidecar sets must agree. Duplicate row IDs,
noncanonical cells, changed lengths/hashes, symlinks and unsafe paths reject.

Readable Markdown, portable links and standalone attachment files remain alongside
that graph. They are a presentation of format 2; graph restore uses authoritative
sidecars. Editing a presentation file does not rewrite a committed revision.
Import an edited Markdown file explicitly as a note when that is the intended
operation. Format 1 retains its previous Markdown import behavior.

## Snapshot and publication

The existing export transaction now captures every table and every byte through
the same SQLite connection/read snapshot, including graph and metadata. Capture
refuses an autocommit/unowned snapshot or retired writer identity. GC and other
writers may commit to WAL while this snapshot keeps its original generation.

Export and portable import coordinate with an OS publication lease at the index
path. Import rechecks its preflight index after acquiring the shared lease. Export
holds the exclusive lease, publishes hash-addressed sidecars and syncs files and
ancestor directories on Unix, then atomically publishes the index **before**
removing obsolete managed paths. A crash before publication cannot leave the old
authoritative graph pointing at sidecars already deleted by cleanup. Markdown
presentation updates are not a multi-file filesystem transaction. An interruption
can leave extra unreferenced sidecars or incomplete presentation updates; it cannot
turn them into an acknowledged graph. Arbitrary unrelated files are not removed.

Re-export checks existing immutable row identities and complete parent sets
against the current coherent snapshot before publishing. Proper new revisions,
allowed payload/BLOB/history pruning and an incarnation's first lifecycle end
are allowed. Changed immutable collisions, another vault UUID, or previously
exported headers absent from an older restored lineage refuse replacement: retain
that archive and choose a new folder. No foreign graph is silently spliced into it.

## Explicit import modes

The adapter exposes separate operations; none retargets the displayed vault.
Callers retain an operation UUID for retries and use existing all-pane pause/job
barriers. Native commands independently refuse unresolved owners/recovered drafts.
The frontend adds no pane, Editor, autosave, focus or layout changes.

| Mode | Boundary and behavior |
| --- | --- |
| Full restore | `restoreAncestryExport(..., 'full_restore', operationId)` / `restore_export`. Requires an absent or genuinely empty compatible destination, or the exact receipt of this acknowledged operation. Preserves source vault UUID and every original graph ID. Rejects retained graph/data/metadata in an otherwise populated destination. |
| Independent copy | Same explicit destination API with `independent_copy`. A fresh operation UUID allocates a different deterministic vault UUID and translated authority graph; retrying that operation returns the same copy. |
| Same-vault reconciliation | `reconcileAncestryExport` / `reconcile_export`. Requires the matching logical vault UUID. Adds validated immutable ancestors/branches, never promotes incoming heads or overwrites note projections. Incoming heads are pinned for review; incoming mutable metadata is retained as branch provenance. |

Import validates all sidecar bytes and materializes/validates the complete source
graph before mutating its destination. SQLite affinity conversions cannot silently
change wire cells. Full FK, graph/payload/projection, BLOB hash/size and notebook
assignment checks are required. No curated body history is used to invent a
missing/pruned revision payload.

### Full restore and missing ledger

Portable restore reuses the native 4B exclusive maintenance → ledger → immediate
SQL restore protocol. It refuses live managed destination connections. A staged
source snapshot is validated, a fresh storage epoch is allocated, high water is
reserved/fsynced above source counters and the host ledger, and every head,
including deleted heads, gets a restore-baseline child before atomic publication.
UUID/incarnations/old headers remain intact. Reservations may leave gaps.

The outcome always requires recovery review before old draft/review rebinding.
A fresh host/missing ledger is reported explicitly; integer order against a lost
future cannot be proven. New epoch/commit identities distinguish those histories.
Old accepted tokens cannot authorize writes. Opening another existing writable
path with the same UUID in one host ledger remains refused; use independent copy.

The operation's request hash and exact outcome are committed inside the restored
vault before publication. Same-operation/same-input retries return that outcome
without another epoch or baseline. Different input under the same ID refuses.
A retry can finish an interrupted matching ledger finalization only after durable
file/identity validation. A lost ledger or retired epoch after acknowledgment
requires explicit recovery; an old receipt does not silently enroll a new host.

Restore advances generated-summary authority to its new baseline only if the
restored predecessor was exactly the last generated revision. Manual A→B→A
history is not promoted by equal bytes. Repeated or manually edited summary
imports therefore retain the existing provenance policy.

### Independent identity translation

The copy operation translates vault UUID, every storage epoch, incarnation,
revision/commit UUID, writer-instance/operation identity, curated version identity
and relational graph reference. Opaque accepted tokens assembled from these
headers consequently change. A protected origin record carries source vault,
source archive hash and the complete translation map.

Public note IDs and attachment reference names remain stable **inside the new
vault namespace**. This preserves exact Markdown bytes, wiki targets and attachment
hash dedup. Historical `incarnationOrigin` values remain explicitly source
provenance, resolved through the translation map; they are not foreign-key edges
claiming a new merge parent. Retained full-state bytes/digests remain exact.
Generated v3 references and curated verified-version references are translated.
Source retry claims are not new-vault authoring authority. Plain source provenance
and user text are not rewritten by searching for UUID-shaped strings.

### Reconciliation and collisions

Reconciliation validates source completeness and every existing immutable row
before insertion, including header fields, full states, BLOB identity/bytes,
curated versions, attachment links and the **complete** parent set. Mismatched IDs
or SHA aliases requiring reference rewrites reject. Counter floors are durably
reserved before acquiring SQL's immediate lock, and counters only increase.
Headers/edges are never renumbered to hide independent-host counter collisions.

The current projection and current head remain unchanged. New note projections
require restore/copy rather than implicit graph reconciliation. Imported branch
heads are conservatively pinned, and incoming mutable metadata/heads are recorded
as provenance. Source branches can be inspected without becoming live owners.
Conflicting incarnation tombstones, local pruning versus an incoming retained
payload (or the reverse), and incompatible immutable IDs refuse rather than
resurrect data or silently release local holds. This stage adds no merge engine.

## Recovery and asset ownership

The archive carries pin descriptions with stable anonymous identities derived
from vault/revision/kind, never live owner registrations, heartbeat leases or
private dirty journal files. Committed writer/operation identities remain header
audit data; they do not create native owners. Private recovery asset and staging
holds become conservative portable asset holds. Ephemeral fork/apply receipt
caches tied to private owners do not travel as live recovery state; committed
revision ancestry, generated provenance and retained assets do.

Unlinked retained BLOBs receive conservative portable staging holds, including
bytes that would otherwise have depended only on a private receipt cache.
Reconciliation also holds imported assets. These staging holds have no automatic
expiry in this stage. Imported revision pins use the `portable` protocol recognized
by 4D's orphan inbox, require explicit durable cleanup acknowledgment and never
expire because a local journal is absent. No source private recovery journal is
read, copied, resolved or rebound.

## Verification and activation decision

- `npm run check`: passed, zero errors and the same three existing warnings in
  untouched ConflictReview/Editor code.
- `npm test`: passed, all 51 test files.
- New executable Node/SQLite wire fixtures use the actual native table/column
  contract, real WAL files and independent connections. They verify full graph,
  exact CRLF/Unicode body/metadata/BLOB transport, decimal counters beyond JS safe
  integers, stable repeated encoding, concurrent snapshot behavior and preflight
  refusal of unknown capabilities/corrupt bytes. Adapter tests cover explicit
  modes/operation IDs, current-vault routing, no fallback and no display rebinding.
- Native fixtures added for combined pins/retention/pruning/export/restore,
  independent copy translations, exact bytes and graph identities, repeated
  export/import receipts, immutable collisions, unknown capability/schema fields,
  coherent WAL snapshots, fresh-host missing ledger/old-token refusal, legacy
  import refusal, v2-client refusal and real mock-app scoped restore IPC. They are
  source fixtures **not executed in this session**.
- The importer currently holds the decoded package and a validation database;
  large-vault memory/performance has not been proven by these disposable fixtures.
- Not run: Rust check/check --tests/test, feature-enabled native suite, fmt/clippy,
  actual old v2 executable, native migration/race/crash/IPC/CLI/portable fixtures,
  builds, packaging, boot, live multi-window or real restore/user-vault tests.
  AGENTS.md hides Rust tools and forbids attempting cargo in the cage.
- No application components, T4 providers/barriers, schema DDL, live user data,
  config, network or git writes were changed/accessed.

**Activation remains blocked, not achieved.** Default and ancestry-feature native
migration/writer/pin/GC/export suites, actual old-client compatibility and the
remaining required gates must pass together before changing ordinary creation or
opens to writable v3. No such pass was inferred from the npm/SQLite tests or from
previous host collection. Stop here; Stage 4F has not begun.
