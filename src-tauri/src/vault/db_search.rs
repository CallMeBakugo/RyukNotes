//! Paged DB search with exact UTF-16 match locations. The persistent index is
//! contentless: hydrate only one bounded page into a private FTS scratch index
//! for SQLite's own query-aware highlights, rather than guessing query tokens.
use super::db::VaultDb;
use crate::types::{SearchMatch, SearchPage, SearchResult, SearchTargetField};
use rusqlite::{params, Connection};

fn plain(marked: &str, open: &str, close: &str) -> (String, Vec<SearchMatch>) {
    let mut text = String::new();
    let mut ranges = Vec::new();
    let mut rest = marked;
    while let Some(start) = rest.find(open) {
        text.push_str(&rest[..start]);
        rest = &rest[start + open.len()..];
        let Some(end) = rest.find(close) else { break };
        let from = text.len();
        text.push_str(&rest[..end]);
        ranges.push(SearchMatch { start: from, end: text.len() });
        rest = &rest[end + close.len()..];
    }
    text.push_str(rest);
    (text, ranges)
}
fn utf16(text: &str, matches: Vec<SearchMatch>) -> Vec<SearchMatch> {
    matches.into_iter().map(|m| SearchMatch {
        start: text[..m.start].encode_utf16().count(),
        end: text[..m.end].encode_utf16().count(),
    }).collect()
}
impl VaultDb {
    pub fn search_page(&self, query: &str, limit: usize, offset: usize) -> Result<SearchPage, String> {
        if query.trim().is_empty() { return Ok(SearchPage::default()); }
        let limit = limit.clamp(1, 100);
        let offset = i64::try_from(offset).map_err(|_| "Search offset is too large")?;
        let snapshot = self.conn.unchecked_transaction().map_err(|e| e.to_string())?;
        let total = snapshot.query_row("SELECT COUNT(*) FROM notes_fts f JOIN notes n ON n.rowid=f.rowid WHERE notes_fts MATCH ?1 AND n.trashed=0", [query], |r| r.get::<_, usize>(0)).map_err(|e| e.to_string())?;
        let rows = {
            let mut statement = snapshot.prepare("SELECT n.id,n.title,n.content,rank FROM notes_fts f JOIN notes n ON n.rowid=f.rowid WHERE notes_fts MATCH ?1 AND n.trashed=0 ORDER BY rank,n.id LIMIT ?2 OFFSET ?3").map_err(|e| e.to_string())?;
            let rows = statement.query_map(params![query,limit as i64,offset], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,f64>(3)?))).map_err(|e| e.to_string())?;
            rows.collect::<Result<Vec<_>,_>>().map_err(|e| e.to_string())?
        };
        snapshot.commit().map_err(|e| e.to_string())?;
        let scratch = Connection::open_in_memory().map_err(|e| e.to_string())?;
        scratch.execute_batch("CREATE VIRTUAL TABLE page USING fts5(title,content,tokenize='unicode61')").map_err(|e| e.to_string())?;
        // Random markers keep source Markdown/HTML literal, never interpreted as markup.
        let open = format!("{}[", uuid::Uuid::new_v4());
        let close = format!("]{}", uuid::Uuid::new_v4());
        let mut results = Vec::new();
        for (id,title,body,rank) in rows {
            scratch.execute("DELETE FROM page", []).map_err(|e| e.to_string())?;
            scratch.execute("INSERT INTO page(title,content) VALUES(?1,?2)", params![title,body]).map_err(|e| e.to_string())?;
            let (marked_title,marked_body): (String,String) = scratch.query_row("SELECT highlight(page,0,?2,?3),highlight(page,1,?2,?3) FROM page WHERE page MATCH ?1",params![query,open,close],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|e| e.to_string())?;
            let (_, title_ranges) = plain(&marked_title,&open,&close);
            let (_, body_ranges) = plain(&marked_body,&open,&close);
            let match_target = body_ranges.first().map(|m| crate::search::match_target(&body,SearchTargetField::Body,m.start,m.end))
                .or_else(|| title_ranges.first().map(|m| crate::search::match_target(&title,SearchTargetField::Title,m.start,m.end)));
            let start = body_ranges.first().map(|m| body[..m.start].char_indices().rev().nth(39).map_or(0,|(i,_)|i)).unwrap_or(0);
            let end = body[start..].char_indices().nth(240).map_or(body.len(),|(i,_)|start+i);
            let snippet = body[start..end].to_string();
            let matches = body_ranges.into_iter().filter_map(|m| {
                let from = m.start.max(start); let to = m.end.min(end);
                (from < to).then_some(SearchMatch { start: from-start,end:to-start })
            }).collect();
            results.push(SearchResult { path:id,title_matches:utf16(&title,title_ranges),title,snippet_matches:utf16(&snippet,matches),snippet,score:-rank as f32,match_target });
        }
        Ok(SearchPage { results,total })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::db::NewNote;
    #[test]
    fn pages_are_stable_and_highlights_target_exact_unicode_body() {
        let db = VaultDb::open_in_memory().unwrap();
        for n in 0..65 {
            db.upsert_note(&NewNote { id:format!("n{n:03}"),title:"Title".into(),content:format!("{}😀 café needle end", "padding ".repeat(60)),tags:vec![],pinned:false,created:"2026".into(),modified:"2026".into() }).unwrap();
        }
        let mut ids = std::collections::BTreeSet::new();
        for offset in [0,20,40,60] {
            let page = db.search_page("needle",20,offset).unwrap();
            assert_eq!(page.total,65);
            assert_eq!(page.results.len(),(65-offset).min(20));
            for row in page.results {
                assert!(ids.insert(row.path));
                assert!(row.snippet.contains("needle"));
                assert_eq!(row.snippet_matches.len(),1);
                let target = row.match_target.unwrap();
                assert_eq!(target.text,"needle");
                assert!(target.before.contains("😀 café"));
            }
        }
        assert_eq!(ids.len(),65);
        assert!(db.search_page("needle",20,100).unwrap().results.is_empty());
        assert!(db.search_page("\"",20,0).is_err());
    }
}
