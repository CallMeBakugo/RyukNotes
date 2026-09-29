use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::Builder;
use uuid::Uuid;
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;
use zip::ZipArchive;

use crate::types::BackupEntry;

// Only pathological entry counts and paths are rejected; byte capacity is disk-based.
const MAX_ENTRIES: usize = 1_000_000;
const MAX_PATH_BYTES: usize = 4096;
const MAX_NAME_BYTES: usize = 256 * 1024 * 1024;
const MAX_PATH_COMPONENTS: usize = 64;
const MANIFEST: &str = ".helixnotes-backup.json";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BackupManifest {
    version: u32,
    vault: String,
    attachments: bool,
}

fn vault_id(vault: &Path) -> Result<String, String> {
    let canonical = fs::canonicalize(vault).map_err(|e| e.to_string())?;
    Ok(format!(
        "{:x}",
        Sha256::digest(canonical.as_os_str().as_encoded_bytes())
    ))
}

fn options() -> SimpleFileOptions {
    SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(6))
}

fn is_published_backup(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        let name = name.to_string_lossy();
        name.starts_with("helixnotes-backup-") && name.ends_with(".zip")
    })
}

fn excluded_path(path: &Path, vault: &Path, backup_dir: &Path, attachments: bool) -> bool {
    if path == vault {
        return false;
    }
    if path.starts_with(backup_dir) {
        return true;
    }
    // Old configured backup folders may remain inside the vault after relocation.
    // Never recursively archive previously published backups from those folders.
    if path.is_file() && is_published_backup(path) {
        return true;
    }
    let relative = path.strip_prefix(vault).expect("walker stays in vault");
    let mut parts = relative.components();
    let first = parts.next().expect("non-root entry").as_os_str();
    if first == ".helixnotes" {
        return match parts.next() {
            None => !attachments,
            Some(part) if part.as_os_str() == "attachments" && attachments => {
                parts.any(|part| part.as_os_str().to_string_lossy().starts_with('.'))
            }
            _ => true,
        };
    }
    relative.components().any(|part| match part {
        Component::Normal(name) => name.to_string_lossy().starts_with('.'),
        _ => false,
    })
}

/// Returns the default backup directory (~/.config/helixnotes/backups/)
pub fn default_backup_dir() -> Result<PathBuf, String> {
    let config_dir = dirs::config_dir().ok_or("Cannot find config directory")?;
    let dir = config_dir.join("helixnotes").join("backups");
    if !dir.exists() {
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    }
    Ok(dir)
}

/// Returns the effective backup directory (custom or default)
pub fn get_backup_dir(custom: &Option<String>) -> Result<PathBuf, String> {
    match custom {
        Some(p) if !p.is_empty() => {
            let dir = PathBuf::from(p);
            if !dir.exists() {
                fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            }
            Ok(dir)
        }
        _ => default_backup_dir(),
    }
}

/// Create a complete ZIP in an exclusive temporary file before publishing it.
pub fn create_backup(
    vault_path: &str,
    backup_dir: &Path,
    include_attachments: bool,
) -> Result<BackupEntry, String> {
    let vault = fs::canonicalize(vault_path).map_err(|e| e.to_string())?;
    if !vault.is_dir() {
        return Err("Vault directory does not exist".to_string());
    }
    fs::create_dir_all(backup_dir).map_err(|e| e.to_string())?;
    let output = fs::canonicalize(backup_dir).map_err(|e| e.to_string())?;
    if output == vault {
        return Err("Backup directory must not be the vault root".to_string());
    }
    let id = vault_id(&vault)?;
    let now = Utc::now();
    let timestamp = now.format("%Y-%m-%dT%H-%M-%S").to_string();
    let filename = format!("helixnotes-backup-{timestamp}-{id}-{}.zip", Uuid::new_v4());
    let backup_path = output.join(&filename);
    let temporary = Builder::new()
        .prefix(".helixnotes-backup-")
        .suffix(".tmp")
        .tempfile_in(&output)
        .map_err(|e| format!("Failed to create temporary backup: {e}"))?;
    let file = temporary.reopen().map_err(|e| e.to_string())?;
    let mut zip = zip::ZipWriter::new(file);
    let result = (|| -> Result<(), String> {
        zip.start_file(MANIFEST, options())
            .map_err(|e| e.to_string())?;
        serde_json::to_writer(
            &mut zip,
            &BackupManifest {
                version: 1,
                vault: id,
                attachments: include_attachments,
            },
        )
        .map_err(|e| e.to_string())?;
        let mut walker = WalkDir::new(&vault).into_iter();
        while let Some(item) = walker.next() {
            let entry = item.map_err(|e| format!("Unreadable vault entry: {e}"))?;
            let path = entry.path();
            if excluded_path(path, &vault, &output, include_attachments) {
                if entry.file_type().is_dir() {
                    walker.skip_current_dir();
                }
                continue;
            }
            if path == vault {
                continue;
            }
            if entry.file_type().is_symlink() {
                return Err(format!("Cannot back up symlink: {}", path.display()));
            }
            let relative = path.strip_prefix(&vault).map_err(|e| e.to_string())?;
            let name = relative
                .to_str()
                .ok_or("Non-UTF-8 vault path")?
                .replace('\\', "/");
            checked_entry_name(&name)?;
            if name == MANIFEST {
                return Err("Vault path conflicts with backup metadata".to_string());
            }
            if entry.file_type().is_dir() {
                zip.add_directory(format!("{name}/"), options())
                    .map_err(|e| e.to_string())?;
            } else if entry.file_type().is_file() {
                let size = entry.metadata().map_err(|e| e.to_string())?.len();
                zip.start_file(name, options().large_file(size >= u32::MAX as u64))
                    .map_err(|e| e.to_string())?;
                let mut source = File::open(path).map_err(|e| e.to_string())?;
                let copied = io::copy(&mut (&mut source).take(size.saturating_add(1)), &mut zip)
                    .map_err(|e| e.to_string())?;
                if copied != size {
                    return Err(format!(
                        "Vault file changed while backing up: {}",
                        path.display()
                    ));
                }
            } else {
                return Err(format!("Unsupported vault entry: {}", path.display()));
            }
        }
        let finished = zip.finish().map_err(|e| e.to_string())?;
        finished.sync_all().map_err(|e| e.to_string())?;
        Ok(())
    })();
    result?;
    // persist_noclobber does not replace an existing backup even on UUID collision.
    temporary
        .persist_noclobber(&backup_path)
        .map_err(|e| format!("Failed to publish backup: {}", e.error))?;
    let meta = fs::metadata(&backup_path).map_err(|e| e.to_string())?;
    Ok(BackupEntry {
        filename,
        path: backup_path.to_string_lossy().to_string(),
        size: meta.len(),
        created: now.to_rfc3339(),
    })
}

