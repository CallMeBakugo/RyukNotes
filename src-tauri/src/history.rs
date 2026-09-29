use std::fs;
use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::types::VersionEntry;

fn safe_path_component<'a>(value: &'a str, label: &str) -> Result<&'a str, String> {
    let mut components = Path::new(value).components();
    if value.is_empty()
        || !matches!(components.next(), Some(Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(format!("Invalid {label}"));
    }
    Ok(value)
}

/// Directory: .helixnotes/history/<note-id>/
fn history_dir(vault_path: &str, note_id: &str) -> Result<PathBuf, String> {
    Ok(Path::new(vault_path)
        .join(".helixnotes")
        .join("history")
        .join(safe_path_component(note_id, "note ID")?))
}

fn snapshot_path(dir: &Path, raw_content: &str) -> Result<(), String> {
    use std::io::Write;
    let mut temporary = tempfile::Builder::new()
        .prefix(".helixnotes-history-")
        .tempfile_in(dir)
        .map_err(|error| error.to_string())?;
    temporary
        .write_all(raw_content.as_bytes())
        .map_err(|error| error.to_string())?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    let mut instant = Utc::now();
    loop {
        // Fractional seconds keep timestamps valid RFC3339 for the UI while
        // allowing multiple versions within one second.
        let timestamp = instant.format("%Y-%m-%dT%H-%M-%S%.3f");
        let path = dir.join(format!("{timestamp}.md"));
        match temporary.persist_noclobber(&path) {
            Ok(_) => return Ok(()),
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                temporary = error.file;
                instant += chrono::Duration::milliseconds(1);
            }
            Err(error) => return Err(error.error.to_string()),
        }
    }
}

fn version_timestamp(filename: &str) -> String {
    match (filename.get(..13), filename.get(14..16), filename.get(17..)) {
        (Some(hour), Some(minute), Some(rest))
            if filename.as_bytes().get(10) == Some(&b'T')
                && filename.as_bytes().get(13) == Some(&b'-')
                && filename.as_bytes().get(16) == Some(&b'-') =>
        {
            format!("{hour}:{minute}:{rest}Z")
        }
        _ => filename.to_string(),
    }
}

fn snapshot_filename(timestamp: &str) -> String {
    let time = timestamp.strip_suffix('Z').unwrap_or(timestamp);
    match (time.get(..13), time.get(14..16), time.get(17..)) {
        (Some(hour), Some(minute), Some(rest))
            if time.as_bytes().get(10) == Some(&b'T')
                && time.as_bytes().get(13) == Some(&b':')
                && time.as_bytes().get(16) == Some(&b':') =>
        {
            format!("{hour}-{minute}-{rest}.md")
        }
        _ => format!("{timestamp}.md"),
    }
}

/// Save a version snapshot if enough time has passed since the last one.
/// Minimum interval: 5 minutes between snapshots.
pub fn maybe_snapshot(vault_path: &str, note_id: &str, raw_content: &str, max_versions: u32) {
    let Ok(dir) = history_dir(vault_path, note_id) else {
        log::warn!("Skipping history snapshot with an invalid note ID");
        return;
    };

    // Old second-resolution and new fractional-second names share the first
    // 19 ASCII bytes. Ignore arbitrary files (including malformed Unicode).
    if let Ok(entries) = fs::read_dir(&dir) {
        let latest = entries
            .filter_map(|entry| {
                let name = entry.ok()?.file_name();
                let name = name.to_str()?;
                if !name.ends_with(".md") {
                    return None;
                }
                let (Some(hour), Some(minute), Some(second)) =
                    (name.get(..13), name.get(14..16), name.get(17..19))
                else {
                    return None;
                };
                if name.as_bytes().get(10) != Some(&b'T')
                    || name.as_bytes().get(13) != Some(&b'-')
                    || name.as_bytes().get(16) != Some(&b'-')
                {
                    return None;
                }
                format!("{hour}:{minute}:{second}Z")
                    .parse::<DateTime<Utc>>()
                    .ok()
            })
            .max();
        if latest.is_some_and(|last| (Utc::now() - last).num_minutes() < 5) {
            return;
        }
    }

    // Create snapshot
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("Failed to create history dir: {}", e);
        return;
    }

    if let Err(error) = snapshot_path(&dir, raw_content) {
        eprintln!("Failed to write version snapshot: {error}");
        return;
    }

    // Prune old versions
    if max_versions > 0 {
        let _ = prune_versions(&dir, max_versions);
    }
}

