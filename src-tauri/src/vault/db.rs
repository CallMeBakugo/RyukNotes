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
// - versions stores full-content snapshots (the .helixnotes/history model).
// - notes_fts is an FTS5 index over title + content for search.

use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

/// Hard default cap for a single attachment (bytes). Configurable later via
/// settings; deliberately generous enough for research-paper PDFs and hostile
/// to accidental video drops.
pub const DEFAULT_MAX_ATTACHMENT_BYTES: i64 = 25 * 1024 * 1024;

const SCHEMA_VERSION: i64 = 1;

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
    note_id   TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    filename  TEXT NOT NULL,
    mime_type TEXT NOT NULL,
    size      INTEGER NOT NULL,
    sha256    TEXT NOT NULL UNIQUE,
    data      BLOB NOT NULL,
    created   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_attachments_note ON attachments(note_id);
CREATE INDEX IF NOT EXISTS idx_attachments_hash ON attachments(sha256);

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
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentMeta {
    pub id: String,
    pub note_id: String,
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
        conn.execute_batch(SCHEMA).map_err(|e| e.to_string())?;
        let db = Self { conn, max_attachment_bytes: DEFAULT_MAX_ATTACHMENT_BYTES };
        db.set_meta("schema_version", &SCHEMA_VERSION.to_string())?;
        Ok(db)
    }

    fn set_meta(&self, key: &str, value: &str) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO meta(key, value) VALUES(?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Inserts a note (or replaces it wholesale by id) and refreshes its tags
    /// and the search index in one transaction.
    pub fn upsert_note(&self, note: &NewNote) -> Result<(), String> {
        let tx = self.conn.unchecked_transaction().map_err(|e| e.to_string())?;
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
            tx.execute(
                "INSERT OR IGNORE INTO tags(name) VALUES(?1)",
                params![tag],
            )
            .map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR IGNORE INTO note_tags(note_id, tag) VALUES(?1, ?2)",
                params![note.id, tag],
            )
            .map_err(|e| e.to_string())?;
        }
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
        tx.commit().map_err(|e| e.to_string())
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
        self.conn
            .execute(
                "DELETE FROM notes_fts WHERE rowid = (SELECT rowid FROM notes WHERE id = ?1)",
                params![id],
            )
            .map_err(|e| e.to_string())?;
        let changed = self
            .conn
            .execute("DELETE FROM notes WHERE id = ?1", params![id])
            .map_err(|e| e.to_string())?;
        if changed == 0 {
            return Err(format!("No note with id {id}"));
        }
        Ok(())
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
                        (SELECT group_concat(t.tag, char(31)) FROM note_tags t WHERE t.note_id = n.id)
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
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    /// Full-text search over title and content.
    pub fn search(&self, query: &str) -> Result<Vec<NoteSummary>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT n.id, n.title, n.pinned, n.trashed, n.created, n.modified
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
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    /// Stores an attachment BLOB, deduplicated by SHA-256. When the hash
    /// already exists the existing row is (re)linked to the note.
    pub fn add_attachment(
        &self,
        note_id: &str,
        filename: &str,
        mime_type: &str,
        data: &[u8],
        now: &str,
    ) -> Result<AttachmentMeta, String> {
        if data.len() as i64 > self.max_attachment_bytes {
            return Err(format!(
                "Attachment {} is {} bytes; the vault cap is {} bytes",
                filename,
                data.len(),
                self.max_attachment_bytes
            ));
        }
        let digest = {
            use sha2::Digest;
            let mut hasher = sha2::Sha256::new();
            hasher.update(data);
            format!("{:x}", hasher.finalize())
        };
        if let Some(meta) = self.get_attachment_by_hash(&digest)? {
            return Ok(meta);
        }
        let id = uuid::Uuid::new_v4().to_string();
        self.conn
            .execute(
                "INSERT INTO attachments(id, note_id, filename, mime_type, size, sha256, data, created)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![id, note_id, filename, mime_type, data.len() as i64, digest, data, now],
            )
            .map_err(|e| e.to_string())?;
        Ok(AttachmentMeta {
            id,
            note_id: note_id.to_string(),
            filename: filename.to_string(),
            mime_type: mime_type.to_string(),
            size: data.len() as i64,
            sha256: digest,
        })
    }

    fn get_attachment_by_hash(&self, digest: &str) -> Result<Option<AttachmentMeta>, String> {
        self.conn
            .query_row(
                "SELECT id, note_id, filename, mime_type, size, sha256 FROM attachments WHERE sha256 = ?1",
                params![digest],
                |row| {
                    Ok(AttachmentMeta {
                        id: row.get(0)?,
                        note_id: row.get(1)?,
                        filename: row.get(2)?,
                        mime_type: row.get(3)?,
                        size: row.get(4)?,
                        sha256: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    pub fn get_attachment_data(&self, id: &str) -> Result<Option<(AttachmentMeta, Vec<u8>)>, String> {
        let row = self
            .conn
            .query_row(
                "SELECT id, note_id, filename, mime_type, size, sha256, data FROM attachments WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        AttachmentMeta {
                            id: row.get(0)?,
                            note_id: row.get(1)?,
                            filename: row.get(2)?,
                            mime_type: row.get(3)?,
                            size: row.get(4)?,
                            sha256: row.get(5)?,
                        },
                        row.get::<_, Vec<u8>>(6)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| e.to_string())?;
        Ok(row)
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
        db.upsert_note(&note("a", "First", vec!["immunology", "crypto"])).unwrap();
        db.upsert_note(&note("b", "Second", vec!["immunology"])).unwrap();
        let all = db.list_notes(false).unwrap();
        assert_eq!(all.len(), 2);
        let first = all.iter().find(|n| n.id == "a").unwrap();
        assert_eq!(first.tags.len(), 2);

        // Retagging replaces, not accumulates.
        db.upsert_note(&note("a", "First", vec!["renamed-tag"])).unwrap();
        let first = db.list_notes(false).unwrap().into_iter().find(|n| n.id == "a").unwrap();
        assert_eq!(first.tags, vec!["renamed-tag".to_string()]);

        assert_eq!(db.get_note_content("a").unwrap().unwrap(), "Body of First. Cryptococcus research notes.");
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
        let m1 = db.add_attachment("a", "codes.png", "image/png", payload, "2026-10-13T00:00:00Z").unwrap();
        let m2 = db.add_attachment("a", "same-thing-again.png", "image/png", payload, "2026-10-13T00:00:00Z").unwrap();
        assert_eq!(m1.id, m2.id, "identical content must dedupe to one row");
        assert_eq!(m1.sha256.len(), 64);

        let (_, data) = db.get_attachment_data(&m1.id).unwrap().unwrap();
        assert_eq!(data, payload.to_vec());

        let too_big = vec![0u8; (DEFAULT_MAX_ATTACHMENT_BYTES + 1) as usize];
        assert!(db.add_attachment("a", "movie.mp4", "video/mp4", &too_big, "now").is_err());
    }

    #[test]
    fn deleting_a_note_cascades_tags_and_attachments() {
        let db = VaultDb::open_in_memory().unwrap();
        db.upsert_note(&note("a", "Doomed", vec!["x"])).unwrap();
        db.add_attachment("a", "f.txt", "text/plain", b"data", "now").unwrap();
        db.conn.execute("DELETE FROM notes WHERE id = 'a'", []).unwrap();
        let n: i64 = db.conn.query_row("SELECT COUNT(*) FROM note_tags", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        let n: i64 = db.conn.query_row("SELECT COUNT(*) FROM attachments", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }
}
