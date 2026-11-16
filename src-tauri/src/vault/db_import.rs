// .md vault -> SQLite importer.
//
// Walks an existing RyukNotes/RyukNotes filesystem vault (*.md files with
// YAML frontmatter, `.ryuknotes/` metadata) and pours it into a VaultDb:
//
// - note id, title, tags, pinned, created, modified come from frontmatter
//   (falling back to filename/mtime when frontmatter is damaged)
// - notes under `.ryuknotes/trash` keep their trashed state
// - history snapshots under `.ryuknotes/history/<note id>/` are imported
//   into the versions table
// - the source vault is never modified; re-running is idempotent (upserts)
//
// Local attachments are imported transactionally per note and rewritten to
// stable BLOB references. Portable DB exports additionally carry a validated
// index for exact identities, history, ownership and logical metadata.

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

// A source path, rather than content, distinguishes identical ID-less files and
// keeps their identity stable when their contents change. Include the selected
// root so unrelated folder imports cannot replace each other's fallback notes.
fn fallback_id(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    format!(
        "md-import-{:x}",
        Sha256::digest(path.as_os_str().as_encoded_bytes())
    )
}

/// Imports every .md file in `vault_root` into `db`.
pub fn import_md_vault(vault_root: &Path, db: &VaultDb) -> Result<ImportReport, String> {
    let canonical_root = std::fs::canonicalize(vault_root).map_err(|e| e.to_string())?;
    let vault_root = canonical_root.as_path();
    if let Some(report) = super::db_export::import_export(vault_root, db)? {
        return Ok(report);
    }
    let mut report = ImportReport::default();
    let metadata_dir = vault_root.join(".ryuknotes");
    let legacy_dir = vault_root.join(".helixnotes");
    let trash_dirs = [metadata_dir.join("trash"), legacy_dir.join("trash")];
    let history_dirs = [metadata_dir.join("history"), legacy_dir.join("history")];

    let mut md_files: Vec<std::path::PathBuf> = Vec::new();
    collect_md_files(vault_root, &metadata_dir, &mut md_files)
        .map_err(|e| format!("walking vault: {e}"))?;
    // Trashed notes live inside .ryuknotes/trash; collect them too.
    for trash_dir in &trash_dirs {
        collect_md_files(trash_dir, &metadata_dir, &mut md_files)
            .map_err(|e| format!("walking trash: {e}"))?;
    }

    md_files.sort();
    let mut seen_ids = std::collections::HashSet::new();
    let mut identities = std::collections::HashMap::new();
    for path in md_files {
        let raw = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => {
                report
                    .warnings
                    .push(format!("unreadable {}: {e}", path.display()));
                report.files_skipped += 1;
                continue;
            }
        };
        let filename = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let fallback = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .map(chrono::DateTime::<Utc>::from)
            .unwrap_or_else(|_| Utc::now());
        let (mut meta, body) = frontmatter::parse_note_with_fallback(&raw, &filename, fallback);
        if meta.id.trim().is_empty() {
            meta.id = fallback_id(&path);
        }

        if !seen_ids.insert(meta.id.clone()) {
            report.warnings.push(format!(
                "Duplicate source identity rescued as a separate note: {}",
                path.display()
            ));
            meta.id = fallback_id(&path);
            seen_ids.insert(meta.id.clone());
        }
        let in_trash = trash_dirs.iter().any(|dir| path.starts_with(dir));
        // A trashed note keeps its frontmatter id so history still links.
        let mut note = NewNote {
            id: meta.id,
            title: meta.title,
            content: body,
            pinned: meta.pinned,
            tags: meta.tags,
            created: rfc3339(meta.created),
            modified: rfc3339(meta.modified),
        };
        let note_id = note.id.clone();
        let imported = (|| -> Result<(), String> {
            let tx =
                db.begin_write("import", None, &serde_json::json!([note.id, raw, in_trash]))?;
            if tx.is_replay() {
                return tx.commit();
            }
            let replacing = db.get_note_content(&note.id)?.is_some();
            if !replacing {
                super::writer::write_import_on(db, &tx, &note, super::writer::ImportPolicy::Copy)?;
            }
            let rewritten = super::db_attachments::import_references(
                db, &tx, vault_root, &path, &raw, &note.id,
            )?;
            note.content = frontmatter::parse_note(&rewritten, &filename).1;
            super::writer::write_import_on(
                db,
                &tx,
                &note,
                if replacing {
                    super::writer::ImportPolicy::Replace
                } else {
                    super::writer::ImportPolicy::FinalizeCopy
                },
            )?;
            if db.is_v3() {
                tx.execute(
                    "UPDATE notes SET trashed=?2 WHERE id=?1",
                    rusqlite::params![note.id, in_trash],
                )
                .map_err(|e| e.to_string())?;
            }
            if !in_trash {
                let relative = path
                    .parent()
                    .ok_or("Missing source parent")?
                    .strip_prefix(vault_root)
                    .map_err(|e| e.to_string())?
                    .to_string_lossy()
                    .replace('\\', "/");
                let relative = super::db_notebooks::relative("", &relative)?;
                let mut layout = super::db_notebooks::Layout::load(&tx)?;
                let mut ancestor = String::new();
                for part in relative.split('/').filter(|part| !part.is_empty()) {
                    if !ancestor.is_empty() {
                        ancestor.push('/');
                    }
                    ancestor.push_str(part);
                    layout.notebooks.insert(ancestor.clone());
                }
                layout.assign(&note_id, &relative)?;
                layout.save(&tx)?;
            }
            tx.commit().map_err(|e| e.to_string())
        })();
        if let Err(e) = imported {
            report
                .warnings
                .push(format!("failed to import {}: {e}", path.display()));
            report.files_skipped += 1;
            continue;
        }
        identities.insert(
            path.strip_prefix(vault_root)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .replace('\\', "/"),
            note_id.clone(),
        );
        if in_trash {
            if !db.is_v3() {
                db.set_note_trashed(&note_id, true)?;
            }
            report.notes_trashed += 1;
        } else {
            report.notes_imported += 1;
        }

        // History snapshots: .ryuknotes/history/<note id>/*.md (raw bodies).
        for history_dir in &history_dirs {
            let note_history = history_dir.join(sanitize(&note_id));
            if std::fs::canonicalize(&note_history).is_ok_and(|path| path.starts_with(vault_root)) {
                if let Ok(entries) = std::fs::read_dir(&note_history) {
                    for entry in entries.flatten() {
                        let history_path = entry.path();
                        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                            continue;
                        }
                        if history_path.extension().and_then(|e| e.to_str()) != Some("md") {
                            continue;
                        }
                        if let Ok(snapshot) = std::fs::read_to_string(&history_path) {
                            let imported = (|| -> Result<(), String> {
                                let tx = db.begin_auxiliary_write(
                                    "import",
                                    &serde_json::json!([note_id, snapshot]),
                                )?;
                                if tx.is_replay() {
                                    return tx.commit();
                                }
                                let snapshot = super::db_attachments::import_references(
                                    db,
                                    &tx,
                                    vault_root,
                                    &history_path,
                                    &snapshot,
                                    &note_id,
                                )?;
                                tx.commit().map_err(|e| e.to_string())?;
                                db.add_version(
                                    &note_id,
                                    &snapshot,
                                    &history_timestamp(&history_path),
                                )?;
                                Ok(())
                            })();
                            match imported {
                                Ok(()) => report.versions_imported += 1,
                                Err(error) => report.warnings.push(format!(
                                    "failed to import history {}: {error}",
                                    history_path.display()
                                )),
                            }
                        }
                    }
                }
            }
        }
    }
    for dir in [&legacy_dir, &metadata_dir] {
        if let Err(error) =
            super::db_appearance::import_folder_metadata(db, vault_root, dir, &identities)
        {
            report
                .warnings
                .push(format!("Metadata import {}: {error}", dir.display()));
        }
    }
    Ok(report)
}