fn archive_name(filename: &str, id: &str) -> Option<(String, bool)> {
    let stem = filename
        .strip_prefix("helixnotes-backup-")?
        .strip_suffix(".zip")?;
    let timestamp = stem.get(..19)?;
    let suffix = stem.get(19..)?;
    let parsed = chrono::NaiveDateTime::parse_from_str(timestamp, "%Y-%m-%dT%H-%M-%S").ok()?;
    if suffix.is_empty() {
        return Some((parsed.and_utc().to_rfc3339(), false)); // pre-manifest legacy
    }
    let suffix = suffix.strip_prefix('-')?;
    let (vault_hash, uuid) = suffix.split_once('-')?;
    if vault_hash.len() != 64 || !vault_hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Uuid::parse_str(uuid).ok()?;
    Some((parsed.and_utc().to_rfc3339(), vault_hash == id))
}

fn list_owned_backups(
    backup_dir: &Path,
    vault_path: &str,
) -> Result<Vec<(BackupEntry, bool)>, String> {
    if !backup_dir.exists() {
        return Ok(Vec::new());
    }
    let id = vault_id(Path::new(vault_path))?;
    let mut entries = Vec::new();
    for entry in fs::read_dir(backup_dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if !meta.file_type().is_file() {
            continue;
        }
        let filename = entry.file_name().to_string_lossy().to_string();
        if let Some((created, owned)) = archive_name(&filename, &id) {
            entries.push((
                BackupEntry {
                    filename,
                    path: path.to_string_lossy().to_string(),
                    size: meta.len(),
                    created,
                },
                owned,
            ));
        }
    }
    entries.sort_by(|a, b| {
        b.0.created
            .cmp(&a.0.created)
            .then_with(|| b.0.filename.cmp(&a.0.filename))
    });
    Ok(entries)
}

/// List backups from every vault; only automatic pruning uses vault identity.
pub fn list_backups(backup_dir: &Path, vault_path: &str) -> Result<Vec<BackupEntry>, String> {
    Ok(list_owned_backups(backup_dir, vault_path)?
        .into_iter()
        .map(|(entry, _)| entry)
        .collect())
}

fn validated_backup_file(backup_dir: &Path, backup_path: &str) -> Result<PathBuf, String> {
    let backup_dir = fs::canonicalize(backup_dir).map_err(|error| error.to_string())?;
    let backup = fs::canonicalize(backup_path).map_err(|error| error.to_string())?;
    if backup.parent() != Some(backup_dir.as_path())
        || backup.extension().and_then(|extension| extension.to_str()) != Some("zip")
        || !backup.is_file()
    {
        return Err(
            "Backup path must point to a ZIP file in the configured backup directory".to_string(),
        );
    }
    Ok(backup)
}

