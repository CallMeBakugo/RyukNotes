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
    let mut dir = PathBuf::from(vault_path);
    // Each level must be a real folder. A link here (carried in by git, a file-sync tool or an
    // extracted archive) would send snapshots, and the pruning that deletes old ones, to whatever
    // folder it points at.
    for part in [
        ".helixnotes",
        "history",
        safe_path_component(note_id, "note ID")?,
    ] {
        dir.push(part);
        if fs::symlink_metadata(&dir).is_ok_and(|meta| !meta.file_type().is_dir()) {
            return Err("Version history folder is not a real folder".to_string());
        }
    }
    Ok(dir)
}

/// `2026-01-02T03-04-05.md` or `2026-01-02T03-04-05.678.md`: the only names snapshots are given.
fn is_snapshot_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".md") else {
        return false;
    };
    let (Some(time), Some(fraction)) = (stem.get(..19), stem.get(19..)) else {
        return false;
    };
    chrono::NaiveDateTime::parse_from_str(time, "%Y-%m-%dT%H-%M-%S").is_ok()
        && (fraction.is_empty()
            || fraction.strip_prefix('.').is_some_and(|digits| {
                !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
            }))
}

/// A snapshot the app wrote: a regular file (never a link) with a snapshot name.
fn is_snapshot_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(is_snapshot_name)
        && fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_file())
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
        match crate::vault::operations::persist_without_replacing(temporary, &path) {
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
        if is_snapshot_file(&path) {
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
    if !is_snapshot_file(&path) {
        return Err("Version not found".to_string());
    }
    fs::read_to_string(&path).map_err(|e| format!("Version not found: {}", e))
}

/// Remove old versions keeping only the newest `max` entries.
fn prune_versions(dir: &Path, max: u32) -> Result<(), String> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        // Only snapshots are counted and removed, never other Markdown files that ended up here.
        .filter(|p| is_snapshot_file(p))
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
        // The file that is not named like a snapshot is neither a version nor in the way.
        assert_eq!(versions.len(), 2);
        assert_eq!(
            get_version(root, "note", "2026-01-01T00:00:00Z").unwrap(),
            "legacy"
        );
        assert_eq!(
            get_version(root, "note", &versions[0].timestamp).unwrap(),
            "new"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_history_folder_is_never_written_to_or_pruned() {
        use std::os::unix::fs::symlink;
        let vault = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = vault.path().to_str().unwrap();
        // A folder of ordinary notes somewhere else, more than the pruning limit.
        for index in 0..25 {
            fs::write(
                outside.path().join(format!("note-{index:02}.md")),
                "precious",
            )
            .unwrap();
        }
        let history = vault.path().join(".helixnotes/history");
        fs::create_dir_all(&history).unwrap();
        symlink(outside.path(), history.join("linked")).unwrap();

        assert!(super::force_snapshot(root, "linked", "new", 20).is_err());
        super::maybe_snapshot(root, "linked", "new", 20);
        assert!(list_versions(root, "linked").is_err());
        assert!(get_version(root, "linked", "note-00").is_err());
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 25);

        // The same when a higher level is the link.
        let second = tempfile::tempdir().unwrap();
        fs::create_dir_all(second.path().join(".helixnotes")).unwrap();
        symlink(outside.path(), second.path().join(".helixnotes/history")).unwrap();
        assert!(super::force_snapshot(second.path().to_str().unwrap(), "note", "new", 20).is_err());
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 25);
    }

    #[test]
    fn pruning_and_listing_only_touch_snapshots() {
        let vault = tempfile::tempdir().unwrap();
        let root = vault.path().to_str().unwrap();
        let history = vault.path().join(".helixnotes/history/note");
        fs::create_dir_all(&history).unwrap();
        // Markdown files that are not snapshots, sorting before and after real ones.
        fs::write(history.join("README.md"), "keep").unwrap();
        fs::write(history.join("zz-00.md"), "keep").unwrap();
        fs::write(history.join("2026-01-01T00-00-00.md"), "old").unwrap();
        fs::write(history.join("2026-01-02T00-00-00.123.md"), "newer").unwrap();
        super::force_snapshot(root, "note", "newest", 2).unwrap();
        let versions = list_versions(root, "note").unwrap();
        assert_eq!(versions.len(), 2);
        assert!(!history.join("2026-01-01T00-00-00.md").exists());
        assert!(history.join("README.md").exists() && history.join("zz-00.md").exists());
        assert!(get_version(root, "note", "zz-00").is_err());
        #[cfg(unix)]
        {
            let target = vault.path().join("secret.md");
            fs::write(&target, "outside history").unwrap();
            std::os::unix::fs::symlink(&target, history.join("2026-01-03T00-00-00.md")).unwrap();
            assert!(get_version(root, "note", "2026-01-03T00:00:00Z").is_err());
            assert_eq!(list_versions(root, "note").unwrap().len(), 2);
        }
    }
}
