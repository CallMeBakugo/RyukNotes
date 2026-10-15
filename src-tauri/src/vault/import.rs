use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};

use crate::vault::frontmatter::BoundedYaml;
use chrono::Utc;
use gray_matter::Matter;
use regex::Regex;
use uuid::Uuid;

use crate::types::{ImportResult, NoteMeta};
use crate::vault::frontmatter;
use crate::vault::operations::replace_file_atomically;

pub fn import(vault_path: &str) -> Result<ImportResult, String> {
    let vault = Path::new(vault_path);
    // Folders that are empty before the import are notebooks the user made; only folders the
    // import itself empties are removed afterwards.
    let already_empty = empty_dirs(vault)?;

    let file_index = build_file_index(vault)?;

    let md_files: Vec<_> = scanned_files(vault)?
        .into_iter()
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("md"))
        .collect();

    let wiki_embed_re =
        Regex::new(r"!\[\[([^\]|]+?)(?:\|([^\]]*))?\]\]").map_err(|e| e.to_string())?;
    let wiki_link_re =
        Regex::new(r"\[\[([^\]|]+?)(?:\|([^\]]*))?\]\]").map_err(|e| e.to_string())?;
    let highlight_re = Regex::new(r"==([^\n=]+?)==").map_err(|e| e.to_string())?;
    let comment_re = Regex::new(r"(?s)%%([\s\S]*?)%%").map_err(|e| e.to_string())?;
    let md_img_re = Regex::new(r"!\[([^\]]*)\]\(([^)]+)\)").map_err(|e| e.to_string())?;
    let md_link_re = Regex::new(r"\[([^\]]+)\]\(([^)]+)\)").map_err(|e| e.to_string())?;

    let mut result = ImportResult {
        files_converted: 0,
        links_converted: 0,
        frontmatter_normalized: 0,
        syntax_converted: 0,
        attachments_moved: 0,
    };

    for path in &md_files {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("Reading {}: {e}", path.display()))?;
        let note_dir = path.parent().unwrap_or(Path::new(""));

        let mut content = raw.clone();
        let mut changed = false;

        let renamed = rename_deprecated_properties(&content);
        if renamed != content {
            content = renamed;
            changed = true;
        }

        let (meta, body) = normalize_frontmatter(&content, path);
        let normalized = frontmatter::merge_frontmatter(&content, &meta, &body);
        if normalized != content {
            content = normalized;
            changed = true;
            result.frontmatter_normalized += 1;
        }

        let after_syntax = convert_syntax(&content, &highlight_re, &comment_re);
        if after_syntax != content {
            content = after_syntax;
            changed = true;
            result.syntax_converted += 1;
        }

        let links_before = result.links_converted;

        let after_embeds = wiki_embed_re
            .replace_all(&content, |caps: &regex::Captures| {
                let file_ref = &caps[1];
                let alt_param = caps.get(2).map(|m| m.as_str()).unwrap_or("");
                let (file_part, anchor) = split_anchor(file_ref);
                if file_part.is_empty() {
                    if let Some(a) = anchor {
                        let display = if alt_param.is_empty() { a } else { alt_param };
                        result.links_converted += 1;
                        return format!("[{}](#{})", display, a);
                    }
                }
                let resolved = resolve_wiki_ref(&file_index, file_part);
                let abs_target = vault.join(&resolved);
                let rel = pathdiff(
                    abs_target.to_str().unwrap_or(""),
                    note_dir.to_str().unwrap_or(""),
                )
                .unwrap_or(resolved);
                let encoded = rel.replace(' ', "%20");
                let link_target = match anchor {
                    Some(a) => format!("{}#{}", encoded, a),
                    None => encoded,
                };
                let ext = Path::new(file_part)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                let is_embeddable = matches!(
                    ext.as_str(),
                    "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "bmp" | "ico" | "pdf"
                );
                if is_embeddable {
                    let alt = if is_dimension_spec(alt_param) {
                        ""
                    } else {
                        alt_param
                    };
                    result.links_converted += 1;
                    format!("![{}]({})", alt, link_target)
                } else {
                    let display = if alt_param.is_empty() || is_dimension_spec(alt_param) {
                        file_part.rsplit('/').next().unwrap_or(file_part)
                    } else {
                        alt_param
                    };
                    result.links_converted += 1;
                    format!("[{}]({})", display, link_target)
                }
            })
            .to_string();
        content = after_embeds;

        let after_links = wiki_link_re
            .replace_all(&content, |caps: &regex::Captures| {
                let note_ref = &caps[1];
                let display_param = caps.get(2).map(|m| m.as_str()).unwrap_or("");
                let (file_part, anchor) = split_anchor(note_ref);
                if file_part.is_empty() {
                    if let Some(a) = anchor {
                        let display = if display_param.is_empty() {
                            a
                        } else {
                            display_param
                        };
                        result.links_converted += 1;
                        return format!("[{}](#{})", display, a);
                    }
                }
                let resolved = resolve_wiki_ref(&file_index, file_part);
                let abs_target = vault.join(&resolved);
                let rel = pathdiff(
                    abs_target.to_str().unwrap_or(""),
                    note_dir.to_str().unwrap_or(""),
                )
                .unwrap_or(resolved);
                let encoded = rel.replace(' ', "%20");
                let link_target = match anchor {
                    Some(a) => format!("{}#{}", encoded, a),
                    None => encoded,
                };
                let display = if display_param.is_empty() {
                    note_ref
                } else {
                    display_param
                };
                result.links_converted += 1;
                format!("[{}]({})", display, link_target)
            })
            .to_string();
        content = after_links;

        content = fix_md_image_refs(
            &content,
            &md_img_re,
            vault,
            note_dir,
            &file_index,
            &mut result.links_converted,
        );
        content = fix_md_link_refs(
            &content,
            &md_link_re,
            vault,
            note_dir,
            &file_index,
            &mut result.links_converted,
        );

        if result.links_converted > links_before {
            changed = true;
        }

        if changed {
            replace_file_atomically(path, content.as_bytes())
                .map_err(|e| format!("Writing {}: {e}", path.display()))?;
            result.files_converted += 1;
        }
    }

    let moved = move_attachments(vault)?;
    result.attachments_moved = moved.len() as u64;

    if !moved.is_empty() {
        rewrite_attachment_refs(vault, &moved)?;
        for old_rel in moved.keys() {
            let src_path = vault.join(old_rel);
            std::fs::remove_file(&src_path)
                .map_err(|e| format!("Removing {} after copying: {e}", src_path.display()))?;
        }
    }

    cleanup_empty_dirs(vault, &already_empty)?;

    Ok(result)
}