fn checked_entry_name(name: &str) -> Result<PathBuf, String> {
    let trimmed = name.strip_suffix('/').unwrap_or(name);
    if trimmed.is_empty()
        || trimmed.len() > MAX_PATH_BYTES
        || name.contains('\\')
        || name.contains('\0')
        || name.contains(':')
    {
        return Err(format!("Invalid backup entry path: {name:?}"));
    }
    let parts: Vec<_> = trimmed.split('/').collect();
    if parts.len() > MAX_PATH_COMPONENTS
        || parts
            .iter()
            .any(|part| part.is_empty() || *part == "." || *part == "..")
    {
        return Err(format!("Unsafe backup entry path: {name:?}"));
    }
    let path = Path::new(trimmed);
    if path
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(format!("Unsafe backup entry path: {name:?}"));
    }
    Ok(path.to_path_buf())
}

fn remove_entry(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.file_type().is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

fn historical_backups(
    vault: &Path,
    backup_dir: &Path,
    attachments: bool,
) -> Result<Vec<PathBuf>, String> {
    let mut paths = Vec::new();
    let mut walker = WalkDir::new(vault).into_iter();
    while let Some(item) = walker.next() {
        let item = item.map_err(|e| format!("Unreadable vault entry: {e}"))?;
        let path = item.path();
        if path == vault {
            continue;
        }
        if path.starts_with(backup_dir) {
            if item.file_type().is_dir() {
                walker.skip_current_dir();
            }
            continue;
        }
        let relative = path.strip_prefix(vault).map_err(|e| e.to_string())?;
        let hidden = relative
            .components()
            .any(|component| component.as_os_str().to_string_lossy().starts_with('.'));
        if hidden && !(attachments && relative.starts_with(".helixnotes/attachments")) {
            if item.file_type().is_dir() {
                // Continue into .helixnotes only to find historical attachment backups.
                if attachments && relative == Path::new(".helixnotes") {
                    continue;
                }
                walker.skip_current_dir();
            }
            continue;
        }
        if item.file_type().is_file() && is_published_backup(path) {
            paths.push(relative.to_path_buf());
        }
    }
    Ok(paths)
}

fn install_staged(
    vault: &Path,
    contents: &Path,
    previous: &Path,
    backup_dir: &Path,
    attachments: bool,
) -> Result<(), String> {
    let nested = backup_dir
        .strip_prefix(vault)
        .ok()
        .filter(|p| !p.as_os_str().is_empty());
    let held = previous
        .parent()
        .expect("staging parent")
        .join("held-backups");
    let previous_entries = previous.join("entries");
    let previous_attachments = previous.join("internal-attachments");
    let history = historical_backups(vault, backup_dir, attachments)?;
    let held_history = previous
        .parent()
        .expect("staging parent")
        .join("held-history");
    let mut stashed_history = Vec::new();
    let mut restored_history = Vec::new();
    let mut held_output = false;
    let mut displaced = Vec::new();
    let mut installed = Vec::new();
    let attachments_path = vault.join(".helixnotes/attachments");
    let mut displaced_attachments = false;
    let mut installed_attachments = false;
    let operation = (|| -> io::Result<()> {
        fs::create_dir(&previous_entries)?;
        fs::create_dir(&held_history)?;
        for relative in &history {
            let destination = held_history.join(relative);
            fs::create_dir_all(destination.parent().expect("history parent"))?;
            fs::rename(vault.join(relative), destination)?;
            stashed_history.push(relative.clone());
        }
        if let Some(relative) = nested {
            if backup_dir.exists() {
                fs::rename(backup_dir, &held)?;
                held_output = true;
            }
            // Restoring into a vault with nested backup output must not overwrite it.
            let target = contents.join(relative);
            if target.exists() {
                return Err(io::Error::other(
                    "Archive contains the backup output directory",
                ));
            }
        }
        for entry in fs::read_dir(vault)? {
            let entry = entry?;
            let name = entry.file_name();
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            fs::rename(entry.path(), previous_entries.join(&name))?;
            displaced.push(name);
        }
        if attachments {
            if fs::symlink_metadata(&attachments_path).is_ok() {
                fs::rename(&attachments_path, &previous_attachments)?;
                displaced_attachments = true;
            }
            fs::create_dir_all(vault.join(".helixnotes"))?;
        }
        for entry in fs::read_dir(contents)? {
            let entry = entry?;
            let name = entry.file_name();
            if name == ".helixnotes" {
                continue;
            }
            fs::rename(entry.path(), vault.join(&name))?;
            installed.push(name);
        }
        if attachments {
            fs::rename(contents.join(".helixnotes/attachments"), &attachments_path)?;
            installed_attachments = true;
        }
        if let Some(relative) = nested {
            if held_output {
                let destination = vault.join(relative);
                fs::create_dir_all(destination.parent().expect("nested backup parent"))?;
                fs::rename(&held, destination)?;
                held_output = false;
            }
        }
        for relative in &stashed_history {
            let destination = vault.join(relative);
            fs::create_dir_all(destination.parent().expect("history parent"))?;
            fs::rename(held_history.join(relative), &destination)?;
            restored_history.push(relative.clone());
        }
        Ok(())
    })();
    if let Err(error) = operation {
        let mut failures = Vec::new();
        for relative in &restored_history {
            if let Err(e) = fs::rename(vault.join(relative), held_history.join(relative)) {
                failures.push(format!(
                    "retain historical backup {}: {e}",
                    relative.display()
                ));
            }
        }
        // A later failure can happen after the configured output was reinstalled.
        // Re-hold it before removing any newly installed top-level directory.
        if nested.is_some() && !held_output {
            if let Err(e) = fs::rename(backup_dir, &held) {
                failures.push(format!("retain configured backup output: {e}"));
            } else {
                held_output = true;
            }
        }
        if !failures.is_empty() {
            return Err(format!(
                "Restore failed: {error}; rollback incomplete (originals retained in {}): {}",
                previous.parent().expect("staging parent").display(),
                failures.join("; ")
            ));
        }
        if installed_attachments {
            if let Err(e) = remove_entry(&attachments_path) {
                failures.push(format!("remove new attachments: {e}"));
            }
        }
        for name in installed {
            if let Err(e) = remove_entry(&vault.join(&name)) {
                failures.push(format!("remove new entry {:?}: {e}", name));
            }
        }
        for relative in &stashed_history {
            let mut parent = vault.join(relative).parent().map(Path::to_path_buf);
            while let Some(path) = parent {
                if path == vault {
                    break;
                }
                let _ = fs::remove_dir(&path); // remove only now-empty staging parents
                parent = path.parent().map(Path::to_path_buf);
            }
        }
        for name in displaced {
            if let Err(e) = fs::rename(previous_entries.join(&name), vault.join(&name)) {
                failures.push(format!("restore original {:?}: {e}", name));
            }
        }
        if displaced_attachments {
            if let Err(e) = fs::rename(&previous_attachments, &attachments_path) {
                failures.push(format!("restore attachments: {e}"));
            }
        }
        if held_output {
            if let Some(relative) = nested {
                let destination = vault.join(relative);
                if let Err(e) =
                    fs::create_dir_all(destination.parent().expect("nested backup parent"))
                        .and_then(|_| fs::rename(&held, destination))
                {
                    failures.push(format!("restore backup output: {e}"));
                }
            }
        }
        for relative in &stashed_history {
            let destination = vault.join(relative);
            if let Err(e) = fs::create_dir_all(destination.parent().expect("history parent"))
                .and_then(|_| fs::rename(held_history.join(relative), destination))
            {
                failures.push(format!(
                    "restore historical backup {}: {e}",
                    relative.display()
                ));
            }
        }
        if failures.is_empty() {
            return Err(format!(
                "Restore installation failed; previous vault recovered: {error}"
            ));
        }
        return Err(format!(
            "Restore failed: {error}; rollback incomplete (originals retained in {}): {}",
            previous.parent().expect("staging parent").display(),
            failures.join("; ")
        ));
    }
    Ok(())
}

/// Validate and extract everything alongside the vault; install only after success.
/// Limits: 1,000,000 entries, 4 KiB / 64 components per path, 256 MiB
/// combined entry names, and extracted bytes within available sibling-filesystem
/// space, reserving 5% of free space (up to 64 MiB).
pub fn restore_backup(
    vault_path: &str,
    backup_dir: &Path,
    backup_path: &str,
) -> Result<(), String> {
    let vault = fs::canonicalize(vault_path).map_err(|e| e.to_string())?;
    if !vault.is_dir() {
        return Err("Vault directory does not exist".to_string());
    }
    let output = fs::canonicalize(backup_dir).map_err(|e| e.to_string())?;
    let backup = validated_backup_file(&output, backup_path)?;
    let stage = Builder::new()
        .prefix(".helixnotes-restore-")
        .tempdir_in(vault.parent().ok_or("Vault has no parent")?)
        .map_err(|e| e.to_string())?;
    let contents = stage.path().join("contents");
    let previous = stage.path().join("previous");
    fs::create_dir(&contents).map_err(|e| e.to_string())?;
    fs::create_dir(&previous).map_err(|e| e.to_string())?;
    let free = fs4::available_space(stage.path()).map_err(|e| e.to_string())?;
    let reserve = (free / 20).min(64 * 1024 * 1024); // 5%, capped at 64 MiB
    let extraction_budget = free - reserve;

    let mut archive = ZipArchive::new(File::open(&backup).map_err(|e| e.to_string())?)
        .map_err(|e| format!("Invalid backup archive: {e}"))?;
    if archive.len() > MAX_ENTRIES {
        return Err("Backup exceeds the 1,000,000 entry limit".to_string());
    }
    let mut declared = 0u64;
    let mut name_bytes = 0usize;
    for index in 0..archive.len() {
        let item = archive.by_index(index).map_err(|e| e.to_string())?;
        name_bytes = name_bytes
            .checked_add(item.name().len())
            .ok_or("Backup entry names overflow")?;
        if name_bytes > MAX_NAME_BYTES {
            return Err("Backup entry names exceed the 256 MiB limit".to_string());
        }
        declared = declared
            .checked_add(item.size())
            .ok_or("Backup size overflow")?;
        if declared > extraction_budget {
            return Err("Insufficient free space to stage the complete backup".to_string());
        }
    }
    let mut seen = HashMap::new();
    let mut total = 0u64;
    let mut manifest = None;
    let mut legacy_attachments = false;
    for index in 0..archive.len() {
        let mut item = archive.by_index(index).map_err(|e| e.to_string())?;
        let name = item.name().to_string();
        let relative = checked_entry_name(&name)?;
        let directory = item.is_dir();
        if !directory && is_published_backup(&relative) {
            return Err(format!("Archive contains historical backup output: {name}"));
        }
        let mode = item.unix_mode().unwrap_or(0) & 0o170000;
        if (mode != 0 && mode != if directory { 0o040000 } else { 0o100000 })
            || (directory && item.size() != 0)
        {
            return Err(format!("Unsupported archive entry: {name}"));
        }
        if seen.insert(relative.clone(), !directory).is_some() {
            return Err(format!("Duplicate archive entry: {name}"));
        }
        let mut parts = relative.components();
        let first = parts.next().expect("checked path");
        let internal = first.as_os_str() == ".helixnotes";
        if internal {
            match parts.next() {
                None if directory => {}
                Some(component) if component.as_os_str() == "attachments" => {
                    legacy_attachments = true;
                }
                _ => return Err(format!("Archive contains unsupported app metadata: {name}")),
            }
        } else if first.as_os_str().to_string_lossy().starts_with('.')
            && relative != Path::new(MANIFEST)
        {
            return Err(format!("Archive contains hidden metadata: {name}"));
        }
        if relative != Path::new(MANIFEST)
            && relative
                .components()
                .skip(if internal { 2 } else { 1 })
                .any(|part| part.as_os_str().to_string_lossy().starts_with('.'))
        {
            return Err(format!("Archive contains hidden metadata: {name}"));
        }
        if let Ok(output_relative) = output.strip_prefix(&vault) {
            if !output_relative.as_os_str().is_empty()
                && (relative.starts_with(output_relative)
                    || (!directory && output_relative.starts_with(&relative)))
            {
                return Err("Archive contains the configured backup output path".to_string());
            }
        }
        total = total
            .checked_add(item.size())
            .ok_or("Backup size overflow")?;
        if total > extraction_budget {
            return Err("Backup exceeds available staging space".to_string());
        }
        if relative == Path::new(MANIFEST) {
            if directory || item.size() > 4096 {
                return Err("Invalid backup manifest".to_string());
            }
            let declared_size = item.size();
            let mut bounded = (&mut item).take(declared_size.saturating_add(1));
            let parsed: BackupManifest = serde_json::from_reader(&mut bounded)
                .map_err(|e| format!("Invalid backup manifest: {e}"))?;
            if bounded.limit() != 1 {
                return Err("Backup manifest length mismatch".to_string());
            }
            manifest = Some(parsed);
            continue;
        }
        let dest = contents.join(&relative);
        if directory {
            fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
        } else {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut target = File::create(&dest).map_err(|e| e.to_string())?;
            let declared_size = item.size();
            let copied = io::copy(
                &mut (&mut item).take(declared_size.saturating_add(1)),
                &mut target,
            )
            .map_err(|e| format!("Backup extraction failed: {e}"))?;
            if copied != item.size() {
                return Err(format!("Backup entry length mismatch: {name}"));
            }
        }
    }
    for (file, is_file) in &seen {
        if *is_file
            && file
                .ancestors()
                .skip(1)
                .any(|parent| seen.get(parent) == Some(&true))
        {
            return Err(format!(
                "Archive entry conflicts with a file: {}",
                file.display()
            ));
        }
    }
    let attachments = if let Some(meta) = manifest {
        if meta.version != 1
            || meta.vault.len() != 64
            || !meta.vault.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("Unsupported backup manifest version or vault identity".to_string());
        }
        if !meta.attachments && legacy_attachments {
            return Err("Backup has attachments despite metadata declaring none".to_string());
        }
        meta.attachments
    } else {
        legacy_attachments
    };
    if attachments {
        fs::create_dir_all(contents.join(".helixnotes/attachments")).map_err(|e| e.to_string())?;
        if let Ok(relative) = output.strip_prefix(&vault) {
            if relative.starts_with(".helixnotes/attachments") {
                return Err("Cannot replace attachments containing the backup output".to_string());
            }
        }
        let internals = vault.join(".helixnotes");
        if fs::symlink_metadata(&internals).is_ok_and(|meta| !meta.file_type().is_dir()) {
            return Err("Vault app metadata directory is not a real directory".to_string());
        }
    }
    drop(archive);
    let result = install_staged(&vault, &contents, &previous, &output, attachments);
    if result
        .as_ref()
        .is_err_and(|message| message.contains("rollback incomplete"))
    {
        // Keep originals available for manual recovery if an extraordinary rollback fails.
        let _ = stage.keep();
    }
    result
}

