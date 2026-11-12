//! Durable schema 3 identity, migration and restore protocol. Production opens
//! activate the same guarded writer session used by the staged fixtures.
//! No revision integers or content hashes alone authorize a write. See the approved
//! schema proposal for ledger-loss, non-cooperating SQL and multi-host limits.
use super::db::VaultDb;
use fs4::FileExt;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

// GUI and CLI must resolve exactly the same host ledger. The explicit override
// is useful for disposable Stage 4F runs; it never silently re-enrolls v3 data.
static MOBILE_LEDGER_ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
pub fn set_mobile_ledger_root(root: PathBuf) {
    let _ = MOBILE_LEDGER_ROOT.set(root);
}
pub fn production_ledger_root(vault: &Path) -> Result<PathBuf, String> {
    #[cfg(test)]
    {
        Ok(vault
            .parent()
            .unwrap_or(Path::new("."))
            .join("test-revision-ledger"))
    }
    #[cfg(not(test))]
    {
        let _ = vault;
        if let Some(root) = std::env::var_os("RYUK_V3_LEDGER") {
            return Ok(PathBuf::from(root));
        }
        if let Some(root) = MOBILE_LEDGER_ROOT.get() {
            return Ok(root.clone());
        }
        dirs::data_local_dir()
            .map(|root| root.join("ryuknotes").join("revision-ledger"))
            .ok_or("Private data directory unavailable".into())
    }
}

pub(crate) const DDL: &str = include_str!("ancestry.sql");
fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}
fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn sync_dir(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    File::open(path).and_then(|f| f.sync_all()).map_err(err)?;
    #[cfg(not(unix))]
    {
        let _ = path;
        return Err("Durable ancestry directory sync is not implemented on this platform".into());
    }
    #[allow(unreachable_code)]
    Ok(())
}
fn private_file(path: &Path, new: bool) -> Result<File, String> {
    if let Ok(meta) = fs::symlink_metadata(path) {
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err("Unsafe ancestry file".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o077 != 0 {
                return Err("Ancestry file must be private (0600)".into());
            }
        }
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    if new {
        options.create_new(true);
    } else {
        options.create(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(err)
}
fn private_dir(path: &Path) -> Result<(), String> {
    if !path.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true).mode(0o700);
            builder.create(path).map_err(err)?;
        }
        #[cfg(not(unix))]
        fs::create_dir_all(path).map_err(err)?;
    }
    let meta = fs::symlink_metadata(path).map_err(err)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err("Unsafe ledger directory".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err("Ledger directory must be private (0700)".into());
        }
    }
    Ok(())
}

/// Connection-lifetime, cross-process path lock. Exclusive maintenance never
/// waits on a webview or upgrades a held shared lock. Busy means refuse/retry.
/// Sidecar lives outside the replaceable database and is never renamed/deleted.
pub(crate) struct Maintenance {
    _file: File,
}
impl Maintenance {
    fn acquire(path: &Path, exclusive: bool) -> Result<Self, String> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let canonical = if path.exists() {
            fs::canonicalize(path).map_err(err)?
        } else {
            fs::canonicalize(parent)
                .map_err(err)?
                .join(path.file_name().ok_or("Missing vault filename")?)
        };
        let lock_path = PathBuf::from(format!("{}.maintenance.lock", canonical.display()));
        let file = private_file(&lock_path, false)?;
        let result = if exclusive {
            FileExt::try_lock_exclusive(&file)
        } else {
            FileExt::try_lock_shared(&file)
        };
        result.map_err(|e| {
            format!("Vault maintenance is busy; close affected connections and retry: {e}")
        })?;
        Ok(Self { _file: file })
    }
    /// Lock-conversion seam used only by the frozen v2 compatibility fixture
    /// (open_legacy_v2), which itself exists only under test compilation.
    #[cfg(test)]
    pub(crate) fn downgrade(&self) -> Result<(), String> {
        FileExt::lock_shared(&self._file).map_err(err)
    }
    pub(crate) fn shared(path: &Path) -> Result<Self, String> {
        Self::acquire(path, false)
    }
    pub(crate) fn exclusive(path: &Path) -> Result<Self, String> {
        Self::acquire(path, true)
    }
}

/// Read schema metadata without setting journal mode, creating a DB or a snapshot.
/// Canonical decimal versions only: malformed strings never get normalized.
pub fn inspect_version(path: &Path) -> Result<i64, String> {
    if !path.exists() {
        return Ok(0);
    }
    let canonical_path = fs::canonicalize(path).map_err(err)?;
    let path = canonical_path.as_path();
    // Even SQLITE_OPEN_READ_ONLY can create -wal/-shm beside a WAL-mode file.
    // Probe a stable private copy when WAL has frames, otherwise use immutable
    // mode (safe only with no frames). Never set journal pragmas on the source.
    let wal = PathBuf::from(format!("{}-wal", path.display()));
    for _ in 0..3 {
        let main_before = fs::metadata(path).map_err(err)?;
        let wal_before = fs::metadata(&wal).ok();
        if wal_before.as_ref().is_some_and(|m| m.len() > 0) {
            let temp = tempfile::tempdir().map_err(err)?;
            let copy = temp.path().join("probe.rn");
            fs::copy(path, &copy).map_err(err)?;
            if fs::copy(&wal, copy.with_extension("rn-wal")).is_err() {
                continue;
            }
            let main_after = fs::metadata(path).map_err(err)?;
            let wal_after = fs::metadata(&wal).ok();
            if main_before.len() != main_after.len()
                || main_before.modified().ok() != main_after.modified().ok()
                || wal_before.as_ref().map(|m| (m.len(), m.modified().ok()))
                    != wal_after.as_ref().map(|m| (m.len(), m.modified().ok()))
            {
                continue;
            }
            let conn =
                Connection::open_with_flags(copy, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(err)?;
            return schema_version(&conn);
        }
        let canonical = fs::canonicalize(path).map_err(err)?;
        let uri = format!(
            "file:{}?immutable=1",
            urlencoding::encode(&canonical.to_string_lossy())
        );
        let conn = Connection::open_with_flags(
            uri,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )
        .map_err(err)?;
        let version = schema_version(&conn);
        if fs::metadata(&wal).is_ok_and(|m| m.len() > 0) {
            continue;
        }
        return version;
    }
    Err("Vault changed during schema preflight; retry after writers are quiet".into())
}
pub(crate) fn schema_version(conn: &Connection) -> Result<i64, String> {
    let has_meta: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='meta')",
            [],
            |r| r.get(0),
        )
        .map_err(err)?;
    if !has_meta {
        let tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
                [],
                |r| r.get(0),
            )
            .map_err(err)?;
        return if tables == 0 {
            Ok(0)
        } else {
            Err("Unrecognized vault schema".into())
        };
    }
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(err)?;
    match raw.as_deref() {
        Some("1") => Ok(1),
        Some("2") => Ok(2),
        Some("3") => Ok(3),
        Some(other) => Err(format!(
            "Unsupported or malformed vault schema version {other:?}"
        )),
        None => Err("Vault schema version is missing".into()),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Identity {
    pub vault_uuid: String,
    pub storage_epoch: String,
}
pub(crate) fn identity(conn: &Connection) -> Result<Identity, String> {
    let value = conn
        .query_row(
            "SELECT vault_uuid,storage_epoch FROM vault_identity WHERE singleton=1",
            [],
            |r| {
                Ok(Identity {
                    vault_uuid: r.get(0)?,
                    storage_epoch: r.get(1)?,
                })
            },
        )
        .map_err(err)?;
    for field in [&value.vault_uuid, &value.storage_epoch] {
        if uuid::Uuid::parse_str(field).map_err(err)?.to_string() != *field {
            return Err("Noncanonical vault identity".into());
        }
    }
    Ok(value)
}

/// No public connection or write API. SQLite READ_ONLY plus query_only is defense
/// in depth; inspection never grants a production writer capability.
pub struct Inspection {
    conn: Connection,
    _maintenance: Maintenance,
    // Runtime-only capability: reopening after maintenance never reuses it.
    session_generation: String,
}
impl Inspection {
    pub fn open(path: &Path) -> Result<Self, String> {
        if inspect_version(path)? != 3 {
            return Err("Expected schema 3".into());
        }
        let maintenance = Maintenance::shared(path)?;
        let conn =
            Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(err)?;
        conn.pragma_update(None, "query_only", true).map_err(err)?;
        if schema_version(&conn)? != 3 {
            return Err("Vault schema changed while opening".into());
        }
        validate(&conn)?;
        Ok(Self {
            conn,
            _maintenance: maintenance,
            session_generation: uuid(),
        })
    }
    pub fn identity(&self) -> Result<Identity, String> {
        identity(&self.conn)
    }
    pub fn session_generation(&self) -> &str {
        &self.session_generation
    }
    pub fn note_ids(&self) -> Result<Vec<String>, String> {
        let mut statement = self
            .conn
            .prepare("SELECT note_id FROM note_revision_heads ORDER BY note_id")
            .map_err(err)?;
        let rows = statement.query_map([], |r| r.get(0)).map_err(err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(err)
    }
    pub fn revision(&self, revision_id: &str) -> Result<Option<Value>, String> {
        let header = self.conn.query_row("SELECT incarnation_id,revision_no,storage_epoch,commit_token,state_sha256,writer_source,operation_id,kind,ancestry FROM revisions WHERE revision_id=?1", [revision_id], |r| Ok(json!({
            "revisionId":revision_id,"incarnationId":r.get::<_,String>(0)?,"revision":r.get::<_,i64>(1)?,"storageEpoch":r.get::<_,String>(2)?,"commitToken":r.get::<_,String>(3)?,"stateSha256":r.get::<_,String>(4)?,"writerSource":r.get::<_,String>(5)?,"operationId":r.get::<_,String>(6)?,"kind":r.get::<_,String>(7)?,"ancestry":r.get::<_,String>(8)?
        }))).optional().map_err(err)?;
        let Some(mut header) = header else {
            return Ok(None);
        };
        let exists: bool = self
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM revision_states WHERE revision_id=?1)",
                [revision_id],
                |r| r.get(0),
            )
            .map_err(err)?;
        header["state"] = if exists {
            serde_json::to_value(payload(&self.conn, revision_id)?).map_err(err)?
        } else {
            Value::Null
        };
        let mut statement = self.conn.prepare("SELECT parent_id,role,ordinal FROM revision_parents WHERE child_id=?1 ORDER BY role,ordinal").map_err(err)?;
        let parents = statement.query_map([revision_id], |r| Ok(json!({"revisionId":r.get::<_,String>(0)?,"role":r.get::<_,String>(1)?,"ordinal":r.get::<_,i64>(2)?}))).map_err(err)?.collect::<Result<Vec<_>, _>>().map_err(err)?;
        header["parents"] = json!(parents);
        Ok(Some(header))
    }
    pub fn note(&self, id: &str) -> Result<Option<Value>, String> {
        self.conn.query_row("SELECT h.revision_id,h.incarnation_id,r.revision_no,r.commit_token,s.body,s.title,s.tags_json,s.pinned,s.lifecycle,s.extra_json FROM note_revision_heads h JOIN revisions r ON r.revision_id=h.revision_id JOIN revision_states s ON s.revision_id=r.revision_id WHERE h.note_id=?1", [id], |r| {
            let body: Vec<u8> = r.get(4)?;
            Ok(json!({"revisionId":r.get::<_,String>(0)?,"incarnationId":r.get::<_,String>(1)?,"revision":r.get::<_,i64>(2)?,"commitToken":r.get::<_,String>(3)?,"bodyBytes":body,"title":r.get::<_,String>(5)?,"tagsJson":r.get::<_,String>(6)?,"pinned":r.get::<_,bool>(7)?,"lifecycle":r.get::<_,String>(8)?,"extraJson":r.get::<_,String>(9)?}))
        }).optional().map_err(err)
    }
}

