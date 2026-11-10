# DB attachments and Markdown export

Files pasted or dropped into a DB note are stored as capped BLOBs (25 MiB per
file). SHA-256 deduplication shares bytes across notes; each note keeps its own
ownership link. Non-images, including PDFs, use ordinary named inline links.
Clicking a link offers Save attachment and writes the original bytes to the
selected destination. Non-images are never served through the image protocol.

The stored Markdown uses `.rn-attachments/<attachment-id>`. File reads and
insertion retain the originating pane, note and vault. A maintenance barrier
invalidates pending attachment operations even after the barrier finishes;
late completions cannot insert into a restored note or another pane.

Export to Markdown saves live and trashed notes, images and other attachments,
history (including duplicate forced snapshots), ownership links, logical notebook
metadata and session links. It retains conservatively stored BLOBs with no known
owner, matching T9's retention policy. Note filenames combine the title and stable
identity, avoiding collisions between notes with the same title. Re-export updates
the managed files instead of adding numbered copies, and removes obsolete managed
paths without deleting unrelated files.

Portable links point into `.ryuknotes/attachments/`; each unique BLOB is written
once. Query strings, fragments and surrounding Markdown remain intact. The
`.ryuknotes/export.json` index preserves original attachment IDs, filenames, MIME
types, timestamps and ownership, plus history identities and vault metadata.
**Keep the complete export folder, including `.ryuknotes`, for a lossless import.**
The existing Import Markdown folder action recognizes this index, checks attachment
sizes and hashes, and restores it transactionally. Re-import into the same DB is
idempotent; importing into a DB that already has matching bytes reuses its BLOB
and updates imported references to that BLOB's ID.

Ordinary filesystem vault imports also copy referenced local attachments from
notes and history. External single-note viewer imports retain their existing
copy-on-ID-collision behavior. Importing and exporting from Settings acquire the
existing all-pane flush/lock barrier and pin the native operation to its captured
vault. No vault schema migration or BLOB garbage collection is introduced.

Regression coverage is in `tests/db-attachments.test.mjs`,
`tests/editor-pane.test.mjs`, the native command tests, and
`src-tauri/src/vault/{db_attachments,db_export}.rs`. Native tests use disposable
fixtures, never user vaults. The Rust tests require the host-side gates; the
session cage cannot execute them.
