//! Stage 4C: every official v3 mutation uses this transaction, including auxiliary
//! routes. Production opens/creation stay v2 until 4E; opt-in v3 is complete, not
//! a mixture of guarded editor writes and unchecked import/lifecycle writes.
use super::writer::{WriteError, WriteErrorKind};
use super::{
    ancestry::{self, FullState, Identity, Ledger},
    db::VaultDb,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    ops::Deref,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}
pub(crate) fn hash(value: &impl Serialize) -> Result<String, String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).map_err(error)?)
    ))
}
pub(crate) fn reserved_metadata(key: &str) -> bool {
    key == "schema_version"
        || [
            "revision_",
            "generated_summary:",
            "pending_summary:",
            "attachment_stage:",
            "recovery_fork:",
            "review_apply:",
            "recovery_assets:",
            "portable_",
        ]
        .iter()
        .any(|prefix| key.starts_with(*prefix))
}
pub(crate) struct Session {
    pub identity: Identity,
    pub ledger_root: PathBuf,
    pub guard: Arc<AtomicBool>,
    pub instance: String,
    _pin_lease: std::fs::File,
    operation: RefCell<Option<ScopedOperation>>,
}
struct ScopedOperation {
    id: String,
    request_hash: String,
    next: std::cell::Cell<u64>,
}
struct ResetOperation<'a>(&'a RefCell<Option<ScopedOperation>>);
impl Drop for ResetOperation<'_> {
    fn drop(&mut self) {
        drop(self.0.borrow_mut().take());
    }
}

impl VaultDb {
    pub fn is_v3(&self) -> bool {
        self.revision_session.is_some()
    }

    /// Explicit tooling/fixture entry point. Neither GUI startup nor CLI vault
    /// selection activates this. All ordinary opens still reject v3 until 4E.
    #[cfg(any(test, feature = "revision-ancestry"))]
    pub fn open_v3(path: &Path, ledger_root: &Path) -> Result<Self, String> {
        if ancestry::inspect_version(path)? != 3 {
            return Err("Expected schema 3".into());
        }
        let maintenance = ancestry::Maintenance::shared(path)?;
        let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(error)?;
        conn.pragma_update(None, "foreign_keys", true)
            .map_err(error)?;
        conn.pragma_update(None, "synchronous", "FULL")
            .map_err(error)?;
        if ancestry::schema_version(&conn)? != 3 {
            return Err("Schema changed while opening".into());
        }
        {
            let snapshot = conn.unchecked_transaction().map_err(error)?;
            ancestry::validate(&snapshot)?;
            snapshot.commit().map_err(error)?;
        }
        let identity = ancestry::identity(&conn)?;
        let ledger = Ledger::acquire(ledger_root, &identity.vault_uuid)?;
        ledger.verify_session(path, &identity)?;
        drop(ledger);
        let guard = Arc::new(AtomicBool::new(false));
        let flag = guard.clone();
        conn.create_scalar_function(
            "rn_revision_write_allowed",
            0,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8,
            move |_| Ok(flag.load(Ordering::Acquire)),
        )
        .map_err(error)?;
        // Connection-local fail-closed backstop for a forgotten official route.
        // These are TEMP triggers, not a format migration or security against SQL
        // clients that remove triggers. They do not travel into backups/exports.
        for table in [
            "notes",
            "note_tags",
            "note_attachments",
            "attachments",
            "tags",
            "versions",
            "session_links",
            "meta",
            "vault_identity",
            "note_revision_counters",
            "note_incarnations",
            "revisions",
            "revision_states",
            "revision_parents",
            "note_revision_heads",
            "revision_attachments",
            "revision_pins",
            "revision_pruning",
        ] {
            for verb in ["INSERT", "UPDATE", "DELETE"] {
                conn.execute_batch(&format!("CREATE TEMP TRIGGER rn_guard_{table}_{verb} BEFORE {verb} ON main.{table} WHEN rn_revision_write_allowed()=0 BEGIN SELECT RAISE(ABORT,'v3 mutation requires the revision transaction helper'); END;")).map_err(error)?;
            }
        }
        let writer_identity = super::writer::VaultIdentity::capture(&conn)?;
        let instance = uuid();
        let pin_lease = super::revision_retention::session_lease(ledger_root, &instance)?;
        Ok(Self {
            conn,
            max_attachment_bytes: super::db::DEFAULT_MAX_ATTACHMENT_BYTES,
            writer_identity,
            _maintenance: Some(maintenance),
            revision_session: Some(Session {
                identity,
                ledger_root: ledger_root.into(),
                guard,
                instance,
                _pin_lease: pin_lease,
                operation: RefCell::new(None),
            }),
        })
    }

    /// Stable request identity for any official route, including multi-item
    /// imports. A scope is per connection/intent, never a process-global target.
    pub fn with_write_operation<T>(
        &self,
        id: &str,
        request: &Value,
        action: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let Some(session) = &self.revision_session else {
            return action();
        };
        if id.is_empty() || session.operation.borrow().is_some() {
            return Err("Invalid/nested operation scope".into());
        }
        *session.operation.borrow_mut() = Some(ScopedOperation {
            id: id.into(),
            request_hash: hash(request)?,
            next: std::cell::Cell::new(0),
        });
        let _reset = ResetOperation(&session.operation);
        let result = action();
        let empty = session
            .operation
            .borrow()
            .as_ref()
            .is_some_and(|scope| scope.next.get() == 0);
        if result.is_ok() && empty {
            // An acknowledged empty batch still consumes its identity. A later
            // retry must not discover new input and turn the old no-op into writes.
            self.begin_write("gui", None, &json!({"emptyOperation":true}))?
                .commit()?;
        }
        result
    }

    pub(crate) fn validate_revision_session(&self) -> Result<(), WriteError> {
        if let Some(session) = &self.revision_session {
            if ancestry::identity(&self.conn)? != session.identity {
                return Err(WriteError::conflict(
                    WriteErrorKind::VaultReplaced,
                    "The vault restore generation changed.",
                    None,
                ));
            }
        }
        Ok(())
    }

