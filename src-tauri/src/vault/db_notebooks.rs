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
    let tx = db.begin_write("gui", None, &serde_json::json!([vault, parent, name]))?;
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
    if tx.is_replay() {
        // A retried create replays the same synthesized entry the original
        // call returned; create receipts derive from inputs, not stored state.
        return Ok(NotebookEntry {
            name: target.rsplit('/').next().unwrap_or("").into(),
            path: format!("{vault}/{target}"),
            relative_path: target,
            children: vec![],
            note_count: 0,
        });
    }
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
    let tx = db.begin_write("gui", None, &serde_json::json!([vault, id, destination]))?;
    if tx.is_replay() {
        return Ok(id.into());
    }
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
    let tx = db.begin_write(
        "gui",
        None,
        &serde_json::json!([vault, source, destination]),
    )?;
    if tx.is_replay() {
        return Ok(format!("{vault}/{}", relative(vault, destination)?));
    }
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

/// Individual restore must not leave a live note assigned to a notebook that
/// is still in Trash. Group restore restores its own subtree explicitly.
pub(crate) fn restore_orphan_to_root(conn: &Connection, id: &str) -> Result<(), String> {
    let mut layout = Layout::load(conn)?;
    if layout.notes.get(id).is_some_and(|path| !layout.notebooks.contains(path)) {
        layout.notes.remove(id);
        layout.save(conn)?;
    }
    Ok(())
}