/// Delete a single backup file
pub fn delete_backup(backup_dir: &Path, backup_path: &str) -> Result<(), String> {
    let path = validated_backup_file(backup_dir, backup_path)?;
    fs::remove_file(path).map_err(|error| error.to_string())
}

/// Prune only identifiable backups for this vault; ambiguous legacy archives remain.
pub fn cleanup_old_backups(
    backup_dir: &Path,
    vault_path: &str,
    max_count: u32,
) -> Result<(), String> {
    let mut owned: Vec<_> = list_owned_backups(backup_dir, vault_path)?
        .into_iter()
        .filter_map(|(entry, owned)| owned.then_some(entry))
        .collect();
    if owned.len() > max_count as usize {
        for entry in owned.drain(max_count as usize..) {
            delete_backup(backup_dir, &entry.path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{cleanup_old_backups, create_backup, delete_backup, list_backups, restore_backup};
    use std::fs::{self, File};
    use std::io::Write;
    use std::path::Path;
    use tempfile::tempdir;
    use uuid::Uuid;
    use zip::write::SimpleFileOptions;
    use zip::{ZipArchive, ZipWriter};

    #[test]
    fn delete_backup_rejects_files_outside_backup_directory() {
        let root = std::env::temp_dir().join(format!("helixnotes-backup-test-{}", Uuid::new_v4()));
        let backup_dir = root.join("backups");
        let outside = root.join("outside.zip");
        fs::create_dir_all(&backup_dir).unwrap();
        fs::write(&outside, "must survive").unwrap();

        let result = delete_backup(&backup_dir, &outside.to_string_lossy());

        assert!(result.is_err());
        assert!(outside.exists());
        fs::remove_dir_all(root).unwrap();
    }

    fn vault_string(path: &Path) -> String {
        path.to_str().unwrap().to_string()
    }

    #[test]
    fn backups_are_vault_scoped_and_not_published_until_complete() {
        let root = tempdir().unwrap();
        let output = root.path().join("backups");
        let first = root.path().join("first");
        let second = root.path().join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        fs::write(first.join("note.md"), "one").unwrap();
        fs::write(second.join("note.md"), "two").unwrap();
        let a = create_backup(&vault_string(&first), &output, false).unwrap();
        let b = create_backup(&vault_string(&second), &output, false).unwrap();
        assert_ne!(a.filename, b.filename);
        assert_eq!(
            list_backups(&output, &vault_string(&first)).unwrap().len(),
            2
        );
        assert_eq!(
            list_backups(&output, &vault_string(&second)).unwrap().len(),
            2
        );
        cleanup_old_backups(&output, &vault_string(&first), 0).unwrap();
        assert!(!Path::new(&a.path).exists());
        assert!(Path::new(&b.path).exists());

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("missing", first.join("broken.md")).unwrap();
            assert!(create_backup(&vault_string(&first), &output, false).is_err());
            assert_eq!(
                list_backups(&output, &vault_string(&first)).unwrap().len(),
                1
            );
            assert_eq!(fs::read_dir(&output).unwrap().count(), 1);
        }
    }

    #[test]
    fn nested_output_and_hidden_subtrees_are_not_archived() {
        let root = tempdir().unwrap();
        let vault = root.path().join("vault");
        let output = vault.join("archives");
        fs::create_dir_all(&output).unwrap();
        fs::create_dir_all(vault.join(".private/notes")).unwrap();
        fs::create_dir_all(vault.join("former-archives")).unwrap();
        fs::write(
            vault.join("former-archives/helixnotes-backup-2026-01-02T03-04-05.zip"),
            "former",
        )
        .unwrap();
        fs::write(vault.join("note.md"), "visible").unwrap();
        fs::write(vault.join(".private/notes/secret.md"), "secret").unwrap();
        fs::write(output.join("old.zip"), "old").unwrap();
        let backup = create_backup(&vault_string(&vault), &output, false).unwrap();
        let mut zip = ZipArchive::new(File::open(&backup.path).unwrap()).unwrap();
        assert!(zip.by_name("note.md").is_ok());
        assert!(zip.by_name("archives/old.zip").is_err());
        assert!(zip
            .by_name("former-archives/helixnotes-backup-2026-01-02T03-04-05.zip")
            .is_err());
        assert!(zip.by_name(".private/notes/secret.md").is_err());
        drop(zip);
        fs::write(vault.join("note.md"), "changed").unwrap();
        restore_backup(&vault_string(&vault), &output, &backup.path).unwrap();
        assert_eq!(
            fs::read_to_string(vault.join("note.md")).unwrap(),
            "visible"
        );
        assert_eq!(fs::read_to_string(output.join("old.zip")).unwrap(), "old");
        assert_eq!(
            fs::read_to_string(
                vault.join("former-archives/helixnotes-backup-2026-01-02T03-04-05.zip")
            )
            .unwrap(),
            "former"
        );
        assert!(Path::new(&backup.path).exists());
    }

    #[test]
    fn omitted_attachments_survive_restore_and_included_attachments_replace_them() {
        let root = tempdir().unwrap();
        let vault = root.path().join("vault");
        let output = root.path().join("backups");
        let attachments = vault.join(".helixnotes/attachments");
        fs::create_dir_all(&attachments).unwrap();
        fs::write(vault.join("note.md"), "old").unwrap();
        fs::write(attachments.join("picture.png"), "original").unwrap();
        fs::create_dir_all(vault.join("attachments")).unwrap();
        fs::write(vault.join("attachments/notebook.md"), "top-level original").unwrap();
        fs::write(vault.join(".helixnotes/settings.json"), "keep").unwrap();

        let omitted = create_backup(&vault_string(&vault), &output, false).unwrap();
        let mut omitted_zip = ZipArchive::new(File::open(&omitted.path).unwrap()).unwrap();
        let omitted_manifest: super::BackupManifest =
            serde_json::from_reader(omitted_zip.by_name(super::MANIFEST).unwrap()).unwrap();
        assert!(!omitted_manifest.attachments);
        drop(omitted_zip);
        fs::write(vault.join("note.md"), "changed").unwrap();
        fs::write(attachments.join("picture.png"), "live").unwrap();
        restore_backup(&vault_string(&vault), &output, &omitted.path).unwrap();
        assert_eq!(fs::read_to_string(vault.join("note.md")).unwrap(), "old");
        assert_eq!(
            fs::read_to_string(attachments.join("picture.png")).unwrap(),
            "live"
        );
        fs::write(vault.join("attachments/notebook.md"), "top-level snapshot").unwrap();

        let included = create_backup(&vault_string(&vault), &output, true).unwrap();
        let mut included_zip = ZipArchive::new(File::open(&included.path).unwrap()).unwrap();
        let included_manifest: super::BackupManifest =
            serde_json::from_reader(included_zip.by_name(super::MANIFEST).unwrap()).unwrap();
        assert!(included_manifest.attachments);
        drop(included_zip);
        fs::write(vault.join("note.md"), "newer").unwrap();
        fs::write(attachments.join("picture.png"), "latest").unwrap();
        fs::write(vault.join("attachments/notebook.md"), "top-level changed").unwrap();
        fs::write(attachments.join("extra.png"), "extra").unwrap();
        restore_backup(&vault_string(&vault), &output, &included.path).unwrap();
        assert_eq!(fs::read_to_string(vault.join("note.md")).unwrap(), "old");
        assert_eq!(
            fs::read_to_string(attachments.join("picture.png")).unwrap(),
            "live"
        );
        assert_eq!(
            fs::read_to_string(vault.join("attachments/notebook.md")).unwrap(),
            "top-level snapshot"
        );
        assert!(!attachments.join("extra.png").exists());
        assert_eq!(
            fs::read_to_string(vault.join(".helixnotes/settings.json")).unwrap(),
            "keep"
        );
    }

    #[test]
    fn checksum_failure_preserves_notes_and_attachments() {
        let root = tempdir().unwrap();
        let vault = root.path().join("vault");
        let output = root.path().join("backups");
        fs::create_dir_all(vault.join(".helixnotes/attachments")).unwrap();
        fs::create_dir_all(&output).unwrap();
        fs::write(vault.join("note.md"), "live-note").unwrap();
        fs::write(vault.join(".helixnotes/attachments/photo"), "live-photo").unwrap();
        let archive_path = output.join("legacy.zip");
        let mut zip = ZipWriter::new(File::create(&archive_path).unwrap());
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("note.md", options).unwrap();
        zip.write_all(b"archived-note").unwrap();
        zip.start_file(".helixnotes/attachments/photo", options)
            .unwrap();
        zip.write_all(b"archived-photo").unwrap();
        zip.finish().unwrap();
        let mut bytes = fs::read(&archive_path).unwrap();
        let start = bytes
            .windows(13)
            .position(|window| window == b"archived-note")
            .unwrap();
        bytes[start] = b'X';
        fs::write(&archive_path, bytes).unwrap();
        assert!(
            restore_backup(&vault_string(&vault), &output, &vault_string(&archive_path)).is_err()
        );
        assert_eq!(
            fs::read_to_string(vault.join("note.md")).unwrap(),
            "live-note"
        );
        assert_eq!(
            fs::read_to_string(vault.join(".helixnotes/attachments/photo")).unwrap(),
            "live-photo"
        );
    }

    #[test]
    fn understated_manifest_size_cannot_bypass_extraction_bound() {
        let root = tempdir().unwrap();
        let vault = root.path().join("vault");
        let output = root.path().join("backups");
        fs::create_dir_all(&vault).unwrap();
        fs::create_dir_all(&output).unwrap();
        fs::write(vault.join("note.md"), "live-note").unwrap();
        let archive_path = output.join("malformed.zip");
        let mut zip = ZipWriter::new(File::create(&archive_path).unwrap());
        zip.start_file(
            super::MANIFEST,
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        zip.write_all(br#"{"version":1,"vault":"source","attachments":false}"#)
            .unwrap();
        zip.write_all(&[b' '; 64 * 1024]).unwrap();
        zip.finish().unwrap();
        let mut bytes = fs::read(&archive_path).unwrap();
        let central = bytes
            .windows(4)
            .position(|part| part == b"PK\x01\x02")
            .unwrap();
        bytes[22..26].copy_from_slice(&96u32.to_le_bytes());
        bytes[central + 24..central + 28].copy_from_slice(&96u32.to_le_bytes());
        fs::write(&archive_path, bytes).unwrap();

        assert!(
            restore_backup(&vault_string(&vault), &output, &vault_string(&archive_path)).is_err()
        );
        assert_eq!(
            fs::read_to_string(vault.join("note.md")).unwrap(),
            "live-note"
        );
    }

    #[test]
    fn legacy_archives_without_attachments_keep_existing_attachments() {
        let root = tempdir().unwrap();
        let vault = root.path().join("vault");
        let output = root.path().join("backups");
        fs::create_dir_all(vault.join(".helixnotes/attachments")).unwrap();
        fs::create_dir_all(&output).unwrap();
        fs::write(vault.join("note.md"), "live").unwrap();
        fs::write(vault.join(".helixnotes/attachments/photo"), "untouched").unwrap();
        let archive_path = output.join("legacy.zip");
        let mut zip = ZipWriter::new(File::create(&archive_path).unwrap());
        zip.start_file("note.md", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"restored").unwrap();
        zip.finish().unwrap();
        restore_backup(&vault_string(&vault), &output, &vault_string(&archive_path)).unwrap();
        assert_eq!(
            fs::read_to_string(vault.join("note.md")).unwrap(),
            "restored"
        );
        assert_eq!(
            fs::read_to_string(vault.join(".helixnotes/attachments/photo")).unwrap(),
            "untouched"
        );
    }

    #[test]
    fn unsafe_archives_do_not_modify_the_vault() {
        let root = tempdir().unwrap();
        let vault = root.path().join("vault");
        let output = root.path().join("backups");
        fs::create_dir_all(vault.join(".helixnotes")).unwrap();
        fs::create_dir_all(&output).unwrap();
        fs::write(vault.join("note.md"), "original").unwrap();
        fs::write(vault.join(".helixnotes/settings.json"), "settings").unwrap();
        for bad_name in [
            "../escape.md",
            ".helixnotes/settings.json",
            "dir/../../escape.md",
        ] {
            let archive_path = output.join("unsafe.zip");
            let mut zip = ZipWriter::new(File::create(&archive_path).unwrap());
            zip.start_file("note.md", SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"replacement").unwrap();
            zip.start_file(bad_name, SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"unsafe").unwrap();
            zip.finish().unwrap();
            assert!(
                restore_backup(&vault_string(&vault), &output, &vault_string(&archive_path))
                    .is_err()
            );
            assert_eq!(
                fs::read_to_string(vault.join("note.md")).unwrap(),
                "original"
            );
            assert_eq!(
                fs::read_to_string(vault.join(".helixnotes/settings.json")).unwrap(),
                "settings"
            );
            assert!(!root.path().join("escape.md").exists());
        }
    }

    #[test]
    fn explicit_restore_into_a_different_vault_accepts_selected_backup() {
        let root = tempdir().unwrap();
        let source = root.path().join("source");
        let recovered = root.path().join("recovered");
        let output = root.path().join("backups");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&recovered).unwrap();
        fs::write(source.join("note.md"), "source content").unwrap();
        fs::write(recovered.join("note.md"), "stale content").unwrap();
        let entry = create_backup(&vault_string(&source), &output, false).unwrap();
        assert!(list_backups(&output, &vault_string(&recovered))
            .unwrap()
            .iter()
            .any(|candidate| candidate.path == entry.path));
        cleanup_old_backups(&output, &vault_string(&recovered), 0).unwrap();
        assert!(Path::new(&entry.path).exists());
        restore_backup(&vault_string(&recovered), &output, &entry.path).unwrap();
        assert_eq!(
            fs::read_to_string(recovered.join("note.md")).unwrap(),
            "source content"
        );
    }
}