    pub fn begin_write(
        &self,
        source: &str,
        operation_id: Option<&str>,
        request: &Value,
    ) -> Result<WriteTransaction<'_>, String> {
        self.begin_write_behavior(
            source,
            operation_id,
            request,
            TransactionBehavior::Immediate,
            false,
            None,
        )
    }
    pub(crate) fn begin_auxiliary_write(
        &self,
        source: &str,
        request: &Value,
    ) -> Result<WriteTransaction<'_>, String> {
        self.begin_write_behavior(
            source,
            None,
            request,
            TransactionBehavior::Deferred,
            false,
            None,
        )
    }
    pub(crate) fn begin_revision_maintenance(
        &self,
        request: &Value,
    ) -> Result<WriteTransaction<'_>, String> {
        self.begin_write_behavior(
            "recovery",
            None,
            request,
            TransactionBehavior::Immediate,
            true,
            None,
        )
    }
    pub(crate) fn begin_portable_reconcile(
        &self,
        request: &Value,
        floor: i64,
    ) -> Result<WriteTransaction<'_>, String> {
        if floor < 0 {
            return Err("Invalid portable counter floor".into());
        }
        self.begin_write_behavior(
            "recovery",
            None,
            request,
            TransactionBehavior::Immediate,
            true,
            Some(floor),
        )
    }
    #[allow(clippy::too_many_arguments)] // Shared authoring/maintenance lock-order boundary.
    fn begin_write_behavior(
        &self,
        source: &str,
        operation_id: Option<&str>,
        request: &Value,
        legacy_behavior: TransactionBehavior,
        maintenance_only: bool,
        portable_floor: Option<i64>,
    ) -> Result<WriteTransaction<'_>, String> {
        let mut request_hash = if self.is_v3() {
            hash(&(source, request))?
        } else {
            String::new()
        };
        let mut operation = operation_id.map(str::to_owned).unwrap_or_else(uuid);
        let mut root_claim = None;
        if let Some(session) = &self.revision_session {
            if let Some(scope) = session
                .operation
                .borrow()
                .as_ref()
                .filter(|_| !maintenance_only)
            {
                let index = scope.next.get();
                scope
                    .next
                    .set(index.checked_add(1).ok_or("Operation sequence overflow")?);
                operation = format!("{}:{index}", scope.id);
                request_hash = hash(&(&scope.request_hash, source, request))?;
                root_claim = Some((
                    format!(
                        "revision_operation_root:v3:{}:{}",
                        session.identity.storage_epoch, scope.id
                    ),
                    scope.request_hash.clone(),
                ));
            }
        }
        if operation.is_empty()
            || !["gui", "cli", "task", "import", "session_import", "recovery"].contains(&source)
        {
            return Err("Invalid writer source or operation identity".into());
        }
        let mut ledger = None;
        let mut number = 0;
        if let Some(session) = &self.revision_session {
            self.validate_writer_vault(&self.identity())
                .map_err(error)?;
            let mut locked = Ledger::acquire(&session.ledger_root, &session.identity.vault_uuid)?;
            locked.verify_session(Path::new(&self.writer_identity.path), &session.identity)?;
            let high: i64 = self
                .conn
                .query_row(
                    "SELECT COALESCE(MAX(high_water),0) FROM note_revision_counters",
                    [],
                    |r| r.get(0),
                )
                .map_err(error)?;
            // A single globally reserved ticket is valid for every affected note.
            // This permits imports with not-yet-allocated IDs without taking a
            // ledger lock or acknowledging a reservation inside BEGIN IMMEDIATE.
            // Per-note revisions stay strictly increasing; unused tickets are gaps.
            number = locked.reserve_transaction(high.max(portable_floor.unwrap_or(0)))?;
            ledger = Some(locked);
        }
        let behavior = if self.is_v3() {
            TransactionBehavior::Immediate
        } else {
            legacy_behavior
        };
        let tx = Transaction::new_unchecked(&self.conn, behavior).map_err(error)?;
        if self.is_v3() {
            self.validate_writer_vault(&self.identity())
                .map_err(error)?;
        }
        let mut before = BTreeMap::new();
        let mut replay = None;
        if let Some(session) = &self.revision_session {
            if session.guard.swap(true, Ordering::AcqRel) {
                return Err("Nested revision transaction".into());
            }
            // The RAII wrapper owns flag cleanup from this point, including errors.
        }
        let result = WriteTransaction {
            tx: Some(tx),
            db: self,
            _ledger: ledger,
            number,
            operation,
            source: source.into(),
            request_hash,
            before: RefCell::new(BTreeMap::new()),
            prepared: RefCell::new(None),
            items: RefCell::new(BTreeMap::new()),
            replay: RefCell::new(None),
            draft_bases: RefCell::new(BTreeMap::new()),
            kind_hint: RefCell::new(None),
            root_claim,
            maintenance_only: std::cell::Cell::new(maintenance_only),
        };
        if self.is_v3() {
            ancestry::validate(&result)?;
            if let Some((key, expected)) = &result.root_claim {
                let prior: Option<String> = result
                    .query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
                    .optional()
                    .map_err(error)?;
                if prior.as_ref().is_some_and(|prior| prior != expected) {
                    return Err("Root operation identity was reused for different input".into());
                }
            }
            before = ancestry::states(&result)?;
            let prior: Option<String> = result
                .query_row(
                    "SELECT value FROM meta WHERE key=?1",
                    [result.operation_key()],
                    |r| r.get(0),
                )
                .optional()
                .map_err(error)?;
            if let Some(prior) = prior {
                let stored: OperationRecord = serde_json::from_str(&prior).map_err(error)?;
                if stored.source != result.source || stored.request_hash != result.request_hash {
                    return Err(
                        "Operation identity was reused for different input or policy".into(),
                    );
                }
                replay = Some(stored);
            }
        }
        *result.before.borrow_mut() = before;
        *result.replay.borrow_mut() = replay;
        Ok(result)
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct OperationRecord {
    source: String,
    request_hash: String,
    items: BTreeMap<String, String>,
}

