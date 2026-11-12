-- Schema 3: separate immutable revision ancestry. See docs/revision-ancestry-schema.md.
CREATE TABLE vault_identity (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    vault_uuid TEXT NOT NULL UNIQUE,
    storage_epoch TEXT NOT NULL,
    created TEXT NOT NULL
);

-- Survives notes row deletion. One counter across all incarnations of a note ID.
CREATE TABLE note_revision_counters (
    note_id TEXT PRIMARY KEY,
    high_water INTEGER NOT NULL CHECK (high_water >= 0)
);
CREATE TABLE note_incarnations (
    incarnation_id TEXT PRIMARY KEY,
    note_id TEXT NOT NULL REFERENCES note_revision_counters(note_id),
    born_at TEXT NOT NULL,
    ended_at TEXT
);
CREATE INDEX idx_incarnations_note ON note_incarnations(note_id);

CREATE TABLE revisions (
    revision_id TEXT PRIMARY KEY,
    incarnation_id TEXT NOT NULL REFERENCES note_incarnations(incarnation_id),
    revision_no INTEGER NOT NULL CHECK (revision_no > 0),
    storage_epoch TEXT NOT NULL,
    commit_token TEXT NOT NULL UNIQUE,
    state_sha256 TEXT NOT NULL,
    writer_source TEXT NOT NULL CHECK (writer_source IN
        ('gui','cli','task','import','session_import','recovery','migration','restore')),
    writer_instance TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    operation_item TEXT NOT NULL,
    request_sha256 TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN
        ('create','update','metadata','trash','untrash','delete','import',
         'merge','migration_baseline','restore_baseline')),
    ancestry TEXT NOT NULL CHECK (ancestry IN ('known','unknown_baseline')),
    committed_at TEXT NOT NULL,
    UNIQUE (incarnation_id, revision_no),
    UNIQUE (storage_epoch, operation_id, operation_item)
);
CREATE INDEX idx_revisions_incarnation ON revisions(incarnation_id, revision_no);

-- Self-contained state: pruning an older body never damages a newer payload.
CREATE TABLE revision_states (
    revision_id TEXT PRIMARY KEY REFERENCES revisions(revision_id) ON DELETE RESTRICT,
    body BLOB NOT NULL,
    title TEXT NOT NULL,
    tags_json TEXT NOT NULL CHECK (json_valid(tags_json) AND json_type(tags_json) = 'array'),
    pinned INTEGER NOT NULL CHECK (pinned IN (0,1)),
    lifecycle TEXT NOT NULL CHECK (lifecycle IN ('live','trashed','deleted')),
    note_created TEXT NOT NULL,
    note_modified TEXT NOT NULL,
    extra_json TEXT NOT NULL CHECK (json_valid(extra_json) AND json_type(extra_json) = 'object')
);
CREATE TABLE revision_parents (
    child_id TEXT NOT NULL REFERENCES revisions(revision_id),
    parent_id TEXT NOT NULL REFERENCES revisions(revision_id),
    role TEXT NOT NULL CHECK (role IN ('predecessor','merge_parent','draft_base')),
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    PRIMARY KEY (child_id, role, ordinal),
    CHECK (child_id <> parent_id)
);
CREATE INDEX idx_revision_parents_parent ON revision_parents(parent_id);

-- Includes hard-deleted heads, so absence cannot be mistaken for a fresh identity.
CREATE TABLE note_revision_heads (
    note_id TEXT PRIMARY KEY REFERENCES note_revision_counters(note_id),
    incarnation_id TEXT NOT NULL REFERENCES note_incarnations(incarnation_id),
    revision_id TEXT NOT NULL REFERENCES revision_states(revision_id)
);
CREATE TABLE revision_attachments (
    revision_id TEXT NOT NULL REFERENCES revision_states(revision_id) ON DELETE CASCADE,
    attachment_id TEXT NOT NULL REFERENCES attachments(id) ON DELETE RESTRICT,
    PRIMARY KEY (revision_id, attachment_id)
);
CREATE TABLE revision_pins (
    pin_id TEXT NOT NULL,
    revision_id TEXT NOT NULL REFERENCES revision_states(revision_id) ON DELETE RESTRICT,
    owner_kind TEXT NOT NULL CHECK (owner_kind IN ('recovery','review','export')),
    owner_id TEXT NOT NULL,
    created TEXT NOT NULL,
    PRIMARY KEY (pin_id, revision_id)
);

-- Preserve auditability of deliberate payload pruning; graph headers never go.
CREATE TABLE revision_pruning (
    revision_id TEXT PRIMARY KEY REFERENCES revisions(revision_id),
    pruned_at TEXT NOT NULL,
    policy_version INTEGER NOT NULL CHECK (policy_version > 0)
);
-- Legacy history is explicitly not proof of ancestry. New curated snapshots
-- may bind an exact revision, without turning versions into the commit graph.
ALTER TABLE versions ADD COLUMN ancestry TEXT NOT NULL DEFAULT 'unknown'
    CHECK (ancestry IN ('unknown','verified'));
ALTER TABLE versions ADD COLUMN revision_id TEXT REFERENCES revisions(revision_id);

CREATE TRIGGER revisions_no_update BEFORE UPDATE ON revisions BEGIN
    SELECT RAISE(ABORT, 'revision headers are immutable');
END;
CREATE TRIGGER revisions_no_delete BEFORE DELETE ON revisions BEGIN
    SELECT RAISE(ABORT, 'revision headers are retained');
END;
CREATE TRIGGER revision_states_no_update BEFORE UPDATE ON revision_states BEGIN
    SELECT RAISE(ABORT, 'revision states are immutable');
END;
CREATE TRIGGER revision_parents_no_update BEFORE UPDATE ON revision_parents BEGIN
    SELECT RAISE(ABORT, 'revision parents are immutable');
END;
CREATE TRIGGER revision_parents_no_delete BEFORE DELETE ON revision_parents BEGIN
    SELECT RAISE(ABORT, 'revision parents are retained');
END;