fn history_timestamp(path: &Path) -> String {
    let mut stamp = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    if stamp.as_bytes().get(10) == Some(&b'T')
        && stamp.as_bytes().get(13) == Some(&b'-')
        && stamp.as_bytes().get(16) == Some(&b'-')
    {
        stamp.replace_range(16..17, ":");
        stamp.replace_range(13..14, ":");
        stamp.push('Z');
    }
    let parsed = frontmatter::parse_date_flexible(&stamp).or_else(|| {
        std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .map(chrono::DateTime::<Utc>::from)
    });
    rfc3339(parsed.unwrap_or_else(Utc::now))
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
    let selected_root = skip
        .parent()
        .ok_or_else(|| std::io::Error::other("Missing vault root"))?;
    if !std::fs::canonicalize(dir)?.starts_with(selected_root) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "Import directory escapes the selected vault",
        ));
    }
    for entry in walkdir::WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            // Never descend into the metadata dir from the vault walk (the
            // trash/history passes handle their subdirectories explicitly).
            e.path() != skip && e.path() != selected_root.join(".helixnotes")
        })
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let path = entry.path();
        if entry.file_type().is_file() && path.extension().and_then(|e| e.to_str()) == Some("md") {
            out.push(path.to_path_buf());
        }
    }
    Ok(())
}

