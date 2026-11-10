//! Shared attachment bytes, MIME classification and bulk Markdown transport.
use super::{db::VaultDb, operations::ExternalNoteReference};
use std::io::Read;
use std::path::Path;

pub(crate) fn mime_type(name: &str, data: &[u8]) -> &'static str {
    crate::commands::image_mime_type(name, data).unwrap_or_else(|_| {
        if data.starts_with(b"%PDF-") || name.to_ascii_lowercase().ends_with(".pdf") {
            "application/pdf"
        } else if name.to_ascii_lowercase().ends_with(".txt") {
            "text/plain"
        } else {
            "application/octet-stream"
        }
    })
}

pub(crate) fn read_capped(path: &Path, cap: i64) -> Result<Vec<u8>, String> {
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let before = file.metadata().map_err(|e| e.to_string())?;
    if !before.is_file() {
        return Err("Attachment is not a regular file".into());
    }
    let mut data = Vec::new();
    (&mut file)
        .take(cap as u64 + 1)
        .read_to_end(&mut data)
        .map_err(|e| e.to_string())?;
    if data.len() as i64 > cap {
        return Err(format!("Attachment exceeds the vault cap of {cap} bytes"));
    }
    let after = file.metadata().map_err(|e| e.to_string())?;
    if before.len() != data.len() as u64
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
    {
        return Err("Attachment changed while importing".into());
    }
    Ok(data)
}

fn unescape(value: &str, html: bool) -> String {
    if html {
        value
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&")
    } else {
        let mut output = String::new();
        let mut chars = value.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                if let Some(next) = chars.next() {
                    output.push(next);
                }
            } else {
                output.push(c);
            }
        }
        output
    }
}

/// Byte ranges only: preserve all surrounding Markdown and HTML verbatim.
/// Handle inline destinations, reference definitions and attachment HTML;
/// skip fenced/inline code, comments and literal HTML code blocks.
pub(crate) fn references(raw: &str) -> Vec<ExternalNoteReference> {
    scan_references(raw, true)
}

pub(crate) fn body_references(raw: &str) -> Vec<ExternalNoteReference> {
    scan_references(raw, false)
}