#[derive(Default, Serialize, Deserialize)]
struct LedgerData {
    format: u32,
    vault_uuid: String,
    storage_epoch: String,
    #[serde(default)]
    pending_restore_epoch: Option<String>,
    bound_path: String,
    counters: BTreeMap<String, i64>,
    #[serde(default)]
    global_high_water: i64,
}
/// Lock this before any SQLite write transaction; reserve() fsyncs before return.
/// Locks are OS-released on process death. Reservations may leave gaps, never reuse.
pub struct Ledger {
    root: PathBuf,
    path: PathBuf,
    data: LedgerData,
    _lock: File,
    pub was_missing: bool,
}
impl Ledger {
    pub fn acquire(root: &Path, vault_uuid: &str) -> Result<Self, String> {
        if uuid::Uuid::parse_str(vault_uuid).map_err(err)?.to_string() != vault_uuid {
            return Err("Invalid ledger UUID".into());
        }
        private_dir(root)?;
        let lock = private_file(&root.join(format!("{vault_uuid}.lock")), false)?;
        FileExt::try_lock_exclusive(&lock).map_err(|e| format!("Ledger busy: {e}"))?;
        let path = root.join(format!("{vault_uuid}.json"));
        let was_missing = !path.exists();
        let data = if was_missing {
            LedgerData {
                format: 1,
                vault_uuid: vault_uuid.into(),
                ..Default::default()
            }
        } else {
            let file = private_file(&path, false)?;
            let raw: Value = serde_json::from_reader(file).map_err(err)?;
            let data: LedgerData = serde_json::from_value(raw.clone()).map_err(err)?;
            if !matches!(data.format, 1 | 2)
                || (data.format == 2
                    && raw
                        .get("global_high_water")
                        .and_then(Value::as_i64)
                        .is_none())
                || (data.format == 1 && data.global_high_water != 0)
                || data.vault_uuid != vault_uuid
                || data.global_high_water < 0
                || data.counters.values().any(|n| *n < 0)
            {
                return Err("Invalid high-water ledger".into());
            }
            data
        };
        Ok(Self {
            root: root.into(),
            path,
            data,
            _lock: lock,
            was_missing,
        })
    }
    fn persist(&self) -> Result<(), String> {
        let mut temp = tempfile::NamedTempFile::new_in(&self.root).map_err(err)?;
        serde_json::to_writer(temp.as_file_mut(), &self.data).map_err(err)?;
        temp.as_file_mut().flush().map_err(err)?;
        temp.as_file().sync_all().map_err(err)?;
        temp.persist(&self.path).map_err(err)?;
        sync_dir(&self.root)
    }
    pub fn reserve(&mut self, note_id: &str, database_high_water: i64) -> Result<i64, String> {
        if database_high_water < 0 {
            return Err("Invalid database counter".into());
        }
        let previous = self
            .data
            .counters
            .get(note_id)
            .copied()
            .unwrap_or(0)
            .max(database_high_water)
            .max(self.data.global_high_water);
        let next = previous
            .checked_add(1)
            .ok_or("Revision counter exhausted")?;
        self.data.counters.insert(note_id.into(), next);
        // On failure the caller MUST abort; a later attempt still treats memory
        // as reserved. Never return a number before disk and directory sync.
        self.persist()?;
        Ok(next)
    }
    pub(crate) fn reserve_transaction(&mut self, database_high_water: i64) -> Result<i64, String> {
        if database_high_water < 0 {
            return Err("Invalid database high water".into());
        }
        let highest = self
            .data
            .counters
            .values()
            .copied()
            .max()
            .unwrap_or(0)
            .max(self.data.global_high_water)
            .max(database_high_water);
        let next = highest.checked_add(1).ok_or("Revision counter exhausted")?;
        // Older 4B tooling must reject, rather than ignore, the global floor.
        // Vault schema stays 3; this is the external ledger's own format version.
        self.data.format = 2;
        self.data.global_high_water = next;
        self.persist()?;
        Ok(next)
    }
    pub(crate) fn verify_session(&self, path: &Path, identity: &Identity) -> Result<(), String> {
        if self.was_missing
            || self.data.pending_restore_epoch.is_some()
            || self.data.storage_epoch != identity.storage_epoch
        {
            return Err(
                "Vault ledger requires explicit restore/recovery enrollment before writing".into(),
            );
        }
        let canonical = fs::canonicalize(path)
            .map_err(err)?
            .to_string_lossy()
            .into_owned();
        if self.data.bound_path != canonical {
            return Err("Vault path changed; use explicit restore/recovery enrollment".into());
        }
        Ok(())
    }
    fn bind(&mut self, path: &Path, epoch: &str) -> Result<(), String> {
        let canonical = if path.exists() {
            fs::canonicalize(path).map_err(err)?
        } else {
            fs::canonicalize(
                path.parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new(".")),
            )
            .map_err(err)?
            .join(path.file_name().ok_or("Missing restore filename")?)
        }
        .to_string_lossy()
        .into_owned();
        if !self.data.bound_path.is_empty()
            && self.data.bound_path != canonical
            && Path::new(&self.data.bound_path).exists()
        {
            return Err("This vault UUID is already bound to another existing path; use explicit clone/restore".into());
        }
        self.data.bound_path = canonical;
        self.data.storage_epoch = epoch.into();
        self.persist()
    }
    fn prepare_restore(&mut self, epoch: &str) -> Result<(), String> {
        self.data.pending_restore_epoch = Some(epoch.into());
        self.persist()
    }
    fn finish_restore(&mut self, epoch: &str) -> Result<(), String> {
        self.data.storage_epoch = epoch.into();
        self.data.pending_restore_epoch = None;
        self.persist()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fault {
    None,
    Snapshot,
    Ddl,
    Validation,
    Commit,
}
pub(crate) fn fail(fault: Fault, point: Fault) -> Result<(), String> {
    if fault == point {
        Err(format!("Injected ancestry {point:?} failure"))
    } else {
        Ok(())
    }
}
pub(crate) struct Migration {
    pub identity: Identity,
    pub numbers: BTreeMap<String, i64>,
    pub operation: String,
    pub fault: Fault,
}

pub(crate) fn snapshot(
    conn: &Connection,
    path: &Path,
    inject_failure: bool,
    strict_durability: bool,
) -> Result<(), String> {
    drop(private_file(path, true)?);
    let result = (|| {
        if inject_failure {
            return Err("Injected ancestry Snapshot failure".into());
        }
        conn.execute("VACUUM INTO ?1", params![path.to_string_lossy().as_ref()])
            .map_err(err)?;
        if strict_durability {
            sync_snapshot(path)
        } else {
            // Preserve the existing cross-platform v2 upgrade path. Strict v3
            // maintenance refuses platforms without a directory-sync backend.
            File::open(path).and_then(|f| f.sync_all()).map_err(err)?;
            #[cfg(unix)]
            sync_dir(path.parent().unwrap_or(Path::new(".")))?;
            Ok(())
        }
    })();
    if result.is_err() {
        // Never leave an empty/partial file masquerading as a recovery snapshot.
        let _ = fs::remove_file(path);
    }
    result
}
pub(crate) fn sync_snapshot(path: &Path) -> Result<(), String> {
    File::open(path).and_then(|f| f.sync_all()).map_err(err)?;
    sync_dir(path.parent().unwrap_or(Path::new(".")))
}
pub(crate) fn validate_foreign_keys(conn: &Connection) -> Result<(), String> {
    let mut statement = conn.prepare("PRAGMA foreign_key_check").map_err(err)?;
    let violations = statement
        .query_map([], |r| {
            let table: String = r.get(0)?;
            let rowid: Option<i64> = r.get(1)?;
            let parent: String = r.get(2)?;
            let fk: i64 = r.get(3)?;
            Ok(format!(
                "table={table}, rowid={}, parent={parent}, foreign_key={fk}",
                rowid.map_or("NULL".into(), |n| n.to_string())
            ))
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    if !violations.is_empty() {
        return Err(format!(
            "Vault foreign-key validation failed: {} violation(s): {}",
            violations.len(),
            violations.join("; ")
        ));
    }
    Ok(())
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FullState {
    pub(crate) body: Vec<u8>,
    pub(crate) title: String,
    pub(crate) tags: Vec<String>,
    pub(crate) pinned: bool,
    pub(crate) lifecycle: String,
    pub(crate) created: String,
    pub(crate) modified: String,
    pub(crate) extra: Value,
}
impl FullState {
    pub(crate) fn hash(&self) -> Result<String, String> {
        Ok(digest(&serde_json::to_vec(&(1, self)).map_err(err)?))
    }
}
pub(crate) fn states(conn: &Connection) -> Result<BTreeMap<String, FullState>, String> {
    let layout = super::db_notebooks::Layout::load(conn)?;
    let mut statement = conn
        .prepare("SELECT id,content,title,pinned,trashed,created,modified FROM notes ORDER BY id")
        .map_err(err)?;
    let rows = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, bool>(3)?,
                r.get::<_, bool>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
            ))
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    let mut result = BTreeMap::new();
    for (id, body, title, pinned, trashed, created, modified) in rows {
        let mut tags = conn
            .prepare("SELECT tag FROM note_tags WHERE note_id=?1 ORDER BY tag")
            .map_err(err)?;
        let tags = tags
            .query_map([&id], |r| r.get::<_, String>(0))
            .map_err(err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(err)?;
        let mut links = conn.prepare("SELECT attachment_id FROM note_attachments WHERE note_id=?1 ORDER BY attachment_id").map_err(err)?;
        let links = links
            .query_map([&id], |r| r.get::<_, String>(0))
            .map_err(err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(err)?;
        let session: Option<(String, String)> = conn
            .query_row(
                "SELECT session_id,created FROM session_links WHERE note_id=?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(err)?;
        let generated: Option<String> = conn
            .query_row(
                "SELECT value FROM meta WHERE key=?1",
                [format!("generated_summary:v1:{}", digest(id.as_bytes()))],
                |r| r.get(0),
            )
            .optional()
            .map_err(err)?;
        let mut extra = json!({"notebook":layout.notes.get(&id),"attachments":links,"session":session,"generatedSummaryV1":generated});
        let predecessor_extra: Option<String> = conn.query_row("SELECT s.extra_json FROM note_revision_heads h JOIN revision_states s USING(revision_id) WHERE h.note_id=?1", [&id], |r| r.get(0)).optional().map_err(err)?;
        if let Some(previous) = predecessor_extra {
            let previous: Value = serde_json::from_str(&previous).map_err(err)?;
            if let Some(origin) = previous.get("incarnationOrigin") {
                extra["incarnationOrigin"] = origin.clone();
            }
        }
        result.insert(
            id.clone(),
            FullState {
                body: body.into_bytes(),
                title,
                tags,
                pinned,
                lifecycle: if trashed {
                    "trashed".into()
                } else {
                    "live".into()
                },
                created,
                modified,
                extra,
            },
        );
    }
    Ok(result)
}
pub(crate) fn insert_state(conn: &Connection, id: &str, state: &FullState) -> Result<(), String> {
    conn.execute(
        "INSERT INTO revision_states VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            id,
            state.body,
            state.title,
            serde_json::to_string(&state.tags).map_err(err)?,
            state.pinned,
            state.lifecycle,
            state.created,
            state.modified,
            serde_json::to_string(&state.extra).map_err(err)?
        ],
    )
    .map_err(err)?;
    Ok(())
}
fn insert_revision(
    conn: &Connection,
    migration: &Migration,
    note: &str,
    incarnation: &str,
    state: &FullState,
    parent: Option<&str>,
) -> Result<String, String> {
    let revision = uuid();
    let now = chrono::Utc::now().to_rfc3339();
    let number = *migration
        .numbers
        .get(note)
        .ok_or("Missing durable reservation")?;
    let kind = if parent.is_some() {
        "restore_baseline"
    } else {
        "migration_baseline"
    };
    let source = if parent.is_some() {
        "restore"
    } else {
        "migration"
    };
    conn.execute(
        "INSERT INTO revisions VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
        params![
            revision,
            incarnation,
            number,
            migration.identity.storage_epoch,
            uuid(),
            state.hash()?,
            source,
            migration.operation,
            migration.operation,
            note,
            state.hash()?,
            kind,
            if parent.is_some() {
                "known"
            } else {
                "unknown_baseline"
            },
            now
        ],
    )
    .map_err(err)?;
    insert_state(conn, &revision, state)?;
    if let Some(parent) = parent {
        conn.execute(
            "INSERT INTO revision_parents VALUES(?1,?2,'predecessor',0)",
            params![revision, parent],
        )
        .map_err(err)?;
    }
    if let Some(parent) = parent {
        conn.execute("INSERT INTO revision_attachments SELECT ?1,attachment_id FROM revision_attachments WHERE revision_id=?2",params![revision,parent]).map_err(err)?;
    } else {
        conn.execute("INSERT INTO revision_attachments SELECT ?1,attachment_id FROM note_attachments WHERE note_id=?2",params![revision,note]).map_err(err)?;
    }
    conn.execute("INSERT INTO note_revision_heads VALUES(?1,?2,?3) ON CONFLICT(note_id) DO UPDATE SET incarnation_id=excluded.incarnation_id,revision_id=excluded.revision_id",params![note,incarnation,revision]).map_err(err)?;
    conn.execute(
        "UPDATE note_revision_counters SET high_water=?2 WHERE note_id=?1",
        params![note, number],
    )
    .map_err(err)?;
    Ok(revision)
}
pub(crate) fn bootstrap(conn: &Connection, migration: &Migration) -> Result<(), String> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO vault_identity VALUES(1,?1,?2,?3)",
        params![
            migration.identity.vault_uuid,
            migration.identity.storage_epoch,
            now
        ],
    )
    .map_err(err)?;
    for (note, state) in states(conn)? {
        let incarnation = uuid();
        conn.execute("INSERT INTO note_revision_counters VALUES(?1,0)", [&note])
            .map_err(err)?;
        conn.execute(
            "INSERT INTO note_incarnations VALUES(?1,?2,?3,NULL)",
            params![incarnation, note, now],
        )
        .map_err(err)?;
        insert_revision(conn, migration, &note, &incarnation, &state, None)?;
    }
    Ok(())
}
pub(crate) fn payload(conn: &Connection, revision: &str) -> Result<FullState, String> {
    let (body,title,tags,pinned,lifecycle,created,modified,extra) = conn.query_row("SELECT body,title,tags_json,pinned,lifecycle,note_created,note_modified,extra_json FROM revision_states WHERE revision_id=?1",[revision],|r| Ok((r.get::<_,Vec<u8>>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,bool>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?,r.get::<_,String>(7)?))).map_err(err)?;
    Ok(FullState {
        body,
        title,
        tags: serde_json::from_str(&tags).map_err(err)?,
        pinned,
        lifecycle,
        created,
        modified,
        extra: serde_json::from_str(&extra).map_err(err)?,
    })
}
pub(crate) fn validate(conn: &Connection) -> Result<(), String> {
    identity(conn)?;
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM vault_identity", [], |r| r.get(0))
        .map_err(err)?;
    if count != 1 {
        return Err("Invalid vault identity cardinality".into());
    }
    let integrity: String = conn
        .query_row("PRAGMA quick_check", [], |r| r.get(0))
        .map_err(err)?;
    if integrity != "ok" {
        return Err(format!("Vault integrity check failed: {integrity}"));
    }
    validate_foreign_keys(conn)?;
    let invalid: bool = conn.query_row("SELECT EXISTS(
        SELECT 1 FROM note_revision_heads h JOIN note_incarnations i ON i.incarnation_id=h.incarnation_id JOIN revisions r ON r.revision_id=h.revision_id JOIN note_revision_counters c ON c.note_id=h.note_id
        WHERE i.note_id<>h.note_id OR r.incarnation_id<>h.incarnation_id OR r.revision_no>c.high_water OR r.storage_epoch<>(SELECT storage_epoch FROM vault_identity WHERE singleton=1)
        UNION ALL SELECT 1 FROM note_revision_heads h JOIN revision_states s USING(revision_id) WHERE (s.lifecycle='deleted')<>NOT EXISTS(SELECT 1 FROM notes n WHERE n.id=h.note_id)
        UNION ALL SELECT 1 FROM revision_parents p JOIN revisions c ON c.revision_id=p.child_id JOIN revisions r ON r.revision_id=p.parent_id WHERE c.incarnation_id<>r.incarnation_id OR c.revision_no<=r.revision_no
        UNION ALL SELECT 1 FROM versions WHERE (ancestry='unknown' AND revision_id IS NOT NULL) OR (ancestry='verified' AND revision_id IS NULL)
        UNION ALL SELECT 1 FROM revisions r WHERE (SELECT COUNT(*) FROM revision_parents p WHERE p.child_id=r.revision_id AND p.role='predecessor') <> CASE WHEN r.kind IN ('create','migration_baseline') THEN 0 ELSE 1 END
        UNION ALL SELECT 1 FROM revisions r JOIN note_incarnations i USING(incarnation_id) JOIN note_revision_counters c ON c.note_id=i.note_id WHERE r.revision_no>c.high_water
        UNION ALL SELECT 1 FROM revisions r JOIN note_incarnations i USING(incarnation_id) GROUP BY i.note_id,r.revision_no HAVING COUNT(*)>1
        UNION ALL SELECT 1 FROM revisions WHERE (ancestry='unknown_baseline')<>(kind='migration_baseline')
        UNION ALL SELECT 1 FROM revision_states s JOIN revision_pruning p USING(revision_id)
        UNION ALL SELECT 1 FROM revisions r LEFT JOIN revision_states s USING(revision_id) LEFT JOIN revision_pruning p USING(revision_id) WHERE s.revision_id IS NULL AND p.revision_id IS NULL
    )",[],|r|r.get(0)).map_err(err)?;
    if invalid {
        return Err("Revision graph/head/counter validation failed".into());
    }
    let mut statement=conn.prepare("SELECT r.revision_id,r.state_sha256 FROM revisions r JOIN revision_states s USING(revision_id)").map_err(err)?;
    let rows = statement
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    for (revision, hash) in rows {
        let state = payload(conn, &revision)?;
        if state.hash()? != hash {
            return Err(format!("Revision payload digest mismatch: {revision}"));
        }
        let mut tags = state.tags.clone();
        tags.sort();
        tags.dedup();
        if tags != state.tags || std::str::from_utf8(&state.body).is_err() {
            return Err("Noncanonical revision state".into());
        }
        let expected: Vec<String> =
            serde_json::from_value(state.extra["attachments"].clone()).map_err(err)?;
        let mut statement = conn.prepare("SELECT attachment_id FROM revision_attachments WHERE revision_id=?1 ORDER BY attachment_id").map_err(err)?;
        let actual = statement
            .query_map([&revision], |r| r.get::<_, String>(0))
            .map_err(err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(err)?;
        if actual != expected {
            return Err(format!(
                "Revision attachment ownership mismatch: {revision}"
            ));
        }
    }
    let mut assets = conn
        .prepare("SELECT id,sha256,size,data FROM attachments")
        .map_err(err)?;
    let rows = assets
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Vec<u8>>(3)?,
            ))
        })
        .map_err(err)?;
    for row in rows {
        let (id, hash, size, bytes) = row.map_err(err)?;
        if size < 0 || bytes.len() as u64 != size as u64 || digest(&bytes) != hash {
            return Err(format!("Attachment byte/hash validation failed: {id}"));
        }
    }
    for (note, state) in states(conn)? {
        let revision: String = conn
            .query_row(
                "SELECT revision_id FROM note_revision_heads WHERE note_id=?1",
                [&note],
                |r| r.get(0),
            )
            .map_err(err)?;
        if payload(conn, &revision)? != state {
            return Err(format!("Revision projection mismatch: {note}"));
        }
    }
    Ok(())
}