/// A single external import is a copy: never overwrite a colliding note id.
pub(crate) fn import_single_receipt(
    db: &VaultDb,
    prepared: super::operations::PreparedNoteImport,
    notebook: Option<&str>,
) -> Result<serde_json::Value, String> {
    let payloads = prepared
        .targets
        .iter()
        .map(|(path, _)| super::db_attachments::read_capped(path, db.max_attachment_bytes))
        .collect::<Result<Vec<_>, _>>()?;
    let filename = prepared
        .source
        .file_name()
        .ok_or("Source has no filename")?
        .to_string_lossy();
    let (mut meta, body) = frontmatter::parse_note(&prepared.raw, &filename);
    if meta.id.trim().is_empty() {
        meta.id = uuid::Uuid::new_v4().to_string();
    }
    let payload_hashes = payloads
        .iter()
        .map(|bytes| format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(bytes)))
        .collect::<Vec<_>>();
    let tx = db.begin_write(
        "import",
        None,
        &serde_json::json!([prepared.raw, notebook, payload_hashes]),
    )?;
    if let Some(receipt) = tx.replay_any_receipt()? {
        return Ok(receipt);
    }
    let mut layout = super::db_notebooks::Layout::load(&tx)?;
    let relative = super::db_notebooks::relative("", notebook.unwrap_or(""))?;
    layout.require(&relative)?;
    while tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM notes WHERE id=?1)",
            rusqlite::params![meta.id],
            |r| r.get::<_, bool>(0),
        )
        .map_err(|e| e.to_string())?
    {
        meta.id = uuid::Uuid::new_v4().to_string();
    }
    let mut note = NewNote {
        id: meta.id,
        title: meta.title,
        content: body,
        pinned: meta.pinned,
        tags: meta.tags,
        created: meta.created.to_rfc3339(),
        modified: meta.modified.to_rfc3339(),
    };
    super::writer::write_import_on(db, &tx, &note, super::writer::ImportPolicy::Copy)?;
    let mut rewritten = String::new();
    let mut previous = 0;
    for ((reference, (path, suffix)), data) in prepared
        .references
        .iter()
        .zip(&prepared.targets)
        .zip(&payloads)
    {
        let name = path
            .file_name()
            .ok_or("Attachment has no filename")?
            .to_string_lossy();
        let mime = super::db_attachments::mime_type(&name, data);
        let id =
            db.store_attachment_on(&tx, &note.id, &name, mime, data, &Utc::now().to_rfc3339())?;
        let mut href = format!(".rn-attachments/{id}{suffix}");
        if reference.html {
            href = href
                .replace('&', "&amp;")
                .replace('"', "&quot;")
                .replace('\'', "&#39;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
        } else {
            href = href
                .replace(' ', "%20")
                .replace('"', "%22")
                .replace('<', "%3C")
                .replace('>', "%3E");
        }
        rewritten.push_str(&prepared.raw[previous..reference.from]);
        rewritten.push_str(&href);
        previous = reference.to;
    }
    rewritten.push_str(&prepared.raw[previous..]);
    note.content = frontmatter::parse_note(&rewritten, &filename).1;
    super::writer::write_import_on(db, &tx, &note, super::writer::ImportPolicy::FinalizeCopy)?;
    layout.assign(&note.id, &relative)?;
    layout.save(&tx)?;
    let receipt = tx.receipt(&note.id)?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(receipt)
}