// Logical trash records survive export/backup without a schema change. Only
// notes newly trashed by this operation belong to the group; older trash does
// not become live again when the notebook is restored.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct NotebookTrash {
    pub path: String,
    pub name: String,
    pub modified: String,
    pub notebooks: BTreeSet<String>,
    pub notes: BTreeMap<String, String>,
    pub tokens: BTreeMap<String, String>,
    #[serde(default)]
    pub revisions: BTreeMap<String, String>,
}
const TRASH_PREFIX: &str = "notebook-trash:";
fn trash_key(id: &str) -> Result<String, String> {
    let suffix = id.strip_prefix(TRASH_PREFIX).ok_or("Invalid notebook trash identity")?;
    uuid::Uuid::parse_str(suffix).map_err(|e| e.to_string())?;
    Ok(format!("notebook_trash:v1:{suffix}"))
}
pub fn trash(db: &VaultDb, vault: &str, source: &str, operation: &str) -> Result<(), String> {
    let source = relative(vault, source)?;
    if source.is_empty() { return Err("Cannot trash the vault root".into()); }
    uuid::Uuid::parse_str(operation).map_err(|e| e.to_string())?;
    let tx = db.begin_write("gui", Some(operation), &serde_json::json!(["trash-notebook",vault,source]))?;
    if tx.is_replay() { return tx.commit(); }
    let mut layout = Layout::load(&tx)?;
    layout.require(&source)?;
    let notebooks: BTreeSet<String> = layout.notebooks.iter().filter(|p| *p == &source || p.starts_with(&format!("{source}/"))).cloned().collect();
    let mut record = NotebookTrash { path:format!("{TRASH_PREFIX}{operation}"),name:source.clone(),modified:chrono::Utc::now().to_rfc3339(),notebooks,notes:BTreeMap::new(),tokens:BTreeMap::new(),revisions:BTreeMap::new() };
    for (id,path) in &layout.notes {
        if record.notebooks.contains(path) && tx.execute("UPDATE notes SET trashed=1 WHERE id=?1 AND trashed=0",[id]).map_err(|e|e.to_string())? != 0 {
            record.notes.insert(id.clone(),path.clone());
        }
    }
    layout.notebooks.retain(|p| !record.notebooks.contains(p));
    // The group record preserves each trashed note's assignment for restore;
    // keeping it in the live layout would dangle against the removed notebooks.
    for id in record.notes.keys() { layout.notes.remove(id); }
    layout.save(&tx)?;
    for id in record.notes.keys() {
        let receipt = tx.receipt(id)?;
        if let Some(revision) = receipt["accepted"]["revision"]["revisionId"].as_str() {
            record.revisions.insert(id.clone(),revision.into());
        }
        record.tokens.insert(id.clone(),receipt["accepted"]["token"].as_str().ok_or("Missing trash receipt")?.into());
    }
    tx.execute("INSERT INTO meta(key,value) VALUES(?1,?2)",params![trash_key(&record.path)?,serde_json::to_string(&record).map_err(|e|e.to_string())?]).map_err(|e|e.to_string())?;
    tx.commit()
}
pub fn trash_groups(db: &VaultDb) -> Result<Vec<NotebookTrash>, String> {
    let mut statement = db.conn.prepare("SELECT value FROM meta WHERE substr(key,1,18)='notebook_trash:v1:' ORDER BY key").map_err(|e|e.to_string())?;
    let rows = statement.query_map([],|r|r.get::<_,String>(0)).map_err(|e|e.to_string())?;
    rows.map(|r|serde_json::from_str(&r.map_err(|e|e.to_string())?).map_err(|e|e.to_string())).collect()
}
pub fn resolve_trash(db: &VaultDb, vault: &str, id: &str, remove: bool, operation: &str) -> Result<String, String> {
    let key = trash_key(id)?;
    let tx = db.begin_write("gui",Some(operation),&serde_json::json!(["resolve-notebook",vault,id,remove]))?;
    if tx.is_replay() { tx.commit()?; return Ok(vault.into()); }
    let raw: String = tx.query_row("SELECT value FROM meta WHERE key=?1",[&key],|r|r.get(0)).map_err(|e|e.to_string())?;
    let record: NotebookTrash = serde_json::from_str(&raw).map_err(|e|e.to_string())?;
    let mut layout = Layout::load(&tx)?;
    if !remove {
        if record.notebooks.iter().any(|p|layout.notebooks.contains(p)) {
            return Err("A notebook already occupies this location. Rename it before restoring.".into());
        }
        for path in &record.notebooks {
            let mut parent = Some(path.as_str());
            while let Some(p) = parent {
                layout.notebooks.insert(p.into());
                parent = p.rsplit_once('/').map(|(a,_)|a);
            }
        }
    }
    for (note,path) in &record.notes {
        let state: Option<bool> = tx.query_row("SELECT trashed FROM notes WHERE id=?1",[note],|r|r.get(0)).optional().map_err(|e|e.to_string())?;
        if state != Some(true) { continue; } // Individually restored/deleted notes stay that way.
        let current = db.accepted_note(note).map_err(|e| e.to_string())?.ok_or("Trashed note disappeared")?;
        // A backup/full-copy restore adds unchanged restore-baseline children.
        // Permit only that chain plus non-authoritative bookkeeping revisions:
        // notebook repath/move rewrites membership metadata without touching the
        // note body, so it must not strand an otherwise-unchanged trash group.
        // Arbitrary A→B→A edits (update/merge/import) or delete/recreate (new
        // incarnation, no predecessor link) never acquire authority merely by
        // looking like the old trashed state.
        let unchanged = if db.is_v3() {
            if let Some(expected) = record.revisions.get(note) {
                tx.query_row("WITH RECURSIVE lineage(id) AS (SELECT revision_id FROM note_revision_heads WHERE note_id=?1 UNION ALL SELECT p.parent_id FROM lineage l JOIN revisions r ON r.revision_id=l.id JOIN revision_parents p ON p.child_id=l.id AND p.role='predecessor' WHERE r.kind IN ('metadata','restore_baseline')) SELECT EXISTS(SELECT 1 FROM lineage WHERE id=?2)",params![note,expected],|r|r.get::<_,bool>(0)).map_err(|e|e.to_string())?
            } else { false }
        } else { current["accepted"]["token"].as_str() == record.tokens.get(note).map(String::as_str) };
        if !unchanged {
            return Err("A contained note changed since notebook deletion. Resolve it individually before restoring or deleting the group.".into());
        }
        if remove {
            tx.execute("DELETE FROM notes_fts WHERE rowid=(SELECT rowid FROM notes WHERE id=?1)",[note]).map_err(|e|e.to_string())?;
            tx.execute("DELETE FROM notes WHERE id=?1",[note]).map_err(|e|e.to_string())?;
            layout.notes.remove(note);
        } else {
            tx.execute("UPDATE notes SET trashed=0 WHERE id=?1",[note]).map_err(|e|e.to_string())?;
            layout.notes.insert(note.clone(),path.clone());
        }
    }
    layout.save(&tx)?;
    tx.execute("DELETE FROM meta WHERE key=?1",[key]).map_err(|e|e.to_string())?;
    tx.commit()?;
    Ok(format!("{vault}/{}",record.name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_note(db: &VaultDb, id: &str) {
        use crate::vault::{db::NewNote,writer::{NoteWrite,WritePolicy}};
        let note = NewNote { id:id.into(),title:id.into(),content:"exact 😀 body\r\n".into(),tags:vec!["tag".into()],pinned:true,created:"created".into(),modified:"modified".into() };
        db.write_authoritative(&NoteWrite::from_note(db,&note,WritePolicy::Create),0).unwrap();
    }
    #[test]
    fn recursive_trash_restore_is_atomic_and_preserves_older_trash_and_racing_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notebooks.rn");
        let (db,_) = VaultDb::open_with_report(&path,&dir.path().join("ledger")).unwrap();
        let vault = path.to_str().unwrap();
        create(&db,vault,None,"Work/Child").unwrap();
        for id in ["live","older"] { fixture_note(&db,id); move_note(&db,vault,id,"Work/Child").unwrap(); }
        db.set_note_trashed("older",true).unwrap();
        let operation = uuid::Uuid::new_v4().to_string();
        trash(&db,vault,"Work",&operation).unwrap();
        trash(&db,vault,"Work",&operation).unwrap(); // Same intent, one group/revision.
        let groups = trash_groups(&db).unwrap();
        assert_eq!(groups.len(),1);
        assert_eq!(groups[0].notes.len(),1);
        assert!(list(&db,vault).unwrap().is_empty());
        assert_eq!(db.get_note_content("live").unwrap().unwrap(),"exact 😀 body\r\n");
        create(&db,vault,None,"Work").unwrap();
        assert!(resolve_trash(&db,vault,&groups[0].path,false,&uuid::Uuid::new_v4().to_string()).is_err());
        repath(&db,vault,"Work","Other").unwrap();
        resolve_trash(&db,vault,&groups[0].path,false,&uuid::Uuid::new_v4().to_string()).unwrap();
        assert_eq!(db.accepted_note("live").unwrap().unwrap()["trashed"],false);
        assert_eq!(db.accepted_note("older").unwrap().unwrap()["trashed"],true);
        assert!(Layout::load(&db.conn).unwrap().notebooks.contains("Work/Child"));
        assert!(trash_groups(&db).unwrap().is_empty());
        trash(&db,vault,"Work",&uuid::Uuid::new_v4().to_string()).unwrap();
        let group = trash_groups(&db).unwrap().remove(0);
        db.set_note_trashed("live",false).unwrap();
        db.set_note_trashed("live",true).unwrap();
        assert!(resolve_trash(&db,vault,&group.path,false,&uuid::Uuid::new_v4().to_string()).is_err());
        assert_eq!(trash_groups(&db).unwrap().len(),1);
    }
    #[test]
    fn failed_notebook_trash_rolls_back_notes_layout_and_group() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("failure.rn");
        let (db,_) = VaultDb::open_with_report(&path,&dir.path().join("ledger")).unwrap();
        let vault = path.to_str().unwrap();
        create(&db,vault,None,"Work").unwrap();
        fixture_note(&db,"live");move_note(&db,vault,"live","Work").unwrap();
        db.conn.execute_batch("CREATE TEMP TRIGGER refuse_trash BEFORE UPDATE OF trashed ON notes BEGIN SELECT RAISE(ABORT,'fixture disk full'); END;").unwrap();
        assert!(trash(&db,vault,"Work",&uuid::Uuid::new_v4().to_string()).is_err());
        assert_eq!(db.accepted_note("live").unwrap().unwrap()["trashed"],false);
        assert_eq!(list(&db,vault).unwrap().len(),1);
        assert!(trash_groups(&db).unwrap().is_empty());
    }
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