/// Hidden files and folders are never imported, moved or cleaned up. The app's own `.ryuknotes`
/// and Obsidian's `.obsidian` and `.trash` are among them, and so is `.git`: every file that is
/// not a note becomes an attachment and its original is deleted, which took a repository apart.
fn is_hidden_entry(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.'))
}

fn vault_entries(
    vault: &Path,
) -> impl Iterator<Item = Result<walkdir::DirEntry, walkdir::Error>> + '_ {
    walkdir::WalkDir::new(vault)
        .into_iter()
        .filter_entry(|entry| entry.depth() == 0 || !is_hidden_entry(entry.path()))
}

fn scanned_files(vault: &Path) -> Result<Vec<PathBuf>, String> {
    vault_entries(vault)
        .filter_map(|entry| match entry {
            Ok(entry) if entry.file_type().is_file() => Some(Ok(entry.into_path())),
            Ok(_) => None,
            Err(e) => Some(Err(e)),
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Scanning {}: {e}", vault.display()))
}

fn vault_relative(path: &Path, vault: &Path) -> Result<String, String> {
    path.strip_prefix(vault)
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
        .map_err(|e| format!("Relativizing {}: {e}", path.display()))
}

fn build_file_index(vault: &Path) -> Result<HashMap<String, String>, String> {
    let mut index = HashMap::new();
    for entry in vault_entries(vault) {
        let entry = entry.map_err(|e| format!("Scanning {}: {e}", vault.display()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            index.insert(name.to_string(), vault_relative(path, vault)?);
        }
    }
    Ok(index)
}

fn rename_deprecated_properties(raw: &str) -> String {
    let matter = Matter::<BoundedYaml>::new();
    let result = matter.parse(raw);

    if result.matter.trim().is_empty() {
        return raw.to_string();
    }

    let mut mapping: serde_yaml::Mapping = match serde_yaml::from_str(&result.matter) {
        Ok(m) => m,
        Err(_) => return raw.to_string(),
    };

    let mut renamed = false;
    for (old, new) in &[("alias", "aliases"), ("cssclass", "cssclasses")] {
        let old_key = serde_yaml::Value::String((*old).into());
        let new_key = serde_yaml::Value::String((*new).into());
        if !mapping.contains_key(&new_key) {
            if let Some(val) = mapping.remove(&old_key) {
                mapping.insert(new_key, val);
                renamed = true;
            }
        } else if mapping.remove(&old_key).is_some() {
            renamed = true;
        }
    }

    if !renamed {
        return raw.to_string();
    }

    let yaml_str = match serde_yaml::to_string(&serde_yaml::Value::Mapping(mapping)) {
        Ok(s) => s,
        Err(_) => return raw.to_string(),
    };

    format!("---\n{}---\n{}", yaml_str, result.content)
}

fn normalize_frontmatter(raw: &str, path: &Path) -> (NoteMeta, String) {
    let matter = Matter::<BoundedYaml>::new();
    let result = matter.parse(raw);
    let yaml_str = result.matter.trim();

    let mapping: serde_yaml::Mapping = if yaml_str.is_empty() {
        serde_yaml::Mapping::new()
    } else {
        match serde_yaml::from_str::<serde_yaml::Mapping>(yaml_str) {
            Ok(m) => m,
            Err(_) => serde_yaml::Mapping::new(),
        }
    };

    let tags = normalize_tags(&mapping);

    let title = mapping
        .get(serde_yaml::Value::String("title".into()))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            if let Some(h) = extract_heading_title(&result.content) {
                h
            } else {
                let fname = path
                    .file_stem()
                    .and_then(|n| n.to_str())
                    .unwrap_or("Untitled");
                frontmatter::filename_to_title(fname)
            }
        });

    let id = mapping
        .get(serde_yaml::Value::String("id".into()))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    let created = ["created", "date", "ctime"]
        .iter()
        .find_map(|key| {
            mapping
                .get(serde_yaml::Value::String((*key).into()))
                .and_then(|v| v.as_str())
                .and_then(frontmatter::parse_date_flexible)
        })
        .or_else(|| file_created(path))
        .unwrap_or_else(Utc::now);

    let modified = ["modified", "mtime", "lastmod"]
        .iter()
        .find_map(|key| {
            mapping
                .get(serde_yaml::Value::String((*key).into()))
                .and_then(|v| v.as_str())
                .and_then(frontmatter::parse_date_flexible)
        })
        .or_else(|| file_modified(path))
        .unwrap_or_else(Utc::now);

    let pinned = mapping
        .get(serde_yaml::Value::String("pinned".into()))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let meta = NoteMeta {
        id,
        title,
        tags,
        pinned,
        created,
        modified,
    };
    (meta, result.content)
}

fn normalize_tags(mapping: &serde_yaml::Mapping) -> Vec<String> {
    let mut tags = Vec::new();
    let mut seen = HashSet::new();

    for key in &["tags", "tag"] {
        if let Some(val) = mapping.get(serde_yaml::Value::String((*key).into())) {
            for raw in yaml_value_to_strings(val) {
                let cleaned = raw.trim().trim_start_matches('#').trim().to_string();
                if !cleaned.is_empty() && seen.insert(cleaned.to_lowercase()) {
                    tags.push(cleaned);
                }
            }
        }
    }
    tags
}

fn yaml_value_to_strings(val: &serde_yaml::Value) -> Vec<String> {
    match val {
        serde_yaml::Value::String(s) => {
            if s.contains(',') {
                s.split(',')
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect()
            } else {
                vec![s.clone()]
            }
        }
        serde_yaml::Value::Sequence(seq) => seq
            .iter()
            .filter_map(|v| match v {
                serde_yaml::Value::String(s) => Some(s.clone()),
                serde_yaml::Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .collect(),
        serde_yaml::Value::Number(n) => vec![n.to_string()],
        serde_yaml::Value::Bool(b) => vec![b.to_string()],
        _ => Vec::new(),
    }
}

fn extract_heading_title(body: &str) -> Option<String> {
    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("# ") {
            return Some(rest.trim().to_string());
        }
        return None;
    }
    None
}

fn file_created(path: &Path) -> Option<chrono::DateTime<Utc>> {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.created().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|d| chrono::DateTime::from_timestamp(d.as_secs() as i64, d.subsec_nanos()))
}

fn file_modified(path: &Path) -> Option<chrono::DateTime<Utc>> {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|d| chrono::DateTime::from_timestamp(d.as_secs() as i64, d.subsec_nanos()))
}