pub struct WriteTransaction<'a> {
    tx: Option<Transaction<'a>>,
    db: &'a VaultDb,
    _ledger: Option<Ledger>,
    number: i64,
    operation: String,
    source: String,
    request_hash: String,
    before: RefCell<BTreeMap<String, FullState>>,
    prepared: RefCell<Option<BTreeMap<String, FullState>>>,
    items: RefCell<BTreeMap<String, String>>,
    replay: RefCell<Option<OperationRecord>>,
    draft_bases: RefCell<BTreeMap<String, String>>,
    kind_hint: RefCell<Option<String>>,
    root_claim: Option<(String, String)>,
    maintenance_only: std::cell::Cell<bool>,
}
impl Deref for WriteTransaction<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.tx.as_ref().expect("live transaction").deref()
    }
}
impl Drop for WriteTransaction<'_> {
    fn drop(&mut self) {
        // Rollback/close the SQL transaction before releasing the external ledger.
        drop(self.tx.take());
        if let Some(session) = &self.db.revision_session {
            session.guard.store(false, Ordering::Release);
        }
    }
}
impl WriteTransaction<'_> {
    fn operation_key(&self) -> String {
        let epoch = self
            .db
            .revision_session
            .as_ref()
            .map(|s| s.identity.storage_epoch.as_str())
            .unwrap_or("");
        format!("revision_operation:v3:{epoch}:{}", self.operation)
    }
    pub(crate) fn merge_base(&self, note: &str, revision: &str) {
        self.draft_bases
            .borrow_mut()
            .insert(note.into(), revision.into());
        *self.kind_hint.borrow_mut() = Some("merge".into());
    }
    pub fn is_replay(&self) -> bool {
        self.replay.borrow().is_some()
    }
    pub fn replay_item_count(&self) -> usize {
        self.replay.borrow().as_ref().map_or(0, |r| r.items.len())
    }
    pub fn replay_any_receipt(&self) -> Result<Option<Value>, String> {
        let stored = self.replay.borrow();
        let Some(stored) = stored.as_ref() else {
            return Ok(None);
        };
        let (id, revision) = stored
            .items
            .iter()
            .next()
            .ok_or("This operation had no note receipt")?;
        Ok(Some(receipt_for_revision(self.db, id, revision)?))
    }
    pub fn replay_receipt(&self, id: &str) -> Result<Option<Value>, String> {
        let stored = self.replay.borrow();
        let Some(stored) = stored.as_ref() else {
            return Ok(None);
        };
        let revision = stored
            .items
            .get(id)
            .ok_or("This operation did not return that note")?;
        Ok(Some(receipt_for_revision(self.db, id, revision)?))
    }
    pub fn receipt(&self, id: &str) -> Result<Value, String> {
        self.receipt_typed(id).map_err(error)
    }
    pub fn receipt_typed(&self, id: &str) -> Result<Value, WriteError> {
        self.db.validate_writer_vault(&self.db.identity())?;
        if let Some(receipt) = self.replay_receipt(id)? {
            return Ok(receipt);
        }
        self.prepare()?;
        let receipt = self
            .db
            .accepted_note(id)?
            .ok_or("Committed note disappeared")?;
        if self.db.is_v3() {
            let revision = receipt["accepted"]["revision"]["revisionId"]
                .as_str()
                .ok_or("Missing revision acknowledgment")?;
            self.items.borrow_mut().insert(id.into(), revision.into());
        }
        Ok(receipt)
    }
    fn prepare(&self) -> Result<(), String> {
        if !self.db.is_v3() {
            return Ok(());
        }
        let after = ancestry::states(self)?;
        if self.maintenance_only.get() && after != *self.before.borrow() {
            return Err("Recovery maintenance cannot mutate semantic note state".into());
        }
        if let Some(prepared) = self.prepared.borrow().as_ref() {
            if prepared != &after {
                return Err("Semantic state changed after preparing the exact receipt".into());
            }
            return Ok(());
        }
        if self.replay.borrow().is_some() {
            if after != *self.before.borrow() {
                return Err("A retried operation attempted a new mutation".into());
            }
            *self.prepared.borrow_mut() = Some(after);
            return Ok(());
        }
        let before = self.before.borrow();
        let ids: BTreeSet<_> = before.keys().chain(after.keys()).cloned().collect();
        for id in ids {
            let old = before.get(&id);
            let new = after.get(&id);
            if let (Some(old), Some(new)) = (old, new) {
                if new.created != old.created {
                    return Err("A note's creation identity is immutable".into());
                }
                let mut semantic = new.clone();
                semantic.modified = old.modified.clone();
                if &semantic == old {
                    if new.modified != old.modified {
                        self.execute(
                            "UPDATE notes SET modified=?2 WHERE id=?1",
                            params![id, old.modified],
                        )
                        .map_err(error)?;
                    }
                    continue;
                }
            }
            let mut state = match (old, new) {
                (_, Some(new)) => new.clone(),
                (Some(old), None) => {
                    let mut deleted = old.clone();
                    deleted.lifecycle = "deleted".into();
                    deleted.modified = chrono::Utc::now().to_rfc3339();
                    deleted
                }
                _ => continue,
            };
            if old.is_some() && new.is_some() && self.source != "import" {
                state.modified = chrono::Utc::now().to_rfc3339();
                self.execute(
                    "UPDATE notes SET modified=?2 WHERE id=?1",
                    params![id, state.modified],
                )
                .map_err(error)?;
            }
            let revision = self.record(&id, old, &state)?;
            if new.is_some() {
                let assets: Vec<String> =
                    serde_json::from_value(state.extra["attachments"].clone()).map_err(error)?;
                for asset in assets {
                    self.execute(
                        "DELETE FROM meta WHERE key=?1",
                        [format!("attachment_stage:v3:{id}:{asset}")],
                    )
                    .map_err(error)?;
                }
            }
            self.items.borrow_mut().insert(id, revision);
        }
        *self.prepared.borrow_mut() = Some(ancestry::states(self)?);
        ancestry::validate(self)?;
        Ok(())
    }
    fn record(
        &self,
        id: &str,
        old: Option<&FullState>,
        state: &FullState,
    ) -> Result<String, String> {
        let session = self
            .db
            .revision_session
            .as_ref()
            .ok_or("Missing v3 session")?;
        let previous: Option<(String, String)> = self
            .query_row(
                "SELECT incarnation_id,revision_id FROM note_revision_heads WHERE note_id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(error)?;
        let incarnation = if old.is_some() {
            previous.as_ref().ok_or("Missing predecessor")?.0.clone()
        } else {
            uuid()
        };
        let now = chrono::Utc::now().to_rfc3339();
        let mut state = state.clone();
        if old.is_none() {
            if let Some((previous_incarnation, deleted_revision)) = &previous {
                state.extra["incarnationOrigin"] = json!({"previousIncarnation":previous_incarnation,"deletedRevision":deleted_revision});
            }
            self.execute(
                "INSERT INTO note_revision_counters VALUES(?1,0) ON CONFLICT(note_id) DO NOTHING",
                [id],
            )
            .map_err(error)?;
            self.execute(
                "INSERT INTO note_incarnations VALUES(?1,?2,?3,NULL)",
                params![incarnation, id, now],
            )
            .map_err(error)?;
        }
        let kind = if old.is_none() {
            "create"
        } else if state.lifecycle == "deleted" {
            "delete"
        } else if old.is_some_and(|s| s.lifecycle != state.lifecycle) {
            if state.lifecycle == "trashed" {
                "trash"
            } else {
                "untrash"
            }
        } else if self.kind_hint.borrow().as_deref() == Some("merge") {
            "merge"
        } else if self.source == "import" || self.source == "session_import" {
            "import"
        } else if old.is_some_and(|s| s.body == state.body) {
            "metadata"
        } else {
            "update"
        };
        let revision = uuid();
        self.execute(
            "INSERT INTO revisions VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,'known',?13)",
            params![
                revision,
                incarnation,
                self.number,
                session.identity.storage_epoch,
                uuid(),
                state.hash()?,
                self.source,
                session.instance,
                self.operation,
                id,
                self.request_hash,
                kind,
                now
            ],
        )
        .map_err(error)?;
        ancestry::insert_state(self, &revision, &state)?;
        if old.is_some() {
            let parent = &previous.as_ref().ok_or("Missing predecessor")?.1;
            self.execute(
                "INSERT INTO revision_parents VALUES(?1,?2,'predecessor',0)",
                params![revision, parent],
            )
            .map_err(error)?;
        }
        if let Some(base) = self.draft_bases.borrow().get(id) {
            let belongs:bool=self.query_row("SELECT EXISTS(SELECT 1 FROM revisions WHERE revision_id=?1 AND incarnation_id=?2)",params![base,incarnation],|r|r.get(0)).map_err(error)?;
            if !belongs {
                return Err("Unverified committed merge base".into());
            }
            self.execute(
                "INSERT INTO revision_parents VALUES(?1,?2,'draft_base',0)",
                params![revision, base],
            )
            .map_err(error)?;
        }
        let assets: Vec<String> =
            serde_json::from_value(state.extra["attachments"].clone()).map_err(error)?;
        for asset in assets {
            self.execute(
                "INSERT INTO revision_attachments VALUES(?1,?2)",
                params![revision, asset],
            )
            .map_err(error)?;
        }
        self.execute("INSERT INTO note_revision_heads VALUES(?1,?2,?3) ON CONFLICT(note_id) DO UPDATE SET incarnation_id=excluded.incarnation_id,revision_id=excluded.revision_id",params![id,incarnation,revision]).map_err(error)?;
        self.execute(
            "UPDATE note_revision_counters SET high_water=?2 WHERE note_id=?1",
            params![id, self.number],
        )
        .map_err(error)?;
        if state.lifecycle == "deleted" {
            self.execute(
                "UPDATE note_incarnations SET ended_at=?2 WHERE incarnation_id=?1",
                params![incarnation, now],
            )
            .map_err(error)?;
        }
        Ok(revision)
    }
    pub fn commit(self) -> Result<(), String> {
        self.commit_typed().map_err(error)
    }
    pub fn commit_typed(mut self) -> Result<(), WriteError> {
        if self.db.is_v3() {
            self.db.validate_writer_vault(&self.db.identity())?;
        }
        self.prepare()?;
        if self.db.is_v3() && self.replay.borrow().is_none() && !self.maintenance_only.get() {
            if let Some((key, expected)) = &self.root_claim {
                self.execute(
                    "INSERT OR IGNORE INTO meta(key,value) VALUES(?1,?2)",
                    params![key, expected],
                )
                .map_err(error)?;
            }
            let record = OperationRecord {
                source: self.source.clone(),
                request_hash: self.request_hash.clone(),
                items: self.items.borrow().clone(),
            };
            self.execute(
                "INSERT INTO meta(key,value) VALUES(?1,?2)",
                params![
                    self.operation_key(),
                    serde_json::to_string(&record).map_err(error)?
                ],
            )
            .map_err(error)?;
        }
        if self.db.is_v3() {
            self.db.validate_writer_vault(&self.db.identity())?;
        }
        self.tx
            .take()
            .ok_or("Missing transaction")?
            .commit()
            .map_err(|e| WriteError::from(e.to_string()))
    }
}