/// Shared production migration boundary; ordinary opens use this protocol.
pub fn migrate(path: &Path, ledger_root: &Path) -> Result<Identity, String> {
    migrate_with_fault(path, ledger_root, Fault::None)
}
fn migrate_with_fault(path: &Path, ledger_root: &Path, fault: Fault) -> Result<Identity, String> {
    let version = inspect_version(path)?; // Must precede locks, snapshots or new files.
    if version == 3 {
        return Inspection::open(path)?.identity();
    }
    let _maintenance = Maintenance::exclusive(path)?;
    if !path.exists() {
        drop(private_file(path, true)?);
    }
    let conn = Connection::open(path).map_err(err)?;
    if schema_version(&conn)? != version {
        return Err("Vault schema changed while opening".into());
    }
    let identity = Identity {
        vault_uuid: uuid(),
        storage_epoch: uuid(),
    };
    let mut ledger = Ledger::acquire(ledger_root, &identity.vault_uuid)?;
    ledger.bind(path, &identity.storage_epoch)?;
    let notes = if version == 0 {
        Vec::new()
    } else {
        let mut stmt = conn
            .prepare("SELECT id FROM notes ORDER BY id")
            .map_err(err)?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(err)?
    };
    let mut numbers = BTreeMap::new();
    for note in notes {
        numbers.insert(note.clone(), ledger.reserve(&note, 0)?);
    }
    let migration = Migration {
        identity: identity.clone(),
        numbers,
        operation: uuid(),
        fault,
    };
    drop(VaultDb::init_target(conn, 3, Some(&migration))?);
    sync_snapshot(path).map_err(|e| format!("Migration committed but final durability sync failed; inspect read-only before retrying: {e}"))?;
    Ok(identity)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RestoreOutcome {
    pub identity: Identity,
    pub ledger_was_missing: bool,
    // No restored draft/review may silently rebind, even with an intact ledger.
    pub requires_recovery_review: bool,
}

/// Shared epoch-safe production restore boundary.
/// Exclusive maintenance refuses all managed live readers/owners. A v3 source
/// must be explicitly migrated first; restore never upgrades an unknown backup.
pub fn restore(path: &Path, backup: &Path, ledger_root: &Path) -> Result<RestoreOutcome, String> {
    restore_inner(path, backup, ledger_root, None)
}
/// Portable restoration admits an empty destination only, except exact replay
/// of the acknowledged operation. It never implicitly overwrites a live vault.
pub(crate) fn restore_portable(
    path: &Path,
    backup: &Path,
    ledger_root: &Path,
    key: &str,
    request: &str,
) -> Result<RestoreOutcome, String> {
    restore_inner(path, backup, ledger_root, Some((key, request)))
}
fn restore_inner(
    path: &Path,
    backup: &Path,
    ledger_root: &Path,
    portable: Option<(&str, &str)>,
) -> Result<RestoreOutcome, String> {
    let target_version = inspect_version(path)?;
    if (target_version != 3 && !(portable.is_some() && matches!(target_version, 0 | 2)))
        || inspect_version(backup)? != 3
    {
        return Err(
            "Ancestry restore requires a schema 3 source and compatible destination".into(),
        );
    }
    let source = Inspection::open(backup)?;
    let source_identity = source.identity()?;
    let _maintenance = Maintenance::exclusive(path)?;
    if inspect_version(path)? != target_version {
        return Err("Restore destination changed during preflight".into());
    }
    let target_identity = if target_version != 0 {
        let target =
            Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(err)?;
        if target_version == 3 {
            validate(&target)?;
        } else {
            validate_foreign_keys(&target)?;
        }
        if let Some((key, request)) = portable {
            let prior: Option<String> = target
                .query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
                .optional()
                .map_err(err)?;
            if let Some(prior) = prior {
                let prior: Value = serde_json::from_str(&prior).map_err(err)?;
                if prior["request"] != request {
                    return Err("Portable operation ID reused for different input".into());
                }
                let outcome: RestoreOutcome =
                    serde_json::from_value(prior["outcome"].clone()).map_err(err)?;
                if identity(&target)? != outcome.identity {
                    return Err("Portable receipt belongs to a retired restore epoch; explicit recovery required".into());
                }
                let mut ledger = Ledger::acquire(ledger_root, &outcome.identity.vault_uuid)?;
                if ledger.was_missing
                    || (ledger.data.storage_epoch != outcome.identity.storage_epoch
                        && ledger.data.pending_restore_epoch.as_deref()
                            != Some(outcome.identity.storage_epoch.as_str()))
                    || ledger
                        .data
                        .pending_restore_epoch
                        .as_deref()
                        .is_some_and(|e| e != outcome.identity.storage_epoch)
                {
                    return Err("Portable replay lost its ledger/epoch; explicit recovery enrollment required".into());
                }
                ledger.bind(path, &outcome.identity.storage_epoch)?;
                sync_snapshot(path)?;
                ledger.finish_restore(&outcome.identity.storage_epoch)?;
                return Ok(outcome);
            }
            for table in [
                "notes",
                "tags",
                "note_tags",
                "attachments",
                "note_attachments",
                "versions",
                "session_links",
            ] {
                let count: i64 = target
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                    .map_err(err)?;
                if count != 0 {
                    return Err("Full portable restore requires an empty destination; select reconciliation or independent copy".into());
                }
            }
            let metadata: i64 = target
                .query_row(
                    "SELECT COUNT(*) FROM meta WHERE key!='schema_version'",
                    [],
                    |r| r.get(0),
                )
                .map_err(err)?;
            if metadata != 0 {
                return Err("Destination contains metadata; explicit restore/copy requires a disposable empty destination".into());
            }
            if target_version == 3 {
                let graph:i64=target.query_row("SELECT (SELECT COUNT(*) FROM revisions)+(SELECT COUNT(*) FROM note_revision_counters)+(SELECT COUNT(*) FROM revision_pins)",[],|r|r.get(0)).map_err(err)?;
                if graph != 0 {
                    return Err("Destination has retained ancestry; it is not empty".into());
                }
            }
            source_identity.clone()
        } else {
            let identity = identity(&target)?;
            if identity.vault_uuid != source_identity.vault_uuid {
                return Err("Backup belongs to a different vault UUID".into());
            }
            identity
        }
    } else {
        source_identity.clone()
    };
    let mut ledger = Ledger::acquire(ledger_root, &source_identity.vault_uuid)?;
    let next = Identity {
        vault_uuid: source_identity.vault_uuid.clone(),
        storage_epoch: uuid(),
    };
    ledger.bind(path, &target_identity.storage_epoch)?;
    ledger.prepare_restore(&next.storage_epoch)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let stage = tempfile::NamedTempFile::new_in(parent).map_err(err)?;
    // query_only also forbids writing VACUUM's destination. Use a separate
    // READ_ONLY source handle, still under the inspection's shared lease.
    let snapshot_source =
        Connection::open_with_flags(backup, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(err)?;
    snapshot_source
        .execute(
            "VACUUM INTO ?1",
            params![stage.path().to_string_lossy().as_ref()],
        )
        .map_err(err)?;
    drop(snapshot_source);
    let conn = Connection::open(stage.path()).map_err(err)?;
    if identity(&conn)? != source_identity {
        return Err("Backup changed while preparing restore".into());
    }
    conn.pragma_update(None, "foreign_keys", true)
        .map_err(err)?;
    conn.pragma_update(None, "synchronous", "FULL")
        .map_err(err)?;
    let mut numbers = BTreeMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT note_id,high_water FROM note_revision_counters ORDER BY note_id")
            .map_err(err)?;
        let counters = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .map_err(err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(err)?;
        // Reserve/fsync all counters BEFORE acquiring the database write lock.
        for (note, high_water) in counters {
            numbers.insert(note.clone(), ledger.reserve(&note, high_water)?);
        }
    }
    let migration = Migration {
        identity: next.clone(),
        numbers,
        operation: uuid(),
        fault: Fault::None,
    };
    let tx = rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)
        .map_err(err)?;
    tx.execute(
        "UPDATE vault_identity SET storage_epoch=?1 WHERE singleton=1",
        [&next.storage_epoch],
    )
    .map_err(err)?;
    let heads = {
        let mut stmt=tx.prepare("SELECT note_id,incarnation_id,revision_id FROM note_revision_heads ORDER BY note_id").map_err(err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(err)?;
        rows
    };
    for (note, incarnation, previous) in heads {
        let state = payload(&tx, &previous)?;
        let restored_revision = insert_revision(
            &tx,
            &migration,
            &note,
            &incarnation,
            &state,
            Some(&previous),
        )?;
        // Restore preserves generated provenance only when the exported current
        // head was still exactly the last generated state. A manual A→B→A head
        // never gains generated authority by resemblance.
        let generated_key = format!("generated_summary:v3:{}", digest(note.as_bytes()));
        tx.execute(
            "UPDATE meta SET value=?2 WHERE key=?1 AND value=?3",
            params![generated_key, restored_revision, previous],
        )
        .map_err(err)?;
    }
    let outcome = RestoreOutcome {
        identity: next.clone(),
        ledger_was_missing: ledger.was_missing,
        requires_recovery_review: true,
    };
    if let Some((key, request)) = portable {
        tx.execute(
            "INSERT INTO meta(key,value) VALUES(?1,?2)",
            params![
                key,
                json!({"request":request,"outcome":outcome}).to_string()
            ],
        )
        .map_err(err)?;
    }
    validate(&tx)?;
    tx.commit().map_err(err)?;
    drop(conn);
    sync_snapshot(stage.path())?;
    // No managed connection can hold target WAL pages under this exclusive lock.
    // Checkpoint before replacing the main file; never replay stale WAL into it.
    if path.exists() {
        let old = Connection::open(path).map_err(err)?;
        let (busy, _, _): (i64, i64, i64) = old
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .map_err(err)?;
        if busy != 0 {
            return Err("Restore refused: an unmanaged connection holds the WAL".into());
        }
        drop(old);
    }
    for suffix in ["-wal", "-shm"] {
        if PathBuf::from(format!("{}{suffix}", path.display())).exists() {
            return Err(
                "Restore refused: an unmanaged SQLite connection still owns sidecars".into(),
            );
        }
    }
    stage.persist(path).map_err(err)?;
    sync_dir(parent).map_err(|e| format!("Restore replacement occurred but directory sync failed; reopen read-only for recovery: {e}"))?;
    ledger.finish_restore(&next.storage_epoch).map_err(|e| {
        format!(
            "Restore committed but ledger finalization failed; reopen read-only for recovery: {e}"
        )
    })?;
    Ok(RestoreOutcome {
        identity: next,
        ledger_was_missing: ledger.was_missing,
        requires_recovery_review: true,
    })
}

/// Compare every pre-existing source field, including BLOB bytes, across upgrade.
/// Only v1 orphan links and the attachment ownership representation may change.
pub(crate) struct PreservedProjection {
    queries: Vec<&'static str>,
    hash: String,
}
impl PreservedProjection {
    pub(crate) fn capture(conn: &Connection, version: i64) -> Result<Self, String> {
        let mut queries = vec![
            "SELECT id,title,content,pinned,trashed,created,modified FROM notes ORDER BY id",
            "SELECT id,note_id,content,created FROM versions ORDER BY id",
            "SELECT id,filename,mime_type,size,sha256,data,created FROM attachments ORDER BY id",
            "SELECT name FROM tags ORDER BY name",
            "SELECT note_id,tag FROM note_tags WHERE note_id IN (SELECT id FROM notes) ORDER BY note_id,tag",
            "SELECT key,value FROM meta ORDER BY key",
        ];
        if version == 2 {
            queries.push(
                "SELECT note_id,attachment_id FROM note_attachments ORDER BY note_id,attachment_id",
            );
        }
        let session_table: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='session_links')", [], |r| r.get(0)).map_err(err)?;
        if session_table {
            queries.push("SELECT note_id,session_id,created FROM session_links ORDER BY note_id");
        }
        let hash = Self::hash(conn, &queries)?;
        Ok(Self { queries, hash })
    }
    pub(crate) fn verify(&self, conn: &Connection) -> Result<(), String> {
        if Self::hash(conn, &self.queries)? != self.hash {
            return Err("Migration changed preserved source state".into());
        }
        Ok(())
    }
    fn hash(conn: &Connection, queries: &[&str]) -> Result<String, String> {
        let mut hash = Sha256::new();
        for query in queries {
            hash.update(query.as_bytes());
            let mut statement = conn.prepare(query).map_err(err)?;
            let columns = statement.column_count();
            let mut rows = statement.query([]).map_err(err)?;
            while let Some(row) = rows.next().map_err(err)? {
                hash.update([0xff]);
                for index in 0..columns {
                    use rusqlite::types::ValueRef;
                    match row.get_ref(index).map_err(err)? {
                        ValueRef::Null => hash.update([0]),
                        ValueRef::Integer(value) => {
                            hash.update([1]);
                            hash.update(value.to_le_bytes());
                        }
                        ValueRef::Real(value) => {
                            hash.update([2]);
                            hash.update(value.to_bits().to_le_bytes());
                        }
                        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => {
                            hash.update([
                                if matches!(row.get_ref(index).map_err(err)?, ValueRef::Text(_)) {
                                    3
                                } else {
                                    4
                                },
                            ]);
                            hash.update((bytes.len() as u64).to_le_bytes());
                            hash.update(bytes);
                        }
                    }
                }
            }
        }
        Ok(format!("{:x}", hash.finalize()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    struct Fixture {
        dir: tempfile::TempDir,
        path: PathBuf,
        ledger: PathBuf,
    }
    impl Fixture {
        fn new(version: i64) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("vault.rn");
            let ledger = dir.path().join("ledger");
            if version != 0 {
                let conn = Connection::open(&path).unwrap();
                conn.pragma_update(None, "journal_mode", "WAL").unwrap();
                conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
                conn.execute_batch(super::super::db::SCHEMA).unwrap();
                conn.execute(
                    "INSERT INTO meta VALUES('schema_version',?1)",
                    [version.to_string()],
                )
                .unwrap();
                conn.execute_batch("INSERT INTO notes VALUES('a','Title','  exact\r\nbody\n',1,0,'created','modified');
                    INSERT INTO notes VALUES('b','Trashed','asset',0,1,'created','modified');
                    INSERT INTO tags VALUES('tag'); INSERT INTO note_tags VALUES('a','tag');
                    INSERT INTO versions VALUES('legacy','a','old\r\nbody','history-time');
                    INSERT INTO session_links VALUES('a','session','linked');
                    INSERT INTO meta VALUES('notebook_layout','{\"notebooks\":[\"Work\"],\"notes\":{\"a\":\"Work\"}}');").unwrap();
                let bytes = [0, 1, 255, 13, 10];
                conn.execute("INSERT INTO attachments VALUES('asset','f.bin','application/octet-stream',?1,?2,?3,'then')",params![bytes.len() as i64,digest(&bytes),bytes.to_vec()]).unwrap();
                conn.execute_batch(
                    "INSERT INTO note_attachments VALUES('a','asset'),('b','asset');",
                )
                .unwrap();
                if version == 1 {
                    conn.execute_batch("DROP TABLE note_attachments;
                        ALTER TABLE attachments RENAME TO old_attachments;
                        CREATE TABLE attachments(id TEXT PRIMARY KEY,note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,filename TEXT NOT NULL,mime_type TEXT NOT NULL,size INTEGER NOT NULL,sha256 TEXT NOT NULL UNIQUE,data BLOB NOT NULL,created TEXT NOT NULL);
                        INSERT INTO attachments SELECT id,'a',filename,mime_type,size,sha256,data,created FROM old_attachments;
                        DROP TABLE old_attachments;
                        PRAGMA foreign_keys=OFF;
                        INSERT INTO note_tags VALUES('deleted','tag');
                        PRAGMA foreign_keys=ON;").unwrap();
                }
            }
            Self { dir, path, ledger }
        }
        fn raw(&self) -> Connection {
            Connection::open(&self.path).unwrap()
        }
        fn backup(&self) -> PathBuf {
            let path = self.dir.path().join("backup.rn");
            self.raw()
                .execute("VACUUM INTO ?1", params![path.to_string_lossy().as_ref()])
                .unwrap();
            path
        }
    }
    fn source_rows(conn: &Connection, version: i64) -> Vec<String> {
        // Compare exact semantic bytes in all source projections, not just counts.
        let mut queries=vec![
            "SELECT json_group_array(json_array(id,title,content,pinned,trashed,created,modified)) FROM (SELECT * FROM notes ORDER BY id)",
            "SELECT json_group_array(json_array(id,note_id,content,created)) FROM (SELECT id,note_id,content,created FROM versions ORDER BY id)",
            "SELECT json_group_array(json_array(id,filename,mime_type,size,sha256,hex(data),created)) FROM (SELECT * FROM attachments ORDER BY id)",
            "SELECT json_group_array(json_array(note_id,tag)) FROM (SELECT note_id,tag FROM note_tags WHERE note_id IN (SELECT id FROM notes) ORDER BY note_id,tag)",
            "SELECT json_group_array(json_array(note_id,session_id,created)) FROM (SELECT * FROM session_links ORDER BY note_id)",
        ];
        if version == 1 {
            queries.push("SELECT json_group_array(note_id) FROM attachments");
        }
        queries
            .into_iter()
            .map(|q| conn.query_row(q, [], |r| r.get(0)).unwrap())
            .collect()
    }

    #[test]
    fn empty_v1_v2_upgrade_preserves_full_state_unknown_history_and_private_wal_snapshot() {
        for version in [0, 1, 2] {
            let fixture = Fixture::new(version);
            // Keep an independent raw connection alive so committed pages remain in WAL.
            let wal = if version > 0 {
                Some(fixture.raw())
            } else {
                None
            };
            if let Some(conn) = &wal {
                conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
                conn.execute("UPDATE notes SET content=content||' WAL' WHERE id='a'", [])
                    .unwrap();
                assert!(
                    fs::metadata(fixture.path.with_extension("rn-wal"))
                        .unwrap()
                        .len()
                        > 0
                );
            }
            let before = wal.as_ref().map(|conn| source_rows(conn, 2));
            let original_rows = wal.as_ref().map(|conn| source_rows(conn, version));
            let id = migrate(&fixture.path, &fixture.ledger).unwrap();
            assert_eq!(inspect_version(&fixture.path).unwrap(), 3);
            assert!(
                VaultDb::open_legacy_v2(&fixture.path).is_err(),
                "frozen v2 compatibility opener must refuse v3"
            );
            let inspection = Inspection::open(&fixture.path).unwrap();
            assert_eq!(inspection.identity().unwrap(), id);
            let other = Inspection::open(&fixture.path).unwrap();
            assert_ne!(inspection.session_generation(), other.session_generation());
            assert_eq!(
                inspection.note_ids().unwrap().len(),
                if version > 0 { 2 } else { 0 }
            );
            drop(other);
            if version > 0 {
                assert_eq!(before.unwrap(), source_rows(&inspection.conn, 2));
                let note = inspection.note("a").unwrap().unwrap();
                assert_eq!(note["bodyBytes"], json!(b"  exact\r\nbody\n WAL".to_vec()));
                assert_eq!(
                    inspection.note("b").unwrap().unwrap()["lifecycle"],
                    "trashed"
                );
                let count: i64 = inspection
                    .conn
                    .query_row(
                        "SELECT COUNT(*) FROM revisions WHERE ancestry='unknown_baseline'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(count, 2);
                let history: (String, Option<String>) = inspection
                    .conn
                    .query_row(
                        "SELECT ancestry,revision_id FROM versions WHERE id='legacy'",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .unwrap();
                assert_eq!(history, ("unknown".into(), None));
                let backups: Vec<_> = fs::read_dir(fixture.dir.path())
                    .unwrap()
                    .map(|e| e.unwrap().path())
                    .filter(|p| p.extension().is_some_and(|s| s == "bak"))
                    .collect();
                assert_eq!(backups.len(), 1);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    assert_eq!(
                        fs::metadata(&backups[0]).unwrap().permissions().mode() & 0o777,
                        0o600
                    );
                }
                let backup =
                    Connection::open_with_flags(&backups[0], OpenFlags::SQLITE_OPEN_READ_ONLY)
                        .unwrap();
                assert_eq!(schema_version(&backup).unwrap(), version);
                assert_eq!(source_rows(&backup, version), original_rows.unwrap());
            }
            assert!(inspection
                .conn
                .execute("UPDATE notes SET title='write'", [])
                .is_err());
        }
    }

    #[test]
    fn snapshot_ddl_validation_and_commit_faults_leave_original_bytes_and_rows_untouched() {
        for version in [1, 2] {
            for fault in [
                Fault::Snapshot,
                Fault::Ddl,
                Fault::Validation,
                Fault::Commit,
            ] {
                let fixture = Fixture::new(version);
                let wal = fixture.raw();
                wal.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
                wal.execute(
                    "UPDATE notes SET content=content||' committed WAL' WHERE id='a'",
                    [],
                )
                .unwrap();
                assert!(
                    fs::metadata(fixture.path.with_extension("rn-wal"))
                        .unwrap()
                        .len()
                        > 0
                );
                let before_bytes = fs::read(&fixture.path).unwrap();
                let before = source_rows(&wal, version);
                let error = migrate_with_fault(&fixture.path, &fixture.ledger, fault).unwrap_err();
                assert!(error.contains("Injected ancestry"), "{error}");
                assert_eq!(inspect_version(&fixture.path).unwrap(), version);
                assert_eq!(source_rows(&fixture.raw(), version), before);
                assert_eq!(fs::read(&fixture.path).unwrap(), before_bytes);
                let count: i64 = fixture
                    .raw()
                    .query_row(
                        "SELECT COUNT(*) FROM sqlite_master WHERE name='revisions'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(count, 0);
                if version == 1 {
                    // Even the safe orphan sweep must roll back.
                    let orphan: i64 = fixture
                        .raw()
                        .query_row(
                            "SELECT COUNT(*) FROM note_tags WHERE note_id='deleted'",
                            [],
                            |r| r.get(0),
                        )
                        .unwrap();
                    assert_eq!(orphan, 1);
                }
            }
        }
    }

    #[test]
    fn malformed_and_future_versions_fail_before_any_artifact_or_header_change() {
        for version in ["4", "999", "0", "-1", "2x", "+2", "02", " 2", ""] {
            let fixture = Fixture::new(2);
            fixture
                .raw()
                .execute(
                    "UPDATE meta SET value=?1 WHERE key='schema_version'",
                    [version],
                )
                .unwrap();
            let before = fs::read(&fixture.path).unwrap();
            let names = fs::read_dir(fixture.dir.path()).unwrap().count();
            assert!(migrate(&fixture.path, &fixture.ledger).is_err());
            assert!(VaultDb::open_legacy_v2(&fixture.path).is_err());
            assert_eq!(fs::read(&fixture.path).unwrap(), before);
            assert_eq!(fs::read_dir(fixture.dir.path()).unwrap().count(), names);
            assert!(!fixture.ledger.exists());
        }
    }

    #[test]
    fn foreign_key_diagnostics_include_table_rowids_and_count_and_roll_back_orphan_sweep() {
        let fixture = Fixture::new(1);
        fixture
            .raw()
            .execute_batch(
                "PRAGMA foreign_keys=OFF;
             INSERT INTO note_tags(rowid,note_id,tag) VALUES(41,'a','missing'),(42,'b','missing');
             PRAGMA foreign_keys=ON;",
            )
            .unwrap();
        let before = source_rows(&fixture.raw(), 1);
        let error = migrate(&fixture.path, &fixture.ledger).unwrap_err();
        assert!(error.contains("2 violation(s)"), "{error}");
        for rowid in [41, 42] {
            assert!(
                error.contains(&format!("table=note_tags, rowid={rowid}")),
                "{error}"
            );
        }
        assert_eq!(source_rows(&fixture.raw(), 1), before);
        assert_eq!(inspect_version(&fixture.path).unwrap(), 1);
    }

    #[test]
    fn reservations_are_durable_ordered_and_fail_closed_on_corruption_or_disk_failure() {
        let fixture = Fixture::new(0);
        let id = uuid();
        {
            let mut ledger = Ledger::acquire(&fixture.ledger, &id).unwrap();
            assert!(ledger.was_missing);
            assert_eq!(ledger.reserve("a", 8).unwrap(), 9);
            assert!(Ledger::acquire(&fixture.ledger, &id).is_err());
        }
        {
            let mut ledger = Ledger::acquire(&fixture.ledger, &id).unwrap();
            assert!(!ledger.was_missing);
            assert_eq!(ledger.reserve("a", 2).unwrap(), 10);
            assert!(ledger.reserve("overflow", i64::MAX).is_err());
            // Rename the destination away and replace with a directory: atomic
            // persist must fail, never acknowledge a non-durable reservation.
            let saved = ledger.path.with_extension("saved");
            fs::rename(&ledger.path, &saved).unwrap();
            fs::create_dir(&ledger.path).unwrap();
            assert!(ledger.reserve("a", 0).is_err());
        }
        fs::remove_dir(fixture.ledger.join(format!("{id}.json"))).unwrap();
        fs::write(fixture.ledger.join(format!("{id}.json")), "broken").unwrap();
        assert!(Ledger::acquire(&fixture.ledger, &id).is_err());
    }

    #[test]
    fn uuid_survives_rename_restore_rotates_epoch_and_rollback_never_reuses_reserved_numbers() {
        let fixture = Fixture::new(2);
        let original = migrate(&fixture.path, &fixture.ledger).unwrap();
        let backup = fixture.backup();
        {
            let mut ledger = Ledger::acquire(&fixture.ledger, &original.vault_uuid).unwrap();
            assert_eq!(ledger.reserve("a", 200).unwrap(), 201);
        }
        let restored = restore(&fixture.path, &backup, &fixture.ledger)
            .unwrap()
            .identity;
        assert_eq!(restored.vault_uuid, original.vault_uuid);
        assert_ne!(restored.storage_epoch, original.storage_epoch);
        {
            let inspection = Inspection::open(&fixture.path).unwrap();
            assert_eq!(inspection.note("a").unwrap().unwrap()["revision"], 202);
            let edges: i64 = inspection
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM revision_parents WHERE role='predecessor'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(edges, 2);
            assert!(
                restore(&fixture.path, &backup, &fixture.ledger).is_err(),
                "live reader blocks restore"
            );
        }
        let renamed = fixture.dir.path().join("renamed.rn");
        fs::rename(&fixture.path, &renamed).unwrap();
        assert_eq!(
            Inspection::open(&renamed).unwrap().identity().unwrap(),
            restored
        );
        let next = restore(&renamed, &backup, &fixture.ledger)
            .unwrap()
            .identity;
        assert_eq!(next.vault_uuid, original.vault_uuid);
        assert_ne!(next.storage_epoch, restored.storage_epoch);
        assert_eq!(
            Inspection::open(&renamed)
                .unwrap()
                .note("a")
                .unwrap()
                .unwrap()["revision"],
            203
        );
        // Losing the host ledger cannot prove numeric ordering against lost
        // futures. A new epoch, not guessed ancestry, is the authority boundary.
        fs::remove_file(fixture.ledger.join(format!("{}.json", original.vault_uuid))).unwrap();
        let outcome = restore(&renamed, &backup, &fixture.ledger).unwrap();
        assert!(outcome.ledger_was_missing && outcome.requires_recovery_review);
        let reenrolled = outcome.identity;
        assert_eq!(reenrolled.vault_uuid, original.vault_uuid);
        assert_ne!(reenrolled.storage_epoch, next.storage_epoch);
        assert_eq!(
            Inspection::open(&renamed)
                .unwrap()
                .note("a")
                .unwrap()
                .unwrap()["revision"],
            2
        );
    }

    #[test]
    fn managed_v2_connections_block_migration_and_foreign_backup_never_replaces_target() {
        let fixture = Fixture::new(2);
        let live = VaultDb::open_legacy_v2(&fixture.path).unwrap();
        assert!(migrate(&fixture.path, &fixture.ledger).is_err());
        drop(live);
        let id = migrate(&fixture.path, &fixture.ledger).unwrap();
        let foreign = Fixture::new(2);
        migrate(&foreign.path, &foreign.ledger).unwrap();
        assert!(restore(&fixture.path, &foreign.path, &fixture.ledger).is_err());
        assert_eq!(
            Inspection::open(&fixture.path).unwrap().identity().unwrap(),
            id
        );
    }

    #[test]
    #[ignore = "subprocess fixture entry point; invoked by two_process_restore"]
    fn protocol_child() {
        let Some(role) = std::env::var_os("RYUK_ANCESTRY_CHILD") else {
            return;
        };
        let path = PathBuf::from(std::env::var_os("RYUK_ANCESTRY_VAULT").unwrap());
        let ready = PathBuf::from(std::env::var_os("RYUK_ANCESTRY_READY").unwrap());
        if role == "hold" {
            let _inspection = Inspection::open(&path).unwrap();
            fs::write(&ready, "ready").unwrap();
            let release = ready.with_extension("release");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !release.exists() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "parent never released child"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        } else {
            let backup = PathBuf::from(std::env::var_os("RYUK_ANCESTRY_BACKUP").unwrap());
            let ledger = PathBuf::from(std::env::var_os("RYUK_ANCESTRY_LEDGER").unwrap());
            let identity = restore(&path, &backup, &ledger).unwrap();
            fs::write(&ready, serde_json::to_vec(&identity).unwrap()).unwrap();
        }
    }

    #[test]
    fn two_process_restore_refuses_live_reader_then_reserves_above_other_process_commits() {
        let fixture = Fixture::new(2);
        migrate(&fixture.path, &fixture.ledger).unwrap();
        let backup = fixture.backup();
        let ready = fixture.dir.path().join("child-ready");
        let child_command = |role: &str| {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "vault::ancestry::tests::protocol_child",
                    "--ignored",
                    "--nocapture",
                ])
                .env("RYUK_ANCESTRY_CHILD", role)
                .env("RYUK_ANCESTRY_VAULT", &fixture.path)
                .env("RYUK_ANCESTRY_BACKUP", &backup)
                .env("RYUK_ANCESTRY_LEDGER", &fixture.ledger)
                .env("RYUK_ANCESTRY_READY", &ready);
            command
        };
        let mut child = child_command("hold").spawn().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !ready.exists() && std::time::Instant::now() < deadline {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let ready_in_time = ready.exists();
        let refused = restore(&fixture.path, &backup, &fixture.ledger).is_err();
        fs::write(ready.with_extension("release"), "release").unwrap();
        if !ready_in_time {
            let _ = child.kill();
        }
        let status = child.wait().unwrap();
        assert!(ready_in_time && status.success() && refused);
        fs::remove_file(&ready).unwrap();
        assert!(child_command("restore").status().unwrap().success());
        assert!(ready.exists(), "exact child test must actually execute");
        assert_eq!(
            Inspection::open(&fixture.path)
                .unwrap()
                .note("a")
                .unwrap()
                .unwrap()["revision"],
            2
        );
        let previous = Inspection::open(&fixture.path).unwrap().identity().unwrap();
        fs::remove_file(&ready).unwrap();
        assert!(child_command("restore").status().unwrap().success());
        assert!(ready.exists());
        let after = Inspection::open(&fixture.path).unwrap();
        assert_eq!(after.note("a").unwrap().unwrap()["revision"], 3);
        assert_eq!(after.identity().unwrap().vault_uuid, previous.vault_uuid);
        assert_ne!(
            after.identity().unwrap().storage_epoch,
            previous.storage_epoch
        );
    }

    // Frozen schema-version gate from af52dd5's v2 opener (SCHEMA_VERSION=2).
    // Audited with read-only git show; the full old
    // client can additionally be exercised host-side via the ignored test below.
    fn frozen_v2_gate(path: &Path) -> Result<(), String> {
        let conn = Connection::open(path).map_err(err)?;
        let raw: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key='schema_version'",
                [],
                |r| r.get(0),
            )
            .map_err(err)?;
        let version = raw.parse::<i64>().map_err(err)?;
        if !(0..=2).contains(&version) {
            return Err(format!("Unsupported vault schema version {version}"));
        }
        Ok(())
    }
    #[test]
    fn frozen_v2_gate_and_explicit_legacy_fixture_stay_v2() {
        let fixture = Fixture::new(0);
        drop(VaultDb::open_legacy_v2(&fixture.path).unwrap());
        assert_eq!(inspect_version(&fixture.path).unwrap(), 2);
        migrate(&fixture.path, &fixture.ledger).unwrap();
        assert!(frozen_v2_gate(&fixture.path)
            .unwrap_err()
            .contains("Unsupported vault schema version 3"));
    }

    #[test]
    #[ignore = "host-side optional real pre-4B CLI binary, RYUK_V2_BINARY"]
    fn actual_v2_cli_refuses_v3() {
        let Some(binary) = std::env::var_os("RYUK_V2_BINARY") else {
            return;
        };
        let fixture = Fixture::new(2);
        migrate(&fixture.path, &fixture.ledger).unwrap();
        let output = Command::new(binary)
            .arg("--vault")
            .arg(&fixture.path)
            .args(["notes", "list"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("Unsupported vault schema version 3")
        );
    }
    #[test]
    fn activated_open_creates_v3_and_migrates_v1_v2_with_receipt_and_private_snapshot() {
        for version in [0, 1, 2] {
            let fixture = Fixture::new(version);
            let before = if version > 0 {
                Some(source_rows(&fixture.raw(), 2))
            } else {
                None
            };
            let (db, report) = VaultDb::open_with_report(&fixture.path, &fixture.ledger).unwrap();
            assert!(db.is_v3());
            assert_eq!(report.schema, 3);
            assert_eq!(report.previous_schema, version);
            if version > 0 {
                assert!(report
                    .status
                    .as_deref()
                    .unwrap()
                    .contains("pre-migration snapshot"));
                assert_eq!(source_rows(&db.conn, 2), before.unwrap());
                let snapshots = fs::read_dir(fixture.dir.path())
                    .unwrap()
                    .map(Result::unwrap)
                    .filter(|entry| entry.file_name().to_string_lossy().contains(".pre-v3-"))
                    .collect::<Vec<_>>();
                assert_eq!(snapshots.len(), 1);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    assert_eq!(
                        snapshots[0].metadata().unwrap().permissions().mode() & 0o777,
                        0o600
                    );
                }
                assert!(
                    db.accepted_note("a").unwrap().unwrap()["accepted"]["revision"].is_object()
                );
            }
            assert_eq!(
                db.conn
                    .pragma_query_value(None, "journal_mode", |r| r.get::<_, String>(0))
                    .unwrap(),
                "wal"
            );
            assert!(frozen_v2_gate(&fixture.path)
                .unwrap_err()
                .contains("Unsupported vault schema version 3"));
            drop(db);
            let (again, report) =
                VaultDb::open_with_report(&fixture.path, &fixture.ledger).unwrap();
            assert_eq!(report.previous_schema, 3);
            assert!(report.status.is_none());
            assert!(again.is_v3());
        }
    }
    #[test]
    fn activated_open_refuses_managed_legacy_connection_and_missing_v3_ledger() {
        let fixture = Fixture::new(2);
        let legacy = VaultDb::open_legacy_v2(&fixture.path).unwrap();
        assert!(VaultDb::open_with_report(&fixture.path, &fixture.ledger).is_err());
        assert_eq!(inspect_version(&fixture.path).unwrap(), 2);
        drop(legacy);
        let (db, report) = VaultDb::open_with_report(&fixture.path, &fixture.ledger).unwrap();
        drop(db);
        fs::remove_file(fixture.ledger.join(format!("{}.json", report.vault_uuid))).unwrap();
        assert!(VaultDb::open_with_report(&fixture.path, &fixture.ledger)
            .err()
            .unwrap()
            .contains("enrollment"));
        assert_eq!(inspect_version(&fixture.path).unwrap(), 3);
    }
}