fn convert_syntax(content: &str, highlight_re: &Regex, comment_re: &Regex) -> String {
    let segments = split_code_segments(content);
    let mut result = String::with_capacity(content.len());

    for (is_code, text) in segments {
        if is_code {
            result.push_str(&text);
        } else {
            let with_marks = highlight_re.replace_all(&text, "<mark>$1</mark>");
            let with_comments = comment_re.replace_all(&with_marks, "<!-- $1 -->");
            result.push_str(&with_comments);
        }
    }
    result
}

fn split_code_segments(content: &str) -> Vec<(bool, String)> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut in_code = false;
    let mut fence_char = '`';

    for line in content.split_inclusive('\n') {
        let trimmed = line.trim_start();

        let is_fence = if in_code {
            trimmed.starts_with(&fence_char.to_string().repeat(3))
        } else if trimmed.starts_with("```") {
            fence_char = '`';
            true
        } else if trimmed.starts_with("~~~") {
            fence_char = '~';
            true
        } else {
            false
        };

        if is_fence {
            current.push_str(line);
            segments.push((in_code, std::mem::take(&mut current)));
            in_code = !in_code;
        } else {
            current.push_str(line);
        }
    }
    if !current.is_empty() {
        segments.push((in_code, current));
    }
    segments
}

fn fix_md_image_refs(
    content: &str,
    re: &Regex,
    vault: &Path,
    note_dir: &Path,
    file_index: &HashMap<String, String>,
    links_converted: &mut u64,
) -> String {
    re.replace_all(content, |caps: &regex::Captures| {
        let alt = &caps[1];
        let src = &caps[2];
        let decoded = percent_decode(src);
        if decoded.starts_with("http") || decoded.starts_with('/') || decoded.starts_with("data:") {
            return format!("![{}]({})", alt, src);
        }
        if !decoded.contains('/') {
            if let Some(rel) = file_index.get(&decoded) {
                *links_converted += 1;
                let abs_target = vault.join(rel);
                let note_rel = pathdiff(
                    abs_target.to_str().unwrap_or(""),
                    note_dir.to_str().unwrap_or(""),
                )
                .unwrap_or(rel.clone());
                return format!("![{}]({})", alt, note_rel.replace(' ', "%20"));
            }
        }
        if decoded.contains('/') {
            let vault_abs = vault.join(&decoded);
            if vault_abs.exists() {
                let note_rel = pathdiff(
                    vault_abs.to_str().unwrap_or(""),
                    note_dir.to_str().unwrap_or(""),
                )
                .unwrap_or(decoded.clone());
                if note_rel != decoded {
                    *links_converted += 1;
                    return format!("![{}]({})", alt, note_rel.replace(' ', "%20"));
                }
            }
        }
        format!("![{}]({})", alt, src)
    })
    .to_string()
}

fn fix_md_link_refs(
    content: &str,
    re: &Regex,
    vault: &Path,
    note_dir: &Path,
    file_index: &HashMap<String, String>,
    links_converted: &mut u64,
) -> String {
    re.replace_all(content, |caps: &regex::Captures| {
        let display = &caps[1];
        let href = &caps[2];
        let decoded = percent_decode(href);
        if decoded.starts_with("http")
            || decoded.starts_with('/')
            || decoded.starts_with("data:")
            || decoded.starts_with('#')
        {
            return format!("[{}]({})", display, href);
        }
        if !decoded.contains('/') {
            if let Some(rel) = file_index.get(&decoded) {
                *links_converted += 1;
                let abs_target = vault.join(rel);
                let note_rel = pathdiff(
                    abs_target.to_str().unwrap_or(""),
                    note_dir.to_str().unwrap_or(""),
                )
                .unwrap_or(rel.clone());
                return format!("[{}]({})", display, note_rel.replace(' ', "%20"));
            }
        }
        if decoded.contains('/') {
            let vault_abs = vault.join(&decoded);
            if vault_abs.exists() {
                let note_rel = pathdiff(
                    vault_abs.to_str().unwrap_or(""),
                    note_dir.to_str().unwrap_or(""),
                )
                .unwrap_or(decoded.clone());
                if note_rel != decoded {
                    *links_converted += 1;
                    return format!("[{}]({})", display, note_rel.replace(' ', "%20"));
                }
            }
        }
        format!("[{}]({})", display, href)
    })
    .to_string()
}