#[cfg(test)]
pub(crate) fn import_single(
    db: &VaultDb,
    prepared: super::operations::PreparedNoteImport,
    notebook: Option<&str>,
) -> Result<String, String> {
    Ok(import_single_receipt(db, prepared, notebook)?["id"]
        .as_str()
        .ok_or("Missing imported identity")?
        .into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_folder_import_preserves_layout_history_metadata_and_duplicate_sources() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        for path in [
            "Projects/Child",
            ".helixnotes/trash",
            ".helixnotes/history/shared",
        ] {
            std::fs::create_dir_all(source.join(path)).unwrap();
        }
        let raw = "---\nid: shared\ntitle: Original\n---\nbody";
        std::fs::write(source.join("Projects/Child/A.md"), raw).unwrap();
        std::fs::write(source.join("Projects/Child/B.md"), raw).unwrap();
        std::fs::write(
            source.join(".helixnotes/trash/old.md"),
            "---\nid: old\ntitle: Deleted\n---\nold",
        )
        .unwrap();
        std::fs::write(
            source.join(".helixnotes/history/shared/2020-01-02T03-04-05.md"),
            "historical body",
        )
        .unwrap();
        std::fs::write(
            source.join(".helixnotes/notebook_icons.json"),
            r#"{"Projects":"builtin:star"}"#,
        )
        .unwrap();
        std::fs::write(
            source.join(".helixnotes/tag_styles.json"),
            r##"{"work":{"color":"#abc"}}"##,
        )
        .unwrap();
        std::fs::write(
            source.join(".helixnotes/quick_access.json"),
            r#"["Projects/Child/A.md"]"#,
        )
        .unwrap();
        let (db, _) =
            VaultDb::open_with_report(&root.path().join("fixture.rn"), &root.path().join("ledger"))
                .unwrap();
        let report = import_md_vault(&source, &db).unwrap();
        assert_eq!(
            (
                report.notes_imported,
                report.notes_trashed,
                report.files_skipped
            ),
            (2, 1, 0)
        );
        assert_eq!(report.warnings.len(), 1);
        let layout = super::super::db_notebooks::Layout::load(&db.conn).unwrap();
        assert_eq!(layout.notes["shared"], "Projects/Child");
        let icons: std::collections::HashMap<String, String> =
            super::super::db_appearance::load(&db, "notebook_icons").unwrap();
        assert_eq!(icons["Projects"], "builtin:star");
        assert_eq!(
            db.get_state("quick_access").unwrap().unwrap(),
            serde_json::json!(["shared"])
        );
        let stamp: String = db
            .conn
            .query_row(
                "SELECT created FROM versions WHERE note_id='shared'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(stamp.starts_with("2020-01-02T03:04:05"));
        import_md_vault(&source, &db).unwrap();
        assert_eq!(db.list_notes(true).unwrap().len(), 3);
        let versions: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM versions WHERE note_id='shared'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(versions, 1);
        let duplicate = super::super::db_notebooks::duplicate_note(&db, "shared").unwrap();
        let id = duplicate["id"].as_str().unwrap();
        assert_ne!(id, "shared");
        assert_eq!(
            super::super::db_notebooks::Layout::load(&db.conn)
                .unwrap()
                .notes[id],
            "Projects/Child"
        );
        assert_eq!(duplicate["content"], "body");
    }

    #[test]
    fn plain_files_keep_distinct_stable_identities_in_production_vaults() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("A.md"), "same body").unwrap();
        std::fs::write(source.join("B.md"), "same body").unwrap();
        let (db, _) =
            VaultDb::open_with_report(&root.path().join("test.rn"), &root.path().join("ledger"))
                .unwrap();
        let first = import_md_vault(&source, &db).unwrap();
        assert_eq!(first.notes_imported, 2);
        assert!(first.warnings.is_empty());
        let before = db.list_notes(false).unwrap();
        assert_eq!(before.len(), 2);
        assert!(before.iter().all(|n| !n.id.is_empty()));
        let a = before.iter().find(|n| n.title == "A").unwrap().id.clone();
        let b = before.iter().find(|n| n.title == "B").unwrap().id.clone();
        assert_ne!(a, b);
        import_md_vault(&source, &db).unwrap();
        assert_eq!(db.list_notes(false).unwrap().len(), 2);
        std::fs::write(source.join("A.md"), "changed A").unwrap();
        import_md_vault(&source, &db).unwrap();
        assert_eq!(db.list_notes(false).unwrap().len(), 2);
        assert_eq!(
            db.get_note_content(&a).unwrap().as_deref(),
            Some("changed A")
        );
        assert_eq!(
            db.get_note_content(&b).unwrap().as_deref(),
            Some("same body")
        );
    }

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

        let meta = root.join(".ryuknotes");
        std::fs::create_dir_all(meta.join("trash")).unwrap();
        std::fs::create_dir_all(meta.join("history/id-alpha")).unwrap();
        write_note(
            &meta.join("trash"),
            "Deleted.md",
            "id: id-deleted\ntitle: Deleted\ntags: [old]\npinned: false\ncreated: 2026-01-03T00:00:00Z\nmodified: 2026-02-03T00:00:00Z\n",
            "Deleted body.",
        );
        std::fs::write(
            meta.join("history/id-alpha/2026-01-15.md"),
            "Alpha old body.",
        )
        .unwrap();
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
        let history = vault.path().join(".ryuknotes/history/id-only");
        std::fs::create_dir_all(&history).unwrap();
        std::fs::write(history.join("snap.md"), "v1").unwrap();
        import_md_vault(vault.path(), &db).unwrap();
        import_md_vault(vault.path(), &db).unwrap();
        assert_eq!(db.list_versions("id-only").unwrap().len(), 1);
    }

    #[test]
    fn reimport_links_existing_blob_from_body_and_history_idempotently() {
        let source = tempfile::tempdir().unwrap();
        let db = VaultDb::open_in_memory().unwrap();
        db.upsert_note(&NewNote {
            id: "owner".into(),
            title: "Owner".into(),
            content: String::new(),
            pinned: false,
            tags: vec![],
            created: "now".into(),
            modified: "now".into(),
        })
        .unwrap();
        let asset = db
            .add_attachment("owner", "file", "text/plain", b"shared", "now")
            .unwrap();
        write_note(
            source.path(),
            "Copy.md",
            "id: copy\ntitle: Copy\n",
            &format!("![copy]({})", asset.id),
        );
        write_note(
            source.path(),
            "History.md",
            "id: history\ntitle: History\n",
            "",
        );
        let history = source.path().join(".ryuknotes/history/history");
        std::fs::create_dir_all(&history).unwrap();
        std::fs::write(history.join("snapshot.md"), &asset.id).unwrap();
        for _ in 0..2 {
            import_md_vault(source.path(), &db).unwrap();
        }
        assert_eq!(
            db.get_attachment_data(&asset.id)
                .unwrap()
                .unwrap()
                .0
                .note_ids,
            vec!["copy", "history", "owner"]
        );
        db.delete_note("owner").unwrap();
        assert_eq!(
            db.get_attachment_data(&asset.id).unwrap().unwrap().1,
            b"shared"
        );
        assert_eq!(db.list_versions("history").unwrap().len(), 1);
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
        // RYUK_REAL_DB_UPSERT=1 with an existing OUT file imports INTO it
        // (upserts) instead of rebuilding — preserves DB-only notes.
        let db = match std::env::var("RYUK_REAL_DB_OUT") {
            Ok(out) => {
                let path = std::path::PathBuf::from(&out);
                let upsert = std::env::var("RYUK_REAL_DB_UPSERT").as_deref() == Ok("1");
                if path.exists() && !upsert {
                    std::fs::remove_file(&path).unwrap();
                }
                VaultDb::open_legacy_v2(&path).unwrap()
            }
            Err(_) => VaultDb::open_in_memory().unwrap(),
        };
        let report = import_md_vault(std::path::Path::new(&vault), &db).unwrap();
        let total = db.list_notes(true).unwrap().len();
        let live = db.list_notes(false).unwrap().len();
        println!(
            "imported={} trashed={} versions={} skipped={} total={} live={}",
            report.notes_imported,
            report.notes_trashed,
            report.versions_imported,
            report.files_skipped,
            total,
            live
        );
        for w in &report.warnings {
            println!("WARN: {w}");
        }
        assert!(total > 50, "expected the real vault to have notes");
        assert!(report.warnings.is_empty(), "import must be warning-free");
    }
}
