//! Owner-scoped recovery. Hints are never acknowledgments; journals never save
//! the original. No SQLite schema change and no text merge.
use crate::state::AppState;
use crate::vault::db::{NewNote, VaultDb};
use crate::vault::revision_retention::{PinGroup, PinRef, RevisionBinding};
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
    reviews: HashMap<String, Review>,
    bindings: HashMap<String, RevisionBinding>,
    claimed: std::collections::HashSet<String>,
}
// Crash probes exist only in disposable native fixture subprocesses.
#[cfg(test)]
pub(crate) fn crash_boundary(name: &str) {
    if std::env::var("RYUK_4D_CRASH_AT").ok().as_deref() == Some(name) {
        std::process::exit(87);
    }
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
    pub(crate) fn private_storage_bytes(&self) -> Result<u64, String> {
        let Some(root) = &self.root else { return Ok(0) };
        let mut bytes = 0u64;
        for entry in walkdir::WalkDir::new(root).follow_links(false) {
            let entry = entry.map_err(|e|e.to_string())?;
            if entry.file_type().is_file() {
                bytes = bytes.saturating_add(entry.metadata().map_err(|e|e.to_string())?.len());
            }
        }
        Ok(bytes)
    }

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
        #[cfg(test)]
        crash_boundary("checkpoint-bytes");
        let bytes = serde_json::to_vec(&persisted).map_err(|e| e.to_string())?;
        atomic(&dir.join(format!("{:020}.json", draft.sequence)), &bytes)?;
        #[cfg(test)]
        crash_boundary("checkpoint-published");
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
        let db = slot.as_ref().ok_or("Originating vault is closed")?;
        if db.is_v3() {
            return Err("Revision owners require an atomic pinned accepted read".into());
        }
        db.validate_writer_vault(&vault)
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
    let mut service = state.recovery.lock().map_err(|e| e.to_string())?;
    {
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
        if db.is_v3() {
            let proposed = RevisionBinding::from_accepted(&draft.base["meta"]["accepted"])?;
            if service.bindings.get(&draft.owner).is_none_or(|b| {
                b.vault_uuid != proposed.vault_uuid
                    || b.storage_epoch != proposed.storage_epoch
                    || b.incarnation_id != proposed.incarnation_id
            }) {
                return Err(
                    "Checkpoint cannot rebind an owner to another vault, epoch or incarnation"
                        .into(),
                );
            }
            let assets = refs
                .captures_iter(&referenced)
                .map(|c| c[2].to_string())
                .collect::<Vec<_>>();
            let binding = db.hold_checkpoint(
                &draft.owner,
                &draft.note_id,
                &draft.base["meta"]["accepted"],
                &assets,
                false,
            )?;
            #[cfg(test)]
            crash_boundary("checkpoint-holds");
            draft.base["recoveryIdentity"] =
                serde_json::to_value(&binding).map_err(|e| e.to_string())?;
            draft.base["recoveryIdentity"]["localRevision"] = json!(draft.revision);
        }
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
    service.checkpoint(window.label(), &draft)?;
    if let Some(identity) = draft.base.get("recoveryIdentity") {
        service.bindings.insert(
            draft.owner.clone(),
            serde_json::from_value(identity.clone()).map_err(|e| e.to_string())?,
        );
    }
    Ok(())
}
#[tauri::command]
pub fn recovery_resolve<R: Runtime>(
    window: tauri::Window<R>,
    state: State<'_, AppState>,
    owner: String,
    sequence: u64,
) -> Result<(), String> {
    let mut service = state.recovery.lock().map_err(|e| e.to_string())?;
    if service.bindings.contains_key(&owner) {
        return Err("Revision resolution requires matching identity and local revision".into());
    }
    service.resolve(window.label(), &owner, sequence)
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
    if service.bindings.contains_key(&owner) {
        let slot = state.vault_db.lock().map_err(|e| e.to_string())?;
        let db = slot.as_ref().ok_or("Originating vault is closed")?;
        if !db.owner_pins(&owner)?.is_empty() {
            return Err(
                "Release the accepted-base pin with an ordered acknowledgment first".into(),
            );
        }
        service.bindings.remove(&owner);
    }
    service.owners.remove(&owner);
    service.claimed.remove(&owner);
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
        .filter(|d| {
            !service.owners.contains_key(&d.owner)
                && !service.reviews.get(&d.owner).is_some_and(|r| r.active)
        })
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
    if let Some(identity) = draft.base.get("recoveryIdentity") {
        service.bindings.insert(
            owner.clone(),
            serde_json::from_value(identity.clone()).map_err(|e| e.to_string())?,
        );
    }
    service.claimed.insert(owner.clone());
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
    // Polling releases both guards before reacquiring the registry. Native
    // recovery mutations consistently use registry -> DB -> ledger -> SQL.
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
    fork_with_origin(db, draft, false)
}
fn fork_with_origin(db: &VaultDb, draft: &Draft, recovered: bool) -> Result<Value, String> {
    if recovered {
        let known_uuid = if let Some(identity) = draft.base.get("recoveryIdentity") {
            Some(
                serde_json::from_value::<RevisionBinding>(identity.clone())
                    .map_err(|e| e.to_string())?
                    .vault_uuid,
            )
        } else {
            draft.base["meta"]["accepted"]["vaultUuid"]
                .as_str()
                .map(str::to_owned)
        };
        if let Some(vault_uuid) = known_uuid {
            if db
                .revision_session
                .as_ref()
                .is_none_or(|s| s.identity.vault_uuid != vault_uuid)
            {
                return Err("Recovered copy belongs to another logical vault".into());
            }
        } else {
            // Explicit Keep both for a v2 checkpoint after in-place migration:
            // require its exact physical origin; create a fresh incarnation and
            // retain unknown ancestry, never promote this draft into a v3 base.
            db.validate_writer_vault(&draft.vault)
                .map_err(|e| e.to_string())?;
        }
    } else {
        db.validate_writer_vault(&draft.vault)
            .map_err(|e| e.to_string())?;
        if let Some(session) = &db.revision_session {
            let accepted = &draft.base["meta"]["accepted"];
            if accepted["vaultUuid"].as_str() != Some(session.identity.vault_uuid.as_str())
                || accepted["storageEpoch"].as_str()
                    != Some(session.identity.storage_epoch.as_str())
            {
                return Err("Recovery origin requires explicit vault/restore enrollment; the draft is preserved".into());
            }
        }
    }
    validate_blobs(draft)?;
    let key = format!("recovery_fork:v1:{}:{}", draft.owner, draft.sequence);
    let mut payload_draft = draft.clone();
    for blob in &mut payload_draft.blobs {
        blob.checkpoint_bytes = None;
    }
    let payload = digest(&serde_json::to_vec(&payload_draft).map_err(|e| e.to_string())?);
    let tx = db.begin_write(
        "recovery",
        Some(&format!("fork:{}:{}", draft.owner, draft.sequence)),
        &serde_json::to_value(&payload_draft).map_err(|e| e.to_string())?,
    )?;
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
    let receipt = tx.receipt(&note.id)?;
    tx.execute(
        "INSERT INTO meta(key,value) VALUES(?1,?2)",
        params![
            key,
            json!({"payload":payload,"source":draft.note_id,"sourceAncestry":if draft.base["meta"]["accepted"]["revision"].is_object(){"captured-revision"}else{"legacy-unknown"},"receipt":receipt}).to_string()
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
    let claimed = service.claimed.contains(&owner);
    drop(service);
    let receipt = if draft.vault.path.ends_with(".rn") {
        let slot = state.vault_db.lock().map_err(|e| e.to_string())?;
        if let Some(db) = slot.as_ref().filter(|db| claimed && db.is_v3()) {
            fork_with_origin(db, &draft, true)?
        } else if let Some(db) = slot.as_ref().filter(|db| db.identity() == draft.vault) {
            fork(db, &draft)?
        } else {
            drop(slot);
            // Restart recovery does not retarget the displayed vault. A private
            // checkpoint may reopen only its exact originating file identity.
            if filesystem_identity(Path::new(&draft.vault.path))? != draft.vault {
                return Err("Originating vault identity changed".into());
            }
            if crate::vault::ancestry::inspect_version(Path::new(&draft.vault.path))? != 3 {
                return Err("Open the originating vault to complete its visible upgrade before recovering this draft".into());
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
    fn explicit_legacy_recovery_copy_after_migration_preserves_original_without_guessing_ancestry()
    {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.rn");
        let db = VaultDb::open_legacy_v2(&path).unwrap();
        let draft = draft(&db);
        drop(db);
        let (db, _) = VaultDb::open_with_report(&path, &dir.path().join("ledger")).unwrap();
        assert!(fork(&db, &draft).is_err()); // No implicit legacy rebinding.
        let copied = fork_with_origin(&db, &draft, true).unwrap();
        assert_eq!(copied["content"], "local draft");
        assert_eq!(copied["tags"], json!(["hermes/recovery"]));
        assert_eq!(db.get_note_content("original").unwrap().unwrap(), "saved");
        assert_ne!(copied["id"], "original");
        assert_eq!(fork_with_origin(&db, &draft, true).unwrap(), copied);
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
        let a = VaultDb::open_legacy_v2(&path).unwrap();
        let b = VaultDb::open_legacy_v2(&path).unwrap();
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
        let db = VaultDb::open_legacy_v2(&path).unwrap();
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
        *state.vault_db.lock().unwrap() = Some(VaultDb::open_legacy_v2(&path).unwrap());
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
        service.reviews.retain(|_, review| review.window != label);
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

// Stage 3: explicit proposals. Display diffs have no authority to apply a patch.
#[derive(Clone)]
struct Review {
    window: String,
    owner: String,
    draft: Draft,
    remote: Value,
    active: bool,
    proposal_sequence: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewProposal {
    pub body: String,
    pub title: String,
    pub tags: Vec<String>,
    pub pinned: bool,
}
/// Verify the journal's exact accepted body AND metadata against its captured
/// state token. Never select a base by snapshot date or resemblance. This is a
/// verified live accepted state, not durable revision ancestry (ABA still applies).
fn verified_review_base(db: &VaultDb, draft: &Draft) -> bool {
    let base = &draft.base;
    let accepted = &base["meta"]["accepted"];
    let state = json!({"id":draft.note_id,"content":accepted["body"],"title":accepted["title"],
        "tags":accepted["tags"],"pinned":accepted["pinned"],"trashed":accepted["trashed"]});
    accepted["vault"] == json!(draft.vault)
        && base["path"] == draft.note_id
        && base["content"].is_string()
        && base["content"] == accepted["body"]
        && base["raw"] == accepted["body"]
        && base["meta"]["id"] == draft.note_id
        && base["meta"]["title"] == accepted["title"]
        && base["meta"]["tags"] == accepted["tags"]
        && base["meta"]["pinned"] == accepted["pinned"]
        && base["meta"]["created"] == accepted["created"]
        && base["meta"]["modified"] == accepted["modified"]
        && accepted["trashed"] == false
        && if db.is_v3() {
            crate::vault::revision_writer::verify_snapshot(db, &draft.note_id, accepted)
        } else {
            accepted["token"].as_str() == Some(db.state_token(&state).as_str())
        }
}
fn collect_review_blobs(db: &VaultDb, content: &str, blobs: &mut Vec<Blob>) -> Result<(), String> {
    let refs =
        regex::Regex::new(r#"(?:\]\(|(?:src|href)=["']|<)(\.rn-attachments/([a-zA-Z0-9-]+))"#)
            .map_err(|e| e.to_string())?;
    for cap in refs.captures_iter(content) {
        if blobs.iter().any(|b| b.reference == cap[1]) {
            continue;
        }
        let (meta, data) = db
            .get_attachment_data(&cap[2])?
            .ok_or("Review attachment is unavailable; use Keep both")?;
        blobs.push(Blob {
            reference: cap[1].into(),
            name: meta.filename,
            mime: meta.mime_type,
            data,
            checkpoint_bytes: None,
        });
    }
    Ok(())
}
#[tauri::command]
pub fn recovery_review_open<R: Runtime>(
    window: tauri::Window<R>,
    state: State<'_, AppState>,
    owner: String,
    sequence: u64,
    revision: u64,
) -> Result<Value, String> {
    let service = state.recovery.lock().map_err(|e| e.to_string())?;
    let registered = service.owner(&owner, window.label())?;
    if registered.sequence != sequence || service.barrier.is_some() {
        return Err("The owner changed before review opened".into());
    }
    let mut draft = read_draft(&service.dir(&owner)?.join(format!("{sequence:020}.json")))?;
    if draft.owner != owner || draft.sequence != sequence || draft.revision != revision {
        return Err("The captured local revision changed".into());
    }
    drop(service);
    let id = uuid::Uuid::new_v4().to_string();
    let slot = state.vault_db.lock().map_err(|e| e.to_string())?;
    let Some(db) = slot.as_ref().filter(|db| db.identity() == draft.vault) else {
        return Ok(
            json!({"available":false,"reason":"No verified accepted base in this originating connection. Use Keep both."}),
        );
    };
    let saved = if db.is_v3() && verified_review_base(db, &draft) {
        Some(db.pin_review(&id, &draft.note_id, &draft.base["meta"]["accepted"])?)
    } else {
        db.accepted_note(&draft.note_id)
            .map_err(|e| e.to_string())?
    };
    if !verified_review_base(db, &draft) {
        let local = json!({"path":draft.note_id,"raw":draft.body,"content":draft.body,"meta":{
            "id":draft.note_id,"title":draft.title,"tags":draft.tags,"pinned":draft.pinned,
            "created":draft.base["meta"]["created"],"modified":draft.base["meta"]["modified"]}});
        return Ok(
            json!({"available":false,"local":local,"remote":saved,"reason":"The journal does not hold an exact verified accepted base. Inspect both sources or use Keep both."}),
        );
    }
    let Some(remote) = saved else {
        return Ok(
            json!({"available":false,"reason":"The saved note was deleted. Use Keep both."}),
        );
    };
    if remote["trashed"] == true {
        return Ok(
            json!({"available":false,"reason":"The saved note is in trash. Use Keep both."}),
        );
    }
    collect_review_blobs(
        db,
        remote["content"].as_str().unwrap_or(""),
        &mut draft.blobs,
    )?;
    drop(slot);
    let mut service = state.recovery.lock().map_err(|e| e.to_string())?;
    if service.owner(&owner, window.label())?.sequence != sequence || service.barrier.is_some() {
        return Err("The draft changed while opening review".into());
    }
    let local = json!({"path":draft.note_id,"raw":draft.body,"content":draft.body,"meta":{
        "id":draft.note_id,"title":draft.title,"tags":draft.tags,"pinned":draft.pinned,
        "created":draft.base["meta"]["created"],"modified":draft.base["meta"]["modified"]}});
    let previews: Vec<_> = draft
        .blobs
        .iter()
        .filter(|blob| blob.mime.starts_with("image/"))
        .map(|blob| blob.reference.clone())
        .collect();
    let ancestry = if remote["accepted"].get("revision").is_some() {
        Some(
            json!({"base":RevisionBinding::from_accepted(&draft.base["meta"]["accepted"])?,"remote":RevisionBinding::from_accepted(&remote["accepted"])?}),
        )
    } else {
        None
    };
    let result = json!({"ancestry":ancestry,"available":true,"reviewId":id,"revision":revision,"base":draft.base,"local":local,"remote":remote,"previewReferences":previews});
    service.reviews.insert(
        id,
        Review {
            window: window.label().into(),
            owner,
            draft,
            remote,
            active: true,
            proposal_sequence: 0,
        },
    );
    Ok(result)
}
fn review_for(service: &Recovery, window: &str, owner: &str, id: &str) -> Result<Review, String> {
    service.owner(owner, window)?;
    service
        .reviews
        .get(id)
        .filter(|r| r.active && r.owner == owner && r.window == window)
        .cloned()
        .ok_or("This review no longer belongs to this owner".into())
}
#[tauri::command]
pub fn recovery_review_checkpoint<R: Runtime>(
    window: tauri::Window<R>,
    state: State<'_, AppState>,
    owner: String,
    review_id: String,
    sequence: u64,
    proposal: ReviewProposal,
) -> Result<(), String> {
    let mut service = state.recovery.lock().map_err(|e| e.to_string())?;
    let review = review_for(&service, window.label(), &owner, &review_id)?;
    let mut draft = review.draft.clone();
    draft.owner = review_id.clone();
    draft.sequence = sequence;
    draft.base["reviewResult"] = json!({"reviewId":review_id,"sourceOwner":owner,"baseVerified":true,
        "yourDraft":{"body":review.draft.body,"title":review.draft.title,"tags":review.draft.tags,"pinned":review.draft.pinned,"revision":review.draft.revision},
        "saved":review.remote});
    draft.body = proposal.body;
    draft.title = proposal.title;
    draft.tags = proposal.tags;
    draft.pinned = proposal.pinned;
    let refs =
        regex::Regex::new(r#"(?:\]\(|(?:src|href)=["']|<)(\.rn-attachments/([a-zA-Z0-9-]+))"#)
            .map_err(|e| e.to_string())?;
    {
        let slot = state.vault_db.lock().map_err(|e| e.to_string())?;
        if let Some(db) = slot.as_ref().filter(|db| db.is_v3()) {
            let assets = refs
                .captures_iter(&draft.body)
                .map(|c| c[2].to_string())
                .collect::<Vec<_>>();
            let binding = db.hold_checkpoint(
                &review_id,
                &draft.note_id,
                &draft.base["meta"]["accepted"],
                &assets,
                true,
            )?;
            draft.base["recoveryIdentity"] =
                serde_json::to_value(&binding).map_err(|e| e.to_string())?;
            draft.base["recoveryIdentity"]["localRevision"] = json!(draft.revision);
            service.bindings.insert(review_id.clone(), binding);
        }
    }
    if refs
        .captures_iter(&draft.body)
        .any(|cap| !draft.blobs.iter().any(|blob| blob.reference == cap[1]))
    {
        let slot = state.vault_db.lock().map_err(|e| e.to_string())?;
        let db = slot
            .as_ref()
            .ok_or("The new result references an unavailable vault attachment")?;
        db.validate_writer_vault(&draft.vault)
            .map_err(|e| e.to_string())?;
        collect_review_blobs(db, &draft.body, &mut draft.blobs)?;
    }
    // A result is a separate recoverable artifact, never the original live L.
    // Failed publication retains an unsafe native owner and renderer memory.
    service.owners.entry(review_id.clone()).or_insert(Owner {
        window: window.label().into(),
        vault: draft.vault.clone(),
        note: draft.note_id.clone(),
        dirty: true,
        conflicted: true,
        locked: false,
        sequence: review.proposal_sequence,
    });
    if sequence == review.proposal_sequence && sequence > 0 {
        let prior = read_draft(
            &service
                .dir(&review_id)?
                .join(format!("{sequence:020}.json")),
        )?;
        if prior.body != draft.body
            || prior.title != draft.title
            || prior.tags != draft.tags
            || prior.pinned != draft.pinned
        {
            return Err("Proposal sequence reused for different content".into());
        }
    } else {
        service.checkpoint(window.label(), &draft)?;
    }
    service.owners.remove(&review_id);
    service
        .reviews
        .get_mut(&review_id)
        .unwrap()
        .proposal_sequence = sequence;
    Ok(())
}
#[tauri::command]
pub fn recovery_review_cancel<R: Runtime>(
    window: tauri::Window<R>,
    state: State<'_, AppState>,
    owner: String,
    review_id: String,
) -> Result<(), String> {
    let mut service = state.recovery.lock().map_err(|e| e.to_string())?;
    service.owner(&owner, window.label())?;
    if !service.reviews.contains_key(&review_id) {
        return Ok(());
    }
    let review = review_for(&service, window.label(), &owner, &review_id)?;
    if service
        .owners
        .get(&review_id)
        .is_some_and(|o| o.dirty || o.conflicted)
    {
        return Err(
            "The edited result is not durable; keep it in memory and retry preservation".into(),
        );
    }
    if review.proposal_sequence == 0 && review.remote["accepted"].get("revision").is_some() {
        let slot = state.vault_db.lock().map_err(|e| e.to_string())?;
        let db = slot.as_ref().ok_or("Originating vault is closed")?;
        let binding = RevisionBinding::from_accepted(&review.draft.base["meta"]["accepted"])?;
        service.bindings.insert(review_id.clone(), binding.clone());
        service.owners.entry(review_id.clone()).or_insert(Owner {
            window: window.label().into(),
            vault: review.draft.vault.clone(),
            note: review.draft.note_id.clone(),
            dirty: false,
            conflicted: false,
            locked: false,
            sequence: 0,
        });
        service.resolve_revision(
            db,
            window.label(),
            &review_id,
            0,
            review.draft.revision,
            &binding,
            "review_cancel",
            None,
        )?;
        service.owners.remove(&review_id);
        service.bindings.remove(&review_id);
    }
    service.reviews.remove(&review_id);
    drop(service);
    let _ = window.app_handle().emit("recovery-available", ());
    Ok(())
}
fn apply_review(
    db: &VaultDb,
    id: &str,
    review: &Review,
    proposal: &Draft,
) -> Result<Value, crate::vault::writer::WriteError> {
    use crate::vault::writer::{WriteError, WriteErrorKind, WritePolicy};
    db.validate_writer_vault(&review.draft.vault)?;
    validate_blobs(proposal)?;
    let tx = db.begin_write(
        "recovery",
        Some(&format!("review:{id}")),
        &serde_json::to_value(proposal).map_err(|e| e.to_string())?,
    )?;
    let key = format!("review_apply:v1:{id}");
    let payload = digest(
        &serde_json::to_vec(&json!([
            proposal.sequence,
            proposal.body,
            proposal.title,
            proposal.tags,
            proposal.pinned
        ]))
        .map_err(|e| e.to_string())?,
    );
    let prior: Option<String> = tx
        .query_row("SELECT value FROM meta WHERE key=?1", [&key], |r| r.get(0))
        .optional()
        .map_err(|e| e.to_string())?;
    if let Some(prior) = prior {
        let prior: Value = serde_json::from_str(&prior).map_err(|e| e.to_string())?;
        if prior["payload"] != payload {
            return Err(WriteError::conflict(
                WriteErrorKind::Changed,
                "This review already applied another result. Renew review.",
                None,
            ));
        }
        return Ok(prior["receipt"].clone());
    }
    db.check_policy(
        &review.draft.note_id,
        &WritePolicy::Update {
            expected_token: review.remote["accepted"]["token"]
                .as_str()
                .ok_or("Missing reviewed token")?
                .into(),
        },
    )?;
    let now = chrono::Utc::now().to_rfc3339();
    let mut note = NewNote {
        id: review.draft.note_id.clone(),
        title: proposal.title.clone(),
        content: proposal.body.clone(),
        tags: proposal.tags.clone(),
        pinned: proposal.pinned,
        created: review.remote["created"].as_str().unwrap_or(&now).into(),
        modified: now.clone(),
    };
    let previous = db
        .get_note_content(&note.id)?
        .ok_or("Reviewed note disappeared")?;
    // Forced predecessor checkpoint: interactive apply must not lose R to the
    // ordinary GUI history cooldown. The snapshot remains ancestry-unknown.
    for blob in &proposal.blobs {
        let attachment =
            db.store_attachment_on(&tx, &note.id, &blob.name, &blob.mime, &blob.data, &now)?;
        note.content = note
            .content
            .replace(&blob.reference, &format!(".rn-attachments/{attachment}"));
    }
    if previous != note.content {
        VaultDb::snapshot_on(&tx, &note.id, &previous, 0, true)?;
    }
    VaultDb::write_note(&tx, &note, None, None)?;
    if let Some(base) = review.draft.base["meta"]["accepted"]["revision"]["revisionId"].as_str() {
        tx.merge_base(&note.id, base);
    }
    let receipt = tx.receipt_typed(&note.id)?;
    tx.execute(
        "INSERT INTO meta(key,value) VALUES(?1,?2)",
        params![
            key,
            json!({"payload":payload,"receipt":receipt}).to_string()
        ],
    )
    .map_err(|e| e.to_string())?;
    tx.commit_typed()?;
    Ok(receipt)
}
#[tauri::command]
pub fn recovery_review_apply<R: Runtime>(
    window: tauri::Window<R>,
    state: State<'_, AppState>,
    owner: String,
    review_id: String,
    revision: u64,
    sequence: u64,
) -> Result<Value, crate::vault::writer::WriteError> {
    use crate::vault::writer::{WriteError, WriteErrorKind};
    let mut service = state.recovery.lock().map_err(|e| e.to_string())?;
    let review = review_for(&service, window.label(), &owner, &review_id)?;
    let registered = service.owner(&owner, window.label())?;
    if revision != review.draft.revision
        || registered.sequence != review.draft.sequence
        || service.barrier.is_some()
    {
        return Err(WriteError::conflict(
            WriteErrorKind::Changed,
            "Your draft changed during review. The result is preserved; renew review.",
            None,
        ));
    }
    if sequence == 0 || review.proposal_sequence != sequence {
        return Err("Checkpoint the exact edited result before applying".into());
    }
    let proposal = read_draft(
        &service
            .dir(&review_id)?
            .join(format!("{sequence:020}.json")),
    )?;
    if proposal.owner != review_id
        || proposal.sequence != sequence
        || proposal.revision != revision
        || proposal.vault != review.draft.vault
        || proposal.note_id != review.draft.note_id
    {
        return Err("The result artifact does not match this captured owner/revision".into());
    }
    // Keep the owner generation stable through the short CAS transaction.
    // No DB->registry lock is held elsewhere while waiting for this guard.
    let slot = state.vault_db.lock().map_err(|e| e.to_string())?;
    let db = slot.as_ref().ok_or_else(|| {
        WriteError::conflict(
            WriteErrorKind::VaultReplaced,
            "The originating vault is closed",
            None,
        )
    })?;
    let receipt = apply_review(db, &review_id, &review, &proposal)?;
    drop(slot);
    // A post-commit journal error is retryable through the receipt key above.
    service.owners.insert(
        review_id.clone(),
        Owner {
            window: window.label().into(),
            vault: proposal.vault.clone(),
            note: proposal.note_id.clone(),
            dirty: true,
            conflicted: true,
            locked: false,
            sequence,
        },
    );
    if let Some(binding) = service.bindings.get(&review_id).cloned() {
        let slot = state.vault_db.lock().map_err(|e| e.to_string())?;
        service.resolve_revision(
            slot.as_ref().ok_or("Originating vault is closed")?,
            window.label(),
            &review_id,
            sequence,
            revision,
            &binding,
            "apply",
            None,
        )?;
        service.bindings.remove(&review_id);
    } else {
        service.resolve(window.label(), &review_id, sequence)?;
    }
    service.owners.remove(&review_id);
    drop(service);
    notify(&state);
    Ok(receipt)
}

#[cfg(test)]
mod review_tests {
    use super::*;
    use crate::vault::writer::{NoteWrite, WriteErrorKind, WritePolicy};
    fn setup(db: &VaultDb) -> (Review, Draft) {
        let note = NewNote {
            id: "review-note".into(),
            title: "Title".into(),
            content: "base\r\n".into(),
            tags: vec!["base".into()],
            pinned: false,
            created: "2026-11-11T00:00:00Z".into(),
            modified: "2026-11-11T00:00:00Z".into(),
        };
        let base = db
            .write_authoritative(&NoteWrite::from_note(db, &note, WritePolicy::Create), 20)
            .unwrap();
        let accepted = json!({"vault":base["accepted"]["vault"],"token":base["accepted"]["token"],"body":base["content"],"title":base["title"],"tags":base["tags"],"pinned":base["pinned"],"trashed":false,"created":base["created"],"modified":base["modified"]});
        let snapshot = json!({"path":note.id,"content":note.content,"raw":note.content,"meta":{"id":note.id,"title":base["title"],"tags":base["tags"],"pinned":false,"created":base["created"],"modified":base["modified"],"accepted":accepted}});
        let draft = Draft {
            owner: uuid::Uuid::new_v4().to_string(),
            sequence: 1,
            revision: 2,
            captured_at: note.created,
            vault: db.identity(),
            note_id: note.id,
            base: snapshot,
            body: "local\r\n".into(),
            title: "Local title".into(),
            tags: vec!["local".into()],
            pinned: true,
            blobs: vec![],
            conflicted: true,
        };
        assert!(verified_review_base(db, &draft));
        let review = Review {
            window: "main".into(),
            owner: draft.owner.clone(),
            draft: draft.clone(),
            remote: base,
            active: true,
            proposal_sequence: 1,
        };
        let proposal = Draft {
            owner: uuid::Uuid::new_v4().to_string(),
            body: "reviewed\r\n".into(),
            title: "Chosen title".into(),
            ..draft
        };
        (review, proposal)
    }
    #[test]
    fn exact_base_requires_accepted_body_metadata_and_origin_not_snapshot_resemblance() {
        let db = VaultDb::open_in_memory().unwrap();
        let (review, _) = setup(&db);
        for field in ["raw", "title", "tags", "vault", "missing"] {
            let mut draft = review.draft.clone();
            match field {
                "raw" => draft.base["raw"] = json!("base\n"),
                "title" => draft.base["meta"]["title"] = json!("wrong title"),
                "tags" => draft.base["meta"]["tags"] = json!(["wrong"]),
                "vault" => draft.base["meta"]["accepted"]["vault"]["file"] = json!("another file"),
                _ => draft.base["meta"]
                    .as_object_mut()
                    .unwrap()
                    .remove("accepted")
                    .map(|_| ())
                    .unwrap(),
            }
            assert!(!verified_review_base(&db, &draft));
        }
    }
    #[test]
    fn explicit_apply_is_exact_atomic_and_retry_idempotent_without_replaying_over_later_writer() {
        let db = VaultDb::open_in_memory().unwrap();
        let (review, proposal) = setup(&db);
        let receipt = apply_review(&db, "ticket", &review, &proposal).unwrap();
        assert_eq!(receipt["content"], "reviewed\r\n");
        assert_eq!(receipt["title"], "Chosen title");
        assert_eq!(receipt["tags"], json!(["local"]));
        assert_eq!(receipt["pinned"], true);
        assert_eq!(db.list_versions("review-note").unwrap().len(), 1);
        db.tag_atomic("review-note", &["later".into()], &[])
            .unwrap();
        let later = db.accepted_note("review-note").unwrap().unwrap();
        assert_eq!(
            apply_review(&db, "ticket", &review, &proposal).unwrap(),
            receipt
        );
        assert_eq!(db.accepted_note("review-note").unwrap().unwrap(), later);
        let mut changed = proposal.clone();
        changed.body = "different proposal".into();
        assert_eq!(
            apply_review(&db, "ticket", &review, &changed)
                .unwrap_err()
                .kind,
            WriteErrorKind::Changed
        );
    }
    #[test]
    fn separate_connection_body_and_metadata_races_reject_without_partial_apply_or_history() {
        for mutation in ["body", "title", "tags", "pinned", "deleted", "trashed"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("test.rn");
            let db = VaultDb::open_legacy_v2(&path).unwrap();
            let other = VaultDb::open_legacy_v2(&path).unwrap();
            let (review, proposal) = setup(&db);
            match mutation {
                "body" => {
                    other
                        .conn
                        .execute(
                            "UPDATE notes SET content='racing writer' WHERE id='review-note'",
                            [],
                        )
                        .unwrap();
                }
                "title" => {
                    other
                        .conn
                        .execute(
                            "UPDATE notes SET title='racing title' WHERE id='review-note'",
                            [],
                        )
                        .unwrap();
                }
                "tags" => {
                    other
                        .tag_atomic("review-note", &["racing".into()], &[])
                        .unwrap();
                }
                "pinned" => {
                    other
                        .conn
                        .execute("UPDATE notes SET pinned=1 WHERE id='review-note'", [])
                        .unwrap();
                }
                "deleted" => {
                    other.delete_note("review-note").unwrap();
                }
                _ => {
                    other.set_note_trashed("review-note", true).unwrap();
                }
            }
            let before = other.get_note_full("review-note").unwrap();
            let error = apply_review(&db, "stale-ticket", &review, &proposal).unwrap_err();
            assert_eq!(
                error.kind,
                match mutation {
                    "deleted" => WriteErrorKind::Deleted,
                    "trashed" => WriteErrorKind::Trashed,
                    _ => WriteErrorKind::Changed,
                }
            );
            assert_eq!(other.get_note_full("review-note").unwrap(), before);
            assert_eq!(
                db.conn
                    .query_row("SELECT COUNT(*) FROM versions", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
    }
    #[test]
    fn result_artifact_is_independent_of_original_draft_and_recoverable_after_cancel_or_restart() {
        let db = VaultDb::open_in_memory().unwrap();
        let (review, proposal) = setup(&db);
        let dir = tempfile::tempdir().unwrap();
        let mut service = Recovery::default();
        service.set_root(dir.path().into()).unwrap();
        for draft in [&review.draft, &proposal] {
            service.owners.insert(
                draft.owner.clone(),
                Owner {
                    window: "main".into(),
                    vault: draft.vault.clone(),
                    note: draft.note_id.clone(),
                    dirty: true,
                    conflicted: true,
                    locked: false,
                    sequence: 0,
                },
            );
            service.checkpoint("main", draft).unwrap();
        }
        let mut restarted = Recovery::default();
        restarted.set_root(dir.path().into()).unwrap();
        let records = restarted.records().unwrap();
        assert_eq!(records.len(), 2);
        assert!(records.iter().any(|d| d.body == "local\r\n"));
        assert!(records.iter().any(|d| d.body == "reviewed\r\n"));
        assert_eq!(
            db.get_note_content("review-note").unwrap().unwrap(),
            "base\r\n"
        );
    }
}

#[cfg(test)]
mod review_ipc_tests {
    use super::*;
    use crate::types::AppConfig;
    use crate::vault::writer::{NoteWrite, WritePolicy};
    #[test]
    fn real_review_commands_gate_owner_revision_and_exact_result_before_adoption() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.rn");
        let db = VaultDb::open_legacy_v2(&path).unwrap();
        let note = NewNote {
            id: "n".into(),
            title: "Title".into(),
            content: "base\r\n".into(),
            tags: vec![],
            pinned: false,
            created: "2026-11-11T00:00:00Z".into(),
            modified: "2026-11-11T00:00:00Z".into(),
        };
        let saved = db
            .write_authoritative(&NoteWrite::from_note(&db, &note, WritePolicy::Create), 20)
            .unwrap();
        let identity = db.identity();
        let image = db
            .add_attachment(
                "n",
                "snapshot.png",
                "image/png",
                b"captured image bytes",
                "2026-11-11T00:00:00Z",
            )
            .unwrap();
        let base = json!({"path":"n","raw":saved["content"],"content":saved["content"],"meta":{
            "id":"n","title":saved["title"],"tags":saved["tags"],"pinned":false,"created":saved["created"],"modified":saved["modified"],
            "accepted":{"vault":identity,"token":saved["accepted"]["token"],"body":saved["content"],"title":saved["title"],"tags":saved["tags"],"pinned":false,"trashed":false,"created":saved["created"],"modified":saved["modified"]}}});
        let app = tauri::test::mock_builder()
            .invoke_handler(tauri::generate_handler![
                recovery_register,
                recovery_checkpoint,
                recovery_review_open,
                recovery_review_checkpoint,
                recovery_review_apply,
                recovery_review_cancel,
                recovery_review_asset,
                recovery_list
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
        let other = tauri::WebviewWindowBuilder::new(&app, "note-other", Default::default())
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
            json!({"owner":owner,"vault":identity,"noteId":"n"}),
        )
        .unwrap();
        let mut draft = Draft {
            owner: owner.clone(),
            sequence: 1,
            revision: 1,
            captured_at: note.created,
            vault: identity,
            note_id: "n".into(),
            base,
            body: format!("local ![image](.rn-attachments/{})\r\n", image.id),
            title: "Title".into(),
            tags: vec![],
            pinned: false,
            blobs: vec![],
            conflicted: true,
        };
        request(&main, "recovery_checkpoint", json!({"draft":draft})).unwrap();
        let ticket: Value = request(
            &main,
            "recovery_review_open",
            json!({"owner":owner,"sequence":1,"revision":1}),
        )
        .unwrap()
        .deserialize()
        .unwrap();
        assert_eq!(ticket["available"], true);
        let id = ticket["reviewId"].as_str().unwrap();
        let reference = format!(".rn-attachments/{}", image.id);
        assert!(request(
            &other,
            "recovery_review_asset",
            json!({"owner":owner,"reviewId":id,"reference":reference})
        )
        .is_err());
        let preview: Value = request(
            &main,
            "recovery_review_asset",
            json!({"owner":owner,"reviewId":id,"reference":reference}),
        )
        .unwrap()
        .deserialize()
        .unwrap();
        assert_eq!(preview["data"], json!(b"captured image bytes".to_vec()));
        let proposal =
            json!({"body":"chosen\r\n","title":"Chosen title","tags":["chosen"],"pinned":true});
        request(
            &main,
            "recovery_review_checkpoint",
            json!({"owner":owner,"reviewId":id,"sequence":1,"proposal":proposal}),
        )
        .unwrap();
        assert!(request(
            &other,
            "recovery_review_apply",
            json!({"owner":owner,"reviewId":id,"sequence":1,"revision":1})
        )
        .is_err());
        draft.sequence = 2;
        draft.revision = 2;
        draft.body = "new typing".into();
        request(&main, "recovery_checkpoint", json!({"draft":draft})).unwrap();
        let stale = request(
            &main,
            "recovery_review_apply",
            json!({"owner":owner,"reviewId":id,"sequence":1,"revision":1}),
        )
        .unwrap_err();
        assert_eq!(stale["kind"], "changed");
        request(
            &main,
            "recovery_review_cancel",
            json!({"owner":owner,"reviewId":id}),
        )
        .unwrap();
        let artifacts: Value = request(&main, "recovery_list", json!({}))
            .unwrap()
            .deserialize()
            .unwrap();
        assert_eq!(artifacts.as_array().unwrap().len(), 1);
        assert_eq!(artifacts[0]["body"], "chosen\r\n");
        let renewed: Value = request(
            &main,
            "recovery_review_open",
            json!({"owner":owner,"sequence":2,"revision":2}),
        )
        .unwrap()
        .deserialize()
        .unwrap();
        let next = renewed["reviewId"].as_str().unwrap();
        request(
            &main,
            "recovery_review_checkpoint",
            json!({"owner":owner,"reviewId":next,"sequence":1,"proposal":proposal}),
        )
        .unwrap();
        let receipt: Value = request(
            &main,
            "recovery_review_apply",
            json!({"owner":owner,"reviewId":next,"sequence":1,"revision":2}),
        )
        .unwrap()
        .deserialize()
        .unwrap();
        assert_eq!(receipt["content"], "chosen\r\n");
        assert_eq!(receipt["title"], "Chosen title");
        assert_eq!(receipt["tags"], json!(["chosen"]));
        assert_eq!(receipt["pinned"], true);
        assert!(
            state.recovery.lock().unwrap().owners[&owner].dirty,
            "Native commit alone cannot acknowledge the live editor revision"
        );
        let again: Value = request(
            &main,
            "recovery_review_apply",
            json!({"owner":owner,"reviewId":next,"sequence":1,"revision":2}),
        )
        .unwrap()
        .deserialize()
        .unwrap();
        assert_eq!(again, receipt);
        draft.sequence = 3;
        draft.revision = 3;
        draft.base["meta"]
            .as_object_mut()
            .unwrap()
            .remove("accepted");
        request(&main, "recovery_checkpoint", json!({"draft":draft})).unwrap();
        let unknown: Value = request(
            &main,
            "recovery_review_open",
            json!({"owner":owner,"sequence":3,"revision":3}),
        )
        .unwrap()
        .deserialize()
        .unwrap();
        assert_eq!(unknown["available"], false);
        assert!(unknown.get("reviewId").is_none());
    }
}

#[cfg(test)]
mod review_race_tests {
    use super::*;
    use crate::vault::writer::{NoteWrite, WriteErrorKind, WritePolicy};
    #[test]
    fn a_writer_committing_while_apply_waits_for_the_lock_invalidates_the_reviewed_token() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static WAITING: AtomicBool = AtomicBool::new(false);
        WAITING.store(false, Ordering::Release);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("race.rn");
        let writer = VaultDb::open_legacy_v2(&path).unwrap();
        let reviewer = VaultDb::open_legacy_v2(&path).unwrap();
        let note = NewNote {
            id: "n".into(),
            title: "Title".into(),
            content: "base".into(),
            tags: vec![],
            pinned: false,
            created: "2026-11-11T00:00:00Z".into(),
            modified: "2026-11-11T00:00:00Z".into(),
        };
        let remote = writer
            .write_authoritative(
                &NoteWrite::from_note(&writer, &note, WritePolicy::Create),
                20,
            )
            .unwrap();
        let draft = Draft {
            owner: uuid::Uuid::new_v4().to_string(),
            sequence: 1,
            revision: 1,
            captured_at: note.created.clone(),
            vault: reviewer.identity(),
            note_id: note.id.clone(),
            base: json!({}),
            body: "local".into(),
            title: note.title.clone(),
            tags: vec![],
            pinned: false,
            blobs: vec![],
            conflicted: true,
        };
        let review = Review {
            window: "main".into(),
            owner: draft.owner.clone(),
            draft: draft.clone(),
            remote,
            active: true,
            proposal_sequence: 1,
        };
        let proposal = Draft {
            body: "result".into(),
            ..draft
        };
        let tx = rusqlite::Transaction::new_unchecked(
            &writer.conn,
            rusqlite::TransactionBehavior::Immediate,
        )
        .unwrap();
        tx.execute(
            "UPDATE notes SET content='concurrent committed body' WHERE id='n'",
            [],
        )
        .unwrap();
        reviewer
            .conn
            .busy_handler(Some(|_| {
                WAITING.store(true, Ordering::Release);
                std::thread::sleep(std::time::Duration::from_millis(1));
                true
            }))
            .unwrap();
        let applying = std::thread::spawn(move || {
            apply_review(&reviewer, "racing-review", &review, &proposal)
        });
        let started = std::time::Instant::now();
        while !WAITING.load(Ordering::Acquire) {
            assert!(started.elapsed() < std::time::Duration::from_secs(5));
            std::thread::yield_now();
        }
        tx.commit().unwrap();
        assert_eq!(
            applying.join().unwrap().unwrap_err().kind,
            WriteErrorKind::Changed
        );
        assert_eq!(
            writer.get_note_content("n").unwrap().unwrap(),
            "concurrent committed body"
        );
        assert_eq!(writer.list_versions("n").unwrap().len(), 0);
    }
}

/// Snapshot assets come from the captured review, never the currently displayed
/// vault or a global active note. Non-images stay inert attachment references.
#[tauri::command]
pub fn recovery_review_asset<R: Runtime>(
    window: tauri::Window<R>,
    state: State<'_, AppState>,
    owner: String,
    review_id: String,
    reference: String,
) -> Result<Value, String> {
    let service = state.recovery.lock().map_err(|e| e.to_string())?;
    service.owner(&owner, window.label())?;
    let review = service
        .reviews
        .get(&review_id)
        .filter(|review| review.active && review.owner == owner && review.window == window.label())
        .ok_or("This image does not belong to the active owner review")?;
    let blob = review
        .draft
        .blobs
        .iter()
        .find(|blob| {
            blob.reference == reference
                && matches!(
                    blob.mime.as_str(),
                    "image/png"
                        | "image/jpeg"
                        | "image/gif"
                        | "image/webp"
                        | "image/avif"
                        | "image/bmp"
                )
        })
        .ok_or("Image is not part of this captured review")?;
    Ok(json!({"mime":blob.mime,"data":blob.data}))
}

// Only successful private journal publication can construct this capability.
// Retention cannot authorize pin deletion using a fabricated digest/string.
pub(crate) struct DurableResolution {
    owner: String,
    vault_uuid: String,
    storage_epoch: String,
    pins: Vec<PinRef>,
    keep_revision: Option<String>,
    digest: String,
}
impl DurableResolution {
    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }
    pub(crate) fn matches(
        &self,
        db: &VaultDb,
        owner: &str,
        pins: &[PinRef],
        keep: Option<&str>,
    ) -> bool {
        db.revision_session.as_ref().is_some_and(|s| {
            s.identity.vault_uuid == self.vault_uuid
                && s.identity.storage_epoch == self.storage_epoch
        }) && self.owner == owner
            && self.pins.as_slice() == pins
            && self.keep_revision.as_deref() == keep
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ResolutionTombstone<'a> {
    owner: &'a str,
    vault_uuid: &'a str,
    storage_epoch: &'a str,
    through_sequence: u64,
    local_revision: u64,
    identity: Option<&'a RevisionBinding>,
    action: &'a str,
    pins: &'a [PinRef],
    keep_revision: Option<&'a str>,
    stream_order: u64,
}
impl Recovery {
    /// Immutable ordered proof first, then the stream's atomic resolved marker.
    /// A failed fsync/SQL release never authorizes disposal or a guessed cleanup.
    #[allow(clippy::too_many_arguments)] // All resolution fences are recorded explicitly.
    fn publish_resolution(
        &self,
        db: &VaultDb,
        owner: &str,
        sequence: u64,
        local: u64,
        identity: Option<&RevisionBinding>,
        action: &str,
        pins: &[PinRef],
        keep: Option<&str>,
    ) -> Result<DurableResolution, String> {
        let dir = self.dir(owner)?;
        private_dir(&dir)?;
        let mut order = 0u64;
        for file in std::fs::read_dir(&dir).map_err(|e| e.to_string())? {
            let name = file
                .map_err(|e| e.to_string())?
                .file_name()
                .to_string_lossy()
                .into_owned();
            if let Some(n) = name
                .strip_prefix("tombstone-")
                .and_then(|n| n.strip_suffix(".json"))
                .and_then(|n| n.parse::<u64>().ok())
            {
                order = order.max(n);
            }
        }
        let order = order.checked_add(1).ok_or("Recovery stream exhausted")?;
        let vault = &db
            .revision_session
            .as_ref()
            .ok_or("Expected revision vault")?
            .identity;
        let proof = ResolutionTombstone {
            owner,
            vault_uuid: &vault.vault_uuid,
            storage_epoch: &vault.storage_epoch,
            through_sequence: sequence,
            local_revision: local,
            identity,
            action,
            pins,
            keep_revision: keep,
            stream_order: order,
        };
        let bytes = serde_json::to_vec(&proof).map_err(|e| e.to_string())?;
        atomic(&dir.join(format!("tombstone-{order:020}.json")), &bytes)?;
        #[cfg(test)]
        crash_boundary("resolution-proof");
        atomic(&dir.join(format!("resolved-{sequence}")), &bytes)?;
        #[cfg(test)]
        crash_boundary("resolution-marker");
        Ok(DurableResolution {
            owner: owner.into(),
            vault_uuid: vault.vault_uuid.clone(),
            storage_epoch: vault.storage_epoch.clone(),
            pins: pins.to_vec(),
            keep_revision: keep.map(str::to_owned),
            digest: digest(&bytes),
        })
    }
    #[allow(clippy::too_many_arguments)] // Mirrors the owner-scoped acknowledgment contract.
    fn resolve_revision(
        &mut self,
        db: &VaultDb,
        window: &str,
        id: &str,
        sequence: u64,
        local: u64,
        expected: &RevisionBinding,
        action: &str,
        next: Option<&Value>,
    ) -> Result<(), String> {
        let owner = self.owner(id, window)?.clone();
        let current = &db
            .revision_session
            .as_ref()
            .ok_or("Expected revision vault")?
            .identity;
        // A paused clean owner can explicitly retire its old pin after restore,
        // with the same local-revision/tombstone proof. This never rebinds a draft.
        let retiring_clean = action == "release"
            && owner.locked
            && !owner.dirty
            && !owner.conflicted
            && next.is_none();
        if expected.vault_uuid != current.vault_uuid
            || (action != "discard"
                && !retiring_clean
                && expected.storage_epoch != current.storage_epoch)
        {
            return Err(
                "Reopen the originating vault; saved baselines cannot cross restore epochs".into(),
            );
        }
        let enrolled = self
            .bindings
            .get(id)
            .ok_or("Owner has no revision enrollment")?;
        if !matches!(
            action,
            "save" | "discard" | "release" | "apply" | "review_cancel"
        ) || enrolled.vault_uuid != expected.vault_uuid
            || enrolled.storage_epoch != expected.storage_epoch
            || enrolled.incarnation_id != expected.incarnation_id
        {
            return Err("Resolution identity/action does not match this owner".into());
        }
        // A checkpoint/save reply may be lost after native enrollment advanced.
        // Older accepted bases remain pinned until this ordered acknowledgment.
        // Never treat an arbitrary unpinned historical token as owner authority.
        if enrolled != expected && !db.owns_binding(id, expected)? {
            if action == "save" && !owner.dirty && !owner.conflicted {
                if let Some(next) = next {
                    let next_binding = RevisionBinding::from_accepted(next)?;
                    next_binding.verify(db, &owner.note, next)?;
                    if &next_binding == enrolled {
                        for file in std::fs::read_dir(self.dir(id)?).map_err(|e| e.to_string())? {
                            let file = file.map_err(|e| e.to_string())?;
                            if !file.file_name().to_string_lossy().starts_with("tombstone-") {
                                continue;
                            }
                            let proof: Value = serde_json::from_slice(
                                &std::fs::read(file.path()).map_err(|e| e.to_string())?,
                            )
                            .map_err(|e| e.to_string())?;
                            if proof["identity"] == json!(expected)
                                && proof["action"] == "save"
                                && proof["localRevision"] == local
                                && proof["throughSequence"]
                                    .as_u64()
                                    .is_some_and(|n| n >= sequence)
                                && proof["keepRevision"] == enrolled.revision_id
                            {
                                let pins = db.owner_pins(id)?;
                                if !pins.is_empty()
                                    && pins.iter().all(|p| p.revision_id == enrolled.revision_id)
                                {
                                    return Ok(());
                                }
                            }
                        }
                    }
                }
            }
            return Err("Resolution base is no longer pinned by this owner".into());
        }
        if action == "save" && next.is_none() {
            return Err("Saved resolution requires its exact committed receipt".into());
        }
        if action == "release" && (owner.dirty || owner.conflicted) {
            return Err("Unresolved owner cannot release pins".into());
        }
        if sequence > owner.sequence {
            return Err("Unacknowledged journal sequence".into());
        }
        let mut published = sequence;
        let mut latest: Option<(u64, u64)> = None;
        let dir = self.dir(id)?;
        match std::fs::read_dir(&dir) {
            Ok(files) => {
                for file in files {
                    let file = file.map_err(|e| e.to_string())?;
                    let name = file.file_name().to_string_lossy().into_owned();
                    if let Some(n) = name
                        .strip_suffix(".json")
                        .and_then(|n| n.parse::<u64>().ok())
                    {
                        let draft = read_draft(&file.path())?;
                        if draft.owner != id || draft.revision > local {
                            return Err("A newer local draft still needs recovery".into());
                        }
                        let identity: RevisionBinding =
                            serde_json::from_value(draft.base["recoveryIdentity"].clone())
                                .map_err(|e| e.to_string())?;
                        if identity.vault_uuid != expected.vault_uuid
                            || identity.storage_epoch != expected.storage_epoch
                            || identity.incarnation_id != expected.incarnation_id
                        {
                            return Err(
                                "Journal ancestry changed; explicit enrollment required".into()
                            );
                        }
                        if latest.is_none_or(|(seq, _)| n > seq) {
                            latest = Some((n, draft.revision));
                        }
                        published = published.max(n);
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && sequence == 0 => {}
            Err(e) => return Err(e.to_string()),
        }
        if matches!(action, "discard" | "apply")
            && latest.is_some_and(|(_, revision)| revision != local)
        {
            return Err("Acknowledgment does not match the latest published local revision".into());
        }
        if let Some(accepted) = next {
            let next_identity = RevisionBinding::from_accepted(accepted)?;
            if next_identity.vault_uuid != expected.vault_uuid
                || next_identity.storage_epoch != expected.storage_epoch
                || next_identity.incarnation_id != expected.incarnation_id
            {
                return Err("Resolution cannot rebind a vault, epoch or incarnation".into());
            }
        }
        let next_binding = next
            .map(|accepted| db.pin_owner_base(id, &owner.note, accepted))
            .transpose()?;
        let pins = db.owner_pins(id)?;
        let proof = self.publish_resolution(
            db,
            id,
            published,
            local,
            Some(expected),
            action,
            &pins,
            next_binding.as_ref().map(|b| b.revision_id.as_str()),
        )?;
        db.release_pins(
            id,
            &pins,
            next_binding.as_ref().map(|b| b.revision_id.as_str()),
            &proof,
        )?;
        #[cfg(test)]
        crash_boundary("resolution-released");
        if let Some(binding) = next_binding {
            self.bindings.insert(id.into(), binding);
        }
        let owner = self
            .owners
            .get_mut(id)
            .ok_or("Owner ended during resolution")?;
        owner.sequence = published;
        owner.dirty = false;
        owner.conflicted = false;
        Ok(())
    }
    pub(crate) fn cleanup_pin_group(
        &mut self,
        db: &VaultDb,
        group: &PinGroup,
    ) -> Result<(), String> {
        if group.pins.is_empty() {
            return Err("Empty pin cleanup is not an acknowledgment".into());
        }
        if self.owners.contains_key(&group.owner)
            || self
                .reviews
                .values()
                .any(|r| r.active && (r.owner == group.owner))
            || self.reviews.contains_key(&group.owner)
            || self.records()?.iter().any(|d| d.owner == group.owner)
        {
            return Err("Resolve the live/recovered artifact instead of cleaning its pins".into());
        }
        let session = &db
            .revision_session
            .as_ref()
            .ok_or("Expected revision vault")?
            .instance;
        for pin in &group.pins {
            let origin = crate::vault::revision_retention::pin_session(pin)?;
            if (origin != session || group.owner == *session)
                && crate::vault::revision_retention::session_active(db, origin)?
            {
                return Err("This pin's process is still alive".into());
            }
        }
        if db.owner_pins(&group.owner)? != group.pins {
            return Err("Orphan pin set changed; refresh recovery inbox".into());
        }
        let proof = self.publish_resolution(
            db,
            &group.owner,
            0,
            0,
            None,
            "explicit_orphan_cleanup",
            &group.pins,
            None,
        )?;
        db.release_pins(&group.owner, &group.pins, None, &proof)
    }
}

#[tauri::command]
pub fn recovery_register_revision<R: Runtime>(
    window: tauri::Window<R>,
    state: State<'_, AppState>,
    owner: String,
    vault: VaultIdentity,
    note_id: String,
    accepted: Value,
) -> Result<Value, String> {
    init(window.app_handle(), &state)?;
    valid_owner(&owner)?;
    let mut service = state.recovery.lock().map_err(|e| e.to_string())?;
    if service.barrier.is_some() {
        return Err("A vault operation is pausing new owners".into());
    }
    if let Some(existing) = service.owners.get(&owner) {
        if existing.window != window.label() || existing.vault != vault || existing.note != note_id
        {
            return Err("Owner rebinding refused".into());
        }
    }
    let slot = state.vault_db.lock().map_err(|e| e.to_string())?;
    let db = slot.as_ref().ok_or("Originating vault is closed")?;
    db.validate_writer_vault(&vault)
        .map_err(|e| e.to_string())?;
    let proposed = RevisionBinding::from_accepted(&accepted)?;
    if service.bindings.get(&owner).is_some_and(|b| {
        b.vault_uuid != proposed.vault_uuid
            || b.storage_epoch != proposed.storage_epoch
            || b.incarnation_id != proposed.incarnation_id
    }) {
        return Err("Owner revision identity rebinding refused".into());
    }
    let receipt = db.pinned_owner_read(&owner, &note_id, &accepted)?;
    #[cfg(test)]
    crash_boundary("owner-accepted");
    let binding = RevisionBinding::from_accepted(&receipt["accepted"])?;
    service.owners.entry(owner.clone()).or_insert(Owner {
        window: window.label().into(),
        vault,
        note: note_id,
        dirty: false,
        conflicted: false,
        locked: false,
        sequence: 0,
    });
    service.bindings.insert(owner, binding.clone());
    Ok(json!({"identity":binding,"accepted":receipt}))
}
#[allow(clippy::too_many_arguments)] // Explicit wire fields preserve the narrow recovery API.
#[tauri::command]
pub fn recovery_resolve_revision<R: Runtime>(
    window: tauri::Window<R>,
    state: State<'_, AppState>,
    owner: String,
    sequence: u64,
    local_revision: u64,
    identity: RevisionBinding,
    action: String,
    next_accepted: Option<Value>,
) -> Result<(), String> {
    let mut service = state.recovery.lock().map_err(|e| e.to_string())?;
    let slot = state.vault_db.lock().map_err(|e| e.to_string())?;
    let db = slot.as_ref().ok_or("Originating vault is closed")?;
    service.resolve_revision(
        db,
        window.label(),
        &owner,
        sequence,
        local_revision,
        &identity,
        &action,
        next_accepted.as_ref(),
    )
}
#[tauri::command]
pub fn recovery_orphan_pins<R: Runtime>(
    window: tauri::Window<R>,
    state: State<'_, AppState>,
) -> Result<Vec<PinGroup>, String> {
    init(window.app_handle(), &state)?;
    let service = state.recovery.lock().map_err(|e| e.to_string())?;
    let slot = state.vault_db.lock().map_err(|e| e.to_string())?;
    let Some(db) = slot.as_ref().filter(|db| db.is_v3()) else {
        return Ok(vec![]);
    };
    let records = service.records()?;
    let mut result = Vec::new();
    for group in db.pin_groups()? {
        if service.owners.contains_key(&group.owner)
            || service.reviews.contains_key(&group.owner)
            || records.iter().any(|r| r.owner == group.owner)
        {
            continue;
        }
        let instance = &db.revision_session.as_ref().unwrap().instance;
        let mut active = false;
        for pin in &group.pins {
            let origin = crate::vault::revision_retention::pin_session(pin)?;
            if (origin != instance || group.owner == *instance)
                && crate::vault::revision_retention::session_active(db, origin)?
            {
                active = true;
                break;
            }
        }
        if !active {
            result.push(group);
        }
    }
    Ok(result)
}
#[tauri::command]
pub fn recovery_cleanup_pins<R: Runtime>(
    window: tauri::Window<R>,
    state: State<'_, AppState>,
    group: PinGroup,
) -> Result<(), String> {
    init(window.app_handle(), &state)?;
    let mut service = state.recovery.lock().map_err(|e| e.to_string())?;
    let slot = state.vault_db.lock().map_err(|e| e.to_string())?;
    service.cleanup_pin_group(slot.as_ref().ok_or("Originating vault is closed")?, &group)
}

#[cfg(test)]
mod revision_retention_tests {
    use super::*;
    use crate::vault::{
        ancestry,
        writer::{NoteWrite, WritePolicy},
    };
    struct Fixture {
        root: tempfile::TempDir,
        path: PathBuf,
        ledger: PathBuf,
        journal: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("test.rn");
            let ledger = root.path().join("ledger");
            let journal = root.path().join("journal");
            ancestry::migrate(&path, &ledger).unwrap();
            Self {
                root,
                path,
                ledger,
                journal,
            }
        }
        fn open(&self) -> VaultDb {
            VaultDb::open_v3(&self.path, &self.ledger).unwrap()
        }
        fn service(&self) -> Recovery {
            let mut r = Recovery::default();
            r.set_root(self.journal.clone()).unwrap();
            r
        }
    }
    fn write(db: &VaultDb, prior: Option<&Value>, body: &str) -> Value {
        let note = NewNote {
            id: "n".into(),
            title: "Title".into(),
            content: body.into(),
            tags: vec![],
            pinned: false,
            created: "created".into(),
            modified: "modified".into(),
        };
        let policy = prior
            .map(|p| WritePolicy::Update {
                expected_token: p["accepted"]["token"].as_str().unwrap().into(),
            })
            .unwrap_or(WritePolicy::Create);
        db.write_authoritative(&NoteWrite::from_note(db, &note, policy), 1)
            .unwrap()
    }
    fn snapshot(receipt: &Value) -> Value {
        let mut accepted = receipt["accepted"].clone();
        for (target, source) in [
            ("body", "content"),
            ("title", "title"),
            ("tags", "tags"),
            ("pinned", "pinned"),
            ("trashed", "trashed"),
            ("created", "created"),
            ("modified", "modified"),
        ] {
            accepted[target] = receipt[source].clone();
        }
        json!({"path":"n","content":receipt["content"],"raw":receipt["content"],"meta":{"id":"n","title":receipt["title"],"tags":receipt["tags"],"pinned":receipt["pinned"],"created":receipt["created"],"modified":receipt["modified"],"accepted":accepted}})
    }
    fn enroll(service: &mut Recovery, db: &VaultDb, owner: &str, base: &Value) -> Draft {
        let receipt = db.pinned_owner_read(owner, "n", &base["accepted"]).unwrap();
        let binding = RevisionBinding::from_accepted(&receipt["accepted"]).unwrap();
        let mut base = snapshot(base);
        base["recoveryIdentity"] = json!(binding);
        base["recoveryIdentity"]["localRevision"] = json!(2);
        service.bindings.insert(owner.into(), binding);
        service.owners.insert(
            owner.into(),
            Owner {
                window: "main".into(),
                vault: db.identity(),
                note: "n".into(),
                dirty: true,
                conflicted: true,
                locked: false,
                sequence: 0,
            },
        );
        Draft {
            owner: owner.into(),
            sequence: 1,
            revision: 2,
            captured_at: chrono::Utc::now().to_rfc3339(),
            vault: db.identity(),
            note_id: "n".into(),
            base,
            body: "local draft".into(),
            title: "Local title".into(),
            tags: vec![],
            pinned: false,
            blobs: vec![],
            conflicted: true,
        }
    }
    fn prune_unclaimed_sessions(service: &mut Recovery, db: &VaultDb, except: &[&str]) {
        for group in db.pin_groups().unwrap() {
            if !except.contains(&group.owner.as_str()) {
                service.cleanup_pin_group(db, &group).unwrap();
            }
        }
    }
    fn count(db: &VaultDb, table: &str) -> i64 {
        db.conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
    #[test]
    fn retention_rechecks_fixed_floors_and_review_pins_and_rolls_back_every_marker_on_failure() {
        let f = Fixture::new();
        let db = f.open();
        let base = write(&db, None, "B");
        let owner = uuid::Uuid::new_v4().to_string();
        let review = uuid::Uuid::new_v4().to_string();
        let mut service = f.service();
        let draft = enroll(&mut service, &db, &owner, &base);
        service.checkpoint("main", &draft).unwrap();
        let remote = write(&db, Some(&base), "R");
        assert_eq!(
            db.pin_review(&review, "n", &draft.base["meta"]["accepted"])
                .unwrap(),
            remote
        );
        let mut head = remote.clone();
        for n in 2..130 {
            head = write(&db, Some(&head), &format!("body {n}"));
        }
        let header_count = count(&db, "revisions");
        let parent_count = count(&db, "revision_parents");
        let counter: i64 = db
            .conn
            .query_row(
                "SELECT high_water FROM note_revision_counters WHERE note_id='n'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        drop(db);
        let db = f.open();
        // Restart does not expire any pin, even with no native owner/heartbeat.
        assert_eq!(
            db.collect_revisions_at(chrono::Utc::now() + chrono::Duration::days(120))
                .unwrap()
                .states_pruned,
            0
        );
        prune_unclaimed_sessions(&mut service, &db, &[&owner, &review]);
        db.conn.execute_batch("CREATE TEMP TRIGGER fail_retention BEFORE DELETE ON revision_states BEGIN SELECT RAISE(ABORT,'fixture disk failure'); END;").unwrap();
        assert!(db
            .collect_revisions_at(chrono::Utc::now() + chrono::Duration::days(120))
            .is_err());
        assert_eq!(count(&db, "revision_pruning"), 0);
        assert_eq!(count(&db, "revision_states"), 130);
        db.conn
            .execute_batch("DROP TRIGGER fail_retention")
            .unwrap();
        let report = db
            .collect_revisions_at(chrono::Utc::now() + chrono::Duration::days(120))
            .unwrap();
        assert_eq!(report.states_pruned, 28);
        assert_eq!(count(&db, "revision_states"), 102);
        assert_eq!(count(&db, "revisions"), header_count);
        assert_eq!(count(&db, "revision_parents"), parent_count);
        assert_eq!(
            db.conn
                .query_row(
                    "SELECT high_water FROM note_revision_counters WHERE note_id='n'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            counter
        );
        assert_eq!(
            db.accepted_at_token("n", base["accepted"]["token"].as_str().unwrap())
                .unwrap(),
            base
        );
        assert_eq!(
            db.accepted_at_token("n", remote["accepted"]["token"].as_str().unwrap())
                .unwrap(),
            remote
        );
        // Clearing curated history changes neither graph retention nor pins.
        let pins = count(&db, "revision_pins");
        let tx = db
            .begin_write("gui", None, &json!(["clear-history"]))
            .unwrap();
        tx.execute("DELETE FROM versions", []).unwrap();
        tx.commit().unwrap();
        assert_eq!(count(&db, "revision_pins"), pins);
        assert_eq!(db.collect_revisions().unwrap().states_pruned, 0);
        assert!(service.assert_quiet(None, None).is_err());
    }
    #[test]
    fn exact_resolution_rejects_newer_typing_missing_proofs_and_journal_disk_failure() {
        let f = Fixture::new();
        let db = f.open();
        let base = write(&db, None, "B");
        let mut service = f.service();
        let owner = uuid::Uuid::new_v4().to_string();
        let draft = enroll(&mut service, &db, &owner, &base);
        service.checkpoint("main", &draft).unwrap();
        let binding = service.bindings[&owner].clone();
        let pins = db.owner_pins(&owner).unwrap();
        assert!(service
            .resolve_revision(&db, "main", &owner, 1, 2, &binding, "invented-action", None)
            .is_err());
        let mut next = draft.clone();
        next.sequence = 2;
        next.revision = 3;
        next.body = "new typing".into();
        next.base["recoveryIdentity"]["localRevision"] = json!(3);
        service.checkpoint("main", &next).unwrap();
        assert!(service
            .resolve_revision(&db, "main", &owner, 1, 2, &binding, "discard", None)
            .is_err());
        assert!(service
            .resolve_revision(&db, "other-window", &owner, 2, 3, &binding, "discard", None)
            .is_err());
        // An unusable private directory fails before releasing even one pin.
        let dir = service.dir(&owner).unwrap();
        std::fs::write(dir.join("tombstone-00000000000000000001.json"), b"occupied").unwrap();
        let original = service.root.take();
        service.root = Some(f.root.path().join("not-a-directory"));
        std::fs::write(service.root.as_ref().unwrap(), b"disk failure").unwrap();
        assert!(service
            .resolve_revision(&db, "main", &owner, 2, 3, &binding, "discard", None)
            .is_err());
        assert_eq!(db.owner_pins(&owner).unwrap(), pins);
        assert!(service.owners[&owner].dirty);
        service.root = original;
        // A lost checkpoint reply leaves a lower renderer sequence; exact local
        // revision plus the published stream permits an ordered explicit discard.
        service
            .resolve_revision(&db, "main", &owner, 1, 3, &binding, "discard", None)
            .unwrap();
        assert!(db.owner_pins(&owner).unwrap().is_empty());
        assert!(service.records().unwrap().is_empty());
        assert!(dir.join("tombstone-00000000000000000002.json").exists());
    }
    #[test]
    fn lost_save_acknowledgment_replays_only_the_matching_durable_proof_and_never_resolves_new_typing(
    ) {
        let f = Fixture::new();
        let db = f.open();
        let base = write(&db, None, "B");
        let mut service = f.service();
        let owner = uuid::Uuid::new_v4().to_string();
        let draft = enroll(&mut service, &db, &owner, &base);
        service.checkpoint("main", &draft).unwrap();
        let binding = service.bindings[&owner].clone();
        let committed = write(&db, Some(&base), "saved local");
        let accepted = snapshot(&committed)["meta"]["accepted"].clone();
        service
            .resolve_revision(&db, "main", &owner, 1, 2, &binding, "save", Some(&accepted))
            .unwrap();
        assert_eq!(db.owner_pins(&owner).unwrap().len(), 1);
        service
            .resolve_revision(&db, "main", &owner, 1, 2, &binding, "save", Some(&accepted))
            .unwrap();
        let mut next = draft.clone();
        next.sequence = 2;
        next.revision = 3;
        next.body = "newer typing".into();
        next.base = snapshot(&committed);
        next.base["recoveryIdentity"] = json!(service.bindings[&owner]);
        next.base["recoveryIdentity"]["localRevision"] = json!(3);
        service.checkpoint("main", &next).unwrap();
        assert!(service
            .resolve_revision(&db, "main", &owner, 1, 2, &binding, "save", Some(&accepted))
            .is_err());
        assert!(service.owners[&owner].dirty);
        assert_eq!(service.records().unwrap()[0].body, "newer typing");
    }
    #[test]
    fn missing_journal_never_releases_orphans_and_live_process_cleanup_is_refused() {
        let f = Fixture::new();
        let db = f.open();
        let base = write(&db, None, "B");
        let id = db.revision_session.as_ref().unwrap().instance.clone();
        db.accepted_note("n").unwrap().unwrap();
        let read = db
            .pin_groups()
            .unwrap()
            .into_iter()
            .find(|p| p.owner == id)
            .unwrap();
        let mut service = f.service();
        assert!(service.cleanup_pin_group(&db, &read).is_err());
        let owner = uuid::Uuid::new_v4().to_string();
        db.pin_owner_base(&owner, "n", &base["accepted"]).unwrap();
        let group = PinGroup {
            owner: owner.clone(),
            pins: db.owner_pins(&owner).unwrap(),
        };
        let other = f.open();
        assert!(service.cleanup_pin_group(&other, &group).is_err());
        drop(other);
        drop(db);
        let db = f.open();
        assert!(!db.owner_pins(&owner).unwrap().is_empty());
        // No local journal merely offers a choice. Only this explicit inbox
        // acknowledgement writes a durable proof before releasing the holds.
        service.cleanup_pin_group(&db, &group).unwrap();
        assert!(db.owner_pins(&owner).unwrap().is_empty());
        assert!(service
            .dir(&owner)
            .unwrap()
            .join("tombstone-00000000000000000001.json")
            .exists());
    }
    #[test]
    fn deleted_original_keeps_shared_revision_and_recovery_bytes_and_failed_checkpoint_holds() {
        let f = Fixture::new();
        let db = f.open();
        let base = write(&db, None, "B");
        let asset = db
            .add_attachment(
                "n",
                "proof.pdf",
                "application/pdf",
                b"shared private bytes",
                &chrono::Utc::now().to_rfc3339(),
            )
            .unwrap()
            .id;
        let body = format!("[proof](.rn-attachments/{asset})");
        let base = write(&db, Some(&base), &body);
        let mut service = f.service();
        let owner = uuid::Uuid::new_v4().to_string();
        let mut draft = enroll(&mut service, &db, &owner, &base);
        db.hold_checkpoint(
            &owner,
            "n",
            &draft.base["meta"]["accepted"],
            std::slice::from_ref(&asset),
            false,
        )
        .unwrap();
        draft.body = body.clone();
        draft.blobs.push(Blob {
            reference: format!(".rn-attachments/{asset}"),
            name: "proof.pdf".into(),
            mime: "application/pdf".into(),
            data: b"shared private bytes".to_vec(),
            checkpoint_bytes: None,
        });
        service.checkpoint("main", &draft).unwrap();
        db.delete_note("n").unwrap();
        db.collect_revisions().unwrap();
        assert_eq!(
            db.get_attachment_data(&asset).unwrap().unwrap().1,
            b"shared private bytes"
        );
        let recovered = read_draft(
            &service
                .dir(&owner)
                .unwrap()
                .join("00000000000000000001.json"),
        )
        .unwrap();
        assert_eq!(recovered.blobs[0].data, b"shared private bytes");
        // Failing private storage after establishing holds does not pretend the
        // checkpoint is durable, nor does it make the asset a GC candidate.
        let invalid = service.dir(&owner).unwrap().join("blocked");
        std::fs::write(&invalid, b"full").unwrap();
        let mut failed = Recovery {
            root: Some(invalid),
            owners: service.owners.clone(),
            ..Default::default()
        };
        let mut next = draft.clone();
        next.sequence = 2;
        assert!(failed.checkpoint("main", &next).is_err());
        assert!(!db.owner_pins(&owner).unwrap().is_empty());
        assert!(service.assert_quiet(None, None).is_err());
        // The same bytes copied to a fresh note are linked before fork ack.
        let copy = fork(&db, &recovered).unwrap();
        let copy_id = copy["id"].as_str().unwrap();
        assert!(copy["content"].as_str().unwrap().contains(&asset));
        assert_eq!(
            db.conn
                .query_row(
                    "SELECT COUNT(*) FROM note_attachments WHERE note_id=?1",
                    [copy_id],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        assert!(service
            .assert_quiet(Some(&db.identity().path), None)
            .is_err());
    }
    #[test]
    #[ignore = "crash fixture subprocess; invoked by parent"]
    fn crash_probe() {
        let Some(root) = std::env::var_os("RYUK_4D_CRASH_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let db = VaultDb::open_v3(&root.join("test.rn"), &root.join("ledger")).unwrap();
        let base = db.accepted_note("n").unwrap().unwrap();
        let owner = std::env::var("RYUK_4D_CRASH_OWNER").unwrap();
        let mut service = Recovery::default();
        service.set_root(root.join("journal")).unwrap();
        let mut draft = enroll(&mut service, &db, &owner, &base);
        draft.body = "[file](.rn-recovery/transient)".into();
        draft.blobs.push(Blob {
            reference: ".rn-recovery/transient".into(),
            name: "transient.bin".into(),
            mime: "application/octet-stream".into(),
            data: b"captured transient bytes".to_vec(),
            checkpoint_bytes: None,
        });
        db.hold_checkpoint(&owner, "n", &draft.base["meta"]["accepted"], &[], false)
            .unwrap();
        crash_boundary("checkpoint-holds");
        if std::env::var("RYUK_4D_CRASH_AT").unwrap() == "review-pins" {
            db.pin_review(
                &uuid::Uuid::new_v4().to_string(),
                "n",
                &draft.base["meta"]["accepted"],
            )
            .unwrap();
        }
        service.checkpoint("main", &draft).unwrap();
        let binding = service.bindings[&owner].clone();
        service
            .resolve_revision(&db, "main", &owner, 1, 2, &binding, "discard", None)
            .unwrap();
        panic!("probe did not crash");
    }
    #[test]
    fn restart_at_pin_checkpoint_and_resolution_boundaries_never_infers_release() {
        for boundary in [
            "owner-pin",
            "review-pins",
            "checkpoint-holds",
            "checkpoint-bytes",
            "checkpoint-published",
            "resolution-proof",
            "resolution-marker",
            "resolution-released",
        ] {
            let f = Fixture::new();
            let db = f.open();
            write(&db, None, "B");
            drop(db);
            let owner = uuid::Uuid::new_v4().to_string();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "recovery::revision_retention_tests::crash_probe",
                    "--exact",
                    "--ignored",
                    "--nocapture",
                ])
                .env("RYUK_4D_CRASH_AT", boundary)
                .env("RYUK_4D_CRASH_ROOT", f.root.path())
                .env("RYUK_4D_CRASH_OWNER", &owner)
                .status()
                .unwrap();
            assert_eq!(status.code(), Some(87), "{boundary}");
            let db = f.open();
            let service = f.service();
            let pins = db.owner_pins(&owner).unwrap();
            assert_eq!(
                pins.is_empty(),
                boundary == "resolution-released",
                "{boundary}"
            );
            assert_eq!(count(&db, "revision_states"), 1);
            assert_eq!(
                db.collect_revisions_at(chrono::Utc::now() + chrono::Duration::days(400))
                    .unwrap()
                    .states_pruned,
                0
            );
            if matches!(
                boundary,
                "checkpoint-bytes"
                    | "checkpoint-published"
                    | "resolution-proof"
                    | "resolution-marker"
                    | "resolution-released"
            ) {
                assert_eq!(
                    std::fs::read(
                        service
                            .dir(&owner)
                            .unwrap()
                            .join(format!("blob-{}", digest(b"captured transient bytes")))
                    )
                    .unwrap(),
                    b"captured transient bytes"
                );
            }
            if matches!(boundary, "checkpoint-published" | "resolution-proof") {
                let records = service.records().unwrap();
                assert_eq!(records.len(), 1);
                assert_eq!(records[0].blobs[0].data, b"captured transient bytes");
                assert!(service.assert_quiet(None, None).is_err());
            }
            if matches!(boundary, "resolution-marker" | "resolution-released") {
                assert!(service.records().unwrap().is_empty());
            }
        }
    }
    #[test]
    fn ipc_revision_enrollment_and_review_pins_preserve_dirty_detached_owner_across_maintenance() {
        use crate::commands;
        let f = Fixture::new();
        let db = f.open();
        let base = write(&db, None, "B");
        let identity = db.identity();
        let app = tauri::test::mock_builder()
            .invoke_handler(tauri::generate_handler![
                recovery_register_revision,
                recovery_status,
                recovery_checkpoint,
                recovery_resolve_revision,
                recovery_unregister,
                recovery_review_open,
                recovery_review_cancel,
                recovery_barrier,
                recovery_orphan_pins,
                recovery_cleanup_pins,
                commands::db_collect_revisions,
                commands::db_close,
                commands::db_open,
                commands::db_open_vault,
                commands::db_empty_trash,
                commands::db_restore_backup
            ])
            .manage(AppState::new(crate::types::AppConfig::default()))
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let state = app.state::<AppState>();
        *state.vault_db.lock().unwrap() = Some(db);
        state
            .recovery
            .lock()
            .unwrap()
            .set_root(f.journal.clone())
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
        let a = uuid::Uuid::new_v4().to_string();
        let b = uuid::Uuid::new_v4().to_string();
        let accepted = snapshot(&base)["meta"]["accepted"].clone();
        for (view, owner) in [(&main, &a), (&detached, &b)] {
            let receipt: Value = request(
                view,
                "recovery_register_revision",
                json!({"owner":owner,"vault":identity,"noteId":"n","accepted":accepted}),
            )
            .unwrap()
            .deserialize()
            .unwrap();
            assert_eq!(receipt["accepted"], base);
            let slot = state.vault_db.lock().unwrap();
            assert_eq!(slot.as_ref().unwrap().owner_pins(owner).unwrap().len(), 1);
        }
        request(
            &main,
            "recovery_status",
            json!({"owner":a,"dirty":false,"conflicted":false,"locked":true}),
        )
        .unwrap();
        let mut draft = Draft {
            owner: b.clone(),
            sequence: 1,
            revision: 2,
            captured_at: chrono::Utc::now().to_rfc3339(),
            vault: identity.clone(),
            note_id: "n".into(),
            base: snapshot(&base),
            body: "detached unsaved".into(),
            title: "Title".into(),
            tags: vec![],
            pinned: false,
            blobs: vec![],
            conflicted: true,
        };
        request(&detached, "recovery_checkpoint", json!({"draft":draft})).unwrap();
        assert!(request(&main, "recovery_unregister", json!({"owner":b})).is_err());
        let ticket: Value = request(
            &detached,
            "recovery_review_open",
            json!({"owner":b,"sequence":1,"revision":2}),
        )
        .unwrap()
        .deserialize()
        .unwrap();
        assert_eq!(
            ticket["ancestry"]["base"]["revisionId"],
            base["accepted"]["revision"]["revisionId"]
        );
        let review = ticket["reviewId"].as_str().unwrap();
        assert!(!state
            .vault_db
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .owner_pins(review)
            .unwrap()
            .is_empty());
        for (cmd, args) in [
            (
                "recovery_barrier",
                json!({"vault":identity.path,"roots":null}),
            ),
            ("db_close", json!({})),
            ("db_open", json!({"path":f.path})),
            ("db_open_vault", json!({"path":f.path})),
            ("db_empty_trash", json!({})),
            (
                "db_restore_backup",
                json!({"vaultPath":f.path,"backupPath":f.path}),
            ),
        ] {
            assert!(
                request(&main, cmd, args).is_err(),
                "{cmd} must refuse without disposing any owner"
            );
            assert!(state.vault_db.lock().unwrap().is_some());
            assert!(state.recovery.lock().unwrap().owners[&b].dirty);
        }
        assert!(close_window(&state, "note-detached").is_err());
        assert!(
            close_window(&state, "main").is_ok(),
            "closing another clean paused window retains detached ownership"
        );
        // GC is safe with dirty windows because their base pins are independent
        // roots, but the retention gate still refuses while a dirty owner is
        // enrolled; refusal preserves the detached draft either way.
        assert!(request(&main,"db_collect_revisions",json!({"vaultPath":f.path})).is_err());
        assert!(state.recovery.lock().unwrap().owners[&b].dirty);
        request(
            &detached,
            "recovery_review_cancel",
            json!({"owner":b,"reviewId":review}),
        )
        .unwrap();
        assert!(state
            .vault_db
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .owner_pins(review)
            .unwrap()
            .is_empty());
        draft.sequence = 2;
        draft.revision = 3;
        draft.body = "latest detached typing".into();
        request(&detached, "recovery_checkpoint", json!({"draft":draft})).unwrap();
        let binding = state.recovery.lock().unwrap().bindings[&b].clone();
        assert!(request(&detached,"recovery_resolve_revision",json!({"owner":b,"sequence":1,"localRevision":2,"identity":binding,"action":"discard","nextAccepted":null})).is_err());
        assert!(close_window(&state, "note-detached").is_err());
        window_gone(&state, "note-detached");
        assert!(!state
            .vault_db
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .owner_pins(&b)
            .unwrap()
            .is_empty());
        assert_eq!(
            state.recovery.lock().unwrap().records().unwrap()[0].body,
            "latest detached typing"
        );
        assert!(state
            .recovery
            .lock()
            .unwrap()
            .assert_quiet(None, None)
            .is_err());
    }
}

impl Recovery {
    /// Metadata-only diagnostics for the human Stage 4F boot gate. No event or
    /// diagnostic read acknowledges, resolves, or advances an owner baseline.
    pub(crate) fn owner_diagnostics(&self, vault: &str) -> Value {
        let mut rows = self
            .owners
            .iter()
            .filter(|(_, owner)| owner.vault.path == vault)
            .map(|(id, owner)| {
                json!({
                    "owner":id,"window":owner.window,"noteId":owner.note,"dirty":owner.dirty,
                    "conflicted":owner.conflicted,"locked":owner.locked,"sequence":owner.sequence,
                    "acceptedBase":self.bindings.get(id)
                })
            })
            .collect::<Vec<_>>();
        rows.sort_by_key(|row| row["owner"].as_str().unwrap_or("").to_owned());
        json!(rows)
    }
}