pub(crate) fn revision_identity(conn: &Connection, id: &str) -> Result<Option<Value>, String> {
    conn.query_row("SELECT h.incarnation_id,h.revision_id,r.revision_no,r.commit_token,r.state_sha256,r.storage_epoch FROM note_revision_heads h JOIN revisions r USING(revision_id) WHERE h.note_id=?1",[id],|r|Ok(json!({"incarnationId":r.get::<_,String>(0)?,"revisionId":r.get::<_,String>(1)?,"revision":r.get::<_,i64>(2)?.to_string(),"commitToken":r.get::<_,String>(3)?,"stateSha256":r.get::<_,String>(4)?,"storageEpoch":r.get::<_,String>(5)?}))).optional().map_err(error)
}
pub(crate) fn accepted_for_revision(db: &VaultDb, revision: &Value) -> Result<Value, String> {
    let identity = &db
        .revision_session
        .as_ref()
        .ok_or("Missing revision session")?
        .identity;
    let epoch = revision["storageEpoch"]
        .as_str()
        .ok_or("Missing committed storage epoch")?;
    let token = format!(
        "revision-v3.{}.{}.{}.{}.{}.{}",
        identity.vault_uuid,
        epoch,
        revision["incarnationId"]
            .as_str()
            .ok_or("Missing incarnation")?,
        revision["revisionId"].as_str().ok_or("Missing revision")?,
        revision["commitToken"]
            .as_str()
            .ok_or("Missing commit token")?,
        revision["stateSha256"]
            .as_str()
            .ok_or("Missing state digest")?
    );
    Ok(
        json!({"vault":db.identity(),"token":token,"revision":revision,"vaultUuid":identity.vault_uuid,"storageEpoch":epoch}),
    )
}
pub(crate) fn receipt_for_revision(
    db: &VaultDb,
    id: &str,
    revision_id: &str,
) -> Result<Value, String> {
    let state = ancestry::payload(&db.conn, revision_id).map_err(|_| {
        "Historical operation payload unavailable; never retry as a fresh write".to_string()
    })?;
    if state.lifecycle == "deleted" {
        return Err("The operation committed a deletion, not a live note receipt".into());
    }
    let revision:Value=db.conn.query_row("SELECT incarnation_id,revision_no,commit_token,state_sha256,storage_epoch FROM revisions WHERE revision_id=?1 AND operation_item=?2",params![revision_id,id],|r|Ok(json!({"incarnationId":r.get::<_,String>(0)?,"revisionId":revision_id,"revision":r.get::<_,i64>(1)?.to_string(),"commitToken":r.get::<_,String>(2)?,"stateSha256":r.get::<_,String>(3)?,"storageEpoch":r.get::<_,String>(4)?}))).map_err(error)?;
    Ok(
        json!({"id":id,"title":state.title,"content":String::from_utf8(state.body).map_err(error)?,"tags":state.tags,"pinned":state.pinned,"trashed":state.lifecycle=="trashed","created":state.created,"modified":state.modified,"accepted":accepted_for_revision(db,&revision)?}),
    )
}

pub(crate) fn verify_snapshot(db: &VaultDb, id: &str, accepted: &Value) -> bool {
    let Some(token) = accepted["token"].as_str() else {
        return false;
    };
    let parts: Vec<_> = token.split('.').collect();
    if parts.len() != 7 || parts[0] != "revision-v3" {
        return false;
    }
    let Some(session) = &db.revision_session else {
        return false;
    };
    if parts[1] != session.identity.vault_uuid || parts[2] != session.identity.storage_epoch {
        return false;
    }
    let Ok(receipt) = receipt_for_revision(db, id, parts[4]) else {
        return false;
    };
    receipt["accepted"]["token"] == token
        && receipt["content"] == accepted["body"]
        && receipt["title"] == accepted["title"]
        && receipt["tags"] == accepted["tags"]
        && receipt["pinned"] == accepted["pinned"]
        && receipt["created"] == accepted["created"]
        && receipt["modified"] == accepted["modified"]
        && receipt["trashed"] == accepted["trashed"]
}
impl VaultDb {
    pub(crate) fn empty_trash_v3(&self) -> Result<u32, String> {
        let tx = self.begin_write("gui", None, &json!(["empty_trash"]))?;
        if tx.is_replay() {
            return Ok(tx.replay_item_count() as u32);
        }
        let ids = {
            // Route through Deref explicitly: WriteTransaction has its own
            // zero-argument prepare() (the semantic revision builder), which
            // shadows rusqlite's Connection::prepare for SQL statements.
            let mut statement =
                Connection::prepare(&tx, "SELECT id FROM notes WHERE trashed=1 ORDER BY id")
                    .map_err(error)?;
            let rows = statement
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(error)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(error)?
        };
        for id in &ids {
            tx.execute(
                "DELETE FROM notes_fts WHERE rowid=(SELECT rowid FROM notes WHERE id=?1)",
                [id],
            )
            .map_err(error)?;
            tx.execute("DELETE FROM notes WHERE id=?1 AND trashed=1", [id])
                .map_err(error)?;
        }
        let mut layout = super::db_notebooks::Layout::load(&tx)?;
        for id in &ids {
            layout.notes.remove(id);
        }
        layout.save(&tx)?;
        tx.commit()?;
        Ok(ids.len() as u32)
    }
}

