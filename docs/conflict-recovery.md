# Coordinated recovery — Stage 2

The UX order remains Keep both, review, automatic merge. This stage implements
Keep both and confirmed discard/reload. There is no text merge or merge UI;
Review changes will be offered when Stage 3 provides it. Main-window same-note
selection still activates its existing pane. Detached windows remain independent
editors with independent buffers, queues, focus and recovery owners.

## Owners and invalidation

Each editable note session gets a random owner ID bound natively to its invoking
window, captured vault identity and note. Register/status/checkpoint/fork/resolve
requests cannot claim another live window's owner. Registration retries are
idempotent for the same binding. No dirty body travels in invalidation events or
ordinary save-error logs.

A native `data_version` observation belongs to the open SQLite connection; its
previous value is not compared across connection epochs, even when the same
file is closed and reopened without changing its filesystem identity. Visible editors poll
through that service every two seconds. App commits emit invalidation hints;
focus, visibility resume and lifecycle events also request a re-read. A periodic
re-read occurs even without a changed counter or delivered event, covering
same-connection writes and missed/reordered hints. Reads wait for the owner's
in-flight save acknowledgments so its own commit echo cannot cause a conflict.
A hint cannot advance queue raw/savedRevision, resolve a journal or move focus.

Dirty owners keep their live document and enter a fenced queue when coherent
saved state differs, or when CAS rejects. Both autosave deadlines are canceled;
callbacks already waiting in the queue, and acknowledgments invalidated by a
fence, cannot mark the draft saved. Typing continues, updating recovery rather
than repeatedly retrying the original. Clean-buffer refresh is session-checked.
The inline owner banner says: “This note changed elsewhere. Your draft is still
here.” Reload saved version requires an explicit discard confirmation from the
active pane in the focused window. Invalidation never opens a modal.

## Journal and fork

Runtime storage is `app_local_data_dir()/recovery-v1/<owner UUID>/`, outside the
vault. It is not accessed by repository tests except under disposable directories.
On Unix, directories are 0700 and records 0600; non-Unix storage inherits the
app-private user directory's platform permissions. Each owner has an ordered
immutable checkpoint stream and ordered resolution tombstones. Attachment bytes use
private content-addressed files rather than being duplicated into every JSON
record; loading verifies their checksums. Memory updates on every keystroke,
with journal writes coalesced at 250 ms idle / 1 s maximum wait, and immediate
checkpoints at conflict, explicit flush and fork. Writes use a
private temporary file, file fsync, atomic rename and directory fsync (including
new-directory publication on Unix). Reject old sequence numbers and rebinding;
older completions cannot replace newer checkpoints. Records retain accepted B,
metadata/vault identity, draft L, local revision and conflict state.

Object URLs are read into checkpoint-owned bytes and rewritten to journal-local
references before native durable acknowledgment. DB references in both B and L
include their BLOB bytes in the journal. Capture/attachment/journal errors keep
memory, the dirty owner and its mounted editor; pause and destructive transitions
refuse. A successful note commit remains an exact accepted commit even if later
journal resolution fails: the journal cleanup is retried without inventing a
read-back acknowledgment or disposing the live buffer. Journaling is not a note
save and no checkpoint claims to protect typing that preceded a crash but never
reached durable storage.

A DB fork is one immediate transaction: fresh ID, chosen captured title/tags/pin,
attachments deduplicated and linked to the new note, final rewritten body, exact
receipt, and the operation's payload hash/receipt in existing `meta` storage.
The key is owner plus checkpoint sequence. Retrying a lost reply returns that
receipt; reusing the key for different content rejects. It never writes the
original. Attachments remain linked to both notes as applicable. A summary fork
removes `hermes/session`, adds `hermes/recovery`, labels its title as a recovery
copy, records source provenance, and receives no generated-summary provenance or
session link. A copied summary cannot masquerade as a restorable session summary.

Filesystem forks publish a deterministic fresh recovery note with an exclusive
hard link from a fully written/fsynced temporary file. Checkpoint-owned attachments
have stable content-derived references, published before the note. Retry does not
replace an existing fork. Existing relative references retain the source notebook
and vault interpretation. This is recovery-copy publication, not filesystem CAS
against arbitrary outside writers.

Typing while fork is in flight invalidates adoption of its result. The captured
copy remains committed, while the newer draft remains dirty and mounted. Only an
unchanged captured revision may resolve its record and navigate to its fork.
After actual renderer destruction (which is not permission for an ordinary
dirty close), the last durable stream becomes claimable. At restart, an inbox claims inactive journal owners and offers Keep both or an
explicitly confirmed discard; it never overwrites a saved original. Recovery can
open only the journal's exact originating DB identity, without switching the
vault currently displayed in the window. Replacement mismatches refuse.

## Barriers and limits

Local panes flush and pause before native mutation leases. A native lease blocks
new owners until the operation completes. If an affected detached window has not
paused, or an unresolved journal remains, destructive operations refuse rather
than assume the main-window queue covers it. Ordinary window close uses its own
acknowledged owners so two windows cannot deadlock waiting for each other to
close. Vault switch/restore, lifecycle cleanup and all-pane mutation hooks retain
their existing refusal behavior when a local draft cannot flush. Native DB
open/close/restore/lifecycle guards and attachment cleanup also check owners.
No window is focused to request resolution. Same-note pane routing is unchanged.

Journal records and tombstones are conservatively retained; this stage adds no
unreviewed retention/deletion policy. It adds no schema revision/incarnation or
restore ancestry. Stage 1's state-token ABA and filesystem-identity limitations
remain. External replacement or a crash before the latest durable checkpoint
cannot be described as zero loss. A missing/replaced originating file blocks
forking; the private journal and memory are retained, never rebound implicitly.

## Verification

JS regressions exercise queue/timer fencing, ordered snapshots, journal and
transient-byte failures, fork retries, typing during fork, lost/reordered hints,
barrier refusal and lock release. Existing pane activation and detached context
regressions remain unchanged. Native regressions cover transactional fork rollback,
shared BLOB survival/dedup, summary-fork policy, private ordered journals, restart,
disk failures, lifecycle barriers, connection-local polling and real mock-app IPC.
Rust tests, platform durability/permissions, GUI boot, and real multi-window focus,
close, restart and restore exercises require host-side verification; npm tests do
not establish those runtime properties.
