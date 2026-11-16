//! Vault metadata and icon bytes share the guarded transaction and portable DB.
use super::db::VaultDb;
use crate::types::{TagStyle, TagStyleChange};
use std::collections::HashMap;

pub fn load<T: serde::de::DeserializeOwned + Default>(
    db: &VaultDb,
    key: &str,
) -> Result<T, String> {
    db.get_state(key)?
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| e.to_string())
        .map(|v| v.unwrap_or_default())
}
fn save<T: serde::Serialize>(
    tx: &rusqlite::Connection,
    key: &str,
    value: &T,
) -> Result<(), String> {
    tx.execute("INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        rusqlite::params![key, serde_json::to_string(value).map_err(|e| e.to_string())?]).map_err(|e| e.to_string())?;
    Ok(())
}
pub fn notebook(
    db: &VaultDb,
    path: &str,
    icon: Option<String>,
    bytes: Option<(&str, &[u8])>,
) -> Result<String, String> {
    let tx = db.begin_write(
        "gui",
        None,
        &serde_json::json!(["notebook-icon", path, icon]),
    )?;
    let mut icons: HashMap<String, String> = load(db, "notebook_icons")?;
    let reference = if let Some((name, data)) = bytes {
        Some(store_icon(db, &tx, name, data)?)
    } else {
        icon
    };
    let path = super::operations::normalize_notebook_icon_key(path);
    if let Some(icon) = &reference {
        icons.insert(path, icon.clone());
    } else {
        icons.remove(&path);
    }
    save(&tx, "notebook_icons", &icons)?;
    tx.commit()?;
    Ok(reference.unwrap_or_default())
}
pub fn tag(
    db: &VaultDb,
    tag: &str,
    change: TagStyleChange,
    bytes: Option<(&str, &[u8])>,
) -> Result<Option<TagStyle>, String> {
    let tx = db.begin_write("gui", None, &serde_json::json!(["tag-style", tag, change]))?;
    let mut styles: HashMap<String, TagStyle> = load(db, "tag_styles")?;
    let change = if let Some((name, data)) = bytes {
        TagStyleChange::Icon(Some(store_icon(db, &tx, name, data)?))
    } else {
        change
    };
    let result = super::operations::patch_tag_style_map(&mut styles, tag, change)?;
    save(&tx, "tag_styles", &styles)?;
    tx.commit()?;
    Ok(result)
}
fn store_icon(
    db: &VaultDb,
    tx: &rusqlite::Connection,
    name: &str,
    data: &[u8],
) -> Result<String, String> {
    let mime = crate::commands::image_mime_type(name, data)?;
    Ok(format!(
        ".rn-attachments/{}",
        db.store_blob_on(tx, name, mime, data, &chrono::Utc::now().to_rfc3339())?
    ))
}

pub(crate) fn import_folder_metadata(
    db: &VaultDb,
    root: &std::path::Path,
    dir: &std::path::Path,
    identities: &HashMap<String, String>,
) -> Result<(), String> {
    fn read<T: serde::de::DeserializeOwned + Default>(
        db: &VaultDb,
        root: &std::path::Path,
        path: &std::path::Path,
    ) -> Result<T, String> {
        if !path.exists() {
            return Ok(T::default());
        }
        let canonical = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
        if !canonical.starts_with(root) {
            return Err("Metadata escapes selected vault".into());
        }
        let bytes = super::db_attachments::read_capped(&canonical, db.max_attachment_bytes)?;
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())
    }
    fn icon(
        db: &VaultDb,
        root: &std::path::Path,
        value: &str,
    ) -> Result<Option<(String, Vec<u8>)>, String> {
        if value.starts_with("builtin:") {
            return Ok(None);
        }
        let path = std::fs::canonicalize(root.join(value)).map_err(|e| e.to_string())?;
        if !path.starts_with(root) {
            return Err("Icon escapes selected vault".into());
        }
        let bytes = super::db_attachments::read_capped(&path, db.max_attachment_bytes)?;
        Ok(Some((
            path.file_name()
                .ok_or("Missing icon name")?
                .to_string_lossy()
                .into_owned(),
            bytes,
        )))
    }
    let icons: HashMap<String, String> = read(db, root, &dir.join("notebook_icons.json"))?;
    for (path, reference) in icons {
        let bytes = icon(db, root, &reference)?;
        notebook(
            db,
            &path,
            Some(reference),
            bytes
                .as_ref()
                .map(|(name, data)| (name.as_str(), data.as_slice())),
        )?;
    }
    let styles: HashMap<String, TagStyle> = read(db, root, &dir.join("tag_styles.json"))?;
    for (name, style) in styles {
        if let Some(reference) = style.icon {
            let bytes = icon(db, root, &reference)?;
            tag(
                db,
                &name,
                TagStyleChange::Icon(Some(reference)),
                bytes
                    .as_ref()
                    .map(|(name, data)| (name.as_str(), data.as_slice())),
            )?;
        }
        if style.color.is_some() {
            tag(db, &name, TagStyleChange::Color(style.color), None)?;
        }
    }
    let quick: Vec<String> = read(db, root, &dir.join("quick_access.json"))?;
    if !quick.is_empty() {
        let mut selected: Vec<String> = load(db, "quick_access")?;
        for source in quick {
            if let Some(id) = identities.get(&source.replace('\\', "/")) {
                if !selected.contains(id) {
                    selected.push(id.clone());
                }
            }
        }
        db.set_state("quick_access", &serde_json::json!(selected))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn icon_metadata_and_bytes_survive_retention_and_portable_round_trip() {
        let root = tempfile::tempdir().unwrap();
        let (db, _) =
            VaultDb::open_with_report(&root.path().join("fixture.rn"), &root.path().join("ledger"))
                .unwrap();
        let reference = notebook(
            &db,
            "Projects",
            None,
            Some(("icon.png", b"\x89PNG\r\n\x1a\nfixture")),
        )
        .unwrap();
        let styled = tag(
            &db,
            "Work",
            TagStyleChange::Color(Some("#ff0000".into())),
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(styled.color.as_deref(), Some("#ff0000"));
        let id = reference.strip_prefix(".rn-attachments/").unwrap();
        db.collect_revisions().unwrap();
        assert!(db.get_attachment_data(id).unwrap().is_some());
        let export = root.path().join("export");
        super::super::db_export::export_md_vault(&export, &db).unwrap();
        let destination = root.path().join("copy.rn");
        let copy_ledger = root.path().join("copy-ledger");
        super::super::portable_ancestry::restore_export(
            &export,
            &destination,
            &copy_ledger,
            super::super::portable_ancestry::RestoreMode::IndependentCopy,
            &uuid::Uuid::new_v4().to_string(),
        )
        .unwrap();
        let imported = VaultDb::open_with_report(&destination, &copy_ledger)
            .unwrap()
            .0;
        assert_eq!(
            load::<HashMap<String, String>>(&imported, "notebook_icons").unwrap()["Projects"],
            reference
        );
        assert!(imported.get_attachment_data(id).unwrap().is_some());
    }
}