pub(crate) fn read_accepted(db: &VaultDb, id: &str) -> Result<Option<Value>, WriteError> {
    if !db.conn.is_autocommit() {
        let result = read_accepted_core(db, id)?;
        if let Some(note) = &result {
            super::revision_retention::pin_read_on(db, note)?;
        }
        return Ok(result);
    }
    {
        let read = db.conn.unchecked_transaction().map_err(error)?;
        let result = read_accepted_core(db, id)?;
        let held = result
            .as_ref()
            .map(|note| super::revision_retention::read_is_pinned(db, note))
            .transpose()?
            .unwrap_or(true);
        read.commit().map_err(error)?;
        if held {
            return Ok(result);
        }
    }
    let tx = db.begin_revision_maintenance(&json!(["accepted-read", id]))?;
    let result = read_accepted_core(db, id)?;
    if let Some(note) = &result {
        super::revision_retention::pin_read_on(db, note)?;
    }
    tx.commit_typed()?;
    Ok(result)
}
fn read_accepted_core(db: &VaultDb, id: &str) -> Result<Option<Value>, WriteError> {
    let Some(mut note) = db.get_note_full(id)? else {
        return Ok(None);
    };
    let revision = revision_identity(&db.conn, id)?.ok_or("Missing durable revision head")?;
    let stored = ancestry::payload(
        &db.conn,
        revision["revisionId"]
            .as_str()
            .ok_or("Missing revision ID")?,
    )?;
    let actual = ancestry::states(&db.conn)?
        .remove(id)
        .ok_or("Missing note projection")?;
    if actual != stored {
        return Err(WriteError::conflict(
            WriteErrorKind::Changed,
            "Stored note changed outside the revision writer contract.",
            None,
        ));
    }
    note["accepted"] = accepted_for_revision(db, &revision)?;
    db.validate_writer_vault(&db.identity())?;
    Ok(Some(note))
}
pub(crate) fn check_policy(
    db: &VaultDb,
    id: &str,
    policy: &super::writer::WritePolicy,
) -> Result<(), WriteError> {
    use super::writer::WritePolicy;
    let current = db.accepted_note(id)?;
    let WritePolicy::Update { expected_token } = policy else {
        return if current.is_some() {
            Err(WriteError::conflict(
                WriteErrorKind::Changed,
                "Create refused: note already exists.",
                current,
            ))
        } else {
            Ok(())
        };
    };
    let parts: Vec<_> = expected_token.split('.').collect();
    if parts.len() != 7
        || parts[0] != "revision-v3"
        || parts[1..6]
            .iter()
            .any(|p| uuid::Uuid::parse_str(p).is_err())
        || parts[6].len() != 64
        || !parts[6].bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(WriteError::conflict(WriteErrorKind::InvalidPolicy,"Schema 3 requires an accepted v3 revision token; body-digest tokens are not authority.",None));
    }
    let identity = &db
        .revision_session
        .as_ref()
        .ok_or("Missing v3 identity")?
        .identity;
    if parts[1] != identity.vault_uuid || parts[2] != identity.storage_epoch {
        return Err(WriteError::conflict(
            WriteErrorKind::VaultReplaced,
            "The token belongs to another vault or restore epoch.",
            current,
        ));
    }
    let Some(note) = current else {
        return Err(WriteError::conflict(
            WriteErrorKind::Deleted,
            "The note was deleted; the draft was not written.",
            None,
        ));
    };
    if note["accepted"]["revision"]["incarnationId"].as_str() != Some(parts[3]) {
        return Err(WriteError::conflict(
            WriteErrorKind::Deleted,
            "That note incarnation was deleted and recreated.",
            Some(note),
        ));
    }
    if note["trashed"] == true {
        return Err(WriteError::conflict(
            WriteErrorKind::Trashed,
            "The note is in trash.",
            Some(note),
        ));
    }
    if note["accepted"]["token"].as_str() != Some(expected_token.as_str()) {
        return Err(WriteError::conflict(
            WriteErrorKind::Changed,
            "The accepted revision changed; the draft was not written.",
            Some(note),
        ));
    }
    Ok(())
}

