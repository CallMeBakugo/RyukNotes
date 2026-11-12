//! Portable Markdown exports. The index preserves BLOB identities, ownership,
//! history and logical metadata. Portable relative links restore to DB references.
//! No schema change and no BLOB garbage collection are involved.
use super::db::{NewNote, VaultDb};
use super::db_import::ImportReport;
use crate::vault::frontmatter;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

const INDEX: &str = ".ryuknotes/export.json";

#[derive(Default, Serialize, Deserialize)]
struct Index {
    format: u32,
    notes: Vec<ExportNote>,
    attachments: Vec<ExportAttachment>,
    versions: Vec<ExportVersion>,
    meta: BTreeMap<String, String>,
    tags: Vec<String>,
    sessions: Vec<(String, String, String)>,
}
#[derive(Serialize, Deserialize)]
struct ExportNote {
    id: String,
    title: String,
    file: String,
    trashed: bool,
    created: String,
    modified: String,
}
#[derive(Serialize, Deserialize)]
struct ExportAttachment {
    id: String,
    filename: String,
    mime: String,
    sha256: String,
    size: i64,
    created: String,
    owners: Vec<String>,
}
#[derive(Serialize, Deserialize)]
struct ExportVersion {
    id: String,
    note_id: String,
    file: String,
    created: String,
}
fn digest(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}
fn note_file(id: &str, title: &str, trashed: bool) -> String {
    let safe = super::operations::sanitize_filename(title);
    let mut label = String::new();
    for c in safe.chars() {
        if label.len() + c.len_utf8() > 96 {
            break;
        }
        label.push(if c.is_control() { '-' } else { c });
    }
    let label = label.trim().trim_matches('.');
    let label = if label.is_empty() { "Untitled" } else { label };
    format!(
        "{}{}--{}.md",
        if trashed { ".ryuknotes/trash/" } else { "" },
        label,
        digest(id.as_bytes())
    )
}

fn asset_path(id: &str, filename: &str) -> String {
    // Preserve useful extensions (including SVG) for standalone viewers and
    // single-note imports. Never turn attached Markdown into an exported note.
    let extension = Path::new(filename)
        .extension()
        .and_then(|s| s.to_str())
        .filter(|s| {
            !s.eq_ignore_ascii_case("md")
                && s.len() <= 12
                && !s.is_empty()
                && s.bytes().all(|b| b.is_ascii_alphanumeric())
        })
        .unwrap_or("bin");
    format!(
        ".ryuknotes/attachments/{id}.{}",
        extension.to_ascii_lowercase()
    )
}

/// Rewrite destinations only, not labels, code examples or unrelated text.
fn convert_links(body: &str, paths: &BTreeMap<String, String>) -> String {
    let mut output = String::new();
    let mut previous = 0;
    for reference in super::db_attachments::body_references(body) {
        let raw = &body[reference.from..reference.to];
        let boundary = raw.find(['?', '#']).unwrap_or(raw.len());
        let old = &raw[..boundary];
        let Some(new) = paths.get(old) else {
            continue;
        };
        output.push_str(&body[previous..reference.from]);
        output.push_str(new);
        output.push_str(&raw[old.len()..]);
        previous = reference.to;
    }
    output.push_str(&body[previous..]);
    output
}

/// Reject traversal and symlinks on both reads and writes, including an export
/// directory planted inside an otherwise user-selected folder.
fn checked_path(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let mut path = root.to_path_buf();
    for part in Path::new(relative).components() {
        let Component::Normal(part) = part else {
            return Err("Invalid export path".into());
        };
        path.push(part);
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(format!("Export path is a symlink: {}", path.display()));
        }
    }
    Ok(path)
}
fn write(root: &Path, relative: &str, bytes: &[u8]) -> Result<(), String> {
    let path = checked_path(root, relative)?;
    std::fs::create_dir_all(path.parent().ok_or("Export file has no parent")?)
        .map_err(|e| e.to_string())?;
    super::operations::replace_file_atomically(&path, bytes)
}
fn files(index: &Index) -> BTreeSet<String> {
    index
        .notes
        .iter()
        .map(|n| n.file.clone())
        .chain(index.versions.iter().map(|v| v.file.clone()))
        .chain(
            index
                .attachments
                .iter()
                .map(|a| asset_path(&a.id, &a.filename)),
        )
        .collect()
}

