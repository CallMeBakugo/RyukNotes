//! Authoritative official-writer contract. No schema or revision ancestry change.
//! Tokens compare present state, not history: A -> B -> A (or same-state
//! delete/recreate in the same file) is still ABA. Never use these as revisions.
use super::db::{NewNote, VaultDb};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultIdentity {
    pub path: String,
    pub file: String,
}
impl VaultIdentity {
    pub(crate) fn capture(conn: &rusqlite::Connection) -> Result<Self, String> {
        let Some(path) = conn.path().filter(|p| !p.is_empty() && *p != ":memory:") else {
            return Ok(Self {
                path: ":memory:".into(),
                file: uuid::Uuid::new_v4().to_string(),
            });
        };
        let path = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
        let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        let file = {
            use std::os::unix::fs::MetadataExt;
            format!("{}:{}", meta.dev(), meta.ino())
        };
        // Best effort on platforms without a stable file ID in std. In-place
        // restoration to identical state is an explicit Stage 1/ABA limitation.
        #[cfg(not(unix))]
        let file = format!("{:?}", meta.created().map_err(|e| e.to_string())?);
        Ok(Self {
            path: path.to_string_lossy().into_owned(),
            file,
        })
    }
    fn hash(&self) -> String {
        hash(&serde_json::to_vec(self).expect("identity serializes"))
    }
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteErrorKind {
    Changed,
    Deleted,
    Trashed,
    VaultReplaced,
    InvalidPolicy,
    Io,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteError {
    pub kind: WriteErrorKind,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact: Option<String>,
}
impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for WriteError {}
impl From<String> for WriteError {
    fn from(message: String) -> Self {
        Self {
            kind: WriteErrorKind::Io,
            message,
            current: None,
            artifact: None,
        }
    }
}
impl From<&str> for WriteError {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}
impl WriteError {
    pub(crate) fn conflict(kind: WriteErrorKind, message: &str, current: Option<Value>) -> Self {
        Self {
            kind,
            message: message.into(),
            current,
            artifact: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WritePolicy {
    Create,
    Update {
        #[serde(rename = "expectedToken")]
        expected_token: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteWrite {
    pub vault: VaultIdentity,
    #[serde(default)]
    pub vault_path: Option<String>,
    pub id: String,
    pub title: String,
    pub content: String,
    pub pinned: bool,
    pub tags: Vec<String>,
    pub policy: WritePolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writer_source: Option<String>,
}

/// Only semantic state participates: modification timestamps are not revisions.
/// Tags are sets, with deterministic ordering and no delimiter-based encoding.
pub fn state_digest(note: &Value) -> String {
    let tags: std::collections::BTreeSet<&str> = note["tags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    hash(
        &serde_json::to_vec(&json!([
            note["id"],
            note["content"],
            note["title"],
            tags,
            note["pinned"],
            note["trashed"]
        ]))
        .expect("note state serializes"),
    )
}
impl NoteWrite {
    pub fn from_note(db: &VaultDb, note: &NewNote, policy: WritePolicy) -> Self {
        Self {
            vault: db.identity(),
            vault_path: None,
            id: note.id.clone(),
            title: note.title.clone(),
            content: note.content.clone(),
            pinned: note.pinned,
            tags: note.tags.clone(),
            policy,
            operation_id: None,
            writer_source: None,
        }
    }
}
impl VaultDb {
    pub fn validate_writer_path(&self, path: Option<&str>) -> Result<(), WriteError> {
        if let Some(path) = path {
            let actual = std::fs::canonicalize(path).map_err(|_| {
                WriteError::conflict(
                    WriteErrorKind::VaultReplaced,
                    "The captured vault path is unavailable.",
                    None,
                )
            })?;
            if actual.to_string_lossy() != self.writer_identity.path {
                return Err(WriteError::conflict(
                    WriteErrorKind::VaultReplaced,
                    "The captured path belongs to another vault.",
                    None,
                ));
            }
        }
        self.validate_writer_vault(&self.identity())
    }
    pub fn identity(&self) -> VaultIdentity {
        self.writer_identity.clone()
    }
    pub fn validate_writer_vault(&self, expected: &VaultIdentity) -> Result<(), WriteError> {
        let live = if self.writer_identity.path == ":memory:" {
            self.identity()
        } else {
            VaultIdentity::capture(&self.conn).map_err(|_| {
                WriteError::conflict(
                    WriteErrorKind::VaultReplaced,
                    "The originating vault is no longer available.",
                    None,
                )
            })?
        };
        if expected != &self.writer_identity || live != self.writer_identity {
            return Err(WriteError::conflict(
                WriteErrorKind::VaultReplaced,
                "The originating vault was replaced or another vault is open.",
                None,
            ));
        }
        self.validate_revision_session()?;
        Ok(())
    }
    pub fn state_token(&self, note: &Value) -> String {
        if self.is_v3() {
            return self
                .accepted_note(note["id"].as_str().unwrap_or(""))
                .ok()
                .flatten()
                .filter(|current| state_digest(current) == state_digest(note))
                .and_then(|current| current["accepted"]["token"].as_str().map(str::to_owned))
                .unwrap_or_else(|| "invalid-v3-state".into());
        }
        format!(
            "state-v1.{}.{}",
            self.writer_identity.hash(),
            state_digest(note)
        )
    }
    pub fn accepted_note(&self, id: &str) -> Result<Option<Value>, WriteError> {
        self.validate_writer_vault(&self.identity())?;
        if self.is_v3() {
            return super::revision_writer::read_accepted(self, id);
        }
        let note = self.get_note_full(id)?;
        let receipt = note.map(|mut note| {
            let token = self.state_token(&note);
            note["accepted"] = json!({ "vault": self.writer_identity, "token": token });
            note
        });
        self.validate_writer_vault(&self.identity())?;
        Ok(receipt)
    }
    pub(crate) fn check_policy(&self, id: &str, policy: &WritePolicy) -> Result<(), WriteError> {
        if self.is_v3() {
            return super::revision_writer::check_policy(self, id, policy);
        }
        let current = self.accepted_note(id)?;
        match policy {
            WritePolicy::Create if current.is_some() => Err(WriteError::conflict(
                WriteErrorKind::Changed,
                "Create refused: note identity already exists.",
                current,
            )),
            WritePolicy::Create => Ok(()),
            WritePolicy::Update { expected_token } => {
                let parts: Vec<_> = expected_token.split('.').collect();
                if parts.len() != 3
                    || parts[0] != "state-v1"
                    || parts[1..]
                        .iter()
                        .any(|p| p.len() != 64 || !p.bytes().all(|b| b.is_ascii_hexdigit()))
                {
                    return Err(WriteError::conflict(
                        WriteErrorKind::InvalidPolicy,
                        "Malformed expected state token.",
                        None,
                    ));
                }
                if expected_token.split('.').nth(1) != Some(self.writer_identity.hash().as_str()) {
                    return Err(WriteError::conflict(
                        WriteErrorKind::VaultReplaced,
                        "The expected token belongs to another vault identity.",
                        current,
                    ));
                }
                let Some(note) = current else {
                    return Err(WriteError::conflict(
                        WriteErrorKind::Deleted,
                        "The note was deleted; your draft was not written.",
                        None,
                    ));
                };
                if note["trashed"] == true {
                    return Err(WriteError::conflict(
                        WriteErrorKind::Trashed,
                        "The note is in trash; your draft was not written.",
                        Some(note),
                    ));
                }
                if note["accepted"]["token"].as_str() != Some(expected_token.as_str()) {
                    return Err(WriteError::conflict(
                        WriteErrorKind::Changed,
                        "The note changed; your draft was not written.",
                        Some(note),
                    ));
                }
                Ok(())
            }
        }
    }
    /// Capture the acknowledgment while holding the SAME immediate transaction.
    /// No read after commit may replace this response with another writer's state.
    pub fn write_authoritative(
        &self,
        request: &NoteWrite,
        max_versions: u32,
    ) -> Result<Value, WriteError> {
        self.write_policy(request, max_versions, false)
    }
    /// An independent CLI cannot safely infer the GUI retention preference:
    /// checkpoint each replaced body and never prune somebody else's history.
    pub fn write_scripted(&self, request: &NoteWrite) -> Result<Value, WriteError> {
        self.write_policy(request, 0, true)
    }
    fn write_policy(
        &self,
        request: &NoteWrite,
        max_versions: u32,
        force_history: bool,
    ) -> Result<Value, WriteError> {
        self.validate_writer_path(request.vault_path.as_deref())?;
        self.validate_writer_vault(&request.vault)?;
        let source = if force_history {
            "cli"
        } else {
            match request.writer_source.as_deref() {
                None | Some("gui") => "gui",
                Some("task") => "task",
                _ => {
                    return Err(WriteError::conflict(
                        WriteErrorKind::InvalidPolicy,
                        "Invalid editor writer source.",
                        None,
                    ))
                }
            }
        };
        let mut operation_request = request.clone();
        operation_request.tags.sort();
        operation_request.tags.dedup();
        let tx = self.begin_write(
            source,
            request.operation_id.as_deref(),
            &serde_json::to_value(&operation_request).map_err(|e| e.to_string())?,
        )?;
        if let Some(receipt) = tx.replay_receipt(&request.id)? {
            return Ok(receipt);
        }
        self.validate_writer_vault(&request.vault)?;
        self.check_policy(&request.id, &request.policy)?;
        let now = chrono::Utc::now().to_rfc3339();
        let note = NewNote {
            id: request.id.clone(),
            title: request.title.clone(),
            content: request.content.clone(),
            pinned: request.pinned,
            tags: request.tags.clone(),
            created: self
                .get_note_created(&request.id)?
                .unwrap_or_else(|| now.clone()),
            modified: now,
        };
        if force_history {
            if let Some(previous) = self.get_note_content(&note.id)? {
                if previous != note.content {
                    Self::snapshot_on(&tx, &note.id, &previous, 0, true)?;
                }
            }
        }
        Self::write_note(
            &tx,
            &note,
            None,
            if force_history {
                None
            } else {
                Some(max_versions)
            },
        )?;
        let receipt = tx.receipt_typed(&note.id)?;
        tx.commit_typed()?;
        Ok(receipt)
    }
    /// Atomic field intent: read after acquiring the write lock, never replay an
    /// earlier whole-note body just to change tags. External writers do not prune history.
    pub fn tag_atomic(
        &self,
        id: &str,
        add: &[String],
        remove: &[String],
    ) -> Result<Value, WriteError> {
        self.tag_atomic_expected(id, add, remove, None, None)
    }
    pub fn tag_atomic_expected(
        &self,
        id: &str,
        add: &[String],
        remove: &[String],
        expected: Option<&str>,
        operation: Option<&str>,
    ) -> Result<Value, WriteError> {
        self.validate_writer_vault(&self.identity())?;
        let tx = self.begin_write("cli", operation, &json!([id, add, remove, expected]))?;
        if let Some(receipt) = tx.replay_receipt(id)? {
            return Ok(receipt);
        }
        if let Some(expected) = expected {
            self.check_policy(
                id,
                &WritePolicy::Update {
                    expected_token: expected.into(),
                },
            )?;
        }
        let current = self.accepted_note(id)?.ok_or_else(|| {
            WriteError::conflict(WriteErrorKind::Deleted, "No note with that identity.", None)
        })?;
        if current["trashed"] == true {
            return Err(WriteError::conflict(
                WriteErrorKind::Trashed,
                "Cannot edit tags on a trashed note.",
                Some(current),
            ));
        }
        let mut tags: std::collections::BTreeSet<String> =
            serde_json::from_value(current["tags"].clone()).map_err(|e| e.to_string())?;
        tags.extend(add.iter().cloned());
        for tag in remove {
            tags.remove(tag);
        }
        let note = NewNote {
            id: id.into(),
            title: current["title"].as_str().unwrap_or("").into(),
            content: current["content"].as_str().unwrap_or("").into(),
            pinned: current["pinned"].as_bool().unwrap_or(false),
            tags: tags.into_iter().collect(),
            created: current["created"].as_str().unwrap_or("").into(),
            modified: chrono::Utc::now().to_rfc3339(),
        };
        Self::write_note(&tx, &note, None, None)?;
        let receipt = tx.receipt_typed(id)?;
        tx.commit_typed()?;
        Ok(receipt)
    }
}

/// Import policy is an explicit replacement, not a stale editor update. Preserve
/// the predecessor in the import transaction. Copy imports use fresh identities.
#[derive(Clone, Copy)]
pub enum ImportPolicy {
    Replace,
    Copy,
    // Only finalize a row created in this same import transaction. Do not
    // snapshot its temporary pre-rewrite attachment references as prior state.
    FinalizeCopy,
}
pub fn create_on(db: &VaultDb, tx: &rusqlite::Connection, note: &NewNote) -> Result<(), String> {
    db.validate_writer_vault(&db.identity())
        .map_err(|e| e.to_string())?;
    if db.get_note_content(&note.id)?.is_some() {
        return Err("Create identity already exists".into());
    }
    VaultDb::write_note(tx, note, None, None)
}
pub fn write_import_on(
    db: &VaultDb,
    tx: &rusqlite::Connection,
    note: &NewNote,
    policy: ImportPolicy,
) -> Result<(), String> {
    db.validate_writer_vault(&db.identity())
        .map_err(|e| e.to_string())?;
    if matches!(policy, ImportPolicy::Copy) {
        return create_on(db, tx, note);
    }
    if let Some(previous) = db.get_note_content(&note.id)? {
        match policy {
            ImportPolicy::Copy => return Err("Copy import identity already exists".into()),
            ImportPolicy::Replace if previous != note.content => {
                VaultDb::snapshot_on(tx, &note.id, &previous, 0, true)?;
            }
            _ => {}
        }
    }
    VaultDb::write_note(tx, note, None, None)
}

impl VaultDb {
    /// Generated-state import policy. Provenance is existing meta JSON, not a
    /// schema extension. No baseline for a legacy summary is guessed from text.
    pub fn import_generated_summary(
        &self,
        note: &NewNote,
        input: &Value,
    ) -> Result<Value, WriteError> {
        if self.is_v3() {
            return super::revision_writer::import_summary(self, note, input, None);
        }
        self.validate_writer_vault(&self.identity())?;
        let tx = self.begin_write(
            "session_import",
            None,
            &json!([
                input,
                note.id,
                note.title,
                note.content,
                note.tags,
                note.pinned
            ]),
        )?;
        let key = format!("generated_summary:v1:{}", hash(note.id.as_bytes()));
        let current = self.accepted_note(&note.id)?;
        let generated: Option<String> = tx
            .query_row("SELECT value FROM meta WHERE key=?1", params![key], |r| {
                r.get(0)
            })
            .optional()
            .map_err(|e| e.to_string())?;
        let incoming = json!({ "id": note.id, "title": note.title, "content": note.content,
            "pinned": note.pinned, "tags": note.tags, "trashed": false });
        if let Some(current) = &current {
            if generated.as_deref() != Some(state_digest(current).as_str())
                || current["trashed"] == true
            {
                let artifact = format!(
                    "pending_summary:v1:{}:{}",
                    hash(note.id.as_bytes()),
                    hash(
                        &serde_json::to_vec(&json!([input, state_digest(current)]))
                            .map_err(|e| e.to_string())?
                    )
                );
                let pending = json!({ "label": format!("Pending summary: {}", note.title), "targetId": note.id,
                    "incoming": incoming, "input": input, "reason": "The saved summary no longer matches proven generated state." });
                tx.execute(
                    "INSERT OR IGNORE INTO meta(key,value) VALUES(?1,?2)",
                    params![
                        artifact,
                        serde_json::to_string(&pending).map_err(|e| e.to_string())?
                    ],
                )
                .map_err(|e| e.to_string())?;
                // Commit preservation deliberately, but never replace the manual note.
                tx.commit_typed()?;
                let mut error = WriteError::conflict(if current["trashed"] == true { WriteErrorKind::Trashed } else { WriteErrorKind::Changed },
                    "Summary import conflicts with saved edits; incoming summary preserved as a labeled pending artifact.", Some(current.clone()));
                error.artifact = Some(artifact);
                return Err(error);
            }
            if state_digest(current) == state_digest(&incoming) {
                tx.commit_typed()?;
                return Ok(current.clone()); // Exact repeated import is a no-op, including modified time.
            }
        }
        write_import_on(
            self,
            &tx,
            note,
            if current.is_some() {
                ImportPolicy::Replace
            } else {
                ImportPolicy::Copy
            },
        )?;
        let receipt = self
            .accepted_note(&note.id)?
            .ok_or("Imported summary disappeared")?;
        tx.execute("INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, state_digest(&receipt)]).map_err(|e| e.to_string())?;
        tx.commit_typed()?;
        Ok(receipt)
    }
    pub fn import_generated_summary_with_operation(
        &self,
        note: &NewNote,
        input: &Value,
        operation: Option<&str>,
    ) -> Result<Value, WriteError> {
        if self.is_v3() {
            super::revision_writer::import_summary(self, note, input, operation)
        } else {
            self.import_generated_summary(note, input)
        }
    }
    pub fn pending_summaries(&self, session_id: &str) -> Result<Vec<Value>, String> {
        let prefix = format!(
            "pending_summary:v1:{}:%",
            hash(format!("hermes-session-{session_id}").as_bytes())
        );
        let mut stmt = self
            .conn
            .prepare("SELECT key,value FROM meta WHERE key LIKE ?1 ORDER BY key")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![prefix], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(|e| e.to_string())?;
        rows.map(|r| {
            let (key, value) = r.map_err(|e| e.to_string())?;
            Ok(json!({ "artifact": key, "value": serde_json::from_str::<Value>(&value).map_err(|e| e.to_string())? }))
        }).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn note(body: &str) -> NewNote {
        NewNote {
            id: "n".into(),
            title: "Title".into(),
            content: body.into(),
            pinned: false,
            tags: vec!["a".into()],
            created: "2026-01-01".into(),
            modified: "2026-01-01".into(),
        }
    }
    fn create(db: &VaultDb) -> Value {
        db.write_authoritative(
            &NoteWrite::from_note(db, &note("base"), WritePolicy::Create),
            20,
        )
        .unwrap()
    }
    fn update(db: &VaultDb, base: &Value, body: &str) -> NoteWrite {
        NoteWrite::from_note(
            db,
            &note(body),
            WritePolicy::Update {
                expected_token: base["accepted"]["token"].as_str().unwrap().into(),
            },
        )
    }
    #[test]
    fn separate_connections_reject_second_writer_in_both_orders_and_keep_exact_ack() {
        for gui_first in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("test.rn");
            let gui = VaultDb::open_legacy_v2(&path).unwrap();
            let cli = VaultDb::open_legacy_v2(&path).unwrap();
            let base = create(&gui);
            assert_eq!(gui.identity(), cli.identity());
            let (first, second) = if gui_first {
                (&gui, &cli)
            } else {
                (&cli, &gui)
            };
            let first_receipt = first
                .write_authoritative(&update(first, &base, "winner"), 20)
                .unwrap();
            let error = second
                .write_authoritative(&update(second, &base, "stale"), 20)
                .unwrap_err();
            assert_eq!(error.kind, WriteErrorKind::Changed);
            assert_eq!(second.get_note_content("n").unwrap().unwrap(), "winner");
            // A third commit before an adapter might formerly read back cannot
            // mutate the already returned value or make it acknowledge R2.
            second
                .write_authoritative(&update(second, &first_receipt, "later"), 20)
                .unwrap();
            assert_eq!(first_receipt["content"], "winner");
            assert_ne!(
                first_receipt["accepted"]["token"],
                second.accepted_note("n").unwrap().unwrap()["accepted"]["token"]
            );
            assert_eq!(first.list_versions("n").unwrap().len(), 1);
        }
    }
    #[test]
    fn full_state_guard_catches_metadata_and_lifecycle_and_does_not_resurrect() {
        for field in ["title", "tags", "pinned", "trashed", "deleted"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("test.rn");
            let gui = VaultDb::open_legacy_v2(&path).unwrap();
            let other = VaultDb::open_legacy_v2(&path).unwrap();
            let base = create(&gui);
            match field {
                "title" => {
                    other
                        .conn
                        .execute("UPDATE notes SET title='other' WHERE id='n'", [])
                        .unwrap();
                }
                "tags" => {
                    other.tag_atomic("n", &["other".into()], &[]).unwrap();
                }
                "pinned" => {
                    other
                        .conn
                        .execute("UPDATE notes SET pinned=1 WHERE id='n'", [])
                        .unwrap();
                }
                "trashed" => {
                    other.set_note_trashed("n", true).unwrap();
                }
                "deleted" => {
                    other.delete_note("n").unwrap();
                }
                _ => unreachable!(),
            }
            let before = other.get_note_full("n").unwrap();
            let error = gui
                .write_authoritative(&update(&gui, &base, "draft"), 20)
                .unwrap_err();
            assert_eq!(
                error.kind,
                match field {
                    "trashed" => WriteErrorKind::Trashed,
                    "deleted" => WriteErrorKind::Deleted,
                    _ => WriteErrorKind::Changed,
                }
            );
            assert_eq!(other.get_note_full("n").unwrap(), before);
        }
    }
    #[test]
    fn atomic_tags_preserve_gui_body_and_make_older_gui_metadata_stale() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.rn");
        let gui = VaultDb::open_legacy_v2(&path).unwrap();
        let cli = VaultDb::open_legacy_v2(&path).unwrap();
        let base = create(&gui);
        let acknowledged = gui
            .write_authoritative(&update(&gui, &base, "GUI body"), 20)
            .unwrap();
        let tagged = cli.tag_atomic("n", &["cli".into()], &["a".into()]).unwrap();
        assert_eq!(tagged["content"], "GUI body");
        assert_eq!(tagged["tags"], json!(["cli"]));
        assert_eq!(
            gui.write_authoritative(&update(&gui, &acknowledged, "new GUI body"), 20)
                .unwrap_err()
                .kind,
            WriteErrorKind::Changed
        );
        // Opposite order: tag first, then a GUI with an older full state.
        let fresh = gui.accepted_note("n").unwrap().unwrap();
        cli.tag_atomic("n", &["next".into()], &[]).unwrap();
        assert_eq!(
            gui.write_authoritative(&update(&gui, &fresh, "stale body"), 20)
                .unwrap_err()
                .kind,
            WriteErrorKind::Changed
        );
        assert_eq!(cli.get_note_content("n").unwrap().unwrap(), "GUI body");
    }
    #[test]
    fn creation_is_absent_only_and_tag_hashes_are_unambiguous_sets() {
        let db = VaultDb::open_in_memory().unwrap();
        let base = create(&db);
        assert_eq!(
            db.write_authoritative(
                &NoteWrite::from_note(&db, &note("overwrite"), WritePolicy::Create),
                20
            )
            .unwrap_err()
            .kind,
            WriteErrorKind::Changed
        );
        let first = db
            .tag_atomic("n", &["x\u{1f}y".into()], &["a".into()])
            .unwrap();
        let second = db
            .tag_atomic("n", &["x".into(), "y".into()], &["x\u{1f}y".into()])
            .unwrap();
        assert_ne!(first["accepted"]["token"], second["accepted"]["token"]);
        assert_eq!(first["tags"], json!(["x\u{1f}y"]));
        let mut state = base.clone();
        state["tags"] = json!(["b", "a", "a"]);
        let mut reordered = state.clone();
        reordered["tags"] = json!(["a", "b"]);
        assert_eq!(state_digest(&state), state_digest(&reordered));
    }
    #[test]
    fn different_or_replaced_vault_identity_is_a_typed_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.rn");
        let db = VaultDb::open_legacy_v2(&path).unwrap();
        let base = create(&db);
        let other = VaultDb::open_in_memory().unwrap();
        let mut wrong = update(&other, &base, "draft");
        wrong.vault = db.identity();
        assert_eq!(
            other.write_authoritative(&wrong, 20).unwrap_err().kind,
            WriteErrorKind::VaultReplaced
        );
        #[cfg(unix)]
        {
            // Preserve the still-open old inode, then install another DB at its path.
            std::fs::rename(&path, dir.path().join("old.rn")).unwrap();
            std::fs::write(&path, b"replacement marker").unwrap();
            assert_eq!(
                db.write_authoritative(&update(&db, &base, "draft"), 20)
                    .unwrap_err()
                    .kind,
                WriteErrorKind::VaultReplaced
            );
            assert_eq!(std::fs::read(&path).unwrap(), b"replacement marker");
        }
    }
    #[test]
    fn summaries_replace_only_proven_generated_state_and_pending_imports_are_idempotent() {
        let db = VaultDb::open_in_memory().unwrap();
        let first = note("generated one");
        let accepted = db
            .import_generated_summary(&first, &json!({"changed":["one"]}))
            .unwrap();
        let repeat = db
            .import_generated_summary(&first, &json!({"changed":["one"]}))
            .unwrap();
        assert_eq!(accepted, repeat);
        let second = note("generated two");
        let generated = db
            .import_generated_summary(&second, &json!({"changed":["two"]}))
            .unwrap();
        assert_eq!(generated["content"], "generated two");
        db.write_authoritative(&update(&db, &generated, "manual draft saved"), 20)
            .unwrap();
        let error = db
            .import_generated_summary(&note("incoming three"), &json!({"changed":["three"]}))
            .unwrap_err();
        let again = db
            .import_generated_summary(&note("incoming three"), &json!({"changed":["three"]}))
            .unwrap_err();
        assert_eq!(error.kind, WriteErrorKind::Changed);
        assert_eq!(error.artifact, again.artifact);
        assert_eq!(
            db.get_note_content("n").unwrap().unwrap(),
            "manual draft saved"
        );
        let pending: String = db
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key=?1",
                params![error.artifact.unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        assert!(pending.contains("Pending summary") && pending.contains("incoming three"));
        // Unknown legacy provenance is not inferred even from identical-looking text.
        let legacy = NewNote {
            id: "legacy".into(),
            ..note("generated-looking")
        };
        db.upsert_note(&legacy).unwrap();
        assert_eq!(
            db.import_generated_summary(&legacy, &json!({}))
                .unwrap_err()
                .kind,
            WriteErrorKind::Changed
        );
    }
    #[test]
    fn state_equality_explicitly_has_an_aba_limit() {
        let db = VaultDb::open_in_memory().unwrap();
        let base = create(&db);
        let changed = db
            .write_authoritative(&update(&db, &base, "other"), 20)
            .unwrap();
        let returned = db
            .write_authoritative(&update(&db, &changed, "base"), 20)
            .unwrap();
        assert_eq!(base["accepted"]["token"], returned["accepted"]["token"]);
    }
}