fn ensure_attachment_parent(
    vault: &Path,
    canonical_vault: &Path,
    parent: &Path,
) -> Result<(), String> {
    let relative = parent
        .strip_prefix(vault)
        .map_err(|e| format!("Invalid attachment destination {}: {e}", parent.display()))?;
    let mut current = vault.to_path_buf();
    let mut relative_path = PathBuf::new();
    for component in relative.components() {
        current.push(component);
        relative_path.push(component);
        match std::fs::create_dir(&current) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("Creating {}: {e}", current.display())),
        }
        let canonical = std::fs::canonicalize(&current)
            .map_err(|e| format!("Resolving {}: {e}", current.display()))?;
        if !current.is_dir() || canonical != canonical_vault.join(&relative_path) {
            return Err(format!(
                "Attachment destination escapes vault: {}",
                current.display()
            ));
        }
    }
    Ok(())
}

fn move_attachments(vault: &Path) -> Result<HashMap<String, String>, String> {
    let attachments_dir = vault.join(".ryuknotes").join("attachments");
    let attachment_files: Vec<_> = scanned_files(vault)?
        .into_iter()
        .filter(|p| p.extension().and_then(|x| x.to_str()) != Some("md"))
        .collect();
    let canonical_vault =
        std::fs::canonicalize(vault).map_err(|e| format!("Resolving {}: {e}", vault.display()))?;

    let mut moved = HashMap::new();
    for src_path in attachment_files {
        let old_rel = vault_relative(&src_path, vault)?;
        let preferred = attachments_dir.join(&old_rel);
        let parent = preferred.parent().expect("attachment has a parent");
        ensure_attachment_parent(vault, &canonical_vault, parent)?;

        // Reserve the destination atomically: neither rename nor std::fs::copy protects
        // a pre-existing attachment (or one created concurrently) from replacement.
        let mut suffix = 0u64;
        let (dest_path, mut dest) = loop {
            let candidate = if suffix == 0 {
                preferred.clone()
            } else {
                let stem = preferred.file_stem().unwrap_or_default().to_string_lossy();
                let ext = preferred
                    .extension()
                    .map(|ext| format!(".{}", ext.to_string_lossy()))
                    .unwrap_or_default();
                preferred.with_file_name(format!("{stem}-{suffix}{ext}"))
            };
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
            {
                Ok(file) => break (candidate, file),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    suffix = suffix
                        .checked_add(1)
                        .ok_or("Too many attachment collisions")?;
                }
                Err(e) => return Err(format!("Creating {}: {e}", candidate.display())),
            }
        };
        let copied = (|| -> io::Result<()> {
            let mut src = std::fs::File::open(&src_path)?;
            io::copy(&mut src, &mut dest)?;
            dest.sync_all()?;
            Ok(())
        })();
        drop(dest);
        if let Err(e) = copied {
            std::fs::remove_file(&dest_path).map_err(|cleanup| {
                format!(
                    "Copying {} to {}: {e}; removing incomplete copy: {cleanup}",
                    src_path.display(),
                    dest_path.display()
                )
            })?;
            return Err(format!(
                "Copying {} to {}: {e}",
                src_path.display(),
                dest_path.display()
            ));
        }
        moved.insert(old_rel, vault_relative(&dest_path, vault)?);
    }
    Ok(moved)
}

fn rewrite_attachment_refs(vault: &Path, moved: &HashMap<String, String>) -> Result<(), String> {
    let md_ref = Regex::new(r"(!?\[[^\]]*\])\(([^)]+)\)").map_err(|e| e.to_string())?;

    let md_files: Vec<_> = scanned_files(vault)?
        .into_iter()
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("md"))
        .collect();

    for path in &md_files {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Reading {}: {e}", path.display()))?;
        let note_dir = path.parent().unwrap_or(Path::new(""));

        let new_content = md_ref
            .replace_all(&content, |caps: &regex::Captures| {
                let prefix = &caps[1];
                let href = &caps[2];
                let decoded_href = percent_decode(href);
                let (reference, suffix) = decoded_href
                    .find(['#', '?'])
                    .map(|index| decoded_href.split_at(index))
                    .unwrap_or((&decoded_href, ""));

                if reference.starts_with("http")
                    || reference.starts_with("data:")
                    || reference.starts_with('/')
                {
                    return format!("{}({})", prefix, href);
                }

                let rel_from_note = note_dir
                    .strip_prefix(vault)
                    .ok()
                    .and_then(|base| resolve_relative_ref(base, reference));
                let old_vault_rel = rel_from_note
                    .as_ref()
                    .filter(|r| moved.contains_key(r.as_str()))
                    .cloned()
                    .or_else(|| {
                        resolve_relative_ref(Path::new(""), reference)
                            .filter(|r| moved.contains_key(r.as_str()))
                    });

                if let Some(old_rel) = old_vault_rel {
                    if let Some(new_rel) = moved.get(&old_rel) {
                        let new_abs = vault.join(new_rel);
                        let new_note_rel = pathdiff(
                            new_abs.to_str().unwrap_or(""),
                            note_dir.to_str().unwrap_or(""),
                        )
                        .unwrap_or(new_rel.clone());
                        return format!(
                            "{}({}{})",
                            prefix,
                            new_note_rel.replace('\\', "/").replace(' ', "%20"),
                            suffix
                        );
                    }
                }
                format!("{}({})", prefix, href)
            })
            .to_string();

        if new_content != content {
            replace_file_atomically(path, new_content.as_bytes())
                .map_err(|e| format!("Writing {}: {e}", path.display()))?;
        }
    }
    Ok(())
}