fn scan_references(raw: &str, skip_frontmatter: bool) -> Vec<ExternalNoteReference> {
    let html = regex::Regex::new(r#"(?is)^<(?:img|a|div)\b(?:"[^"]*"|'[^']*'|[^'">])*>"#).unwrap();
    let attr = regex::Regex::new(
        r#"(?i)(?:\s)(src|href|data-pdf-src)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))"#,
    )
    .unwrap();
    let literal = regex::Regex::new(r"(?is)^<(script|style|textarea|pre|code)\b").unwrap();
    let definition = regex::Regex::new(r"^ {0,3}\[([^\]\n]+)\]:[ \t]*").unwrap();
    let mut result = Vec::new();
    let mut offset = 0;
    let mut consumed_until: usize = 0;
    let mut used_labels = std::collections::BTreeSet::new();
    let mut definitions = std::collections::BTreeMap::new();
    let mut defined_labels = std::collections::BTreeSet::new();
    let mut fence: Option<(u8, usize)> = None;
    let mut inline = 0;
    let mut comment = false;
    let mut literal_end: Option<String> = None;
    let beginning = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let mut frontmatter = skip_frontmatter
        && (beginning.starts_with("---\n") || beginning.starts_with("---\r\n"))
        && beginning.lines().skip(1).any(|line| line == "---");
    for line in raw.split_inclusive('\n') {
        if frontmatter {
            if offset > 0 && line.trim_end_matches(['\r', '\n']) == "---" {
                frontmatter = false;
            }
            offset += line.len();
            continue;
        }
        let start = consumed_until.saturating_sub(offset).min(line.len());
        if start == line.len() {
            offset += line.len();
            continue;
        }
        let trim = line.trim_start_matches(' ');
        let indentation = line.len() - trim.len();
        let marker = trim.as_bytes().first().copied().unwrap_or(0);
        let run = trim.bytes().take_while(|b| *b == marker).count();
        if start == 0
            && !comment
            && literal_end.is_none()
            && inline == 0
            && indentation <= 3
            && matches!(marker, b'`' | b'~')
            && run >= 3
        {
            if let Some((existing, size)) = fence {
                if marker == existing && run >= size {
                    fence = None;
                }
            } else {
                fence = Some((marker, run));
            }
            offset += line.len();
            continue;
        }
        if fence.is_some() {
            offset += line.len();
            continue;
        }
        if start == 0 && trim.trim() == "$$" && !comment && literal_end.is_none() {
            let after = offset + line.len();
            let mut end = after;
            for candidate in raw[after..].split_inclusive('\n') {
                end += candidate.len();
                if candidate.trim() == "$$" {
                    consumed_until = end;
                    break;
                }
            }
            if consumed_until > after {
                offset += line.len();
                continue;
            }
        }
        let bytes = line.as_bytes();
        let mut i = start;
        let definition_match = definition.captures(line);
        let definition_start = definition_match.as_ref().map(|m| m.get(0).unwrap().end());
        let definition_label = definition_match.as_ref().map(|m| normalize_label(&m[1]));
        while i < bytes.len() {
            if comment {
                if let Some(end) = line[i..].find("-->") {
                    i += end + 3;
                    comment = false;
                } else {
                    break;
                }
                continue;
            }
            if let Some(end) = &literal_end {
                if let Some(at) = line[i..].to_ascii_lowercase().find(end) {
                    i += at + end.len();
                    literal_end = None;
                } else {
                    break;
                }
                continue;
            }
            if bytes[i] == b'\\' {
                i = (i + 2).min(bytes.len());
                continue;
            }
            if bytes[i] == b'`' {
                let count = bytes[i..].iter().take_while(|b| **b == b'`').count();
                if inline == 0 {
                    if has_closing_backticks(raw.as_bytes(), offset + i + count, count) {
                        inline = count;
                    }
                } else if inline == count {
                    inline = 0;
                }
                i += count;
                continue;
            }
            if inline > 0 {
                i += 1;
                continue;
            }
            if bytes[i] == b'$' && bytes.get(i + 1).is_some_and(|c| !c.is_ascii_whitespace()) {
                let mut end = i + 1;
                while end < bytes.len() {
                    if bytes[end] == b'\\' {
                        end += 2;
                        continue;
                    }
                    if bytes[end] == b'$' {
                        break;
                    }
                    end += 1;
                }
                if end < bytes.len() {
                    i = end + 1;
                    continue;
                }
            }
            if bytes[i] == b'[' && definition_start.is_none() {
                if let Some(close) = closing_bracket(bytes, i) {
                    if bytes.get(close + 1) == Some(&b'[') {
                        if let Some(end) = closing_bracket(bytes, close + 1) {
                            let explicit = &line[close + 2..end];
                            let label = if explicit.trim().is_empty() {
                                &line[i + 1..close]
                            } else {
                                explicit
                            };
                            used_labels.insert(normalize_label(label));
                            i = end + 1;
                            continue;
                        }
                    } else if bytes.get(close + 1) != Some(&b'(') {
                        used_labels.insert(normalize_label(&line[i + 1..close]));
                        i = close + 1;
                        continue;
                    }
                    // Alt text is a label, not an independent attachment.
                    i = close;
                }
            }
            if bytes[i] == b'<' {
                if line[i..].starts_with("<!--") {
                    comment = true;
                    i += 4;
                    continue;
                }
                if let Some(caps) = literal.captures(&line[i..]) {
                    literal_end = Some(format!("</{}>", caps[1].to_ascii_lowercase()));
                    i += caps[0].len();
                    continue;
                }
                if let Some(tag) = html.find(&raw[offset + i..]) {
                    for caps in attr.captures_iter(tag.as_str()) {
                        let value = caps
                            .get(2)
                            .or_else(|| caps.get(3))
                            .or_else(|| caps.get(4))
                            .unwrap();
                        push_reference(
                            &mut result,
                            raw,
                            offset + i + value.start(),
                            offset + i + value.end(),
                            true,
                            caps[1].eq_ignore_ascii_case("data-pdf-src"),
                        );
                    }
                    consumed_until = offset + i + tag.end();
                    i = (i + tag.end()).min(bytes.len());
                    continue;
                }
            }
            let start = if definition_start == Some(i) {
                Some(i)
            } else if bytes[i..].starts_with(b"](") {
                Some(i + 2)
            } else {
                None
            };
            if let Some(mut from) = start {
                while from < bytes.len() && matches!(bytes[from], b' ' | b'\t') {
                    from += 1;
                }
                let angle = bytes.get(from) == Some(&b'<');
                if angle {
                    from += 1;
                }
                let mut to = from;
                let mut depth = 0;
                while to < bytes.len() {
                    let c = bytes[to];
                    if c == b'\\' {
                        to = (to + 2).min(bytes.len());
                        continue;
                    }
                    if angle && c == b'>' {
                        break;
                    }
                    if !angle {
                        if c == b'\n' || c == b'\r' || c == b')' && depth == 0 {
                            break;
                        }
                        if c.is_ascii_whitespace() {
                            let mut after = to;
                            while after < bytes.len() && matches!(bytes[after], b' ' | b'\t') {
                                after += 1;
                            }
                            if definition_start == Some(i)
                                || bytes.get(after).is_none_or(|c| {
                                    matches!(c, b'\"' | b'\'' | b')' | b'\n' | b'\r')
                                })
                            {
                                break;
                            }
                        }
                        if c == b'(' {
                            depth += 1;
                        }
                        if c == b')' {
                            depth -= 1;
                        }
                    }
                    to += 1;
                }
                let before = result.len();
                push_reference(&mut result, raw, offset + from, offset + to, false, false);
                if definition_start == Some(i) {
                    let label = definition_label.clone().unwrap_or_default();
                    if defined_labels.insert(label.clone()) {
                        if result.len() > before {
                            definitions.insert(offset + from, label);
                        }
                    } else if result.len() > before {
                        result.pop();
                    }
                }
                i = to.max(i + 1);
                continue;
            }
            // Only slice at ASCII delimiters above; byte stepping is safe for UTF-8.
            i += 1;
        }
        offset += line.len();
    }
    result.retain(|reference| {
        definitions
            .get(&reference.from)
            .is_none_or(|label| used_labels.contains(label))
    });
    result
}

fn has_closing_backticks(bytes: &[u8], mut at: usize, count: usize) -> bool {
    while at < bytes.len() {
        if bytes[at] == b'`' {
            let run = bytes[at..].iter().take_while(|b| **b == b'`').count();
            if run == count {
                return true;
            }
            at += run;
        } else {
            at += 1;
        }
    }
    false
}

fn normalize_label(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn closing_bracket(bytes: &[u8], from: usize) -> Option<usize> {
    let mut depth = 1;
    let mut at = from + 1;
    while at < bytes.len() {
        match bytes[at] {
            b'\\' => {
                at += 2;
                continue;
            }
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => {}
        }
        at += 1;
    }
    None
}

fn push_reference(
    out: &mut Vec<ExternalNoteReference>,
    raw: &str,
    from: usize,
    to: usize,
    html: bool,
    pdf: bool,
) {
    let Some(value) = raw.get(from..to) else {
        return;
    };
    let href = unescape(value, html);
    let path = href.split(['?', '#']).next().unwrap_or("");
    let path = urlencoding::decode(path).unwrap_or_else(|_| path.into());
    if path.is_empty()
        || path.contains(':')
        || path.starts_with(['/', '\\'])
        || path.to_ascii_lowercase().ends_with(".md")
    {
        return;
    }
    out.push(ExternalNoteReference {
        from,
        to,
        href,
        html,
        pdf,
    });
}

pub(crate) fn import_references(
    db: &VaultDb,
    tx: &rusqlite::Connection,
    root: &Path,
    source: &Path,
    raw: &str,
    owner: &str,
) -> Result<String, String> {
    let mut refs = Vec::new();
    for reference in references(raw) {
        let id = reference
            .href
            .strip_prefix(".rn-attachments/")
            .unwrap_or(&reference.href)
            .split(['?', '#'])
            .next()
            .unwrap_or("");
        if uuid::Uuid::parse_str(id).is_ok() && db.attachment_meta(id)?.is_some() {
            continue; // Already stored: the normal content-link backfill owns it.
        }
        refs.push(reference);
    }
    if refs.is_empty() {
        return Ok(raw.into());
    }
    let root = std::fs::canonicalize(root).map_err(|e| e.to_string())?;
    let prepared = super::operations::prepare_note_import(
        source.to_str().ok_or("Invalid source path")?,
        &root,
        raw,
        refs,
        |path| {
            if path.starts_with(&root) {
                Ok(())
            } else {
                Err("Attachment escapes source vault".into())
            }
        },
    )?;
    let mut rewritten = String::new();
    let mut previous = 0;
    for (reference, (path, suffix)) in prepared.references.iter().zip(&prepared.targets) {
        let data = read_capped(path, db.max_attachment_bytes)?;
        let filename = path
            .file_name()
            .ok_or("Missing attachment name")?
            .to_string_lossy();
        let id = db.store_attachment_on(
            tx,
            owner,
            &filename,
            mime_type(&filename, &data),
            &data,
            &chrono::Utc::now().to_rfc3339(),
        )?;
        let href = format!(".rn-attachments/{id}{suffix}");
        rewritten.push_str(&raw[previous..reference.from]);
        rewritten.push_str(&if reference.html {
            href.replace('&', "&amp;")
                .replace('"', "&quot;")
                .replace('\'', "&#39;")
        } else {
            href
        });
        previous = reference.to;
    }
    rewritten.push_str(&raw[previous..]);
    Ok(rewritten)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_preserve_unicode_ranges_html_suffixes_and_code_examples() {
        let raw = "---\nid: n\n---\n雪 [pdf](<files/report (雪).pdf#page=2>)\n![image](photo.png)\n[file][ref]\n[ref]: data.bin \"label\"\n<a href=\"data.bin?x=1&amp;y=2\">file</a>\n<div data-pdf-src='paper.pdf'></div>\n`[not a file](code.bin)`\n```\n[not a file](fenced.bin)\n```\n<!-- [not a file](comment.bin) -->\n<pre>[not a file](literal.bin)</pre>\n[note](other.md)\n[web](https://example.com/file.pdf)";
        let refs = references(raw);
        assert_eq!(
            refs.iter().map(|r| r.href.as_str()).collect::<Vec<_>>(),
            vec![
                "files/report (雪).pdf#page=2",
                "photo.png",
                "data.bin",
                "data.bin?x=1&y=2",
                "paper.pdf",
            ]
        );
        for reference in refs {
            assert!(raw.get(reference.from..reference.to).is_some());
        }
        let body = "---\n[a](data.bin)";
        assert_eq!(body_references(body).len(), 1);
    }

    #[test]
    fn oversized_or_missing_files_do_not_publish_a_partial_note_or_blob() {
        let source = tempfile::tempdir().unwrap();
        let raw = "---\nid: note\ntitle: Note\n---\n[file](file.bin)";
        std::fs::write(source.path().join("note.md"), raw).unwrap();
        std::fs::write(source.path().join("file.bin"), [0, 1, 2]).unwrap();
        let mut db = VaultDb::open_in_memory().unwrap();
        db.max_attachment_bytes = 2;
        let report = super::super::db_import::import_md_vault(source.path(), &db).unwrap();
        assert_eq!(report.files_skipped, 1);
        assert!(db.list_notes(true).unwrap().is_empty());
        assert_eq!(
            db.conn
                .query_row("SELECT COUNT(*) FROM attachments", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        std::fs::remove_file(source.path().join("file.bin")).unwrap();
        assert_eq!(
            super::super::db_import::import_md_vault(source.path(), &db)
                .unwrap()
                .files_skipped,
            1
        );
    }
}