pub(crate) fn import_summary(
    db: &VaultDb,
    note: &super::db::NewNote,
    input: &Value,
    operation: Option<&str>,
) -> Result<Value, WriteError> {
    let tx = db.begin_write(
        "session_import",
        operation,
        &json!([
            input,
            note.id,
            note.title,
            note.content,
            note.tags,
            note.pinned
        ]),
    )?;
    if let Some(receipt) = tx.replay_receipt(&note.id)? {
        return Ok(receipt);
    }
    let current = db.accepted_note(&note.id)?;
    let generated_key = format!("generated_summary:v3:{}", hash(&note.id)?);
    let generated: Option<String> = tx
        .query_row(
            "SELECT value FROM meta WHERE key=?1",
            [&generated_key],
            |r| r.get(0),
        )
        .optional()
        .map_err(error)?;
    let head = revision_identity(&tx, &note.id)?;
    let manually_changed = match &current {
        Some(current) => {
            current["trashed"] == true
                || generated.as_deref() != current["accepted"]["revision"]["revisionId"].as_str()
        }
        None => head.is_some(), // Explicit deletion is not permission to regenerate.
    };
    if manually_changed {
        let artifact = format!(
            "pending_summary:v1:{:x}:{}",
            Sha256::digest(note.id.as_bytes()),
            hash(&(input, &head))?
        );
        let pending = json!({"label":format!("Pending summary: {}",note.title),"targetId":note.id,
            "incoming":{"title":note.title,"content":note.content,"tags":note.tags,"pinned":note.pinned},"input":input,
            "reason":"The generated revision is no longer current; manual edits/lifecycle were preserved."});
        tx.execute(
            "INSERT OR IGNORE INTO meta(key,value) VALUES(?1,?2)",
            params![artifact, pending.to_string()],
        )
        .map_err(error)?;
        tx.commit_typed()?;
        let kind = if current.as_ref().is_some_and(|n| n["trashed"] == true) {
            WriteErrorKind::Trashed
        } else if current.is_none() {
            WriteErrorKind::Deleted
        } else {
            WriteErrorKind::Changed
        };
        let mut conflict=WriteError::conflict(kind,"Summary import conflicts with saved edits; incoming summary preserved as a labeled pending artifact.",current);
        conflict.artifact = Some(artifact);
        return Err(conflict);
    }
    let incoming = json!({"id":note.id,"title":note.title,"content":note.content,"tags":note.tags,"pinned":note.pinned,"trashed":false});
    if current
        .as_ref()
        .is_none_or(|n| super::writer::state_digest(n) != super::writer::state_digest(&incoming))
    {
        super::writer::write_import_on(
            db,
            &tx,
            note,
            if current.is_some() {
                super::writer::ImportPolicy::Replace
            } else {
                super::writer::ImportPolicy::Copy
            },
        )?;
    }
    let receipt = tx.receipt_typed(&note.id)?;
    // This is an internal reference to the generated commit, not a semantic
    // field hashed into its own state (which would create a self-reference).
    tx.execute("INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![generated_key,receipt["accepted"]["revision"]["revisionId"].as_str().ok_or("Missing generated revision")?]).map_err(error)?;
    tx.commit_typed()?;
    Ok(receipt)
}
impl VaultDb {
    /// CLI scripted updates retain the metadata from the supplied accepted token,
    /// including on retry after a later writer committed. Never reconstruct a
    /// retry's request from a racing writer's current metadata.
    pub fn accepted_at_token(&self, id: &str, token: &str) -> Result<Value, WriteError> {
        self.validate_writer_vault(&self.identity())?;
        let parts: Vec<_> = token.split('.').collect();
        if !self.is_v3() || parts.len() != 7 || parts[0] != "revision-v3" {
            return Err(WriteError::conflict(
                WriteErrorKind::InvalidPolicy,
                "Expected a v3 accepted token.",
                None,
            ));
        }
        let identity = &self
            .revision_session
            .as_ref()
            .ok_or("Missing v3 session")?
            .identity;
        if parts[1] != identity.vault_uuid || parts[2] != identity.storage_epoch {
            return Err(WriteError::conflict(
                WriteErrorKind::VaultReplaced,
                "Token belongs to a prior restore epoch or vault.",
                None,
            ));
        }
        let receipt = receipt_for_revision(self, id, parts[4])?;
        if receipt["accepted"]["token"] != token {
            return Err(WriteError::conflict(
                WriteErrorKind::InvalidPolicy,
                "Token does not match its immutable revision.",
                None,
            ));
        }
        Ok(receipt)
    }
}

impl VaultDb {
    pub fn set_note_trashed_as_cli(
        &self,
        id: &str,
        trashed: bool,
        operation: Option<&str>,
    ) -> Result<(), String> {
        if !self.is_v3() {
            return self.set_note_trashed(id, trashed);
        }
        let tx = self.begin_write("cli", operation, &json!([id, trashed]))?;
        if tx.is_replay() {
            return tx.commit();
        }
        if tx
            .execute(
                "UPDATE notes SET trashed=?2 WHERE id=?1",
                params![id, trashed],
            )
            .map_err(error)?
            == 0
        {
            return Err(format!("No note with id {id}"));
        }
        tx.commit()
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        db::NewNote,
        writer::{NoteWrite, WritePolicy},
    };
    use super::*;
    struct Fixture {
        _dir: tempfile::TempDir,
        path: PathBuf,
        ledger: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("test.rn");
            let ledger = dir.path().join("ledger");
            ancestry::migrate(&path, &ledger).unwrap();
            Self {
                _dir: dir,
                path,
                ledger,
            }
        }
        fn open(&self) -> VaultDb {
            VaultDb::open_v3(&self.path, &self.ledger).unwrap()
        }
    }
    fn note(id: &str, body: &str) -> NewNote {
        NewNote {
            id: id.into(),
            title: "Title".into(),
            content: body.into(),
            pinned: false,
            tags: vec![],
            created: "created".into(),
            modified: "modified".into(),
        }
    }
    fn create(db: &VaultDb, id: &str, body: &str) -> Value {
        db.write_authoritative(
            &NoteWrite::from_note(db, &note(id, body), WritePolicy::Create),
            0,
        )
        .unwrap()
    }
    fn update(db: &VaultDb, base: &Value, body: &str) -> NoteWrite {
        let mut value = note(base["id"].as_str().unwrap(), body);
        value.title = base["title"].as_str().unwrap().into();
        value.tags = serde_json::from_value(base["tags"].clone()).unwrap();
        value.pinned = base["pinned"].as_bool().unwrap();
        NoteWrite::from_note(
            db,
            &value,
            WritePolicy::Update {
                expected_token: base["accepted"]["token"].as_str().unwrap().into(),
            },
        )
    }
    fn revisions(db: &VaultDb) -> i64 {
        db.conn
            .query_row("SELECT COUNT(*) FROM revisions", [], |r| r.get(0))
            .unwrap()
    }
    fn token(value: &Value) -> &str {
        value["accepted"]["token"].as_str().unwrap()
    }
    fn rev(value: &Value) -> i64 {
        value["accepted"]["revision"]["revision"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap()
    }

    #[test]
    fn independent_connections_both_orders_keep_exact_receipts_and_reject_aba_and_metadata_races() {
        for gui_first in [true, false] {
            let fixture = Fixture::new();
            let gui = fixture.open();
            let cli = fixture.open();
            let base = create(&gui, "n", "A");
            let (first, second) = if gui_first {
                (&gui, &cli)
            } else {
                (&cli, &gui)
            };
            let b = first.write_scripted(&update(first, &base, "B")).unwrap();
            let failure = second
                .write_authoritative(&update(second, &base, "C"), 0)
                .unwrap_err();
            assert_eq!(failure.kind, WriteErrorKind::Changed);
            let a = second
                .write_authoritative(&update(second, &b, "A"), 0)
                .unwrap();
            assert_ne!(token(&a), token(&base)); // identical state is not identical ancestry.
            assert_eq!(b["content"], "B"); // Ack cannot be replaced by a later read.
            assert_eq!(gui.accepted_at_token("n", token(&b)).unwrap(), b);
            assert_eq!(
                first
                    .write_scripted(&update(first, &base, "lost"))
                    .unwrap_err()
                    .kind,
                WriteErrorKind::Changed
            );
            let mut metadata = update(first, &a, "A");
            metadata.title = "New title".into();
            metadata.pinned = true;
            let changed = first.write_scripted(&metadata).unwrap();
            assert!(rev(&changed) > rev(&a));
            assert_eq!(
                second
                    .write_scripted(&update(second, &a, "dirty"))
                    .unwrap_err()
                    .kind,
                WriteErrorKind::Changed
            );
            ancestry::validate(&gui.conn).unwrap();
        }
    }

    #[test]
    fn trash_restore_delete_recreate_and_legacy_tokens_cannot_reuse_an_identity() {
        let fixture = Fixture::new();
        let db = fixture.open();
        let other = fixture.open();
        let a = create(&db, "same", "A");
        other.set_note_trashed("same", true).unwrap();
        assert_eq!(
            db.write_scripted(&update(&db, &a, "dirty"))
                .unwrap_err()
                .kind,
            WriteErrorKind::Trashed
        );
        other.set_note_trashed("same", false).unwrap();
        assert_eq!(
            db.write_scripted(&update(&db, &a, "dirty"))
                .unwrap_err()
                .kind,
            WriteErrorKind::Changed
        );
        other.delete_note("same").unwrap();
        assert_eq!(
            db.write_scripted(&update(&db, &a, "dirty"))
                .unwrap_err()
                .kind,
            WriteErrorKind::Deleted
        );
        let recreated = create(&other, "same", "A");
        assert_ne!(
            a["accepted"]["revision"]["incarnationId"],
            recreated["accepted"]["revision"]["incarnationId"]
        );
        assert!(rev(&recreated) > rev(&a));
        assert_eq!(
            db.write_scripted(&update(&db, &a, "dirty"))
                .unwrap_err()
                .kind,
            WriteErrorKind::Deleted
        );
        let mut legacy = update(&db, &recreated, "dirty");
        legacy.policy = WritePolicy::Update {
            expected_token: format!("state-v1.{}.{}", "a".repeat(64), "b".repeat(64)),
        };
        assert_eq!(
            db.write_scripted(&legacy).unwrap_err().kind,
            WriteErrorKind::InvalidPolicy
        );
        assert!(db.upsert_note(&note("same", "unguarded")).is_err());
        assert!(db
            .conn
            .execute("UPDATE notes SET content='unguarded'", [])
            .is_err());
        ancestry::validate(&db.conn).unwrap();
    }

    #[test]
    fn cli_atomic_tags_preserve_body_in_both_orders_and_reject_stale_expected_tokens() {
        let fixture = Fixture::new();
        let gui = fixture.open();
        let cli = fixture.open();
        let a = create(&gui, "n", "base");
        let tagged = cli
            .tag_atomic_expected("n", &["a".into()], &[], Some(token(&a)), Some("tag-1"))
            .unwrap();
        assert_eq!(tagged["content"], "base");
        assert_eq!(
            gui.write_scripted(&update(&gui, &a, "GUI"))
                .unwrap_err()
                .kind,
            WriteErrorKind::Changed
        );
        let body = gui
            .write_authoritative(&update(&gui, &tagged, "GUI"), 0)
            .unwrap();
        assert_eq!(
            cli.tag_atomic_expected("n", &["b".into()], &[], Some(token(&tagged)), None)
                .unwrap_err()
                .kind,
            WriteErrorKind::Changed
        );
        let final_note = cli
            .tag_atomic_expected("n", &["b".into()], &[], Some(token(&body)), None)
            .unwrap();
        assert_eq!(final_note["content"], "GUI");
        assert_eq!(final_note["tags"], json!(["a", "b"]));
    }

    #[test]
    fn retry_returns_its_original_receipt_after_another_writer_and_reused_input_is_rejected() {
        let fixture = Fixture::new();
        let db = fixture.open();
        let other = fixture.open();
        let a = create(&db, "n", "A");
        let mut operation = update(&db, &a, "B");
        operation.operation_id = Some("retry-update".into());
        let b = db.write_scripted(&operation).unwrap();
        let c = other.write_scripted(&update(&other, &b, "C")).unwrap();
        let count = revisions(&db);
        assert_eq!(db.write_scripted(&operation).unwrap(), b);
        assert_eq!(revisions(&db), count);
        assert_eq!(db.accepted_note("n").unwrap().unwrap(), c);
        operation.content = "different".into();
        assert!(db.write_scripted(&operation).is_err());
        assert_eq!(db.accepted_note("n").unwrap().unwrap(), c);
        // Empty trash records all original per-note operation items atomically.
        create(&db, "other", "other");
        db.set_note_trashed("n", true).unwrap();
        db.set_note_trashed("other", true).unwrap();
        let count = db
            .with_write_operation("empty-1", &json!(["empty"]), || db.empty_trash_v3())
            .unwrap();
        assert_eq!(count, 2);
        assert_eq!(
            db.with_write_operation("empty-1", &json!(["empty"]), || db.empty_trash_v3())
                .unwrap(),
            2
        );
        assert!(db
            .with_write_operation("empty-1", &json!(["changed"]), || db.empty_trash_v3())
            .is_err());
        ancestry::validate(&db.conn).unwrap();
    }

    #[test]
    fn summary_repeats_are_noops_but_manual_edit_then_return_to_same_state_preserves_incoming() {
        let fixture = Fixture::new();
        let db = fixture.open();
        let gui = fixture.open();
        let generated = note("hermes-session-test", "generated");
        let input = json!({"session_id":"test","changed":["generated"]});
        let first = db.import_generated_summary(&generated, &input).unwrap();
        let count = revisions(&db);
        assert_eq!(
            db.import_generated_summary(&generated, &input).unwrap(),
            first
        );
        assert_eq!(revisions(&db), count);
        let manual = gui
            .write_authoritative(&update(&gui, &first, "manual"), 0)
            .unwrap();
        let back = gui
            .write_authoritative(&update(&gui, &manual, "generated"), 0)
            .unwrap();
        assert_ne!(token(&back), token(&first));
        let error = db.import_generated_summary(&generated, &input).unwrap_err();
        assert_eq!(error.kind, WriteErrorKind::Changed);
        let repeated = db.import_generated_summary(&generated, &input).unwrap_err();
        assert_eq!(error.artifact, repeated.artifact);
        assert_eq!(db.pending_summaries("test").unwrap().len(), 1);
        assert_eq!(db.accepted_note(&generated.id).unwrap().unwrap(), back);
        // Truly absent generated identities still create normally.
        assert!(db
            .import_generated_summary(
                &note("hermes-session-new", "new"),
                &json!({"session_id":"new"})
            )
            .is_ok());
    }

    #[test]
    fn notebook_session_metadata_history_restore_and_attachment_staging_use_one_contract() {
        let fixture = Fixture::new();
        let db = fixture.open();
        let other = fixture.open();
        let a = create(&db, "n", "A");
        let count = revisions(&db);
        let blob = db
            .add_attachment(
                "n",
                "f.bin",
                "application/octet-stream",
                b"shared bytes",
                "now",
            )
            .unwrap();
        assert_eq!(revisions(&db), count);
        assert_eq!(db.accepted_note("n").unwrap().unwrap(), a);
        let body = format!("A\n[file](.rn-attachments/{})", blob.id);
        let attached = db.write_authoritative(&update(&db, &a, &body), 0).unwrap();
        let links: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM revision_attachments WHERE revision_id=?1",
                [attached["accepted"]["revision"]["revisionId"]
                    .as_str()
                    .unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(links, 1);
        let vault = db.identity().path;
        super::super::db_notebooks::create(&db, &vault, None, "Work").unwrap();
        super::super::db_notebooks::move_note(&db, &vault, "n", "Work").unwrap();
        assert_eq!(
            other
                .write_scripted(&update(&other, &attached, "stale"))
                .unwrap_err()
                .kind,
            WriteErrorKind::Changed
        );
        let moved = db.accepted_note("n").unwrap().unwrap();
        db.set_session_link("n", "session").unwrap();
        let linked = db.accepted_note("n").unwrap().unwrap();
        assert_ne!(token(&moved), token(&linked));
        let restored = db
            .write_authoritative(&update(&db, &linked, "A"), 0)
            .unwrap();
        let parent: String = db
            .conn
            .query_row(
                "SELECT parent_id FROM revision_parents WHERE child_id=?1 AND role='predecessor'",
                [restored["accepted"]["revision"]["revisionId"]
                    .as_str()
                    .unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            parent.as_str(),
            linked["accepted"]["revision"]["revisionId"]
                .as_str()
                .unwrap()
        );
        db.delete_note("n").unwrap();
        assert_eq!(
            db.get_attachment_data(&blob.id).unwrap().unwrap().1,
            b"shared bytes".to_vec()
        );
        ancestry::validate(&db.conn).unwrap();
    }

    #[test]
    fn disk_full_and_ledger_failure_refuse_without_projection_or_receipt_changes_and_leave_gaps() {
        let fixture = Fixture::new();
        let db = fixture.open();
        let a = create(&db, "n", "A");
        let pages: i64 = db
            .conn
            .query_row("PRAGMA page_count", [], |r| r.get(0))
            .unwrap();
        db.conn
            .pragma_update(None, "max_page_count", pages)
            .unwrap();
        assert!(db
            .write_scripted(&update(&db, &a, &"X".repeat(2 * 1024 * 1024)))
            .is_err());
        assert!(db.conn.is_autocommit());
        assert_eq!(db.accepted_note("n").unwrap().unwrap(), a);
        db.conn
            .pragma_update(None, "max_page_count", 1_073_741_823)
            .unwrap();
        let b = db.write_scripted(&update(&db, &a, "B")).unwrap();
        assert!(rev(&b) > rev(&a) + 1);
        let ledger_path = fixture.ledger.join(format!(
            "{}.json",
            db.revision_session.as_ref().unwrap().identity.vault_uuid
        ));
        let saved = ledger_path.with_extension("saved");
        std::fs::rename(&ledger_path, &saved).unwrap();
        std::fs::create_dir(&ledger_path).unwrap();
        assert!(db.write_scripted(&update(&db, &b, "lost")).is_err());
        assert!(db.conn.is_autocommit());
        assert_eq!(db.accepted_note("n").unwrap().unwrap(), b);
        std::fs::remove_dir(&ledger_path).unwrap();
        std::fs::rename(saved, ledger_path).unwrap();
        assert!(db.write_scripted(&update(&db, &b, "C")).is_ok());
    }

    #[test]
    fn recovery_copy_retries_preserve_summary_provenance_and_require_origin_but_not_a_merge_base() {
        let fixture = Fixture::new();
        let db = fixture.open();
        let a = create(&db, "n", "original");
        let mut draft = crate::recovery::Draft {
            owner: uuid(),
            sequence: 1,
            revision: 1,
            captured_at: "now".into(),
            vault: db.identity(),
            note_id: "n".into(),
            // This is not a verified merge base: Keep both only requires the
            // captured origin, never guessed ancestry or a readable old body.
            base: json!({"meta":{"accepted":a["accepted"]}}),
            body: "unsaved draft".into(),
            title: "Summary copy".into(),
            tags: vec!["hermes/session".into(), "plain".into()],
            pinned: false,
            blobs: vec![],
            conflicted: true,
        };
        let copy = crate::recovery::fork(&db, &draft).unwrap();
        assert_eq!(crate::recovery::fork(&db, &draft).unwrap(), copy);
        assert_ne!(copy["id"], "n");
        assert_eq!(copy["content"], "unsaved draft");
        assert!(!copy["tags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tag| tag == "hermes/session"));
        assert_eq!(db.accepted_note("n").unwrap().unwrap(), a);
        draft.body = "changed under same operation key".into();
        assert!(crate::recovery::fork(&db, &draft).is_err());
        draft.sequence = 2;
        draft.base["meta"]["accepted"]["vaultUuid"] = json!(uuid());
        let count = revisions(&db);
        assert!(crate::recovery::fork(&db, &draft).is_err());
        assert_eq!(revisions(&db), count);
    }

    #[test]
    fn large_revision_numbers_round_trip_as_decimal_strings() {
        let fixture = Fixture::new();
        let db = fixture.open();
        let a = create(&db, "n", "A");
        let reserved = {
            let mut ledger = Ledger::acquire(
                &fixture.ledger,
                &db.revision_session.as_ref().unwrap().identity.vault_uuid,
            )
            .unwrap();
            ledger.reserve("n", 9_007_199_254_740_992).unwrap()
        };
        let b = db.write_scripted(&update(&db, &a, "B")).unwrap();
        assert_eq!(
            b["accepted"]["revision"]["revision"],
            (reserved + 1).to_string()
        );
        assert_eq!(rev(&b), reserved + 1);
    }

    #[test]
    fn imports_and_task_source_roll_out_without_unguarded_legacy_upserts() {
        let fixture = Fixture::new();
        let db = fixture.open();
        let input = tempfile::tempdir().unwrap();
        std::fs::write(
            input.path().join("n.md"),
            "---\nid: imported\ntitle: Imported\n---\n- [ ] task\n",
        )
        .unwrap();
        let run = || {
            super::super::db_import::import_md_vault(input.path(), &db).map(|r| r.notes_imported)
        };
        assert_eq!(
            db.with_write_operation("bulk-1", &json!(["fixture-import"]), run)
                .unwrap(),
            1
        );
        let count = revisions(&db);
        assert_eq!(
            db.with_write_operation("bulk-1", &json!(["fixture-import"]), run)
                .unwrap(),
            1
        );
        assert_eq!(revisions(&db), count);
        let a = db.accepted_note("imported").unwrap().unwrap();
        let mut task = update(&db, &a, "- [x] task\n");
        task.writer_source = Some("task".into());
        let done = db.write_authoritative(&task, 0).unwrap();
        let source: String = db
            .conn
            .query_row(
                "SELECT writer_source FROM revisions WHERE revision_id=?1",
                [done["accepted"]["revision"]["revisionId"].as_str().unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(source, "task");
        assert!(db
            .write_authoritative(&update(&db, &a, "- [ ] task\n"), 0)
            .is_err());
        ancestry::validate(&db.conn).unwrap();
    }
    #[test]
    fn single_copy_and_indexed_replace_imports_finalize_assets_and_metadata_before_the_graph() {
        let fixture = Fixture::new();
        let db = fixture.open();
        create(&db, "same", "original");
        let input = tempfile::tempdir().unwrap();
        let file = input.path().join("single.md");
        let raw = "---\nid: same\ntitle: Imported copy\n---\ncopy body\n";
        std::fs::write(&file, raw).unwrap();
        let run = || {
            let prepared = super::super::operations::prepare_note_import(
                file.to_str().unwrap(),
                input.path(),
                raw,
                vec![],
                |_| Ok(()),
            )?;
            super::super::db_import::import_single_receipt(&db, prepared, None)
        };
        let copy = db
            .with_write_operation("single-copy", &json!([raw]), run)
            .unwrap();
        assert_ne!(copy["id"], "same");
        assert_eq!(
            db.accepted_note("same").unwrap().unwrap()["content"],
            "original"
        );
        assert_eq!(
            db.with_write_operation("single-copy", &json!([raw]), run)
                .unwrap(),
            copy
        );
        let legacy = VaultDb::open_in_memory().unwrap();
        legacy
            .upsert_note(&note("portable", "portable body"))
            .unwrap();
        let asset = legacy
            .add_attachment(
                "portable",
                "a.pdf",
                "application/pdf",
                b"%PDF-synthetic",
                "now",
            )
            .unwrap();
        let mut portable = note("portable", &format!("[file](.rn-attachments/{})", asset.id));
        portable.tags = vec!["imported".into()];
        legacy.upsert_note(&portable).unwrap();
        legacy.set_session_link("portable", "session").unwrap();
        legacy.set_note_trashed("portable", true).unwrap();
        legacy
            .add_version("portable", "legacy body", "history")
            .unwrap();
        let exported = tempfile::tempdir().unwrap();
        super::super::db_export::export_md_vault(exported.path(), &legacy).unwrap();
        let report = super::super::db_import::import_md_vault(exported.path(), &db).unwrap();
        assert_eq!(report.notes_trashed, 1);
        let imported = db.accepted_note("portable").unwrap().unwrap();
        assert_eq!(imported["trashed"], true);
        assert_eq!(imported["tags"], json!(["imported"]));
        let count = revisions(&db);
        super::super::db_import::import_md_vault(exported.path(), &db).unwrap();
        assert_eq!(revisions(&db), count);
        assert_eq!(
            db.get_attachment_data(&asset.id).unwrap().unwrap().1,
            b"%PDF-synthetic".to_vec()
        );
        let ancestry: String = db
            .conn
            .query_row(
                "SELECT ancestry FROM versions WHERE note_id='portable'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ancestry, "unknown");
        ancestry::validate(&db.conn).unwrap();
    }

    #[test]
    #[ignore = "subprocess reservation-crash fixture; invoked by parent test"]
    fn reservation_crash_child() {
        let Some(root) = std::env::var_os("RYUK_REVISION_CRASH_LEDGER") else {
            return;
        };
        let vault = std::env::var("RYUK_REVISION_CRASH_UUID").unwrap();
        let mut ledger = Ledger::acquire(Path::new(&root), &vault).unwrap();
        let number = ledger.reserve_transaction(0).unwrap();
        std::fs::write(
            std::env::var_os("RYUK_REVISION_CRASH_TICKET").unwrap(),
            number.to_string(),
        )
        .unwrap();
        // Exit without running Rust destructors: OS unlock plus durable ticket
        // must suffice, with no vault write to pair with this reservation.
        std::process::exit(0);
    }
    #[test]
    fn another_process_can_crash_after_reservation_without_reusing_its_ticket() {
        let fixture = Fixture::new();
        let db = fixture.open();
        let a = create(&db, "n", "A");
        let ticket = fixture._dir.path().join("crashed-ticket");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "vault::revision_writer::tests::reservation_crash_child",
                "--ignored",
                "--nocapture",
            ])
            .env("RYUK_REVISION_CRASH_LEDGER", &fixture.ledger)
            .env(
                "RYUK_REVISION_CRASH_UUID",
                &db.revision_session.as_ref().unwrap().identity.vault_uuid,
            )
            .env("RYUK_REVISION_CRASH_TICKET", &ticket)
            .status()
            .unwrap();
        assert!(status.success());
        let reserved = std::fs::read_to_string(&ticket)
            .unwrap()
            .parse::<i64>()
            .unwrap();
        assert_eq!(db.accepted_note("n").unwrap().unwrap(), a);
        let b = db.write_scripted(&update(&db, &a, "B")).unwrap();
        assert!(rev(&b) > reserved);
    }
}
