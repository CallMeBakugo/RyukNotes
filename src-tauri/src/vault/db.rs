// RyukNotes vault database.
//
// The vault is a single SQLite database (intended to live inside a VeraCrypt
// container the user mounts). Notes, tags, attachments (BLOBs, deduplicated by
// SHA-256), and version history all live in this one file, so backing up or
// sealing the vault is a single-file operation.
//
// Schema notes:
// - notes carry the same identity fields the .md vault used (id, title, tags,
//   pinned, created, modified) so the importer can migrate vaults losslessly.
// - attachments are BLOB rows capped by size (default 25 MiB) and deduplicated
//   by content hash; the same screenshot referenced from several notes is
//   stored once.
// - versions stores full-content snapshots (the .ryuknotes/history model).
// - notes_fts is an FTS5 index over title + content for search.

use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

/// Hard default cap for a single attachment (bytes). Configurable later via
/// settings; deliberately generous enough for research-paper PDFs and hostile
/// to accidental video drops.
pub const DEFAULT_MAX_ATTACHMENT_BYTES: i64 = 25 * 1024 * 1024;

const SCHEMA_VERSION: i64 = 2;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS notes (
    id       TEXT PRIMARY KEY,
    title    TEXT NOT NULL,
    content  TEXT NOT NULL,
    pinned   INTEGER NOT NULL DEFAULT 0,
    trashed  INTEGER NOT NULL DEFAULT 0,
    created  TEXT NOT NULL,
    modified TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS tags (
    name TEXT PRIMARY KEY
);

CREATE TABLE IF NOT EXISTS note_tags (
    note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    tag     TEXT NOT NULL REFERENCES tags(name) ON DELETE CASCADE,
    PRIMARY KEY (note_id, tag)
);

CREATE TABLE IF NOT EXISTS attachments (
    id        TEXT PRIMARY KEY,
    filename  TEXT NOT NULL,
    mime_type TEXT NOT NULL,
    size      INTEGER NOT NULL,
    sha256    TEXT NOT NULL UNIQUE,
    data      BLOB NOT NULL,
    created   TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS note_attachments (
    note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    attachment_id TEXT NOT NULL REFERENCES attachments(id) ON DELETE RESTRICT,
    PRIMARY KEY (note_id, attachment_id)
);
CREATE INDEX IF NOT EXISTS idx_note_attachments_attachment ON note_attachments(attachment_id);

CREATE TABLE IF NOT EXISTS versions (
    id        TEXT PRIMARY KEY,
    note_id   TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    content   TEXT NOT NULL,
    created   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_versions_note ON versions(note_id);

CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(
    title, content, content='', contentless_delete=1, tokenize='unicode61'
);

CREATE TABLE IF NOT EXISTS session_links (
    note_id    TEXT PRIMARY KEY REFERENCES notes(id) ON DELETE CASCADE,
    session_id TEXT NOT NULL,
    created    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_session_links_session ON session_links(session_id);
"#;

pub struct VaultDb {
    pub conn: Connection,
    pub max_attachment_bytes: i64,
}

pub struct NewNote {
    pub id: String,
    pub title: String,
    pub content: String,
    pub pinned: bool,
    pub tags: Vec<String>,
    pub created: String,
    pub modified: String,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteSummary {
    pub id: String,
    pub title: String,
    pub pinned: bool,
    pub trashed: bool,
    pub created: String,
    pub modified: String,
    pub tags: Vec<String>,
    pub preview: String,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentMeta {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note_id: Option<String>,
    pub note_ids: Vec<String>,
    pub filename: String,
    pub mime_type: String,
    pub size: i64,
    pub sha256: String,
}

impl VaultDb {
    /// Opens (creating if needed) the vault database and applies migrations.
    pub fn open(path: &Path) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|e| e.to_string())?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self, String> {
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| e.to_string())?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|e| e.to_string())?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(|e| e.to_string())?;
        let has_meta: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'meta')",
                [],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        let version = if has_meta {
            let raw: Option<String> = conn
                .query_row(
                    "SELECT value FROM meta WHERE key = 'schema_version'",
                    [],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            raw.ok_or("Vault schema version is missing")?
                .parse::<i64>()
                .map_err(|_| "Invalid vault schema version")?
        } else {
            let tables: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            if tables != 0 {
                return Err("Unrecognized vault schema".into());
            }
            0
        };
        if !(0..=SCHEMA_VERSION).contains(&version) || (has_meta && version == 0) {
            return Err(format!("Unsupported vault schema version {version}"));
        }
        if version == 1 {
            // VACUUM INTO makes a consistent snapshot, including committed WAL data.
            if let Some(path) = conn.path().filter(|p| !p.is_empty() && *p != ":memory:") {
                let backup = format!("{path}.pre-v2-{}.bak", uuid::Uuid::new_v4());
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                // Reserve a private, empty destination; SQLite accepts an empty file.
                drop(options.open(&backup).map_err(|e| e.to_string())?);
                conn.execute("VACUUM INTO ?1", params![backup])
                    .map_err(|e| e.to_string())?;
            }
        }
        if version == SCHEMA_VERSION {
            return Ok(Self {
                conn,
                max_attachment_bytes: DEFAULT_MAX_ATTACHMENT_BYTES,
            });
        }
        let tx =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| e.to_string())?;
        // Another process may have migrated after the initial inspection/snapshot.
        let locked_has_meta: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'meta')",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if locked_has_meta {
            let locked_version: Option<String> = tx
                .query_row(
                    "SELECT value FROM meta WHERE key = 'schema_version'",
                    [],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            if locked_version.as_deref() != Some(version.to_string().as_str()) {
                return Err("Vault schema changed while opening; retry opening the vault".into());
            }
        } else if version != 0 {
            return Err("Vault schema changed while opening".into());
        }
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .map_err(|e| e.to_string())?;
        if version == 1 {
            tx.execute_batch(r#"
                -- v1 deletions could leave tag links behind with FK enforcement off.
                -- Only links to absent notes are safe to discard; retain trashed notes.
                DELETE FROM note_tags
                    WHERE NOT EXISTS (SELECT 1 FROM notes WHERE notes.id = note_tags.note_id);
                CREATE TEMP TABLE attachment_owners AS SELECT note_id, id FROM attachments;
                CREATE TABLE attachments_v2 (
                    id TEXT PRIMARY KEY, filename TEXT NOT NULL, mime_type TEXT NOT NULL,
                    size INTEGER NOT NULL, sha256 TEXT NOT NULL UNIQUE,
                    data BLOB NOT NULL, created TEXT NOT NULL
                );
                INSERT INTO attachments_v2 SELECT id, filename, mime_type, size, sha256, data, created FROM attachments;
            "#).map_err(|e| e.to_string())?;
            let mismatch: bool = tx.query_row(
                "SELECT EXISTS(SELECT id, filename, mime_type, size, sha256, data, created FROM attachments EXCEPT SELECT * FROM attachments_v2) OR (SELECT COUNT(*) FROM attachments) != (SELECT COUNT(*) FROM attachments_v2)",
                [], |r| r.get(0),
            ).map_err(|e| e.to_string())?;
            if mismatch {
                return Err("Attachment migration validation failed".into());
            }
            tx.execute_batch(
                "DROP TABLE attachments; ALTER TABLE attachments_v2 RENAME TO attachments;",
            )
            .map_err(|e| e.to_string())?;
        }
        tx.execute_batch(SCHEMA).map_err(|e| e.to_string())?;
        if version == 1 {
            tx.execute_batch(r#"
                INSERT INTO note_attachments SELECT note_id, id FROM attachment_owners;
                DROP TABLE attachment_owners;
                INSERT OR IGNORE INTO note_attachments
                    SELECT n.id, a.id FROM notes n JOIN attachments a ON instr(n.content, a.id) > 0;
                INSERT OR IGNORE INTO note_attachments
                    SELECT v.note_id, a.id FROM versions v JOIN attachments a ON instr(v.content, a.id) > 0;
            "#).map_err(|e| e.to_string())?;
        }
        let violations = {
            let mut statement = tx
                .prepare("PRAGMA foreign_key_check")
                .map_err(|e| e.to_string())?;
            let rows = statement
                .query_map([], |row| {
                    let table: String = row.get(0)?;
                    let rowid: Option<i64> = row.get(1)?;
                    let parent: String = row.get(2)?;
                    let foreign_key: i64 = row.get(3)?;
                    let rowid = rowid.map_or_else(|| "NULL".to_string(), |id| id.to_string());
                    Ok(format!(
                        "table={table}, rowid={rowid}, parent={parent}, foreign_key={foreign_key}"
                    ))
                })
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?
        };
        if !violations.is_empty() {
            return Err(format!(
                "Vault foreign-key validation failed: {} violation(s): {}",
                violations.len(),
                violations.join("; ")
            ));
        }
        tx.execute("INSERT INTO meta(key, value) VALUES('schema_version', ?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![SCHEMA_VERSION.to_string()]).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(Self {
            conn,
            max_attachment_bytes: DEFAULT_MAX_ATTACHMENT_BYTES,
        })
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO meta(key, value) VALUES(?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Generic key-value state (Quick Access paths, UI prefs) stored as JSON.
    pub fn get_state(&self, key: &str) -> Result<Option<serde_json::Value>, String> {
        let row = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        match row {
            None => Ok(None),
            Some(text) => serde_json::from_str(&text)
                .map(Some)
                .map_err(|e| e.to_string()),
        }
    }

    pub fn set_state(&self, key: &str, value: &serde_json::Value) -> Result<(), String> {
        let text = serde_json::to_string(value).map_err(|e| e.to_string())?;
        self.set_meta(key, &text)
    }

    /// Note -> hermes session linkage ("the session symlink"). One live
    /// session per note; starting over replaces the link.
    pub fn get_session_link(&self, note_id: &str) -> Result<Option<String>, String> {
        self.conn
            .query_row(
                "SELECT session_id FROM session_links WHERE note_id = ?1",
                params![note_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    pub fn set_session_link(&self, note_id: &str, session_id: &str) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO session_links(note_id, session_id, created) VALUES(?1, ?2, ?3)
                 ON CONFLICT(note_id) DO UPDATE SET session_id = excluded.session_id, created = excluded.created",
                params![note_id, session_id, chrono::Utc::now().to_rfc3339()],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Reverse lookup: the note a hermes session is linked to (latest link
    /// wins, matching idx_session_links_session). Used by `session restore`
    /// to refresh the grounding context before resuming.
    pub fn get_note_for_session(&self, session_id: &str) -> Result<Option<String>, String> {
        self.conn
            .query_row(
                "SELECT note_id FROM session_links WHERE session_id = ?1
                 ORDER BY created DESC LIMIT 1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    /// Inserts a note (or replaces it wholesale by id) and refreshes its tags
    /// and the search index in one transaction.
    pub fn upsert_note(&self, note: &NewNote) -> Result<(), String> {
        self.upsert_note_checked(note, None)
    }

    /// Checks the editor baseline under the same write lock as the update.
    /// A missing note is also a conflict: stale editors must not resurrect it.
    pub fn upsert_note_checked(
        &self,
        note: &NewNote,
        expected_raw: Option<&str>,
    ) -> Result<(), String> {
        self.upsert_note_with_history(note, expected_raw, None)
    }

    pub fn upsert_note_with_history(
        &self,
        note: &NewNote,
        expected_raw: Option<&str>,
        max_versions: Option<u32>,
    ) -> Result<(), String> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )
        .map_err(|e| e.to_string())?;
        Self::write_note(&tx, note, expected_raw, max_versions)?;
        tx.commit().map_err(|e| e.to_string())
    }

    pub(crate) fn write_note(
        tx: &Connection,
        note: &NewNote,
        expected_raw: Option<&str>,
        max_versions: Option<u32>,
    ) -> Result<(), String> {
        if let Some(expected) = expected_raw {
            let stored: Option<String> = tx
                .query_row(
                    "SELECT content FROM notes WHERE id = ?1",
                    params![note.id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            if stored.as_deref() != Some(expected) {
                return Err(
                    "Save conflict: note changed in database. Your edits have not been written."
                        .to_string(),
                );
            }
        }
        if let Some(limit) = max_versions {
            let previous: Option<String> = tx
                .query_row(
                    "SELECT content FROM notes WHERE id=?1",
                    params![note.id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            if let Some(previous) = previous.filter(|body| body != &note.content) {
                Self::snapshot_on(tx, &note.id, &previous, limit, false)?;
            }
        }
        tx.execute(
            "INSERT INTO notes(id, title, content, pinned, trashed, created, modified)
             VALUES(?1, ?2, ?3, ?4, 0, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
                title = excluded.title,
                content = excluded.content,
                pinned = excluded.pinned,
                modified = excluded.modified",
            params![
                note.id,
                note.title,
                note.content,
                note.pinned as i64,
                note.created,
                note.modified
            ],
        )
        .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM note_tags WHERE note_id = ?1", params![note.id])
            .map_err(|e| e.to_string())?;
        for tag in &note.tags {
            tx.execute("INSERT OR IGNORE INTO tags(name) VALUES(?1)", params![tag])
                .map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR IGNORE INTO note_tags(note_id, tag) VALUES(?1, ?2)",
                params![note.id, tag],
            )
            .map_err(|e| e.to_string())?;
        }
        Self::link_content(tx, &note.id, &note.content)?;
        // Refresh the FTS row for this note (delete-then-insert by rowid).
        tx.execute(
            "DELETE FROM notes_fts WHERE rowid = (SELECT rowid FROM notes WHERE id = ?1)",
            params![note.id],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO notes_fts(rowid, title, content) VALUES((SELECT rowid FROM notes WHERE id = ?1), ?2, ?3)",
            params![note.id, note.title, note.content],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Stores a history snapshot. The version id is the SHA-256 of
    /// (note id, content), so identical snapshots dedupe across re-imports.
    pub fn add_version(
        &self,
        note_id: &str,
        content: &str,
        created: &str,
    ) -> Result<String, String> {
        let digest = {
            use sha2::Digest;
            let mut hasher = sha2::Sha256::new();
            hasher.update(note_id.as_bytes());
            hasher.update(content.as_bytes());
            format!("{:x}", hasher.finalize())
        };
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO versions(id, note_id, content, created) VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO NOTHING",
            params![digest, note_id, content, created],
        )
        .map_err(|e| e.to_string())?;
        Self::link_content(&tx, note_id, content)?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(digest)
    }

    pub(crate) fn snapshot_on(
        conn: &Connection,
        note_id: &str,
        content: &str,
        max_versions: u32,
        force: bool,
    ) -> Result<String, String> {
        let now = chrono::Utc::now();
        let latest: Option<String> = conn.query_row(
            "SELECT created FROM versions WHERE note_id=?1 ORDER BY julianday(created) DESC, created DESC LIMIT 1",
            params![note_id], |r| r.get(0),
        ).optional().map_err(|e| e.to_string())?;
        if !force
            && latest
                .as_deref()
                .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
                .is_some_and(|last| (now - last.with_timezone(&chrono::Utc)).num_seconds() < 300)
        {
            return Ok(String::new());
        }
        let mut instant = now;
        let timestamp = loop {
            let timestamp = instant.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
            let exists: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM versions WHERE note_id=?1 AND created=?2)",
                    params![note_id, timestamp],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            if !exists {
                break timestamp;
            }
            instant += chrono::Duration::nanoseconds(1);
        };
        conn.execute(
            "INSERT INTO versions(id,note_id,content,created) VALUES(?1,?2,?3,?4)",
            params![
                uuid::Uuid::new_v4().to_string(),
                note_id,
                content,
                timestamp
            ],
        )
        .map_err(|e| e.to_string())?;
        Self::link_content(conn, note_id, content)?;
        if max_versions > 0 {
            conn.execute("DELETE FROM versions WHERE note_id=?1 AND id NOT IN (SELECT id FROM versions WHERE note_id=?1 ORDER BY julianday(created) DESC, created DESC, id DESC LIMIT ?2)", params![note_id, max_versions]).map_err(|e| e.to_string())?;
        }
        Ok(timestamp)
    }

    pub fn force_version(&self, note_id: &str, max_versions: u32) -> Result<(), String> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )
        .map_err(|e| e.to_string())?;
        let content: String = tx
            .query_row(
                "SELECT content FROM notes WHERE id=?1",
                params![note_id],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        Self::snapshot_on(&tx, note_id, &content, max_versions, true)?;
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn list_versions(&self, note_id: &str) -> Result<Vec<(String, String, String)>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, content, created FROM versions WHERE note_id = ?1 ORDER BY created",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![note_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    pub fn get_note_created(&self, id: &str) -> Result<Option<String>, String> {
        self.conn
            .query_row(
                "SELECT created FROM notes WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    pub fn set_note_trashed(&self, id: &str, trashed: bool) -> Result<(), String> {
        let changed = self
            .conn
            .execute(
                "UPDATE notes SET trashed = ?1 WHERE id = ?2",
                params![trashed as i64, id],
            )
            .map_err(|e| e.to_string())?;
        if changed == 0 {
            return Err(format!("No note with id {id}"));
        }
        Ok(())
    }

    pub fn delete_note(&self, id: &str) -> Result<(), String> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "DELETE FROM notes_fts WHERE rowid = (SELECT rowid FROM notes WHERE id = ?1)",
            params![id],
        )
        .map_err(|e| e.to_string())?;
        if tx
            .execute("DELETE FROM notes WHERE id = ?1", params![id])
            .map_err(|e| e.to_string())?
            == 0
        {
            return Err(format!("No note with id {id}"));
        }
        let mut layout = super::db_notebooks::Layout::load(&tx)?;
        if layout.notes.remove(id).is_some() {
            layout.save(&tx)?;
        }
        tx.commit().map_err(|e| e.to_string())
    }

    /// Full note (meta + tags + content) for the frontend adapter.
    pub fn get_note_full(&self, id: &str) -> Result<Option<serde_json::Value>, String> {
        let row = self
            .conn
            .query_row(
                "SELECT id, title, content, pinned, trashed, created, modified,
                        (SELECT group_concat(t.tag, char(31)) FROM note_tags t WHERE t.note_id = n.id)
                 FROM notes n WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, Option<String>>(7)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| e.to_string())?;
        Ok(row.map(
            |(id, title, content, pinned, trashed, created, modified, tags)| {
                serde_json::json!({
                    "id": id,
                    "title": title,
                    "content": content,
                    "pinned": pinned != 0,
                    "trashed": trashed != 0,
                    "created": created,
                    "modified": modified,
                    "tags": tags
                        .map(|b| b.split('\u{1f}').map(str::to_string).collect::<Vec<_>>())
                        .unwrap_or_default(),
                })
            },
        ))
    }

    pub fn get_note_content(&self, id: &str) -> Result<Option<String>, String> {
        self.conn
            .query_row(
                "SELECT content FROM notes WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    pub fn list_notes(&self, include_trashed: bool) -> Result<Vec<NoteSummary>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT n.id, n.title, n.pinned, n.trashed, n.created, n.modified,
                        (SELECT group_concat(t.tag, char(31)) FROM note_tags t WHERE t.note_id = n.id),
                        substr(replace(replace(n.content, char(10), ' '), '#', ''), 1, 120)
                 FROM notes n
                 WHERE n.trashed = ?1 OR ?2
                 ORDER BY n.pinned DESC, n.modified DESC",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![include_trashed as i64, include_trashed], |row| {
                let tags_blob: Option<String> = row.get(6)?;
                Ok(NoteSummary {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    pinned: row.get::<_, i64>(2)? != 0,
                    trashed: row.get::<_, i64>(3)? != 0,
                    created: row.get(4)?,
                    modified: row.get(5)?,
                    tags: tags_blob
                        .map(|b| b.split('\u{1f}').map(str::to_string).collect())
                        .unwrap_or_default(),
                    preview: row.get(7)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    /// Full-text search over title and content.
    pub fn search(&self, query: &str) -> Result<Vec<NoteSummary>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT n.id, n.title, n.pinned, n.trashed, n.created, n.modified,
                        substr(replace(replace(n.content, char(10), ' '), '#', ''), 1, 120)
                 FROM notes_fts f JOIN notes n ON n.rowid = f.rowid
                 WHERE notes_fts MATCH ?1 AND n.trashed = 0
                 ORDER BY rank",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![query], |row| {
                Ok(NoteSummary {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    pinned: row.get::<_, i64>(2)? != 0,
                    trashed: row.get::<_, i64>(3)? != 0,
                    created: row.get(4)?,
                    modified: row.get(5)?,
                    tags: Vec::new(),
                    preview: row.get(6)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    pub(crate) fn link_content(
        conn: &Connection,
        note_id: &str,
        content: &str,
    ) -> Result<(), String> {
        // Conservative UUID matching may retain extra links; it never discards bytes.
        conn.execute(
            "INSERT OR IGNORE INTO note_attachments(note_id, attachment_id)
             SELECT ?1, id FROM attachments WHERE instr(?2, id) > 0",
            params![note_id, content],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Deduplicate bytes and record each requesting note in the same transaction.
    pub fn add_attachment(
        &self,
        note_id: &str,
        filename: &str,
        mime_type: &str,
        data: &[u8],
        now: &str,
    ) -> Result<AttachmentMeta, String> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )
        .map_err(|e| e.to_string())?;
        let id = self.store_attachment_on(&tx, note_id, filename, mime_type, data, now)?;
        let mut meta = self.attachment_meta(&id)?.ok_or("Attachment disappeared")?;
        meta.note_id = Some(note_id.to_string());
        tx.commit().map_err(|e| e.to_string())?;
        Ok(meta)
    }

    pub(crate) fn store_attachment_on(
        &self,
        tx: &Connection,
        note_id: &str,
        filename: &str,
        mime_type: &str,
        data: &[u8],
        now: &str,
    ) -> Result<String, String> {
        if data.len() as i64 > self.max_attachment_bytes {
            return Err(format!(
                "Attachment {filename} exceeds the vault cap of {} bytes",
                self.max_attachment_bytes
            ));
        }
        let digest = {
            use sha2::Digest;
            format!("{:x}", sha2::Sha256::digest(data))
        };
        tx.execute(
            "INSERT INTO attachments(id, filename, mime_type, size, sha256, data, created)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT(sha256) DO NOTHING",
            params![
                uuid::Uuid::new_v4().to_string(),
                filename,
                mime_type,
                data.len() as i64,
                digest,
                data,
                now
            ],
        )
        .map_err(|e| e.to_string())?;
        let id: String = tx
            .query_row(
                "SELECT id FROM attachments WHERE sha256 = ?1",
                params![digest],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        // Even a duplicate must pass the note foreign key.
        tx.execute(
            "INSERT OR IGNORE INTO note_attachments(note_id, attachment_id) VALUES(?1, ?2)",
            params![note_id, id],
        )
        .map_err(|e| e.to_string())?;
        Ok(id)
    }

    fn attachment_meta(&self, id: &str) -> Result<Option<AttachmentMeta>, String> {
        let mut meta = self
            .conn
            .query_row(
                "SELECT id, filename, mime_type, size, sha256 FROM attachments WHERE id = ?1",
                params![id],
                |row| {
                    Ok(AttachmentMeta {
                        id: row.get(0)?,
                        note_id: None,
                        note_ids: Vec::new(),
                        filename: row.get(1)?,
                        mime_type: row.get(2)?,
                        size: row.get(3)?,
                        sha256: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some(meta) = &mut meta {
            let mut stmt = self.conn.prepare("SELECT note_id FROM note_attachments WHERE attachment_id = ?1 ORDER BY note_id")
                .map_err(|e| e.to_string())?;
            meta.note_ids = stmt
                .query_map(params![id], |r| r.get(0))
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
        }
        Ok(meta)
    }

    pub fn get_attachment_data(
        &self,
        id: &str,
    ) -> Result<Option<(AttachmentMeta, Vec<u8>)>, String> {
        let Some(meta) = self.attachment_meta(id)? else {
            return Ok(None);
        };
        let data = self
            .conn
            .query_row(
                "SELECT data FROM attachments WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        Ok(Some((meta, data)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(id: &str, title: &str, tags: Vec<&str>) -> NewNote {
        NewNote {
            id: id.into(),
            title: title.into(),
            content: format!("Body of {title}. Cryptococcus research notes."),
            pinned: false,
            tags: tags.into_iter().map(str::to_string).collect(),
            created: "2026-10-13T00:00:00Z".into(),
            modified: "2026-10-13T00:00:00Z".into(),
        }
    }

    #[test]
    fn notes_round_trip_with_tags_and_retagging() {
        let db = VaultDb::open_in_memory().unwrap();
        db.upsert_note(&note("a", "First", vec!["immunology", "crypto"]))
            .unwrap();
        db.upsert_note(&note("b", "Second", vec!["immunology"]))
            .unwrap();
        let all = db.list_notes(false).unwrap();
        assert_eq!(all.len(), 2);
        let first = all.iter().find(|n| n.id == "a").unwrap();
        assert_eq!(first.tags.len(), 2);

        // Retagging replaces, not accumulates.
        db.upsert_note(&note("a", "First", vec!["renamed-tag"]))
            .unwrap();
        let first = db
            .list_notes(false)
            .unwrap()
            .into_iter()
            .find(|n| n.id == "a")
            .unwrap();
        assert_eq!(first.tags, vec!["renamed-tag".to_string()]);

        assert_eq!(
            db.get_note_content("a").unwrap().unwrap(),
            "Body of First. Cryptococcus research notes."
        );
    }

    #[test]
    fn search_finds_content_and_ignores_trashed() {
        let db = VaultDb::open_in_memory().unwrap();
        db.upsert_note(&note("a", "Alpha note", vec![])).unwrap();
        db.upsert_note(&note("b", "Beta note", vec![])).unwrap();
        let hits = db.search("cryptococcus").unwrap();
        assert_eq!(hits.len(), 2);
        let hits = db.search("beta").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "b");
    }

    #[test]
    fn attachments_dedupe_by_hash_and_enforce_cap() {
        let db = VaultDb::open_in_memory().unwrap();
        db.upsert_note(&note("a", "Receipts", vec![])).unwrap();
        let payload = b"fake png bytes for a recovery code screenshot";
        let m1 = db
            .add_attachment(
                "a",
                "codes.png",
                "image/png",
                payload,
                "2026-10-13T00:00:00Z",
            )
            .unwrap();
        let m2 = db
            .add_attachment(
                "a",
                "same-thing-again.png",
                "image/png",
                payload,
                "2026-10-13T00:00:00Z",
            )
            .unwrap();
        assert_eq!(m1.id, m2.id, "identical content must dedupe to one row");
        assert_eq!(m1.sha256.len(), 64);

        let (_, data) = db.get_attachment_data(&m1.id).unwrap().unwrap();
        assert_eq!(data, payload.to_vec());

        let too_big = vec![0u8; (DEFAULT_MAX_ATTACHMENT_BYTES + 1) as usize];
        assert!(db
            .add_attachment("a", "movie.mp4", "video/mp4", &too_big, "now")
            .is_err());
    }

    #[test]
    fn deleting_a_note_cascades_tags_but_retains_attachment_bytes() {
        let db = VaultDb::open_in_memory().unwrap();
        db.upsert_note(&note("a", "Doomed", vec!["x"])).unwrap();
        db.add_attachment("a", "f.txt", "text/plain", b"data", "now")
            .unwrap();
        db.conn
            .execute("DELETE FROM notes WHERE id = 'a'", [])
            .unwrap();
        let n: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM note_tags", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
        let n: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM attachments", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }
    fn legacy_fixture(conn: &Connection) {
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute_batch(
            r#"
            DROP TABLE note_attachments;
            DROP TABLE attachments;
            CREATE TABLE attachments (
                id TEXT PRIMARY KEY,
                note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
                filename TEXT NOT NULL, mime_type TEXT NOT NULL, size INTEGER NOT NULL,
                sha256 TEXT NOT NULL UNIQUE, data BLOB NOT NULL, created TEXT NOT NULL
            );
            CREATE INDEX idx_attachments_note ON attachments(note_id);
            CREATE INDEX idx_attachments_hash ON attachments(sha256);
            INSERT INTO meta VALUES('schema_version', '1');
            INSERT INTO notes VALUES('a', 'Owner', '', 0, 0, 'then', 'then');
            INSERT INTO notes VALUES('b', 'Shared', '![image](asset-id)', 0, 1, 'then', 'then');
            INSERT INTO notes VALUES('c', 'History', '', 0, 0, 'then', 'then');
            INSERT INTO versions VALUES('version', 'c', 'asset-id', 'then');
            INSERT INTO attachments VALUES('asset-id', 'a', 'f.bin', 'application/octet-stream',
                3, 'hash', X'0001FF', 'then');
        "#,
        )
        .unwrap();
    }

    #[test]
    fn shared_bytes_survive_both_owners_and_failed_requests_are_atomic() {
        let db = VaultDb::open_in_memory().unwrap();
        for id in ["a", "b"] {
            db.upsert_note(&note(id, id, vec![])).unwrap();
        }
        let first = db
            .add_attachment("a", "a.txt", "text/plain", b"shared", "now")
            .unwrap();
        let second = db
            .add_attachment("b", "b.txt", "text/plain", b"shared", "now")
            .unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(second.note_id.as_deref(), Some("b"));
        assert_eq!(second.note_ids, vec!["a", "b"]);
        db.add_attachment("b", "again", "text/plain", b"shared", "now")
            .unwrap();
        for payload in [b"shared".as_slice(), b"new bytes".as_slice()] {
            assert!(db
                .add_attachment("missing", "x", "text/plain", payload, "now")
                .is_err());
        }
        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM attachments", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        db.set_note_trashed("a", true).unwrap();
        db.set_note_trashed("a", false).unwrap();
        db.delete_note("a").unwrap();
        let (meta, data) = db.get_attachment_data(&first.id).unwrap().unwrap();
        assert_eq!(meta.note_ids, vec!["b"]);
        assert_eq!(data, b"shared");
        db.delete_note("b").unwrap();
        assert!(db
            .get_attachment_data(&first.id)
            .unwrap()
            .unwrap()
            .0
            .note_ids
            .is_empty());
    }

    #[test]
    fn copied_content_and_history_acquire_links_without_discarding_old_links() {
        let db = VaultDb::open_in_memory().unwrap();
        db.upsert_note(&note("a", "a", vec![])).unwrap();
        let asset = db
            .add_attachment("a", "x", "text/plain", b"bytes", "now")
            .unwrap();
        let mut copy = note("b", "copy", vec![]);
        copy.content = format!("![copy]({})", asset.id);
        db.upsert_note(&copy).unwrap();
        copy.content.clear();
        db.upsert_note(&copy).unwrap();
        db.upsert_note(&note("c", "history", vec![])).unwrap();
        db.add_version("c", &asset.id, "now").unwrap();
        db.add_version("c", &asset.id, "now").unwrap();
        assert_eq!(
            db.get_attachment_data(&asset.id)
                .unwrap()
                .unwrap()
                .0
                .note_ids,
            vec!["a", "b", "c"]
        );
    }

    #[test]
    fn v1_migration_preserves_bytes_links_history_and_snapshot_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v.rn");
        let conn = Connection::open(&path).unwrap();
        legacy_fixture(&conn);
        drop(conn);
        let db = VaultDb::open(&path).unwrap();
        let (meta, bytes) = db.get_attachment_data("asset-id").unwrap().unwrap();
        assert_eq!(meta.note_ids, vec!["a", "b", "c"]);
        assert_eq!(meta.filename, "f.bin");
        assert_eq!(meta.sha256, "hash");
        assert_eq!(bytes, vec![0, 1, 255]);
        db.delete_note("a").unwrap();
        assert!(db.get_attachment_data("asset-id").unwrap().is_some());
        drop(db);
        let db = VaultDb::open(&path).unwrap();
        assert_eq!(
            db.get_attachment_data("asset-id")
                .unwrap()
                .unwrap()
                .0
                .note_ids,
            vec!["b", "c"]
        );
        let snapshots: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "bak"))
            .collect();
        assert_eq!(snapshots.len(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&snapshots[0])
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let snapshot = Connection::open(&snapshots[0]).unwrap();
        let owner: String = snapshot
            .query_row("SELECT note_id FROM attachments", [], |r| r.get(0))
            .unwrap();
        assert_eq!(owner, "a");
        drop(snapshot);
        // Restoring a v1 snapshot must run the migration again successfully.
        assert!(VaultDb::open(&snapshots[0])
            .unwrap()
            .get_attachment_data("asset-id")
            .unwrap()
            .is_some());
    }

    #[test]
    fn v1_migration_sweeps_orphaned_note_tags_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy-tags.rn");
        let conn = Connection::open(&path).unwrap();
        legacy_fixture(&conn);
        conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
        conn.execute_batch(
            "INSERT INTO tags VALUES('kept');
             INSERT INTO note_tags VALUES('a', 'kept'), ('b', 'kept');
             INSERT INTO note_tags VALUES('deleted-1', 'kept'), ('deleted-2', 'kept'),
                 ('deleted-3', 'kept'), ('deleted-4', 'kept'), ('deleted-5', 'kept');",
        )
        .unwrap();
        let violations: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(violations, 5);
        drop(conn);

        let db = VaultDb::open(&path).unwrap();
        assert_eq!(
            db.get_attachment_data("asset-id").unwrap().unwrap().1,
            vec![0, 1, 255]
        );
        drop(db);
        let db = VaultDb::open(&path).unwrap();
        let version: String = db
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key='schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(version, "2");
        let mut statement = db
            .conn
            .prepare("SELECT note_id FROM note_tags ORDER BY note_id")
            .unwrap();
        let note_ids = statement
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            note_ids,
            vec!["a", "b"],
            "retain live and trashed note tags"
        );
        assert!(!db
            .conn
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap());
    }

    #[test]
    fn v1_migration_reports_remaining_fk_rows_and_rolls_back_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("invalid-tags.rn");
        let conn = Connection::open(&path).unwrap();
        legacy_fixture(&conn);
        conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
        conn.execute_batch(
            "INSERT INTO tags VALUES('kept');
             INSERT INTO note_tags(rowid, note_id, tag) VALUES
                 (11, 'deleted', 'kept'), (21, 'a', 'missing-tag'), (22, 'b', 'missing-tag');",
        )
        .unwrap();
        drop(conn);

        let error = VaultDb::open(&path)
            .err()
            .expect("migration must reject invalid tags");
        assert!(
            error.contains("foreign-key validation failed: 2 violation(s)"),
            "{error}"
        );
        for rowid in [21, 22] {
            assert!(
                error.contains(&format!("table=note_tags, rowid={rowid}, parent=tags")),
                "{error}"
            );
        }
        let conn = Connection::open(&path).unwrap();
        let version: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key='schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(version, "1");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM note_tags WHERE rowid=11", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            count, 1,
            "failed validation must roll back the orphan sweep"
        );
        let owner: String = conn
            .query_row("SELECT note_id FROM attachments", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            owner, "a",
            "failed validation must roll back attachment migration"
        );
    }

    #[test]
    fn failed_migration_rolls_back_and_future_versions_are_rejected() {
        let conn = Connection::open_in_memory().unwrap();
        legacy_fixture(&conn);
        conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
        conn.execute("UPDATE attachments SET note_id = 'missing'", [])
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("invalid.rn");
        conn.execute("VACUUM INTO ?1", params![path.to_string_lossy().as_ref()])
            .unwrap();
        assert!(VaultDb::open(&path).is_err());
        let original = Connection::open(&path).unwrap();
        let version: String = original
            .query_row(
                "SELECT value FROM meta WHERE key='schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(version, "1");
        let owner: String = original
            .query_row("SELECT note_id FROM attachments", [], |r| r.get(0))
            .unwrap();
        assert_eq!(owner, "missing");
        original
            .execute("UPDATE meta SET value='999' WHERE key='schema_version'", [])
            .unwrap();
        drop(original);
        assert!(VaultDb::open(&path).is_err());
        let original = Connection::open(&path).unwrap();
        let version: String = original
            .query_row(
                "SELECT value FROM meta WHERE key='schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(version, "999");
    }
}