fn validate_index(index: &Index) -> Result<(), String> {
    if index.format != 1 {
        return Err("Unsupported Markdown export format".into());
    }
    let mut notes = BTreeSet::new();
    let mut assets = BTreeSet::new();
    let mut versions = BTreeSet::new();
    for note in &index.notes {
        let expected = note_file(&note.id, &note.title, note.trashed);
        if note.file != expected || !notes.insert(note.id.clone()) {
            return Err("Invalid export note path/identity".into());
        }
    }
    for asset in &index.attachments {
        uuid::Uuid::parse_str(&asset.id).map_err(|e| e.to_string())?;
        if !assets.insert(asset.id.clone()) || asset.owners.iter().any(|id| !notes.contains(id)) {
            return Err("Invalid export attachment ownership".into());
        }
    }
    for version in &index.versions {
        let expected = format!(
            ".ryuknotes/history/{}/{}.md",
            digest(version.note_id.as_bytes()),
            digest(version.id.as_bytes())
        );
        if version.file != expected
            || !notes.contains(&version.note_id)
            || !versions.insert(version.id.clone())
        {
            return Err("Invalid export history path/identity".into());
        }
    }
    if index
        .sessions
        .iter()
        .any(|(note, _, _)| !notes.contains(note))
    {
        return Err("Invalid export session ownership".into());
    }
    Ok(())
}

