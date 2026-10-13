// .md vault -> SQLite importer.
//
// Walks an existing HelixNotes/RyukNotes filesystem vault (*.md files with
// YAML frontmatter, `.helixnotes/` metadata) and pours it into a VaultDb:
//
// - note id, title, tags, pinned, created, modified come from frontmatter
//   (falling back to filename/mtime when frontmatter is damaged)
// - notes under `.helixnotes/trash` keep their trashed state
// - history snapshots under `.helixnotes/history/<note id>/` are imported
//   into the versions table
// - the source vault is never modified; re-running is idempotent (upserts)
//
// Attachments (`.helixnotes/attachments/`) are not migrated in this pass:
// .md notes reference them by relative path, and rewiring those references
// to attachment ids is part of the frontend switch.

use crate::vault::db::{NewNote, VaultDb};
use crate::vault::frontmatter;
use chrono::Utc;
use std::path::Path;

#[derive(Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub notes_imported: u32,
    pub notes_trashed: u32,
    pub versions_imported: u32,
    pub files_skipped: u32,
    pub warnings: Vec<String>,
}

fn rfc3339(dt: chrono::DateTime<Utc>) -> String {
    dt.to_rfc3339()
}

/// Imports every .md file in `vault_root` into `db`.
pub fn import_md_vault(vault_root: &Path, db: &VaultDb) -> Result<ImportReport, String> {
    let mut report = ImportReport::default();
    let metadata_dir = vault_root.join(".helixnotes");
    let trash_dir = metadata_dir.join("trash");
    let history_dir = metadata_dir.join("history");

    let mut md_files: Vec<std::path::PathBuf> = Vec::new();
    collect_md_files(vault_root, &metadata_dir, &mut md_files)
        .map_err(|e| format!("walking vault: {e}"))?;
    // Trashed notes live inside .helixnotes/trash; collect them too.
    collect_md_files(&trash_dir, &metadata_dir, &mut md_files)
        .map_err(|e| format!("walking trash: {e}"))?;

    for path in md_files {
        let raw = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => {
                report.warnings.push(format!("unreadable {}: {e}", path.display()));
                report.files_skipped += 1;
                continue;
            }
        };
        let filename = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (meta, body) = frontmatter::parse_note(&raw, &filename);

        let in_trash = path.starts_with(&trash_dir);
        // A trashed note keeps its frontmatter id so history still links.
        let note = NewNote {
            id: meta.id,
            title: meta.title,
            content: body,
            pinned: meta.pinned,
            tags: meta.tags,
            created: rfc3339(meta.created),
            modified: rfc3339(meta.modified),
        };
        let note_id = note.id.clone();
        if let Err(e) = db.upsert_note(&note) {
            report.warnings.push(format!("failed to import {}: {e}", path.display()));
            report.files_skipped += 1;
            continue;
        }
        if in_trash {
            db.set_note_trashed(&note_id, true)?;
            report.notes_trashed += 1;
        } else {
            report.notes_imported += 1;
        }

        // History snapshots: .helixnotes/history/<note id>/*.md (raw bodies).
        let note_history = history_dir.join(sanitize(&note_id));
        if let Ok(entries) = std::fs::read_dir(&note_history) {
            for entry in entries.flatten() {
                let history_path = entry.path();
                if history_path.extension().and_then(|e| e.to_str()) != Some("md") {
                    continue;
                }
                if let Ok(snapshot) = std::fs::read_to_string(&history_path) {
                    if db.add_version(&note_id, &snapshot, &rfc3339(Utc::now())).is_ok() {
                        report.versions_imported += 1;
                    }
                }
            }
        }
    }
    Ok(report)
}

