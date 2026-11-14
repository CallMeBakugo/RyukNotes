//! Payload accounting is separate from allocated SQLite/WAL bytes. These
//! quantities overlap; do not add them together or call payloads 'vault size'.
use super::db::VaultDb;
use serde_json::{json, Value};
impl VaultDb {
    pub fn storage_diagnostics(&self) -> Result<Value, String> {
        let tx = self.conn.unchecked_transaction().map_err(|e|e.to_string())?;
        let mut result = json!({});
        for (name,sql) in [
            ("liveBodyBytes","SELECT COALESCE(SUM(length(CAST(content AS BLOB))),0) FROM notes WHERE trashed=0"),
            ("trashedBodyBytes","SELECT COALESCE(SUM(length(CAST(content AS BLOB))),0) FROM notes WHERE trashed=1"),
            ("attachmentBytes","SELECT COALESCE(SUM(length(data)),0) FROM attachments"),
            ("curatedBodyBytes","SELECT COALESCE(SUM(length(CAST(content AS BLOB))),0) FROM versions"),
        ] {
            let value: i64 = tx.query_row(sql,[],|r|r.get(0)).map_err(|e|e.to_string())?;
            result[name] = json!(value);
        }
        if self.is_v3() {
            let bytes: i64 = tx.query_row("SELECT COALESCE(SUM(length(body)+length(CAST(title AS BLOB))+length(CAST(tags_json AS BLOB))+length(CAST(extra_json AS BLOB))),0) FROM revision_states",[],|r|r.get(0)).map_err(|e|e.to_string())?;
            result["revisionPayloadBytes"] = json!(bytes);
            for (field,table) in [("headers","revisions"),("states","revision_states"),("pins","revision_pins"),("prunedStates","revision_pruning")] {
                let n: i64 = tx.query_row(&format!("SELECT COUNT(*) FROM {table}"),[],|r|r.get(0)).map_err(|e|e.to_string())?;
                result[field] = json!(n);
            }
        }
        let pages: u64 = tx.pragma_query_value(None,"page_count",|r|r.get(0)).map_err(|e|e.to_string())?;
        let size: u64 = tx.pragma_query_value(None,"page_size",|r|r.get(0)).map_err(|e|e.to_string())?;
        let free: u64 = tx.pragma_query_value(None,"freelist_count",|r|r.get(0)).map_err(|e|e.to_string())?;
        result["databasePagesBytes"] = json!(pages.saturating_mul(size));
        result["reusablePageBytes"] = json!(free.saturating_mul(size));
        tx.commit().map_err(|e|e.to_string())?;
        if let Some(path) = self.conn.path().filter(|p| !p.is_empty() && *p != ":memory:") {
            for (field,file) in [("databaseFileBytes",path.to_string()),("walFileBytes",format!("{path}-wal"))] {
                let bytes = match std::fs::metadata(file) {
                    Ok(meta) => meta.len(),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
                    Err(e) => return Err(e.to_string()),
                };
                result[field] = json!(bytes);
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::db::NewNote;
    #[test]
    fn body_accounting_uses_bytes_and_separates_trash_from_live() {
        let db = VaultDb::open_in_memory().unwrap();
        for id in ["live","trash"] {
            db.upsert_note(&NewNote { id:id.into(),title:"Title".into(),content:"😀é".into(),tags:vec![],pinned:false,created:"now".into(),modified:"now".into() }).unwrap();
        }
        db.set_note_trashed("trash",true).unwrap();
        let stats = db.storage_diagnostics().unwrap();
        assert_eq!(stats["liveBodyBytes"],6);
        assert_eq!(stats["trashedBodyBytes"],6);
        assert_eq!(stats["attachmentBytes"],0);
        assert!(stats["databasePagesBytes"].as_u64().unwrap()>12);
    }
}
