//! Logical notebook membership. Note identities and their related rows never move.
use super::db::VaultDb;
use crate::types::NotebookEntry;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct Layout {
    pub notebooks: BTreeSet<String>,
    pub notes: BTreeMap<String, String>,
}

impl Layout {
    pub fn load(conn: &Connection) -> Result<Self, String> {
        let value: Option<String> = conn
            .query_row(
                "SELECT value FROM meta WHERE key='notebook_layout'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        match value {
            Some(value) => serde_json::from_str(&value).map_err(|e| e.to_string()),
            None => Ok(Self::default()),
        }
    }
    pub fn save(&self, conn: &Connection) -> Result<(), String> {
        conn.execute("INSERT INTO meta(key,value) VALUES('notebook_layout',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![serde_json::to_string(self).map_err(|e| e.to_string())?]).map_err(|e| e.to_string())?;
        Ok(())
    }
    pub fn require(&self, relative: &str) -> Result<(), String> {
        if relative.is_empty() || self.notebooks.contains(relative) {
            Ok(())
        } else {
            Err("Destination notebook does not exist".into())
        }
    }
    pub fn assign(&mut self, id: &str, relative: &str) -> Result<(), String> {
        self.require(relative)?;
        if relative.is_empty() {
            self.notes.remove(id);
        } else {
            self.notes.insert(id.into(), relative.into());
        }
        Ok(())
    }
}

pub(crate) fn relative(vault: &str, path: &str) -> Result<String, String> {
    let vault = vault.replace('\\', "/");
    let path = path.replace('\\', "/");
    let path = if path == vault {
        ""
    } else if vault.is_empty() {
        &path
    } else {
        path.strip_prefix(&format!("{vault}/")).unwrap_or(&path)
    };
    if path.is_empty() {
        return Ok(String::new());
    }
    if path.split('/').any(|part| {
        part.is_empty() || part.starts_with('.') || part.contains(':') || part.contains('\0')
    }) {
        return Err("Invalid notebook path".into());
    }
    Ok(path.into())
}

pub fn create(
    db: &VaultDb,
    vault: &str,
    parent: Option<&str>,
    name: &str,
) -> Result<NotebookEntry, String> {
    let tx =
        rusqlite::Transaction::new_unchecked(&db.conn, rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
    let mut layout = Layout::load(&tx)?;
    let parent = relative(vault, parent.unwrap_or(""))?;
    layout.require(&parent)?;
    let name = relative("", name)?;
    if name.is_empty() {
        return Err("Notebook name is empty".into());
    }
    let target = if parent.is_empty() {
        name
    } else {
        format!("{parent}/{name}")
    };
    if layout.notebooks.contains(&target) {
        return Err("Notebook already exists".into());
    }
    let mut ancestor = String::new();
    for part in target.split('/') {
        if !ancestor.is_empty() {
            ancestor.push('/');
        }
        ancestor.push_str(part);
        layout.notebooks.insert(ancestor.clone());
    }
    layout.save(&tx)?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(NotebookEntry {
        name: target.rsplit('/').next().unwrap_or("").into(),
        path: format!("{vault}/{target}"),
        relative_path: target,
        children: vec![],
        note_count: 0,
    })
}

pub fn list(db: &VaultDb, vault: &str) -> Result<Vec<NotebookEntry>, String> {
    let layout = Layout::load(&db.conn)?;
    let notes = db.list_notes(false)?;
    fn children(
        layout: &Layout,
        notes: &[super::db::NoteSummary],
        vault: &str,
        parent: &str,
    ) -> Vec<NotebookEntry> {
        layout
            .notebooks
            .iter()
            .filter(|path| path.rsplit_once('/').map(|(p, _)| p).unwrap_or("") == parent)
            .map(|path| NotebookEntry {
                name: path.rsplit('/').next().unwrap_or("").into(),
                path: format!("{vault}/{path}"),
                relative_path: path.clone(),
                note_count: notes
                    .iter()
                    .filter(|note| layout.notes.get(&note.id) == Some(path))
                    .count(),
                children: children(layout, notes, vault, path),
            })
            .collect()
    }
    Ok(children(&layout, &notes, vault, ""))
}

pub fn move_note(db: &VaultDb, vault: &str, id: &str, destination: &str) -> Result<String, String> {
    let tx =
        rusqlite::Transaction::new_unchecked(&db.conn, rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
    let exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM notes WHERE id=?1)",
            params![id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if !exists {
        return Err("Note does not exist".into());
    }
    let mut layout = Layout::load(&tx)?;
    layout.assign(id, &relative(vault, destination)?)?;
    layout.save(&tx)?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(id.into())
}

pub fn repath(
    db: &VaultDb,
    vault: &str,
    source: &str,
    destination: &str,
) -> Result<String, String> {
    let tx =
        rusqlite::Transaction::new_unchecked(&db.conn, rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
    let mut layout = Layout::load(&tx)?;
    let source = relative(vault, source)?;
    let destination = relative(vault, destination)?;
    if source.is_empty() || destination.is_empty() {
        return Err("Cannot move the vault root".into());
    }
    layout.require(&source)?;
    if source == destination {
        return Ok(format!("{vault}/{source}"));
    }
    if destination.starts_with(&format!("{source}/")) {
        return Err("Cannot move a notebook inside itself".into());
    }
    if layout.notebooks.contains(&destination) {
        return Err("Notebook already exists".into());
    }
    layout.require(destination.rsplit_once('/').map(|(p, _)| p).unwrap_or(""))?;
    let rewrite = |path: &str| {
        if path == source {
            destination.clone()
        } else if let Some(suffix) = path.strip_prefix(&format!("{source}/")) {
            format!("{destination}/{suffix}")
        } else {
            path.to_string()
        }
    };
    layout.notebooks = layout.notebooks.iter().map(|path| rewrite(path)).collect();
    for path in layout.notes.values_mut() {
        *path = rewrite(path);
    }
    layout.save(&tx)?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(format!("{vault}/{destination}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_notebooks_repath_descendants_and_reject_invalid_moves() {
        let db = VaultDb::open_in_memory().unwrap();
        create(&db, "/v.rn", None, "A/Child").unwrap();
        create(&db, "/v.rn", None, "B").unwrap();
        let mut layout = Layout::load(&db.conn).unwrap();
        layout.assign("stable-note", "A/Child").unwrap();
        layout.save(&db.conn).unwrap();
        repath(&db, "/v.rn", "A", "B/A").unwrap();
        let layout = Layout::load(&db.conn).unwrap();
        assert_eq!(layout.notes["stable-note"], "B/A/Child");
        assert!(layout.notebooks.contains("B/A/Child"));
        assert!(!layout.notebooks.contains("A"));
        assert!(repath(&db, "/v.rn", "B", "B/A/Child/B").is_err());
        assert!(create(&db, "/v.rn", None, "../escape").is_err());
        assert!(create(&db, "/v.rn", None, "/absolute").is_err());
        assert!(create(&db, "/v.rn", None, "B").is_err());
    }
}