fn resolve_relative_ref(base: &Path, reference: &str) -> Option<String> {
    let mut rel = base.to_path_buf();
    for component in Path::new(&reference.replace('\\', "/")).components() {
        match component {
            std::path::Component::Normal(name) => rel.push(name),
            std::path::Component::ParentDir => {
                if !rel.pop() {
                    return None;
                }
            }
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    Some(rel.to_string_lossy().replace('\\', "/"))
}

fn empty_dirs(root: &Path) -> Result<HashSet<PathBuf>, String> {
    let mut empty = HashSet::new();
    for entry in vault_entries(root) {
        let entry = entry.map_err(|e| format!("Scanning {}: {e}", root.display()))?;
        if entry.file_type().is_dir()
            && std::fs::read_dir(entry.path()).is_ok_and(|mut entries| entries.next().is_none())
        {
            empty.insert(entry.into_path());
        }
    }
    Ok(empty)
}

fn cleanup_empty_dirs(root: &Path, keep: &HashSet<PathBuf>) -> Result<(), String> {
    let mut dirs: Vec<_> = vault_entries(root)
        .filter_map(|entry| match entry {
            Ok(entry) if entry.file_type().is_dir() => Some(Ok(entry.into_path())),
            Ok(_) => None,
            Err(e) => Some(Err(e)),
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Scanning {}: {e}", root.display()))?;

    dirs.sort_by_key(|path| std::cmp::Reverse(path.components().count()));

    for dir in dirs {
        if dir == root || keep.contains(&dir) {
            continue;
        }
        let mut entries =
            std::fs::read_dir(&dir).map_err(|e| format!("Reading {}: {e}", dir.display()))?;
        if entries
            .next()
            .transpose()
            .map_err(|e| format!("Reading {}: {e}", dir.display()))?
            .is_none()
        {
            std::fs::remove_dir(&dir)
                .map_err(|e| format!("Removing empty directory {}: {e}", dir.display()))?;
        }
    }
    Ok(())
}

fn split_anchor(reference: &str) -> (&str, Option<&str>) {
    if let Some(idx) = reference.find('#') {
        let file_part = &reference[..idx];
        let anchor = &reference[idx + 1..];
        if anchor.is_empty() {
            (reference, None)
        } else {
            (file_part, Some(anchor))
        }
    } else {
        (reference, None)
    }
}

fn is_dimension_spec(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_digit() || c == 'x' || c == 'X')
}

fn resolve_wiki_ref(file_index: &HashMap<String, String>, reference: &str) -> String {
    let reference = reference.trim();
    let reference = reference.split('#').next().unwrap_or(reference).trim();
    let reference = reference.split('^').next().unwrap_or(reference).trim();

    if let Some(rel) = file_index.get(reference) {
        return rel.clone();
    }
    let with_md = format!("{}.md", reference);
    if let Some(rel) = file_index.get(&with_md) {
        return rel.clone();
    }
    if let Some(name) = reference.rsplit('/').next() {
        if let Some(rel) = file_index.get(name) {
            return rel.clone();
        }
        let name_md = format!("{}.md", name);
        if let Some(rel) = file_index.get(&name_md) {
            return rel.clone();
        }
    }
    reference.to_string()
}

fn pathdiff(target: &str, base: &str) -> Result<String, ()> {
    let target = Path::new(target);
    let base = Path::new(base);

    let target_components: Vec<_> = target.components().collect();
    let base_components: Vec<_> = base.components().collect();

    let common = target_components
        .iter()
        .zip(base_components.iter())
        .take_while(|(a, b)| a == b)
        .count();

    let mut result = std::path::PathBuf::new();
    for _ in common..base_components.len() {
        result.push("..");
    }
    for component in &target_components[common..] {
        result.push(component);
    }
    Ok(result.to_string_lossy().replace('\\', "/"))
}

fn percent_decode(s: &str) -> String {
    // Keep the complete original reference if escaped bytes are not valid UTF-8.
    urlencoding::decode(s)
        .map(|decoded| decoded.into_owned())
        .unwrap_or_else(|_| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_preserves_unicode_plus_and_malformed_escapes() {
        for (input, expected) in [
            ("café 雪+.png", "café 雪+.png"),
            ("caf%C3%A9%20%E9%9B%AA+.png", "café 雪+.png"),
            ("bad%GG%20name.png", "bad%GG name.png"),
            ("tail%", "tail%"),
            ("tail%2", "tail%2"),
            ("雪%FF%20+%GG", "雪%FF%20+%GG"),
            ("%E9%9B", "%E9%9B"),
        ] {
            assert_eq!(percent_decode(input), expected, "input {input:?}");
        }
    }

    #[test]
    fn import_leaves_hidden_files_and_existing_empty_folders_alone() {
        let vault = tempfile::tempdir().unwrap();
        let root = vault.path();
        // A git-backed vault with dotfiles, an empty notebook, and a folder of attachments only.
        std::fs::create_dir_all(root.join(".git/objects/ab")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(root.join(".git/objects/ab/cdef"), b"blob").unwrap();
        std::fs::write(root.join(".gitignore"), ".obsidian/workspace.json\n").unwrap();
        std::fs::create_dir_all(root.join(".stversions")).unwrap();
        std::fs::write(root.join(".stversions/old.png"), b"old").unwrap();
        std::fs::create_dir(root.join("Empty notebook")).unwrap();
        std::fs::create_dir(root.join("Pictures")).unwrap();
        std::fs::write(root.join("Pictures/photo.png"), b"png").unwrap();
        std::fs::write(root.join("Note.md"), "# Note\n\n![p](Pictures/photo.png)\n").unwrap();

        let result = import(root.to_str().unwrap()).unwrap();

        assert_eq!(result.attachments_moved, 1);
        assert_eq!(
            std::fs::read_to_string(root.join(".git/HEAD")).unwrap(),
            "ref: refs/heads/main\n"
        );
        assert_eq!(
            std::fs::read(root.join(".git/objects/ab/cdef")).unwrap(),
            b"blob"
        );
        assert!(root.join(".gitignore").is_file());
        assert!(root.join(".stversions/old.png").is_file());
        // Nothing hidden was copied into the attachments either.
        assert!(!root.join(".ryuknotes/attachments/.git").exists());
        assert!(!root.join(".ryuknotes/attachments/.gitignore").exists());
        // The folder the import emptied is gone; the one that was empty already is kept.
        assert!(!root.join("Pictures").exists());
        assert!(root.join("Empty notebook").is_dir());
        assert!(root
            .join(".ryuknotes/attachments/Pictures/photo.png")
            .is_file());
    }

    #[test]
    fn import_rewrites_unicode_encoded_and_malformed_attachment_references() {
        let vault = tempfile::tempdir().unwrap();
        let root = vault.path();
        let note_dir = root.join("笔记");
        std::fs::create_dir(&note_dir).unwrap();
        let attachments = [
            "café 雪+.png",
            "bad%GG.png",
            "tail%.png",
            "tail%2.png",
            "bytes%FF%20.png",
        ];
        for name in attachments {
            std::fs::write(note_dir.join(name), name.as_bytes()).unwrap();
        }
        let note_path = note_dir.join("Entry.md");
        std::fs::write(
            &note_path,
            "# Entry\n\n![raw](café%20雪+.png)\n\
            [encoded](caf%C3%A9%20%E9%9B%AA+.png#preview)\n\
            ![malformed](bad%GG.png)\n![percent](tail%.png)\n\
            [short](tail%2.png)\n[invalid UTF-8](bytes%FF%20.png)\n",
        )
        .unwrap();

        let result = import(root.to_str().unwrap()).unwrap();
        assert_eq!(result.attachments_moved, attachments.len() as u64);
        let note = std::fs::read_to_string(&note_path).unwrap();
        for reference in [
            "![raw](../.ryuknotes/attachments/笔记/café%20雪+.png)",
            "[encoded](../.ryuknotes/attachments/笔记/café%20雪+.png#preview)",
            "![malformed](../.ryuknotes/attachments/笔记/bad%GG.png)",
            "![percent](../.ryuknotes/attachments/笔记/tail%.png)",
            "[short](../.ryuknotes/attachments/笔记/tail%2.png)",
            "[invalid UTF-8](../.ryuknotes/attachments/笔记/bytes%FF%20.png)",
        ] {
            assert!(note.contains(reference), "missing {reference:?} in {note}");
        }
        for name in attachments {
            assert!(!note_dir.join(name).exists());
            assert_eq!(
                std::fs::read(root.join(".ryuknotes/attachments/笔记").join(name)).unwrap(),
                name.as_bytes()
            );
        }
    }

    #[test]
    fn test_normalize_tags_comma_string() {
        let mapping: serde_yaml::Mapping = serde_yaml::from_str("tags: \"a, b, c\"").unwrap();
        let tags = normalize_tags(&mapping);
        assert_eq!(tags, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_normalize_tags_single_string() {
        let mapping: serde_yaml::Mapping = serde_yaml::from_str("tags: only-one").unwrap();
        let tags = normalize_tags(&mapping);
        assert_eq!(tags, vec!["only-one"]);
    }

    #[test]
    fn test_normalize_tags_array() {
        let mapping: serde_yaml::Mapping =
            serde_yaml::from_str("tags:\n  - alpha\n  - beta").unwrap();
        let tags = normalize_tags(&mapping);
        assert_eq!(tags, vec!["alpha", "beta"]);
    }

    #[test]
    fn test_normalize_tags_singular_tag_key() {
        let mapping: serde_yaml::Mapping = serde_yaml::from_str("tag: solo").unwrap();
        let tags = normalize_tags(&mapping);
        assert_eq!(tags, vec!["solo"]);
    }

    #[test]
    fn test_normalize_tags_merge_singular_and_plural() {
        let mapping: serde_yaml::Mapping =
            serde_yaml::from_str("tags:\n  - a\n  - b\ntag: c").unwrap();
        let tags = normalize_tags(&mapping);
        assert_eq!(tags, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_normalize_tags_dedup_case_insensitive() {
        let mapping: serde_yaml::Mapping = serde_yaml::from_str("tags: [Foo, foo]").unwrap();
        let tags = normalize_tags(&mapping);
        assert_eq!(tags, vec!["Foo"]);
    }

    #[test]
    fn test_normalize_tags_nested() {
        let mapping: serde_yaml::Mapping =
            serde_yaml::from_str("tags:\n  - project/work\n  - important").unwrap();
        let tags = normalize_tags(&mapping);
        assert_eq!(tags, vec!["project/work", "important"]);
    }

    #[test]
    fn test_split_anchor() {
        assert_eq!(split_anchor("Note"), ("Note", None));
        assert_eq!(split_anchor("Note#Heading"), ("Note", Some("Heading")));
        assert_eq!(split_anchor("Note#^block-id"), ("Note", Some("^block-id")));
        assert_eq!(split_anchor("Note#"), ("Note#", None));
    }

    #[test]
    fn test_is_dimension_spec() {
        assert!(is_dimension_spec("500"));
        assert!(is_dimension_spec("500x300"));
        assert!(is_dimension_spec("100X200"));
        assert!(!is_dimension_spec(""));
        assert!(!is_dimension_spec("alt text"));
        assert!(!is_dimension_spec("50px"));
    }

    #[test]
    fn test_convert_syntax_highlight() {
        let re = Regex::new(r"==([^\n=]+?)==").unwrap();
        let comment_re = Regex::new(r"(?s)%%([\s\S]*?)%%").unwrap();
        let input = "This is ==important== text.";
        let result = convert_syntax(input, &re, &comment_re);
        assert_eq!(result, "This is <mark>important</mark> text.");
    }

    #[test]
    fn test_convert_syntax_comment() {
        let re = Regex::new(r"==([^\n=]+?)==").unwrap();
        let comment_re = Regex::new(r"(?s)%%([\s\S]*?)%%").unwrap();
        let input = "Visible %%hidden note%% text";
        let result = convert_syntax(input, &re, &comment_re);
        assert_eq!(result, "Visible <!-- hidden note --> text");
    }

    #[test]
    fn test_convert_syntax_multiline_comment() {
        let re = Regex::new(r"==([^\n=]+?)==").unwrap();
        let comment_re = Regex::new(r"(?s)%%([\s\S]*?)%%").unwrap();
        let input = "%%line one\nline two%%";
        let result = convert_syntax(input, &re, &comment_re);
        assert_eq!(result, "<!-- line one\nline two -->");
    }

    #[test]
    fn test_convert_syntax_code_block_protection() {
        let re = Regex::new(r"==([^\n=]+?)==").unwrap();
        let comment_re = Regex::new(r"(?s)%%([\s\S]*?)%%").unwrap();
        let input = "```\n==not a highlight==\n```\n==real highlight==";
        let result = convert_syntax(input, &re, &comment_re);
        assert!(result.contains("==not a highlight=="));
        assert!(result.contains("<mark>real highlight</mark>"));
    }

    #[test]
    fn test_extract_heading_title() {
        assert_eq!(
            extract_heading_title("# My Title\n\nBody"),
            Some("My Title".to_string())
        );
        assert_eq!(
            extract_heading_title("\n\n# Spaced Title"),
            Some("Spaced Title".to_string())
        );
        assert_eq!(extract_heading_title("Body without heading"), None);
        assert_eq!(extract_heading_title("## Subheading"), None);
        assert_eq!(extract_heading_title(""), None);
    }

    #[test]
    fn test_normalize_frontmatter_generates_id() {
        let raw = "# Plain Note\n\nNo frontmatter.";
        let (meta, body) = normalize_frontmatter(raw, Path::new("Plain Note.md"));
        assert!(!meta.id.is_empty());
        assert_eq!(meta.title, "Plain Note");
        assert!(meta.tags.is_empty());
        assert!(!meta.pinned);
        assert!(body.contains("# Plain Note"));
    }

    #[test]
    fn test_normalize_frontmatter_extracts_heading_title() {
        let raw = "# My Real Title\n\nContent.";
        let (meta, _) = normalize_frontmatter(raw, Path::new("note1.md"));
        assert_eq!(meta.title, "My Real Title");
    }

    #[test]
    fn test_normalize_frontmatter_preserves_frontmatter_title() {
        let raw = "---\ntitle: Frontmatter Title\n---\n# Different Heading\n\nBody";
        let (meta, _) = normalize_frontmatter(raw, Path::new("file.md"));
        assert_eq!(meta.title, "Frontmatter Title");
    }

    #[test]
    fn test_normalize_frontmatter_tags_from_comma_string() {
        let raw = "---\ntags: a, b, c\n---\n# Note\n\nBody";
        let (meta, _) = normalize_frontmatter(raw, Path::new("note.md"));
        assert_eq!(meta.tags, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_normalize_tags_strips_hash() {
        let mapping: serde_yaml::Mapping = serde_yaml::from_str("tags:\n  - \"#hashed\"").unwrap();
        let tags = normalize_tags(&mapping);
        assert_eq!(tags, vec!["hashed"]);
    }

    #[test]
    fn test_pathdiff_same_dir() {
        assert_eq!(pathdiff("/a/b/c.md", "/a/b").unwrap(), "c.md");
    }

    #[test]
    fn test_pathdiff_subdir() {
        assert_eq!(pathdiff("/a/b/c.md", "/a").unwrap(), "b/c.md");
    }

    #[test]
    fn test_pathdiff_parent_dir() {
        assert_eq!(pathdiff("/a/c.md", "/a/b").unwrap(), "../c.md");
    }

    #[test]
    fn test_rename_deprecated_alias() {
        let raw = "---\nalias:\n  - Name1\n---\n# Note\n\nBody";
        let result = rename_deprecated_properties(raw);
        assert!(result.contains("aliases:"));
        assert!(!result.contains("\nalias:"));
    }

    #[test]
    fn test_rename_deprecated_cssclass() {
        let raw = "---\ncssclass: custom\n---\n# Note";
        let result = rename_deprecated_properties(raw);
        assert!(result.contains("cssclasses:"));
        assert!(!result.contains("\ncssclass:"));
    }

    #[test]
    fn test_rename_deprecated_no_override() {
        let raw = "---\naliases:\n  - A\nalias:\n  - B\n---\n# Note";
        let result = rename_deprecated_properties(raw);
        assert!(result.contains("aliases:"));
        assert!(!result.contains("\nalias:"));
    }

    #[test]
    fn test_rename_deprecated_no_change_needed() {
        let raw = "---\naliases: [A]\ncssclasses: [c]\n---\n# Note";
        let result = rename_deprecated_properties(raw);
        assert_eq!(result, raw);
    }

    #[test]
    fn import_keeps_colliding_attachments_and_rewrites_note_relative_links() {
        let vault = tempfile::tempdir().unwrap();
        let root = vault.path();
        let note_dir = root.join("Notes");
        let stored = root.join(".ryuknotes/attachments/Notes");
        std::fs::create_dir_all(&note_dir).unwrap();
        std::fs::create_dir_all(&stored).unwrap();
        std::fs::write(note_dir.join("pic.png"), b"NEW").unwrap();
        std::fs::write(stored.join("pic.png"), b"OLD").unwrap();
        std::fs::write(stored.join("pic-1.png"), b"OLDER").unwrap();
        std::fs::write(
            note_dir.join("Entry.md"),
            "# Entry\n\n![picture](pic.png) [picture](./pic.png#preview)",
        )
        .unwrap();

        let result = import(root.to_str().unwrap()).unwrap();

        assert_eq!(result.attachments_moved, 1);
        assert_eq!(std::fs::read(stored.join("pic.png")).unwrap(), b"OLD");
        assert_eq!(std::fs::read(stored.join("pic-1.png")).unwrap(), b"OLDER");
        assert_eq!(std::fs::read(stored.join("pic-2.png")).unwrap(), b"NEW");
        assert!(!note_dir.join("pic.png").exists());
        let note = std::fs::read_to_string(note_dir.join("Entry.md")).unwrap();
        assert!(note.contains("![picture](../.ryuknotes/attachments/Notes/pic-2.png)"));
        assert!(note.contains("[picture](../.ryuknotes/attachments/Notes/pic-2.png#preview)"));
    }

    #[test]
    fn import_prunes_metadata_directories_at_every_depth() {
        let vault = tempfile::tempdir().unwrap();
        let root = vault.path();
        for hidden in [".ryuknotes", ".obsidian", ".trash", ".stfolder", ".claude"] {
            for base in [root.to_path_buf(), root.join("Notebook")] {
                let hidden_dir = base.join(hidden).join("history");
                std::fs::create_dir_all(&hidden_dir).unwrap();
                std::fs::write(hidden_dir.join("Secret.md"), "==untouched==").unwrap();
                std::fs::write(hidden_dir.join("asset.bin"), b"untouched").unwrap();
            }
        }
        let visible_dir = root.join("Notebook/stfolder-visible");
        std::fs::create_dir_all(&visible_dir).unwrap();
        std::fs::write(visible_dir.join("asset.bin"), b"visible").unwrap();
        std::fs::write(
            root.join("Note.md"),
            "# Note\n\n![file](Notebook/stfolder-visible/asset.bin)",
        )
        .unwrap();

        let result = import(root.to_str().unwrap()).unwrap();

        assert_eq!(result.attachments_moved, 1);
        assert_eq!(
            std::fs::read(root.join(".ryuknotes/attachments/Notebook/stfolder-visible/asset.bin"))
                .unwrap(),
            b"visible"
        );
        for hidden in [".ryuknotes", ".obsidian", ".trash", ".stfolder", ".claude"] {
            for base in [root.to_path_buf(), root.join("Notebook")] {
                let hidden_dir = base.join(hidden).join("history");
                assert_eq!(
                    std::fs::read_to_string(hidden_dir.join("Secret.md")).unwrap(),
                    "==untouched=="
                );
                assert_eq!(
                    std::fs::read(hidden_dir.join("asset.bin")).unwrap(),
                    b"untouched"
                );
            }
        }
    }

    #[test]
    fn destination_io_failure_leaves_original_attachment_in_place() {
        let vault = tempfile::tempdir().unwrap();
        let root = vault.path();
        std::fs::create_dir(root.join(".ryuknotes")).unwrap();
        std::fs::write(root.join(".ryuknotes/attachments"), b"existing metadata").unwrap();
        std::fs::write(root.join("asset.bin"), b"original").unwrap();

        assert!(import(root.to_str().unwrap()).is_err());
        assert_eq!(std::fs::read(root.join("asset.bin")).unwrap(), b"original");
        assert_eq!(
            std::fs::read(root.join(".ryuknotes/attachments")).unwrap(),
            b"existing metadata"
        );
    }

    #[test]
    fn readonly_note_does_not_lose_original_or_attachment() {
        let vault = tempfile::tempdir().unwrap();
        let root = vault.path();
        let note = root.join("Note.md");
        let original = "# Note\n\n![asset](asset.bin)";
        std::fs::write(&note, original).unwrap();
        std::fs::write(root.join("asset.bin"), b"original").unwrap();
        let original_permissions = std::fs::metadata(&note).unwrap().permissions();
        let mut permissions = original_permissions.clone();
        permissions.set_readonly(true);
        std::fs::set_permissions(&note, permissions).unwrap();

        let result = import(root.to_str().unwrap());

        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(&note).unwrap(), original);
        assert_eq!(std::fs::read(root.join("asset.bin")).unwrap(), b"original");
        std::fs::set_permissions(&note, original_permissions).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn attachment_directory_symlink_cannot_escape_vault() {
        let vault = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = vault.path();
        std::fs::create_dir(root.join(".ryuknotes")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join(".ryuknotes/attachments")).unwrap();
        std::fs::write(root.join("asset.bin"), b"original").unwrap();

        assert!(import(root.to_str().unwrap()).is_err());
        assert_eq!(std::fs::read(root.join("asset.bin")).unwrap(), b"original");
        assert!(!outside.path().join("asset.bin").exists());
    }

    #[test]
    fn test_split_anchor_same_note() {
        let (file, anchor) = split_anchor("#Heading");
        assert_eq!(file, "");
        assert_eq!(anchor, Some("Heading"));
    }
}