/// Force-create a version snapshot, bypassing the cooldown.
pub fn force_snapshot(
    vault_path: &str,
    note_id: &str,
    raw_content: &str,
    max_versions: u32,
) -> Result<(), String> {
    let dir = history_dir(vault_path, note_id)?;
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;

    snapshot_path(&dir, raw_content)?;

    if max_versions > 0 {
        prune_versions(&dir, max_versions)?;
    }
    Ok(())
}

/// List all version snapshots for a note, newest first.
pub fn list_versions(vault_path: &str, note_id: &str) -> Result<Vec<VersionEntry>, String> {
    let dir = history_dir(vault_path, note_id)?;
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut entries: Vec<VersionEntry> = Vec::new();

    for entry in fs::read_dir(&dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "md") {
            let filename = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let meta = fs::metadata(&path).map_err(|e| e.to_string())?;

            let timestamp = version_timestamp(&filename);
            entries.push(VersionEntry {
                timestamp,
                size: meta.len(),
            });
        }
    }

    // Newest first
    entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    Ok(entries)
}

/// Get the raw content of a specific version.
pub fn get_version(vault_path: &str, note_id: &str, timestamp: &str) -> Result<String, String> {
    safe_path_component(timestamp, "version timestamp")?;
    // Accept both legacy second-resolution names and unique suffixes.
    let filename = snapshot_filename(timestamp);

    let path = history_dir(vault_path, note_id)?.join(&filename);
    fs::read_to_string(&path).map_err(|e| format!("Version not found: {}", e))
}

/// Remove old versions keeping only the newest `max` entries.
fn prune_versions(dir: &Path, max: u32) -> Result<(), String> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "md"))
        .collect();

    // Sort by name (timestamps sort lexicographically) - newest last
    files.sort();

    if files.len() as u32 > max {
        let to_remove = files.len() as u32 - max;
        for path in files.iter().take(to_remove as usize) {
            let _ = fs::remove_file(path);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{get_version, list_versions};
    use std::fs;
    use uuid::Uuid;

    #[test]
    fn rejects_history_path_traversal() {
        let vault =
            std::env::temp_dir().join(format!("helixnotes-history-test-{}", Uuid::new_v4()));
        let metadata = vault.join(".helixnotes");
        let escaped_history = metadata.join("escaped");
        fs::create_dir_all(&escaped_history).unwrap();
        fs::write(escaped_history.join("2026-01-01T00-00-00.md"), "escaped").unwrap();
        fs::create_dir_all(metadata.join("history").join("safe")).unwrap();
        fs::write(metadata.join("secret.md"), "secret").unwrap();

        assert!(list_versions(&vault.to_string_lossy(), "../escaped").is_err());
        assert!(get_version(&vault.to_string_lossy(), "safe", "../../secret").is_err());

        fs::remove_dir_all(vault).unwrap();
    }
    #[test]
    fn rapid_forced_versions_are_individually_retrievable_and_prunable() {
        let vault = tempfile::tempdir().unwrap();
        let root = vault.path().to_str().unwrap();
        for index in 0..200 {
            super::force_snapshot(root, "note", &format!("snapshot {index}"), 0).unwrap();
        }
        let versions = list_versions(root, "note").unwrap();
        assert_eq!(versions.len(), 200);
        let contents: std::collections::HashSet<_> = versions
            .iter()
            .map(|version| get_version(root, "note", &version.timestamp).unwrap())
            .collect();
        assert_eq!(contents.len(), 200);
        assert!(contents.contains("snapshot 0"));
        assert!(contents.contains("snapshot 199"));

        super::force_snapshot(root, "note", "latest", 10).unwrap();
        let retained = list_versions(root, "note").unwrap();
        assert_eq!(retained.len(), 10);
        assert_eq!(
            get_version(root, "note", &retained[0].timestamp).unwrap(),
            "latest"
        );
    }

    #[test]
    fn legacy_versions_remain_readable_and_recent_unique_versions_observe_cooldown() {
        let vault = tempfile::tempdir().unwrap();
        let root = vault.path().to_str().unwrap();
        let history = vault.path().join(".helixnotes/history/note");
        fs::create_dir_all(&history).unwrap();
        fs::write(history.join("2026-01-01T00-00-00.md"), "legacy").unwrap();
        fs::write(history.join("2026-01-01T1é-00-00.md"), "unusual filename").unwrap();
        super::force_snapshot(root, "note", "new", 0).unwrap();
        super::maybe_snapshot(root, "note", "too soon", 0);
        let versions = list_versions(root, "note").unwrap();
        assert_eq!(versions.len(), 3);
        assert_eq!(
            get_version(root, "note", "2026-01-01T00:00:00Z").unwrap(),
            "legacy"
        );
        assert_eq!(
            get_version(root, "note", &versions[0].timestamp).unwrap(),
            "new"
        );
    }
}