pub fn export_md_vault(root: &Path, db: &VaultDb) -> Result<u32, String> {
    if db.is_v3() {
        return Err("Portable v3 ancestry export is gated until Stage 4E".into());
    }
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let root = std::fs::canonicalize(root).map_err(|e| e.to_string())?;
    let previous = checked_path(&root, INDEX)?;
    let previous: Option<Index> = if previous.exists() {
        Some(
            serde_json::from_slice(&std::fs::read(previous).map_err(|e| e.to_string())?)
                .map_err(|e| format!("Invalid previous export index: {e}"))?,
        )
    } else {
        None
    };
    if let Some(previous) = &previous {
        validate_index(previous)?;
    }
    // All reads, including bytes and history, share a single SQLite snapshot.
    let owned = previous.as_ref().map(files).unwrap_or_default();
    let publish = |relative: &str, bytes: &[u8]| {
        let path = checked_path(&root, relative)?;
        if path.exists()
            && !owned.contains(relative)
            && std::fs::read(&path).map_err(|e| e.to_string())?.as_slice() != bytes
        {
            return Err(format!(
                "Export would overwrite an unrelated file: {}",
                path.display()
            ));
        }
        write(&root, relative, bytes)
    };
    let tx = db.conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let mut index = Index {
        format: 1,
        ..Default::default()
    };
    let ids: Vec<String> = tx
        .prepare("SELECT id FROM attachments ORDER BY id")
        .map_err(|e| e.to_string())?
        .query_map([], |r| r.get(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    for id in ids {
        uuid::Uuid::parse_str(&id).map_err(|e| e.to_string())?;
        let (meta, data) = db.get_attachment_data(&id)?.ok_or("Missing attachment")?;
        if meta.size != data.len() as i64 || meta.sha256 != digest(&data) {
            return Err(format!("Attachment {id} failed byte/hash validation"));
        }
        let created = tx
            .query_row(
                "SELECT created FROM attachments WHERE id=?1",
                params![id],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        publish(&asset_path(&id, &meta.filename), &data)?;
        index.attachments.push(ExportAttachment {
            id,
            filename: meta.filename,
            mime: meta.mime_type,
            sha256: meta.sha256,
            size: meta.size,
            created,
            owners: meta.note_ids,
        });
    }
    let portable: BTreeMap<String, String> = index
        .attachments
        .iter()
        .map(|a| {
            (
                format!(".rn-attachments/{}", a.id),
                asset_path(&a.id, &a.filename),
            )
        })
        .collect();
    let mut notes = db.list_notes(true)?;
    notes.sort_by(|a, b| a.id.cmp(&b.id));
    for note in notes {
        let full = db.get_note_full(&note.id)?.ok_or("Missing note")?;
        let file = note_file(&note.id, &note.title, note.trashed);
        let meta = crate::types::NoteMeta {
            id: note.id.clone(),
            title: note.title,
            tags: note.tags,
            pinned: note.pinned,
            created: frontmatter::parse_date_flexible(&note.created)
                .unwrap_or(chrono::DateTime::UNIX_EPOCH),
            modified: frontmatter::parse_date_flexible(&note.modified)
                .unwrap_or(chrono::DateTime::UNIX_EPOCH),
        };
        let body = full["content"].as_str().ok_or("Missing note content")?;
        publish(
            &file,
            (frontmatter::serialize_frontmatter(&meta) + &convert_links(body, &portable))
                .as_bytes(),
        )?;
        let mut versions = db.list_versions(&note.id)?;
        versions.sort_by(|a, b| a.0.cmp(&b.0));
        for (id, content, created) in versions {
            let file = format!(
                ".ryuknotes/history/{}/{}.md",
                digest(note.id.as_bytes()),
                digest(id.as_bytes())
            );
            publish(&file, convert_links(&content, &portable).as_bytes())?;
            index.versions.push(ExportVersion {
                id,
                note_id: note.id.clone(),
                file,
                created,
            });
        }
        index.notes.push(ExportNote {
            id: note.id,
            title: meta.title.clone(),
            file,
            trashed: note.trashed,
            created: note.created,
            modified: note.modified,
        });
    }
    index.meta = tx
        .prepare("SELECT key,value FROM meta WHERE key != 'schema_version' ORDER BY key")
        .map_err(|e| e.to_string())?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    index.tags = tx
        .prepare("SELECT name FROM tags ORDER BY name")
        .map_err(|e| e.to_string())?
        .query_map([], |r| r.get(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    index.sessions = tx
        .prepare("SELECT note_id,session_id,created FROM session_links ORDER BY note_id")
        .map_err(|e| e.to_string())?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    // Re-export updates the same paths. Remove only paths owned by our previous
    // index, never an arbitrary Markdown file already in the selected directory.
    if let Some(previous) = previous {
        let current = files(&index);
        for obsolete in files(&previous).difference(&current) {
            let path = checked_path(&root, obsolete)?;
            if path.is_file() {
                std::fs::remove_file(path).map_err(|e| e.to_string())?;
            }
        }
    }
    write(
        &root,
        INDEX,
        &serde_json::to_vec_pretty(&index).map_err(|e| e.to_string())?,
    )?;
    Ok(index.notes.len() as u32)
}

/// The ordinary note parser normalizes body whitespace. An export must keep
/// the exact bytes following its complete frontmatter delimiter instead.
fn exported_body(raw: &str) -> Result<&str, String> {
    let prefix = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    if !prefix.starts_with("---\n") && !prefix.starts_with("---\r\n") {
        return Err("Export note is missing frontmatter".into());
    }
    let mut offset = raw.len() - prefix.len();
    for (index, line) in prefix.split_inclusive('\n').enumerate() {
        offset += line.len();
        if index > 0 && line.trim_end_matches(['\r', '\n']) == "---" {
            return Ok(&raw[offset..]);
        }
    }
    Err("Export note frontmatter is incomplete".into())
}

pub(crate) fn import_export(root: &Path, db: &VaultDb) -> Result<Option<ImportReport>, String> {
    let root = std::fs::canonicalize(root).map_err(|e| e.to_string())?;
    let path = checked_path(&root, INDEX)?;
    if !path.exists() {
        return Ok(None);
    }
    let index: Index = serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| format!("Invalid export index: {e}"))?;
    validate_index(&index)?;
    let tx = db.begin_write("import", None, &serde_json::json!(&index))?;
    if tx.is_replay() {
        return Ok(Some(ImportReport {
            notes_imported: index.notes.iter().filter(|n| !n.trashed).count() as u32,
            notes_trashed: index.notes.iter().filter(|n| n.trashed).count() as u32,
            versions_imported: index.versions.len() as u32,
            ..Default::default()
        }));
    }
    let mut ids = BTreeMap::new();
    for asset in &index.attachments {
        uuid::Uuid::parse_str(&asset.id).map_err(|e| e.to_string())?;
        let data = super::db_attachments::read_capped(
            &checked_path(&root, &asset_path(&asset.id, &asset.filename))?,
            db.max_attachment_bytes,
        )?;
        if asset.size != data.len() as i64 || digest(&data) != asset.sha256 {
            return Err(format!(
                "Attachment {} failed byte/hash validation",
                asset.id
            ));
        }
        // Preserve stable IDs in a fresh vault; when merging, SHA dedup wins.
        tx.execute("INSERT INTO attachments(id,filename,mime_type,size,sha256,data,created) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(sha256) DO NOTHING",
            params![asset.id,asset.filename,asset.mime,asset.size,asset.sha256,data,asset.created]).map_err(|e| e.to_string())?;
        let actual: String = tx
            .query_row(
                "SELECT id FROM attachments WHERE sha256=?1",
                params![asset.sha256],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        ids.insert(asset.id.clone(), actual);
    }
    let mut restored = BTreeMap::new();
    for asset in &index.attachments {
        let target = format!(".rn-attachments/{}", ids[&asset.id]);
        restored.insert(asset_path(&asset.id, &asset.filename), target.clone());
        restored.insert(format!(".rn-attachments/{}", asset.id), target);
    }
    let rewrite = |body: &str| convert_links(body, &restored);
    for tag in &index.tags {
        tx.execute("INSERT OR IGNORE INTO tags(name) VALUES(?1)", params![tag])
            .map_err(|e| e.to_string())?;
    }
    let mut report = ImportReport::default();
    for exported in &index.notes {
        let raw = std::fs::read_to_string(checked_path(&root, &exported.file)?)
            .map_err(|e| e.to_string())?;
        let (meta, _) = frontmatter::parse_note(&raw, &exported.file);
        let body = exported_body(&raw)?;
        if meta.id != exported.id {
            return Err("Export note identity changed".into());
        }
        super::writer::write_import_on(
            db,
            &tx,
            &NewNote {
                id: meta.id,
                title: meta.title,
                content: rewrite(body),
                pinned: meta.pinned,
                tags: meta.tags,
                created: exported.created.clone(),
                modified: exported.modified.clone(),
            },
            super::writer::ImportPolicy::Replace,
        )?;
        tx.execute(
            "UPDATE notes SET trashed=?2 WHERE id=?1",
            params![exported.id, exported.trashed],
        )
        .map_err(|e| e.to_string())?;
        if exported.trashed {
            report.notes_trashed += 1;
        } else {
            report.notes_imported += 1;
        }
    }
    for asset in &index.attachments {
        for owner in &asset.owners {
            tx.execute(
                "INSERT OR IGNORE INTO note_attachments(note_id,attachment_id) VALUES(?1,?2)",
                params![owner, ids[&asset.id]],
            )
            .map_err(|e| e.to_string())?;
        }
    }
    for version in &index.versions {
        let content = rewrite(
            &std::fs::read_to_string(checked_path(&root, &version.file)?)
                .map_err(|e| e.to_string())?,
        );
        let mut hash = Sha256::new();
        hash.update(version.note_id.as_bytes());
        hash.update(content.as_bytes());
        let id = if ids.iter().all(|(old, new)| old == new) {
            version.id.clone()
        } else {
            // Retain forced duplicate snapshot identity across UUID remapping.
            format!("{}-{:x}", version.id, hash.finalize())
        };
        tx.execute("INSERT INTO versions(id,note_id,content,created) VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO NOTHING", params![id,version.note_id,content,version.created]).map_err(|e| e.to_string())?;
        VaultDb::link_content(&tx, &version.note_id, &content)?;
        report.versions_imported += 1;
    }
    for (key, value) in &index.meta {
        if key == "schema_version"
            || (db.is_v3()
                && super::revision_writer::reserved_metadata(key)
                && !key.starts_with("generated_summary:v1:"))
        {
            return Err("Export cannot replace schema metadata".into());
        }
        let value = if key == "notebook_layout" {
            let mut current = super::db_notebooks::Layout::load(&tx)?;
            let imported: super::db_notebooks::Layout =
                serde_json::from_str(value).map_err(|e| e.to_string())?;
            for path in &imported.notebooks {
                super::db_notebooks::relative("", path)?;
            }
            for (note, path) in &imported.notes {
                imported.require(path)?;
                if !index.notes.iter().any(|n| &n.id == note) {
                    return Err("Invalid notebook note identity".into());
                }
            }
            current.notebooks.extend(imported.notebooks);
            for note in &index.notes {
                current.notes.remove(&note.id);
            }
            current.notes.extend(imported.notes);
            serde_json::to_string(&current).map_err(|e| e.to_string())?
        } else {
            value.clone()
        };
        tx.execute("INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key,value]).map_err(|e| e.to_string())?;
    }
    for (note, session, created) in &index.sessions {
        tx.execute("INSERT INTO session_links(note_id,session_id,created) VALUES(?1,?2,?3) ON CONFLICT(note_id) DO UPDATE SET session_id=excluded.session_id,created=excluded.created", params![note,session,created]).map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(Some(report))
}

#[cfg(test)]
mod tests {
    use super::super::db_import::import_md_vault;
    use super::*;

    fn note(root: &Path, name: &str, id: &str, body: &str) {
        std::fs::write(root.join(name), format!("---\nid: {id}\ntitle: Same title\ntags: [mixed]\npinned: true\ncreated: 2026-01-01T00:00:00Z\nmodified: 2026-02-01T00:00:00Z\n---\n{body}")).unwrap();
    }
    type BlobRow = (String, String, String, i64, String, Vec<u8>, String);
    fn blob_rows(db: &VaultDb) -> Vec<BlobRow> {
        db.conn.prepare("SELECT id,filename,mime_type,size,sha256,data,created FROM attachments ORDER BY id").unwrap()
            .query_map([], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).unwrap()
            .collect::<Result<_, _>>().unwrap()
    }
    fn links(db: &VaultDb) -> Vec<(String, String)> {
        db.conn
            .prepare(
                "SELECT note_id,attachment_id FROM note_attachments ORDER BY note_id,attachment_id",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }
    fn directory(root: &Path) -> BTreeMap<String, Vec<u8>> {
        walkdir::WalkDir::new(root)
            .into_iter()
            .map(Result::unwrap)
            .filter(|e| e.file_type().is_file())
            .map(|e| {
                (
                    e.path()
                        .strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    std::fs::read(e.path()).unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn mixed_vault_export_reimport_is_lossless_shared_and_idempotent() {
        let source = tempfile::tempdir().unwrap();
        let assets = source.path().join(".ryuknotes/attachments");
        std::fs::create_dir_all(&assets).unwrap();
        let payloads: [(&str, &[u8]); 5] = [
            ("image.png", b"\x89PNG\r\n\x1a\nimage"),
            ("report (雪).pdf", b"%PDF-1.7\nreport bytes\x00"),
            ("data.bin", &[0, 255, 1, 2, 13, 10]),
            ("history.txt", b"only in history"),
            (
                "vector.svg",
                b"<svg xmlns='http://www.w3.org/2000/svg'></svg>",
            ),
        ];
        for (name, bytes) in payloads {
            std::fs::write(assets.join(name), bytes).unwrap();
        }
        std::fs::write(assets.join("shared-copy.bin"), payloads[2].1).unwrap();
        let body = "雪\n![image](.ryuknotes/attachments/image.png)\n[report](<.ryuknotes/attachments/report%20%28%E9%9B%AA%29.pdf#page=2>)\n[data](.ryuknotes/attachments/data.bin)\n![vector](.ryuknotes/attachments/vector.svg)\n";
        note(source.path(), "A.md", "a", body);
        note(
            source.path(),
            "B.md",
            "b",
            "[shared](.ryuknotes/attachments/shared-copy.bin)\n",
        );
        let history = source.path().join(".ryuknotes/history/a");
        std::fs::create_dir_all(&history).unwrap();
        std::fs::write(
            history.join("old.md"),
            "[old](.ryuknotes/attachments/history.txt)",
        )
        .unwrap();
        let source_before = directory(source.path());
        let db = VaultDb::open_in_memory().unwrap();
        for _ in 0..2 {
            let report = import_md_vault(source.path(), &db).unwrap();
            assert!(report.warnings.is_empty(), "{:?}", report.warnings);
            assert_eq!(report.notes_imported, 2);
        }
        assert_eq!(blob_rows(&db).len(), 5);
        let shared = blob_rows(&db)
            .into_iter()
            .find(|r| r.5 == payloads[2].1)
            .unwrap();
        assert_eq!(
            db.get_attachment_data(&shared.0)
                .unwrap()
                .unwrap()
                .0
                .note_ids,
            vec!["a", "b"]
        );
        let original = db.get_note_content("a").unwrap().unwrap();
        db.upsert_note(&NewNote {
            id: "a".into(),
            title: "Same title".into(),
            content: format!("\n\n{original}\r\n\r\n"),
            pinned: true,
            tags: vec!["mixed".into()],
            created: "2026-01-01T00:00:00+00:00".into(),
            modified: "2026-02-01T00:00:00+00:00".into(),
        })
        .unwrap();
        // Include trash, forced duplicate snapshots, logical notebooks, session
        // linkage and an unlinked retained BLOB in the same round-trip proof.
        db.force_version("a", 0).unwrap();
        db.force_version("a", 0).unwrap();
        super::super::db_notebooks::create(&db, "", None, "Research").unwrap();
        super::super::db_notebooks::move_note(&db, "", "a", "Research").unwrap();
        db.set_session_link("a", "20261015_120000_abcdef").unwrap();
        db.conn
            .execute("INSERT INTO tags(name) VALUES('unused-tag')", [])
            .unwrap();
        db.set_note_trashed("b", true).unwrap();
        db.add_attachment(
            "a",
            "orphan.dat",
            "application/octet-stream",
            b"retained",
            "2026-01-01",
        )
        .unwrap();
        db.conn.execute("DELETE FROM note_attachments WHERE attachment_id=(SELECT id FROM attachments WHERE filename='orphan.dat')", []).unwrap();
        let exported = tempfile::tempdir().unwrap();
        std::fs::write(exported.path().join("unrelated.md"), "leave me").unwrap();
        assert_eq!(export_md_vault(exported.path(), &db).unwrap(), 2);
        let first = directory(exported.path());
        export_md_vault(exported.path(), &db).unwrap();
        assert_eq!(
            directory(exported.path()),
            first,
            "re-export must not duplicate or alter stable files"
        );
        // The ordinary external viewer's single-note copy path can import
        // the portable links too, including extension-dependent image MIME.
        let index: Index =
            serde_json::from_slice(&std::fs::read(exported.path().join(INDEX)).unwrap()).unwrap();
        let viewer_note = index.notes.iter().find(|n| n.id == "a").unwrap();
        let viewer_path = exported.path().join(&viewer_note.file);
        let viewer_raw = std::fs::read_to_string(&viewer_path).unwrap();
        let prepared = super::super::operations::prepare_note_import(
            viewer_path.to_str().unwrap(),
            exported.path(),
            &viewer_raw,
            super::super::db_attachments::references(&viewer_raw),
            |_| Ok(()),
        )
        .unwrap();
        let viewer_copy = VaultDb::open_in_memory().unwrap();
        let viewer_id =
            super::super::db_import::import_single(&viewer_copy, prepared, None).unwrap();
        let viewer_body = viewer_copy.get_note_content(&viewer_id).unwrap().unwrap();
        assert!(
            viewer_body.contains(".rn-attachments/")
                && !viewer_body.contains(".ryuknotes/attachments/")
        );
        let viewer_blobs = blob_rows(&viewer_copy);
        assert_eq!(viewer_blobs.len(), 4);
        assert!(viewer_blobs.iter().any(|row| row.2 == "image/svg+xml"));
        for payload in [payloads[0].1, payloads[1].1, payloads[2].1, payloads[4].1] {
            assert!(viewer_blobs.iter().any(|row| row.5 == payload));
        }
        let copy = VaultDb::open_in_memory().unwrap();
        for _ in 0..2 {
            import_md_vault(exported.path(), &copy).unwrap();
        }
        for id in ["a", "b"] {
            assert_eq!(
                db.get_note_full(id).unwrap(),
                copy.get_note_full(id).unwrap()
            );
            assert_eq!(
                db.list_versions(id).unwrap(),
                copy.list_versions(id).unwrap()
            );
        }
        assert_eq!(
            blob_rows(&db),
            blob_rows(&copy),
            "metadata and BLOB bytes must be exact"
        );
        assert_eq!(links(&db), links(&copy));
        assert!(copy
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM tags WHERE name='unused-tag')",
                [],
                |r| r.get::<_, bool>(0)
            )
            .unwrap());
        assert_eq!(
            copy.get_session_link("a").unwrap(),
            db.get_session_link("a").unwrap()
        );
        assert_eq!(
            super::super::db_notebooks::Layout::load(&db.conn)
                .unwrap()
                .notes,
            super::super::db_notebooks::Layout::load(&copy.conn)
                .unwrap()
                .notes
        );
        // Merging into an existing vault deduplicates shared bytes with a
        // different UUID, remaps destinations and remains idempotent.
        let merged = VaultDb::open_in_memory().unwrap();
        merged
            .upsert_note(&NewNote {
                id: "existing".into(),
                title: "Existing".into(),
                content: "".into(),
                pinned: false,
                tags: vec![],
                created: "now".into(),
                modified: "now".into(),
            })
            .unwrap();
        let preexisting = merged
            .add_attachment(
                "existing",
                "existing.bin",
                "application/octet-stream",
                payloads[2].1,
                "now",
            )
            .unwrap();
        for _ in 0..2 {
            import_md_vault(exported.path(), &merged).unwrap();
        }
        assert_eq!(blob_rows(&merged).len(), blob_rows(&db).len());
        assert!(merged
            .get_note_content("a")
            .unwrap()
            .unwrap()
            .contains(&preexisting.id));
        assert_eq!(
            merged
                .get_attachment_data(&preexisting.id)
                .unwrap()
                .unwrap()
                .0
                .note_ids,
            vec!["a", "b", "existing"]
        );
        assert_eq!(
            directory(source.path()),
            source_before,
            "imports must leave sources untouched"
        );
    }

    #[test]
    fn damaged_or_oversize_export_import_rolls_back_everything() {
        let db = VaultDb::open_in_memory().unwrap();
        db.upsert_note(&NewNote {
            id: "a".into(),
            title: "A".into(),
            content: "".into(),
            pinned: false,
            tags: vec![],
            created: "now".into(),
            modified: "now".into(),
        })
        .unwrap();
        let asset = db
            .add_attachment(
                "a",
                "bytes.bin",
                "application/octet-stream",
                b"unchanged bytes",
                "now",
            )
            .unwrap();
        let out = tempfile::tempdir().unwrap();
        export_md_vault(out.path(), &db).unwrap();
        let mut copy = VaultDb::open_in_memory().unwrap();
        copy.max_attachment_bytes = 1;
        assert!(import_md_vault(out.path(), &copy).is_err());
        assert!(copy.list_notes(true).unwrap().is_empty());
        assert!(blob_rows(&copy).is_empty());
        copy.max_attachment_bytes = 1024;
        std::fs::write(
            out.path().join(asset_path(&asset.id, &asset.filename)),
            b"damaged",
        )
        .unwrap();
        assert!(import_md_vault(out.path(), &copy).is_err());
        assert!(copy.list_notes(true).unwrap().is_empty());
        assert!(blob_rows(&copy).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn export_and_import_reject_symlinked_assets() {
        let out = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(out.path().join(".ryuknotes")).unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), out.path().join(".ryuknotes/attachments"))
            .unwrap();
        let db = VaultDb::open_in_memory().unwrap();
        db.upsert_note(&NewNote {
            id: "a".into(),
            title: "A".into(),
            content: "".into(),
            pinned: false,
            tags: vec![],
            created: "now".into(),
            modified: "now".into(),
        })
        .unwrap();
        db.add_attachment("a", "file", "text/plain", b"keep", "now")
            .unwrap();
        assert!(export_md_vault(out.path(), &db).is_err());
        assert!(directory(elsewhere.path()).is_empty());
        let clean = tempfile::tempdir().unwrap();
        export_md_vault(clean.path(), &db).unwrap();
        let asset = blob_rows(&db).remove(0);
        let destination = clean.path().join(asset_path(&asset.0, &asset.1));
        std::fs::remove_file(&destination).unwrap();
        let external = elsewhere.path().join("keep.bin");
        std::fs::write(&external, b"keep").unwrap();
        std::os::unix::fs::symlink(&external, &destination).unwrap();
        let copy = VaultDb::open_in_memory().unwrap();
        assert!(import_md_vault(clean.path(), &copy).is_err());
        assert!(copy.list_notes(true).unwrap().is_empty());
        assert_eq!(std::fs::read(external).unwrap(), b"keep");
    }
}