/// Windows forbids a handful of characters in file names; the history store
/// sanitizes note ids for directory names. Mirror that mapping when locating
/// the history directory (best-effort: strip forbidden characters).
fn sanitize(id: &str) -> String {
    id.chars()
        .filter(|c| !matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'))
        .collect()
}

fn collect_md_files(
    dir: &Path,
    skip: &Path,
    out: &mut Vec<std::path::PathBuf>,
) -> std::io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in walkdir::WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            // Never descend into the metadata dir from the vault walk (the
            // trash/history passes handle their subdirectories explicitly).
            e.path() != skip
        })
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("md") {
            out.push(path.to_path_buf());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_note(dir: &Path, name: &str, frontmatter: &str, body: &str) {
        std::fs::write(dir.join(name), format!("---\n{frontmatter}---\n{body}")).unwrap();
    }

    #[test]
    fn imports_notes_tags_trash_and_history() {
        let vault = tempfile::tempdir().unwrap();
        let root = vault.path();

        write_note(
            root,
            "Alpha.md",
            "id: id-alpha\ntitle: Alpha\ntags: [immunology, crypto]\npinned: true\ncreated: 2026-01-01T00:00:00Z\nmodified: 2026-02-01T00:00:00Z\n",
            "Alpha body.",
        );
        write_note(
            root,
            "Beta.md",
            "id: id-beta\ntitle: Beta\ntags: []\npinned: false\ncreated: 2026-01-02T00:00:00Z\nmodified: 2026-02-02T00:00:00Z\n",
            "Beta body.",
        );
        // Damaged frontmatter must not abort the import.
        std::fs::write(root.join("Broken.md"), "no frontmatter at all").unwrap();

        let meta = root.join(".helixnotes");
        std::fs::create_dir_all(meta.join("trash")).unwrap();
        std::fs::create_dir_all(meta.join("history/id-alpha")).unwrap();
        write_note(
            &meta.join("trash"),
            "Deleted.md",
            "id: id-deleted\ntitle: Deleted\ntags: [old]\npinned: false\ncreated: 2026-01-03T00:00:00Z\nmodified: 2026-02-03T00:00:00Z\n",
            "Deleted body.",
        );
        std::fs::write(meta.join("history/id-alpha/2026-01-15.md"), "Alpha old body.").unwrap();
        std::fs::write(meta.join("state.json"), "{}").unwrap();

        let db = VaultDb::open_in_memory().unwrap();
        let report = import_md_vault(root, &db).unwrap();

        // Damaged-frontmatter note is rescued with fallback metadata, not skipped.
        assert_eq!(report.notes_imported, 3);
        assert_eq!(report.notes_trashed, 1);
        assert_eq!(report.versions_imported, 1);

        let notes = db.list_notes(false).unwrap();
        assert_eq!(notes.len(), 3);
        let alpha = notes.iter().find(|n| n.id == "id-alpha").unwrap();
        assert!(alpha.pinned);
        assert_eq!(alpha.tags.len(), 2);
        assert_eq!(
            db.get_note_content("id-alpha").unwrap().unwrap(),
            "Alpha body."
        );

        let trashed = db.list_notes(true).unwrap();
        assert_eq!(trashed.len(), 4);
        assert!(trashed.iter().any(|n| n.id == "id-deleted" && n.trashed));

        let versions = db.list_versions("id-alpha").unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].1, "Alpha old body.");
    }

    #[test]
    fn import_is_idempotent() {
        let vault = tempfile::tempdir().unwrap();
        write_note(
            vault.path(),
            "Only.md",
            "id: id-only\ntitle: Only\ntags: [x]\npinned: false\ncreated: 2026-01-01T00:00:00Z\nmodified: 2026-01-01T00:00:00Z\n",
            "body",
        );
        let db = VaultDb::open_in_memory().unwrap();
        import_md_vault(vault.path(), &db).unwrap();
        import_md_vault(vault.path(), &db).unwrap();
        assert_eq!(db.list_notes(true).unwrap().len(), 1);
        // History dedupes by snapshot id (content-addressed below), so a
        // second import of the same snapshot must not duplicate it.
        let history = vault.path().join(".helixnotes/history/id-only");
        std::fs::create_dir_all(&history).unwrap();
        std::fs::write(history.join("snap.md"), "v1").unwrap();
        import_md_vault(vault.path(), &db).unwrap();
        import_md_vault(vault.path(), &db).unwrap();
        assert_eq!(db.list_versions("id-only").unwrap().len(), 1);
    }
}

#[cfg(test)]
mod real_vault_tests {
    use super::*;
    use crate::vault::db::VaultDb;

    /// Dry-run import of the user's real vault. Ignored by default:
    ///     cargo test real_vault_dry_run -- --ignored --nocapture
    /// Never modifies the source vault; writes only to a temp DB.
    #[test]
    #[ignore]
    fn real_vault_dry_run() {
        let vault = std::env::var("RYUK_REAL_VAULT").expect("set RYUK_REAL_VAULT");
        // RYUK_REAL_DB_OUT (optional): also materialize the vault as a .rn file.
        let db = match std::env::var("RYUK_REAL_DB_OUT") {
            Ok(out) => {
                let path = std::path::PathBuf::from(&out);
                if path.exists() {
                    std::fs::remove_file(&path).unwrap();
                }
                VaultDb::open(&path).unwrap()
            }
            Err(_) => VaultDb::open_in_memory().unwrap(),
        };
        let report = import_md_vault(std::path::Path::new(&vault), &db).unwrap();
        let total = db.list_notes(true).unwrap().len();
        let live = db.list_notes(false).unwrap().len();
        println!(
            "imported={} trashed={} versions={} skipped={} total={} live={}",
            report.notes_imported, report.notes_trashed, report.versions_imported,
            report.files_skipped, total, live
        );
        for w in &report.warnings {
            println!("WARN: {w}");
        }
        assert!(total > 50, "expected the real vault to have notes");
        assert!(report.warnings.is_empty(), "import must be warning-free");
    }
}
