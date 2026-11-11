//! Owner-scoped recovery. Hints are never acknowledgments; journals never save
//! the original. No SQLite schema change and no text merge.
use crate::state::AppState;
use crate::vault::db::{NewNote, VaultDb};
use crate::vault::writer::{create_on, VaultIdentity};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use tauri::{Emitter, Manager, Runtime, State};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Blob {
    pub reference: String,
    pub name: String,
    pub mime: String,
    pub data: Vec<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_bytes: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Draft {
    pub owner: String,
    pub sequence: u64,
    pub revision: u64,
    pub captured_at: String,
    pub vault: VaultIdentity,
    pub note_id: String,
    pub base: Value,
    pub body: String,
    pub title: String,
    pub tags: Vec<String>,
    pub pinned: bool,
    pub blobs: Vec<Blob>,
    pub conflicted: bool,
}
#[derive(Clone)]
struct Owner {
    window: String,
    vault: VaultIdentity,
    note: String,
    dirty: bool,
    conflicted: bool,
    locked: bool,
    sequence: u64,
}
#[derive(Default)]
pub struct Recovery {
    root: Option<PathBuf>,
    owners: HashMap<String, Owner>,
    barrier: Option<(String, String)>,
    versions: HashMap<String, (u64, VaultIdentity, i64)>,
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn valid_owner(id: &str) -> Result<(), String> {
    uuid::Uuid::parse_str(id)
        .map(|_| ())
        .map_err(|_| "Invalid recovery owner".into())
}
fn private_dir(path: &Path) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|e| e.to_string())?;
    if std::fs::symlink_metadata(path)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_symlink()
    {
        return Err("Recovery directory must not be a symlink".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
        std::fs::File::open(path)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::File::open(parent)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
/// Publish a complete immutable stream record, fsync before rename and fsync
/// the directory. The caller serializes this stream under the service mutex.
fn atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("Invalid recovery path")?;
    private_dir(parent)?;
    let temp = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp).map_err(|e| e.to_string())?;
        file.write_all(bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        std::fs::rename(&temp, path).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        std::fs::File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}
impl Recovery {
    pub fn set_root(&mut self, path: PathBuf) -> Result<(), String> {
        private_dir(&path)?;
        self.root = Some(path);
        Ok(())
    }
    fn dir(&self, owner: &str) -> Result<PathBuf, String> {
        valid_owner(owner)?;
        Ok(self
            .root
            .as_ref()
            .ok_or("Recovery storage unavailable")?
            .join(owner))
    }
    fn owner(&self, id: &str, window: &str) -> Result<&Owner, String> {
        self.owners
            .get(id)
            .filter(|o| o.window == window)
            .ok_or("Recovery owner belongs to another window or session".into())
    }
    pub fn records(&self) -> Result<Vec<Draft>, String> {
        let Some(root) = &self.root else {
            return Err("Recovery storage unavailable".into());
        };
        let mut out = Vec::new();
        for entry in std::fs::read_dir(root).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if !entry.file_type().map_err(|e| e.to_string())?.is_dir() {
                continue;
            }
            let owner = entry.file_name().to_string_lossy().into_owned();
            if valid_owner(&owner).is_err() {
                continue;
            }
            let mut resolved = 0;
            let mut latest: Option<(u64, PathBuf)> = None;
            for file in std::fs::read_dir(entry.path()).map_err(|e| e.to_string())? {
                let file = file.map_err(|e| e.to_string())?;
                if !file.file_type().map_err(|e| e.to_string())?.is_file() {
                    continue;
                }
                let name = file.file_name().to_string_lossy().into_owned();
                if let Some(n) = name
                    .strip_prefix("resolved-")
                    .and_then(|n| n.parse::<u64>().ok())
                {
                    resolved = resolved.max(n);
                }
                if let Some(n) = name
                    .strip_suffix(".json")
                    .and_then(|n| n.parse::<u64>().ok())
                {
                    if latest.as_ref().is_none_or(|(previous, _)| n > *previous) {
                        latest = Some((n, file.path()));
                    }
                }
            }
            if let Some((sequence, path)) = latest.filter(|(n, _)| *n > resolved) {
                let draft = read_draft(&path)?;
                if draft.owner != owner || draft.sequence != sequence {
                    return Err("Recovery owner/sequence mismatch".into());
                }
                out.push(draft);
            }
        }
        Ok(out)
    }
    pub fn assert_quiet(
        &self,
        vault: Option<&str>,
        roots: Option<&[String]>,
    ) -> Result<(), String> {
        let canonical = vault.map(|v| {
            std::fs::canonicalize(v)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| v.into())
        });
        let affected = |path: &str, note: &str| {
            canonical.as_deref().is_none_or(|v| v == path)
                && roots.is_none_or(|rs| {
                    rs.iter()
                        .any(|r| note == r || note.starts_with(&(r.clone() + "/")))
                })
        };
        if self
            .owners
            .values()
            .any(|o| affected(&o.vault.path, &o.note) && (!o.locked || o.dirty || o.conflicted))
        {
            return Err(
                "Resolve drafts and pause affected editors in every window before continuing."
                    .into(),
            );
        }
        // Restart artifacts are never permission to discard. Refuse even when
        // no renderer remains alive to supply an in-memory dirty flag.
        if self.root.is_some()
            && self
                .records()?
                .iter()
                .any(|d| affected(&d.vault.path, &d.note_id))
        {
            return Err("Resolve recovered drafts before continuing.".into());
        }
        Ok(())
    }
    fn checkpoint(&mut self, window: &str, draft: &Draft) -> Result<(), String> {
        let owner = self.owner(&draft.owner, window)?;
        if owner.vault != draft.vault
            || owner.note != draft.note_id
            || draft.sequence <= owner.sequence
        {
            return Err("Stale or rebound recovery checkpoint".into());
        }
        // Mark unsafe BEFORE disk I/O. A disk failure must never authorize close.
        let owner = self.owners.get_mut(&draft.owner).unwrap();
        owner.dirty = true;
        owner.conflicted |= draft.conflicted;
        validate_blobs(draft)?;
        let dir = self.dir(&draft.owner)?;
        let mut persisted = draft.clone();
        for blob in &mut persisted.blobs {
            let hash = digest(&blob.data);
            let path = dir.join(format!("blob-{hash}"));
            if path.exists() {
                if digest(&std::fs::read(&path).map_err(|e| e.to_string())?) != hash {
                    return Err("Recovery byte checksum mismatch".into());
                }
            } else {
                atomic(&path, &blob.data)?;
            }
            blob.checkpoint_bytes = Some(hash);
            blob.data.clear();
        }
        let bytes = serde_json::to_vec(&persisted).map_err(|e| e.to_string())?;
        atomic(&dir.join(format!("{:020}.json", draft.sequence)), &bytes)?;
        self.owners.get_mut(&draft.owner).unwrap().sequence = draft.sequence;
        Ok(())
    }
    fn resolve(&mut self, window: &str, id: &str, sequence: u64) -> Result<(), String> {
        let owner = self.owner(id, window)?;
        if owner.sequence != sequence {
            return Err("A newer draft still needs recovery".into());
        }
        let dir = self.dir(id)?;
        // A file can have been published before a failed directory fsync/reply.
        // Resolve all already-published records in this serialized owner stream,
        // never a checkpoint arriving after this resolution. The renderer has
        // checked the captured local revision before requesting resolution.
        let mut published = sequence;
        match std::fs::read_dir(&dir) {
            Ok(files) => {
                for file in files {
                    let file = file.map_err(|e| e.to_string())?;
                    let name = file.file_name().to_string_lossy().into_owned();
                    if let Some(n) = name
                        .strip_suffix(".json")
                        .and_then(|n| n.parse::<u64>().ok())
                    {
                        published = published.max(n);
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
        atomic(&dir.join(format!("resolved-{published}")), b"resolved")?;
        let owner = self.owners.get_mut(id).unwrap();
        owner.dirty = false;
        owner.conflicted = false;
        Ok(())
    }
}
fn read_draft(path: &Path) -> Result<Draft, String> {
    let mut draft: Draft = serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    for blob in &mut draft.blobs {
        if let Some(hash) = &blob.checkpoint_bytes {
            if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("Invalid recovery byte identity".into());
            }
            let data = std::fs::read(
                path.parent()
                    .ok_or("Missing recovery directory")?
                    .join(format!("blob-{hash}")),
            )
            .map_err(|e| e.to_string())?;
            if digest(&data) != *hash {
                return Err("Recovery byte checksum mismatch".into());
            }
            blob.data = data;
        }
    }
    Ok(draft)
}
fn validate_blobs(draft: &Draft) -> Result<(), String> {
    let refs =
        regex::Regex::new(r#"(?:\]\(|(?:src|href)=["']|<)((?:blob:|\.rn-recovery/)[^\s)"'<>]+)"#)
            .map_err(|e| e.to_string())?;
    for matched in refs.captures_iter(&draft.body) {
        if !draft.blobs.iter().any(|b| b.reference == matched[1]) {
            return Err("Transient attachment bytes are not durable".into());
        }
    }
    for blob in &draft.blobs {
        if blob.reference.is_empty() || blob.data.len() > 25 * 1024 * 1024 {
            return Err("Invalid recovery attachment".into());
        }
    }
    Ok(())
}
fn init<R: Runtime>(app: &tauri::AppHandle<R>, state: &AppState) -> Result<(), String> {
    let mut service = state.recovery.lock().map_err(|e| e.to_string())?;
    if service.root.is_none() {
        service.set_root(
            app.path()
                .app_local_data_dir()
                .map_err(|e| e.to_string())?
                .join("recovery-v1"),
        )?;
    }
    Ok(())
}
#[tauri::command]
pub fn recovery_register<R: Runtime>(
    window: tauri::Window<R>,
    state: State<'_, AppState>,
    owner: String,
    vault: VaultIdentity,
    note_id: String,
) -> Result<(), String> {
    init(window.app_handle(), &state)?;
    valid_owner(&owner)?;
    if vault.path.ends_with(".rn") {
        let slot = state.vault_db.lock().map_err(|e| e.to_string())?;
        slot.as_ref()
            .ok_or("Originating vault is closed")?
            .validate_writer_vault(&vault)
            .map_err(|e| e.to_string())?;
    } else if filesystem_identity(Path::new(&vault.path))? != vault {
        return Err("Originating vault identity changed".into());
    }
    let mut service = state.recovery.lock().map_err(|e| e.to_string())?;
    if let Some(existing) = service.owners.get(&owner) {
        if existing.window == window.label() && existing.vault == vault && existing.note == note_id
        {
            return Ok(());
        }
        return Err("Recovery owner already registered in another session".into());
    }
    if service.barrier.is_some() {
        return Err("A vault operation is pausing new editor owners; retry shortly".into());
    }
    service.owners.insert(
        owner,
        Owner {
            window: window.label().into(),
            vault,
            note: note_id,
            dirty: false,
            conflicted: false,
            locked: false,
            sequence: 0,
        },
    );
    Ok(())
}
#[tauri::command]
pub fn recovery_status<R: Runtime>(
    window: tauri::Window<R>,
    state: State<'_, AppState>,
    owner: String,
    dirty: bool,
    conflicted: bool,
    locked: bool,
) -> Result<(), String> {
    let mut service = state.recovery.lock().map_err(|e| e.to_string())?;
    service.owner(&owner, window.label())?;
    let o = service.owners.get_mut(&owner).unwrap();
    // Clean requires a persisted resolution, not a renderer setting dirty=false.
    o.dirty |= dirty;
    o.conflicted |= conflicted;
    o.locked = locked;
    Ok(())
}
#[tauri::command]
pub fn recovery_checkpoint<R: Runtime>(
    window: tauri::Window<R>,
    state: State<'_, AppState>,
    mut draft: Draft,
) -> Result<(), String> {
    {
        let mut service = state.recovery.lock().map_err(|e| e.to_string())?;
        let owner = service.owner(&draft.owner, window.label())?;
        if owner.vault != draft.vault || owner.note != draft.note_id {
            return Err("Recovery owner rebinding refused".into());
        }
        let owner = service.owners.get_mut(&draft.owner).unwrap();
        owner.dirty = true;
        owner.conflicted |= draft.conflicted;
    }
    // Include existing shared BLOBs as bytes, so replacement of the vault cannot
    // make a durable recovery artifact dependent on a disappearing source row.
    if draft.vault.path.ends_with(".rn") {
        let slot = state.vault_db.lock().map_err(|e| e.to_string())?;
        let db = slot.as_ref().ok_or("Originating vault is closed")?;
        db.validate_writer_vault(&draft.vault)
            .map_err(|e| e.to_string())?;
        let refs =
            regex::Regex::new(r#"(?:\]\(|(?:src|href)=["']|<)(\.rn-attachments/([a-zA-Z0-9-]+))"#)
                .map_err(|e| e.to_string())?;
        let referenced = format!(
            "{}\n{}\n{}",
            draft.body,
            draft.base["content"].as_str().unwrap_or(""),
            draft.base["raw"].as_str().unwrap_or("")
        );
        for cap in refs.captures_iter(&referenced) {
            let reference = cap[1].to_string();
            if draft.blobs.iter().any(|b| b.reference == reference) {
                continue;
            }
            let (meta, data) = db
                .get_attachment_data(&cap[2])?
                .ok_or("Draft attachment no longer exists")?;
            draft.blobs.push(Blob {
                reference,
                name: meta.filename,
                mime: meta.mime_type,
                data,
                checkpoint_bytes: None,
            });
        }
    }
    state
        .recovery
        .lock()
        .map_err(|e| e.to_string())?
        .checkpoint(window.label(), &draft)
}
#[tauri::command]
pub fn recovery_resolve<R: Runtime>(
    window: tauri::Window<R>,
    state: State<'_, AppState>,
    owner: String,
    sequence: u64,
) -> Result<(), String> {
    state
        .recovery
        .lock()
        .map_err(|e| e.to_string())?
        .resolve(window.label(), &owner, sequence)
}
#[tauri::command]
pub fn recovery_unregister<R: Runtime>(
    window: tauri::Window<R>,
    state: State<'_, AppState>,
    owner: String,
) -> Result<(), String> {
    let mut service = state.recovery.lock().map_err(|e| e.to_string())?;
    let o = service.owner(&owner, window.label())?;
    if o.dirty || o.conflicted {
        return Err("Owner still has an unresolved draft".into());
    }
    service.owners.remove(&owner);
    service
        .versions
        .remove(&format!("{}:{owner}", window.label()));
    Ok(())
}
#[tauri::command]
pub fn recovery_list<R: Runtime>(
    window: tauri::Window<R>,
    state: State<'_, AppState>,
) -> Result<Vec<Draft>, String> {
    init(window.app_handle(), &state)?;
    let service = state.recovery.lock().map_err(|e| e.to_string())?;
    Ok(service
        .records()?
        .into_iter()
        .filter(|d| !service.owners.contains_key(&d.owner))
        .collect())
}
#[tauri::command]
pub fn recovery_claim<R: Runtime>(
    window: tauri::Window<R>,
    state: State<'_, AppState>,
    owner: String,
) -> Result<Draft, String> {
    init(window.app_handle(), &state)?;
    let mut service = state.recovery.lock().map_err(|e| e.to_string())?;
    if service.owners.contains_key(&owner) {
        return Err("Recovery is active in another editor".into());
    }
    let draft = service
        .records()?
        .into_iter()
        .find(|d| d.owner == owner)
        .ok_or("Recovery record not found")?;
    service.owners.insert(
        owner,
        Owner {
            window: window.label().into(),
            vault: draft.vault.clone(),
            note: draft.note_id.clone(),
            dirty: true,
            conflicted: true,
            locked: false,
            sequence: draft.sequence,
        },
    );
    Ok(draft)
}
/// One connection's data_version only compares to its own previous observation.
/// Own-process commits also emit hints; focus/resume forces a reread regardless.
#[tauri::command]
pub fn recovery_poll<R: Runtime>(
    window: tauri::Window<R>,
    state: State<'_, AppState>,
    owner: String,
) -> Result<bool, String> {
    let mut service = state.recovery.lock().map_err(|e| e.to_string())?;
    let o = service.owner(&owner, window.label())?.clone();
    // Do not hold registry -> DB lock: checkpoint uses DB -> registry.
    drop(service);
    let slot = state.vault_db.lock().map_err(|e| e.to_string())?;
    let Some(db) = slot.as_ref() else {
        return Ok(true);
    };
    if db.identity() != o.vault || db.validate_writer_vault(&o.vault).is_err() {
        return Ok(true);
    }
    let version: i64 = db
        .conn
        .pragma_query_value(None, "data_version", |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let identity = db.identity();
    let epoch = state
        .vault_db_epoch
        .load(std::sync::atomic::Ordering::Acquire);
    drop(slot);
    service = state.recovery.lock().map_err(|e| e.to_string())?;
    let key = format!("{}:{}", window.label(), owner);
    let changed = service
        .versions
        .get(&key)
        .is_none_or(|previous| previous != &(epoch, identity.clone(), version));
    service.versions.insert(key, (epoch, identity, version));
    drop(service);
    if changed {
        let _ = window
            .app_handle()
            .emit("note-invalidated", json!({"vault":o.vault,"noteId":o.note}));
    }
    Ok(changed)
}

/// The operation ID is owner+checkpoint sequence. Its payload hash and exact
/// receipt commit beside the new note in existing meta storage. Lost replies can
/// be retried, but changing a captured draft cannot reuse the same operation ID.
pub fn fork(db: &VaultDb, draft: &Draft) -> Result<Value, String> {
    db.validate_writer_vault(&draft.vault)
        .map_err(|e| e.to_string())?;
    validate_blobs(draft)?;
    let tx =
        rusqlite::Transaction::new_unchecked(&db.conn, rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
    let key = format!("recovery_fork:v1:{}:{}", draft.owner, draft.sequence);
    let mut payload_draft = draft.clone();
    for blob in &mut payload_draft.blobs {
        blob.checkpoint_bytes = None;
    }
    let payload = digest(&serde_json::to_vec(&payload_draft).map_err(|e| e.to_string())?);
    let prior: Option<String> = tx
        .query_row("SELECT value FROM meta WHERE key=?1", [&key], |r| r.get(0))
        .optional()
        .map_err(|e| e.to_string())?;
    if let Some(prior) = prior {
        let prior: Value = serde_json::from_str(&prior).map_err(|e| e.to_string())?;
        if prior["payload"] != payload {
            return Err("Fork operation ID was reused for another draft".into());
        }
        return Ok(prior["receipt"].clone());
    }
    let now = chrono::Utc::now().to_rfc3339();
    let tags = copy_tags(draft);
    let mut note = NewNote {
        id: uuid::Uuid::new_v4().to_string(),
        title: format!("{} (recovery copy)", draft.title),
        content: draft.body.clone(),
        tags,
        pinned: draft.pinned,
        created: now.clone(),
        modified: now.clone(),
    };
    create_on(db, &tx, &note)?;
    for blob in &draft.blobs {
        let id = db.store_attachment_on(&tx, &note.id, &blob.name, &blob.mime, &blob.data, &now)?;
        note.content = note
            .content
            .replace(&blob.reference, &format!(".rn-attachments/{id}"));
    }
    VaultDb::write_note(&tx, &note, None, None)?;
    // Keep provenance as metadata, never a session_links row/generated digest.
    let receipt = db
        .accepted_note(&note.id)
        .map_err(|e| e.to_string())?
        .ok_or("Fork disappeared")?;
    tx.execute(
        "INSERT INTO meta(key,value) VALUES(?1,?2)",
        params![
            key,
            json!({"payload":payload,"source":draft.note_id,"receipt":receipt}).to_string()
        ],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(receipt)
}
#[tauri::command]
pub fn recovery_fork<R: Runtime>(
    window: tauri::Window<R>,
    state: State<'_, AppState>,
    owner: String,
    sequence: u64,
) -> Result<Value, String> {
    let service = state.recovery.lock().map_err(|e| e.to_string())?;
    service.owner(&owner, window.label())?;
    let draft = read_draft(&service.dir(&owner)?.join(format!("{sequence:020}.json")))?;
    if draft.owner != owner || draft.sequence != sequence {
        return Err("Recovery checkpoint identity mismatch".into());
    }
    drop(service);
    let receipt = if draft.vault.path.ends_with(".rn") {
        let slot = state.vault_db.lock().map_err(|e| e.to_string())?;
        if let Some(db) = slot.as_ref().filter(|db| db.identity() == draft.vault) {
            fork(db, &draft)?
        } else {
            drop(slot);
            // Restart recovery does not retarget the displayed vault. A private
            // checkpoint may reopen only its exact originating file identity.
            if filesystem_identity(Path::new(&draft.vault.path))? != draft.vault {
                return Err("Originating vault identity changed".into());
            }
            let db = VaultDb::open(Path::new(&draft.vault.path))?;
            fork(&db, &draft)?
        }
    } else {
        fork_filesystem(&draft)?
    };
    let _ = window.app_handle().emit(
        "note-invalidated",
        json!({"vault":draft.vault,"noteId":receipt["id"]}),
    );
    Ok(receipt)
}

pub fn close_window(state: &AppState, label: &str) -> Result<(), String> {
    let service = state.recovery.lock().map_err(|e| e.to_string())?;
    if service
        .owners
        .values()
        .any(|o| o.window == label && (o.dirty || o.conflicted || !o.locked))
    {
        return Err("This window still has an unresolved editor owner".into());
    }
    Ok(())
}
#[tauri::command]
pub fn recovery_barrier<R: Runtime>(
    window: tauri::Window<R>,
    state: State<'_, AppState>,
    vault: String,
    roots: Option<Vec<String>>,
) -> Result<String, String> {
    let mut service = state.recovery.lock().map_err(|e| e.to_string())?;
    if service.barrier.is_some() {
        return Err("Another window owns a vault operation".into());
    }
    service.assert_quiet(Some(&vault), roots.as_deref())?;
    let token = uuid::Uuid::new_v4().to_string();
    service.barrier = Some((token.clone(), window.label().into()));
    Ok(token)
}
#[tauri::command]
pub fn recovery_release_barrier<R: Runtime>(
    window: tauri::Window<R>,
    state: State<'_, AppState>,
    token: String,
) -> Result<(), String> {
    let mut service = state.recovery.lock().map_err(|e| e.to_string())?;
    if service.barrier.as_ref() != Some(&(token, window.label().into())) {
        return Err("Vault barrier belongs to another window or operation".into());
    }
    service.barrier = None;
    Ok(())
}

fn filesystem_identity(path: &Path) -> Result<VaultIdentity, String> {
    let path = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
    let metadata = std::fs::metadata(&path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::MetadataExt;
        format!("{}:{}", metadata.dev(), metadata.ino())
    };
    #[cfg(not(unix))]
    let file = format!("{:?}", metadata.created().map_err(|e| e.to_string())?);
    Ok(VaultIdentity {
        path: path.to_string_lossy().into_owned(),
        file,
    })
}
#[tauri::command]
pub fn recovery_origin(
    state: State<'_, AppState>,
    vault_path: String,
) -> Result<VaultIdentity, String> {
    state.validate_vault(&vault_path)?;
    filesystem_identity(Path::new(&vault_path))
}
fn copy_tags(draft: &Draft) -> Vec<String> {
    let mut tags: Vec<_> = draft
        .tags
        .iter()
        .filter(|t| t.as_str() != "hermes/session")
        .cloned()
        .collect();
    if draft.tags.iter().any(|t| t == "hermes/session") {
        tags.push("hermes/recovery".into());
    }
    tags
}
fn fork_filesystem(draft: &Draft) -> Result<Value, String> {
    let root = Path::new(&draft.vault.path);
    if filesystem_identity(root)? != draft.vault {
        return Err("The originating vault was replaced".into());
    }
    validate_blobs(draft)?;
    let parent = std::fs::canonicalize(
        Path::new(&draft.note_id)
            .parent()
            .ok_or("Missing note directory")?,
    )
    .map_err(|e| e.to_string())?;
    if !parent.starts_with(root) {
        return Err("Recovery copy must remain in its originating vault".into());
    }
    let id = digest(format!("{}:{}", draft.owner, draft.sequence).as_bytes());
    let destination = parent.join(format!("Recovery {}.md", &id[..20]));
    let mut body = draft.body.clone();
    for blob in &draft.blobs {
        let extension = match blob.mime.as_str() {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/gif" => "gif",
            "image/webp" => "webp",
            "application/pdf" => "pdf",
            _ => "bin",
        };
        let reference = format!(
            ".ryuknotes/attachments/recovery-{}.{}",
            digest(&blob.data),
            extension
        );
        let path = root.join(&reference);
        if path.exists() {
            if std::fs::read(&path).map_err(|e| e.to_string())? != blob.data {
                return Err("Recovered attachment identity collision".into());
            }
        } else {
            atomic(&path, &blob.data)?;
        }
        body = body.replace(&blob.reference, &reference);
    }
    let captured = chrono::DateTime::parse_from_rfc3339(&draft.captured_at)
        .map_err(|e| e.to_string())?
        .with_timezone(&chrono::Utc);
    let meta = crate::types::NoteMeta {
        id,
        title: format!("{} (recovery copy)", draft.title),
        tags: copy_tags(draft),
        pinned: draft.pinned,
        created: captured,
        modified: captured,
    };
    let raw = crate::vault::frontmatter::update_note_raw(&meta, &body);
    // Publish a complete file with an exclusive hard link. A retry never replaces
    // a previously published fork, including one edited after a lost response.
    if !destination.exists() {
        let temp = parent.join(format!(".recovery-{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temp).map_err(|e| e.to_string())?;
            file.write_all(raw.as_bytes()).map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            match std::fs::hard_link(&temp, &destination) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.to_string()),
            }
            #[cfg(unix)]
            std::fs::File::open(&parent)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
            Ok::<_, String>(())
        })();
        let _ = std::fs::remove_file(&temp);
        result?;
    }
    let existing = std::fs::read_to_string(&destination).map_err(|e| e.to_string())?;
    let (existing_meta, _) = crate::vault::frontmatter::parse_note(&existing, "recovery.md");
    if existing_meta.id != meta.id {
        return Err("Recovery destination identity collision".into());
    }
    // Acknowledgment describes the captured publication, never a later writer.
    Ok(json!({"path":destination.to_string_lossy(),"meta":meta,"content":body,"raw":raw}))
}

pub fn forget_window(state: &AppState, label: &str) {
    if let Ok(mut service) = state.recovery.lock() {
        service
            .owners
            .retain(|_, o| o.window != label || o.dirty || o.conflicted);
        if service
            .barrier
            .as_ref()
            .is_some_and(|(_, window)| window == label)
        {
            service.barrier = None;
        }
        service
            .versions
            .retain(|key, _| !key.starts_with(&format!("{label}:")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::writer::{NoteWrite, WritePolicy};
    fn draft(db: &VaultDb) -> Draft {
        let note = NewNote {
            id: "original".into(),
            title: "Summary".into(),
            content: "saved".into(),
            tags: vec!["hermes/session".into()],
            pinned: true,
            created: "2026-11-11T00:00:00Z".into(),
            modified: "2026-11-11T00:00:00Z".into(),
        };
        let base = db
            .write_authoritative(&NoteWrite::from_note(db, &note, WritePolicy::Create), 20)
            .unwrap();
        Draft {
            owner: uuid::Uuid::new_v4().to_string(),
            sequence: 1,
            revision: 1,
            captured_at: "2026-11-11T00:00:00Z".into(),
            vault: db.identity(),
            note_id: note.id,
            base,
            body: "local draft".into(),
            title: note.title,
            tags: note.tags,
            pinned: true,
            blobs: vec![],
            conflicted: true,
        }
    }
    fn service(root: &Path, draft: &Draft) -> Recovery {
        let mut service = Recovery::default();
        service.set_root(root.to_path_buf()).unwrap();
        service.owners.insert(
            draft.owner.clone(),
            Owner {
                window: "main".into(),
                vault: draft.vault.clone(),
                note: draft.note_id.clone(),
                dirty: false,
                conflicted: false,
                locked: false,
                sequence: 0,
            },
        );
        service
    }
    #[test]
    fn forks_are_atomic_idempotent_and_link_shared_bytes_without_summary_authority() {
        let db = VaultDb::open_in_memory().unwrap();
        let mut draft = draft(&db);
        let attachment = db
            .add_attachment(
                "original",
                "shared.pdf",
                "application/pdf",
                b"shared bytes",
                &draft.captured_at,
            )
            .unwrap();
        draft.body = format!("draft [file](.rn-attachments/{})", attachment.id);
        draft.blobs.push(Blob {
            reference: format!(".rn-attachments/{}", attachment.id),
            name: "shared.pdf".into(),
            mime: "application/pdf".into(),
            data: b"shared bytes".to_vec(),
            checkpoint_bytes: None,
        });
        let original = db.accepted_note("original").unwrap().unwrap();
        let first = fork(&db, &draft).unwrap();
        let again = fork(&db, &draft).unwrap();
        assert_eq!(first, again);
        assert_eq!(db.accepted_note("original").unwrap().unwrap(), original);
        assert_eq!(first["tags"], json!(["hermes/recovery"]));
        let id = first["id"].as_str().unwrap();
        assert!(!id.starts_with("hermes-session-"));
        assert_eq!(db.get_session_link(id).unwrap(), None);
        assert_eq!(
            db.attachment_meta(&attachment.id)
                .unwrap()
                .unwrap()
                .note_ids
                .len(),
            2
        );
        assert_eq!(
            db.conn
                .query_row("SELECT COUNT(*) FROM attachments", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        let mut collision = draft.clone();
        collision.body = "changed payload".into();
        assert!(fork(&db, &collision).is_err());
        let mut next = draft.clone();
        next.sequence += 1;
        next.revision += 1;
        next.body.push_str(" newer edit");
        assert_ne!(fork(&db, &next).unwrap()["id"], first["id"]);
        db.delete_note("original").unwrap();
        assert_eq!(
            db.get_attachment_data(&attachment.id).unwrap().unwrap().1,
            b"shared bytes"
        );
    }
    #[test]
    fn failed_fork_rolls_back_note_and_attachments_and_does_not_touch_original() {
        let mut db = VaultDb::open_in_memory().unwrap();
        let mut draft = draft(&db);
        db.max_attachment_bytes = 2;
        draft.body = "![pending](.rn-recovery/blob)".into();
        draft.blobs.push(Blob {
            reference: ".rn-recovery/blob".into(),
            name: "image.png".into(),
            mime: "image/png".into(),
            data: vec![1, 2, 3],
            checkpoint_bytes: None,
        });
        assert!(fork(&db, &draft).is_err());
        assert_eq!(db.list_notes(false).unwrap().len(), 1);
        assert_eq!(db.get_note_content("original").unwrap().unwrap(), "saved");
        assert_eq!(
            db.conn
                .query_row("SELECT COUNT(*) FROM attachments", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn journal_stream_is_ordered_private_and_restart_and_restore_barriers_preserve_unresolved_state(
    ) {
        let db = VaultDb::open_in_memory().unwrap();
        let mut draft = draft(&db);
        let dir = tempfile::tempdir().unwrap();
        let mut service = service(dir.path(), &draft);
        service.checkpoint("main", &draft).unwrap();
        assert!(service.checkpoint("other-window", &draft).is_err());
        draft.sequence = 2;
        draft.revision = 2;
        draft.body = "new typing".into();
        service.checkpoint("main", &draft).unwrap();
        let mut stale = draft.clone();
        stale.sequence = 1;
        assert!(service.checkpoint("main", &stale).is_err());
        assert!(service.resolve("main", &draft.owner, 1).is_err());
        assert!(service.assert_quiet(None, None).is_err());
        let mut restarted = Recovery::default();
        restarted.set_root(dir.path().to_path_buf()).unwrap();
        let records = restarted.records().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].body, "new typing");
        assert!(
            restarted.assert_quiet(None, None).is_err(),
            "restart, switch and restore cannot discard a journal"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(
                    service
                        .dir(&draft.owner)
                        .unwrap()
                        .join("00000000000000000002.json")
                )
                .unwrap()
                .permissions()
                .mode()
                    & 0o777,
                0o600
            );
        }
        service.resolve("main", &draft.owner, 2).unwrap();
        service.owners.get_mut(&draft.owner).unwrap().locked = true;
        assert!(service.assert_quiet(None, None).is_ok());
        assert!(restarted.records().unwrap().is_empty());
    }
    #[test]
    fn disk_and_transient_failures_leave_the_native_owner_unsafe() {
        let db = VaultDb::open_in_memory().unwrap();
        let mut draft = draft(&db);
        let dir = tempfile::tempdir().unwrap();
        let mut service = service(dir.path(), &draft);
        std::fs::write(service.dir(&draft.owner).unwrap(), b"not a directory").unwrap();
        assert!(service.checkpoint("main", &draft).is_err());
        assert!(service.owners[&draft.owner].dirty);
        assert!(service.assert_quiet(None, None).is_err());
        std::fs::remove_file(service.dir(&draft.owner).unwrap()).unwrap();
        draft.body = "![image](blob:missing)".into();
        assert!(service.checkpoint("main", &draft).is_err());
        assert!(service.records().unwrap().is_empty());
    }
    #[test]
    fn native_registry_refuses_other_window_mutations_but_allows_its_own_acknowledged_close() {
        let state = AppState::new(crate::types::AppConfig::default());
        let db = VaultDb::open_in_memory().unwrap();
        let draft = draft(&db);
        state.recovery.lock().unwrap().owners.insert(
            draft.owner,
            Owner {
                window: "note-detached".into(),
                vault: draft.vault,
                note: draft.note_id,
                dirty: true,
                conflicted: true,
                locked: false,
                sequence: 0,
            },
        );
        assert!(close_window(&state, "main").is_ok());
        assert!(close_window(&state, "note-detached").is_err());
        assert!(state
            .recovery
            .lock()
            .unwrap()
            .assert_quiet(None, None)
            .is_err());
        forget_window(&state, "note-detached");
        assert_eq!(
            state.recovery.lock().unwrap().owners.len(),
            1,
            "unresolved owners cannot be forgotten"
        );
    }
    #[test]
    fn data_version_is_connection_local_and_external_commits_do_not_acknowledge_any_draft() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.rn");
        let a = VaultDb::open(&path).unwrap();
        let b = VaultDb::open(&path).unwrap();
        let version = |db: &VaultDb| {
            db.conn
                .pragma_query_value(None, "data_version", |r| r.get::<_, i64>(0))
                .unwrap()
        };
        let before = version(&a);
        let draft = draft(&b);
        assert_ne!(before, version(&a));
        let accepted = a.accepted_note("original").unwrap().unwrap();
        b.tag_atomic("original", &["external".into()], &[]).unwrap();
        assert_ne!(
            accepted["accepted"]["token"],
            a.accepted_note("original").unwrap().unwrap()["accepted"]["token"]
        );
        assert_eq!(draft.body, "local draft");
    }
}

#[cfg(test)]
mod ipc_tests {
    use super::*;
    use crate::types::AppConfig;
    #[test]
    fn registry_journal_fork_and_conflicts_are_registered_through_mock_ipc() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault.rn");
        let db = VaultDb::open(&path).unwrap();
        let identity = db.identity();
        let app = tauri::test::mock_builder()
            .invoke_handler(tauri::generate_handler![
                recovery_register,
                recovery_status,
                recovery_checkpoint,
                recovery_list,
                recovery_claim,
                recovery_poll,
                recovery_fork,
                recovery_resolve,
                recovery_unregister,
                recovery_barrier,
                recovery_release_barrier
            ])
            .manage(AppState::new(AppConfig::default()))
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let state = app.state::<AppState>();
        *state.vault_db.lock().unwrap() = Some(db);
        state
            .recovery
            .lock()
            .unwrap()
            .set_root(dir.path().join("private"))
            .unwrap();
        let main = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        let detached = tauri::WebviewWindowBuilder::new(&app, "note-detached", Default::default())
            .build()
            .unwrap();
        let request =
            |view: &tauri::WebviewWindow<tauri::test::MockRuntime>, cmd: &str, body: Value| {
                tauri::test::get_ipc_response(
                    view,
                    tauri::webview::InvokeRequest {
                        cmd: cmd.into(),
                        callback: tauri::ipc::CallbackFn(0),
                        error: tauri::ipc::CallbackFn(1),
                        url: "http://tauri.localhost".parse().unwrap(),
                        body: tauri::ipc::InvokeBody::Json(body),
                        headers: Default::default(),
                        invoke_key: tauri::test::INVOKE_KEY.into(),
                    },
                )
            };
        let owner = uuid::Uuid::new_v4().to_string();
        request(
            &main,
            "recovery_register",
            json!({"owner":owner,"vault":identity,"noteId":"missing-original"}),
        )
        .unwrap();
        assert!(request(
            &detached,
            "recovery_status",
            json!({"owner":owner,"dirty":true,"conflicted":true,"locked":false})
        )
        .is_err());
        let observed: bool = request(&main, "recovery_poll", json!({"owner":owner}))
            .unwrap()
            .deserialize()
            .unwrap();
        assert!(observed);
        let unchanged: bool = request(&main, "recovery_poll", json!({"owner":owner}))
            .unwrap()
            .deserialize()
            .unwrap();
        assert!(!unchanged);
        // Reopening the exact same file is a new connection even if SQLite's
        // first data_version integer happens to equal the old observation.
        *state.vault_db.lock().unwrap() = Some(VaultDb::open(&path).unwrap());
        state
            .vault_db_epoch
            .fetch_add(1, std::sync::atomic::Ordering::Release);
        let reopened: bool = request(&main, "recovery_poll", json!({"owner":owner}))
            .unwrap()
            .deserialize()
            .unwrap();
        assert!(reopened);
        let draft = Draft {
            owner: owner.clone(),
            sequence: 1,
            revision: 1,
            captured_at: "2026-11-11T00:00:00Z".into(),
            vault: identity.clone(),
            note_id: "missing-original".into(),
            base: json!({}),
            body: "unsaved draft".into(),
            title: "Draft".into(),
            tags: vec![],
            pinned: false,
            blobs: vec![],
            conflicted: true,
        };
        request(&main, "recovery_checkpoint", json!({"draft":draft})).unwrap();
        assert!(request(
            &main,
            "recovery_barrier",
            json!({"vault":identity.path,"roots":null})
        )
        .is_err());
        assert!(close_window(&state, "main").is_err());
        let first: Value = request(&main, "recovery_fork", json!({"owner":owner,"sequence":1}))
            .unwrap()
            .deserialize()
            .unwrap();
        let again: Value = request(&main, "recovery_fork", json!({"owner":owner,"sequence":1}))
            .unwrap()
            .deserialize()
            .unwrap();
        assert_eq!(first, again);
        assert_eq!(first["content"], "unsaved draft");
        let foreign: Value = request(&detached, "recovery_list", json!({}))
            .unwrap()
            .deserialize()
            .unwrap();
        assert!(foreign.as_array().unwrap().is_empty());
        request(
            &main,
            "recovery_resolve",
            json!({"owner":owner,"sequence":1}),
        )
        .unwrap();
        request(
            &main,
            "recovery_status",
            json!({"owner":owner,"dirty":false,"conflicted":false,"locked":true}),
        )
        .unwrap();
        let lease: String = request(
            &main,
            "recovery_barrier",
            json!({"vault":identity.path,"roots":null}),
        )
        .unwrap()
        .deserialize()
        .unwrap();
        assert!(request(
            &detached,
            "recovery_release_barrier",
            json!({"token":lease})
        )
        .is_err());
        let another = uuid::Uuid::new_v4().to_string();
        assert!(request(
            &detached,
            "recovery_register",
            json!({"owner":another,"vault":identity,"noteId":"n"})
        )
        .is_err());
        request(&main, "recovery_release_barrier", json!({"token":lease})).unwrap();
        assert!(close_window(&state, "main").is_ok());
        request(&main, "recovery_unregister", json!({"owner":owner})).unwrap();
    }
}

/// Bodies never enter invalidation events or ordinary logs. A hint only asks an
/// owner to reread; it cannot authorize a save, clean a buffer or move focus.
pub fn notify(state: &AppState) {
    if let Ok(app) = state.recovery_events.lock() {
        if let Some(app) = app.as_ref() {
            let _ = app.emit("note-invalidated", json!({}));
        }
    }
}

#[cfg(test)]
mod publication_tests {
    use super::*;
    #[test]
    fn filesystem_publication_is_retry_idempotent_and_keeps_original_and_shared_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        let original = root.join("original.md");
        std::fs::write(&original, "saved original").unwrap();
        let draft = Draft {
            owner: uuid::Uuid::new_v4().to_string(),
            sequence: 1,
            revision: 1,
            captured_at: "2026-11-11T00:00:00Z".into(),
            vault: filesystem_identity(&root).unwrap(),
            note_id: original.to_string_lossy().into_owned(),
            base: json!({}),
            body: "![draft](.rn-recovery/image)".into(),
            title: "Summary".into(),
            tags: vec!["hermes/session".into()],
            pinned: true,
            conflicted: true,
            blobs: vec![Blob {
                reference: ".rn-recovery/image".into(),
                name: "image.png".into(),
                mime: "image/png".into(),
                data: b"image bytes".to_vec(),
                checkpoint_bytes: None,
            }],
        };
        let first = fork_filesystem(&draft).unwrap();
        assert_eq!(first, fork_filesystem(&draft).unwrap());
        assert_eq!(
            std::fs::read_to_string(&original).unwrap(),
            "saved original"
        );
        assert_eq!(first["meta"]["tags"], json!(["hermes/recovery"]));
        let path = first["path"].as_str().unwrap();
        std::fs::write(
            path,
            first["raw"]
                .as_str()
                .unwrap()
                .replace("(recovery copy)", "(edited copy)"),
        )
        .unwrap();
        // A lost-reply retry returns its captured acknowledgment, never replaces
        // an independently edited fork and never manufactures another copy.
        assert_eq!(first, fork_filesystem(&draft).unwrap());
        assert!(std::fs::read_to_string(path)
            .unwrap()
            .contains("edited copy"));
        assert_eq!(
            std::fs::read_dir(root.join(".ryuknotes/attachments"))
                .unwrap()
                .count(),
            1
        );
    }
}

/// Actual OS/webview destruction is distinct from permission to close. The
/// ordinary close path refuses dirty owners. If a renderer dies anyway, its
/// last durable stream becomes claimable; no stale live-owner entry hides it.
pub fn window_gone(state: &AppState, label: &str) {
    if let Ok(mut service) = state.recovery.lock() {
        service.owners.retain(|_, owner| owner.window != label);
        service
            .versions
            .retain(|key, _| !key.starts_with(&format!("{label}:")));
        if service
            .barrier
            .as_ref()
            .is_some_and(|(_, window)| window == label)
        {
            service.barrier = None;
        }
    }
}

#[cfg(test)]
mod tombstone_tests {
    use super::*;
    #[test]
    fn a_published_but_unacknowledged_record_can_be_retired_after_exact_save_or_confirmed_discard()
    {
        let dir = tempfile::tempdir().unwrap();
        let mut service = Recovery::default();
        service.set_root(dir.path().to_path_buf()).unwrap();
        let owner = uuid::Uuid::new_v4().to_string();
        let identity = VaultIdentity {
            path: ":memory:".into(),
            file: "test".into(),
        };
        service.owners.insert(
            owner.clone(),
            Owner {
                window: "main".into(),
                vault: identity.clone(),
                note: "n".into(),
                dirty: true,
                conflicted: false,
                locked: false,
                sequence: 0,
            },
        );
        let draft = Draft {
            owner: owner.clone(),
            sequence: 1,
            revision: 1,
            captured_at: "2026-11-11T00:00:00Z".into(),
            vault: identity,
            note_id: "n".into(),
            base: json!({}),
            body: "draft".into(),
            title: "Title".into(),
            tags: vec![],
            pinned: false,
            blobs: vec![],
            conflicted: false,
        };
        // Simulate publication followed by a failed directory-fsync/reply: the
        // file exists, but native and renderer acknowledgment sequences remain 0.
        atomic(
            &service
                .dir(&owner)
                .unwrap()
                .join("00000000000000000001.json"),
            &serde_json::to_vec(&draft).unwrap(),
        )
        .unwrap();
        assert_eq!(service.records().unwrap().len(), 1);
        service.resolve("main", &owner, 0).unwrap();
        assert!(service.records().unwrap().is_empty());
        assert!(!service.owners[&owner].dirty);
    }
}
