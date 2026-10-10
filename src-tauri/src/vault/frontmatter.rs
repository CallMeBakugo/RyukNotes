use crate::types::NoteMeta;
use chrono::{NaiveDate, NaiveDateTime, Utc};
use gray_matter::engine::{Engine, YAML as UnboundedYaml};
use gray_matter::{Matter, Pod};
use serde::{Deserialize, Serialize};

const MAX_FRONTMATTER_BYTES: usize = 64 * 1024;
const MAX_FRONTMATTER_ALIASES: usize = 16;
const MAX_FRONTMATTER_INDENT: usize = 200;
const MAX_FRONTMATTER_INDICATORS: usize = 32;

/// YAML frontmatter engine for notes, which are untrusted input (sync, import, shared vaults).
/// The underlying loader expands every alias by copying and recurses once per nesting level, so a
/// note of a few hundred bytes could take gigabytes or overflow the stack and take the app down
/// at every start. Frontmatter outside these bounds is treated as carrying no metadata.
pub struct BoundedYaml;

impl Engine for BoundedYaml {
    fn parse(content: &str) -> Pod {
        if frontmatter_within_bounds(content) {
            UnboundedYaml::parse(content)
        } else {
            Pod::Null
        }
    }
}

fn frontmatter_within_bounds(content: &str) -> bool {
    if content.len() > MAX_FRONTMATTER_BYTES {
        return false;
    }
    // Aliases multiply: with few of them the expansion stays small whatever they point at.
    let bytes = content.as_bytes();
    let aliases = bytes
        .iter()
        .enumerate()
        .filter(|&(index, &byte)| {
            byte == b'*'
                && (index == 0
                    || matches!(
                        bytes[index - 1],
                        b' ' | b'\t' | b'\n' | b'[' | b',' | b':' | b'-' | b'{'
                    ))
                && bytes
                    .get(index + 1)
                    .is_some_and(|next| next.is_ascii_alphanumeric() || matches!(next, b'_' | b'-'))
        })
        .count();
    if aliases > MAX_FRONTMATTER_ALIASES {
        return false;
    }
    // Block nesting deepens by indentation, or by indicators chained on one line (`- - - x`).
    content.lines().all(|line| {
        let indent = line.len() - line.trim_start_matches(' ').len();
        let mut rest = &line.as_bytes()[indent..];
        let mut indicators = 0;
        while let [b'-' | b'?' | b':', b' ' | b'\t', tail @ ..] = rest {
            indicators += 1;
            rest = tail;
            while let [b' ' | b'\t', tail @ ..] = rest {
                rest = tail;
            }
        }
        indent <= MAX_FRONTMATTER_INDENT && indicators <= MAX_FRONTMATTER_INDICATORS
    })
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct RawFrontmatter {
    id: Option<String>,
    title: Option<String>,
    tags: Option<Vec<String>>,
    pinned: Option<bool>,
    created: Option<String>,
    modified: Option<String>,
}

/// Try to parse a date string in multiple common formats.
pub fn parse_date_flexible(s: &str) -> Option<chrono::DateTime<Utc>> {
    let s = s.trim();
    // RFC 3339 / ISO 8601 with timezone (e.g. 2024-01-15T10:30:00+00:00)
    if let Ok(dt) = s.parse::<chrono::DateTime<Utc>>() {
        return Some(dt);
    }
    // Try parsing as DateTime with fixed offset then convert
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    // YYYY-MM-DD HH:MM:SS (no timezone)
    if let Ok(ndt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        return Some(ndt.and_utc());
    }
    // YYYY-MM-DDTHH:MM:SS (no timezone, with T separator)
    if let Ok(ndt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        return Some(ndt.and_utc());
    }
    // YYYY-MM-DD (date only)
    if let Ok(nd) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return nd.and_hms_opt(0, 0, 0).map(|ndt| ndt.and_utc());
    }
    None
}

/// Reads the known keys one by one. Frontmatter written by other editors is often shaped a little
/// differently (`tags: work`, `pinned: yes`, a numeric title or id); reading it as one strict
/// record dropped every field when a single one did not fit, and the next save then wrote a new id,
/// the file name as title and no tags over the originals.
fn read_frontmatter(data: Option<Pod>) -> RawFrontmatter {
    let Some(serde_json::Value::Object(fields)) =
        data.and_then(|data| data.deserialize::<serde_json::Value>().ok())
    else {
        return RawFrontmatter::default();
    };
    let text = |value: &serde_json::Value| match value {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Number(number) => Some(number.to_string()),
        serde_json::Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    };
    let tags = fields.get("tags").map(|value| match value {
        serde_json::Value::Array(items) => items.iter().filter_map(text).collect(),
        // `tags: work` and `tags: a, b` are common outside this app.
        other => text(other)
            .map(|joined| {
                joined
                    .split(',')
                    .map(|tag| tag.trim().to_string())
                    .filter(|tag| !tag.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
    });
    let pinned = fields.get("pinned").and_then(|value| match value {
        serde_json::Value::Bool(flag) => Some(*flag),
        serde_json::Value::Number(number) => Some(number.as_f64() != Some(0.0)),
        serde_json::Value::String(word) => match word.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => Some(true),
            "false" | "no" | "off" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    });
    RawFrontmatter {
        id: fields.get("id").and_then(text),
        title: fields.get("title").and_then(text),
        tags,
        pinned,
        created: fields.get("created").and_then(text),
        modified: fields.get("modified").and_then(text),
    }
}

pub fn parse_note(raw: &str, filename: &str) -> (NoteMeta, String) {
    let matter = Matter::<BoundedYaml>::new();
    let result = matter.parse(raw);

    let fm = read_frontmatter(result.data);

    let title = fm.title.unwrap_or_else(|| filename_to_title(filename));

    let created = fm
        .created
        .and_then(|s| parse_date_flexible(&s))
        .unwrap_or_else(Utc::now);

    let modified = fm
        .modified
        .and_then(|s| parse_date_flexible(&s))
        .unwrap_or_else(Utc::now);

    // Don't generate UUID on read - empty string signals "no ID yet"
    let id = fm.id.unwrap_or_default();

    let meta = NoteMeta {
        id,
        title,
        tags: fm.tags.unwrap_or_default(),
        pinned: fm.pinned.unwrap_or(false),
        created,
        modified,
    };

    let content = result.content;
    (meta, content)
}

/// Ensure the body starts with a top-level heading for external app compatibility, adding
/// `# Title` when it has none. A body that already opens with a top-level heading is left alone
/// even when that heading differs from the title: adding another one above it stacked a new
/// heading on every title change (`# New`, `# Old`, ...).
fn ensure_title_heading(body: &str, title: &str) -> String {
    let trimmed = body.trim_start();

    if trimmed.starts_with("# ") {
        return body.to_string();
    }

    // Don't add heading to empty notes (new/blank notes)
    if trimmed.is_empty() {
        return body.to_string();
    }

    // Prepend `# Title` heading
    format!("# {}\n\n{}", title, body)
}

/// The frontmatter block for a note that has none yet. Written through the YAML serializer like
/// `merge_frontmatter`: assembled by hand, a title with a backslash or a tag such as `2024`,
/// `true` or `a: b` came out as YAML the app could not read back.
pub fn serialize_frontmatter(meta: &NoteMeta) -> String {
    let text = |value: &str| serde_yaml::Value::String(value.to_string());
    let mut mapping = serde_yaml::Mapping::new();
    mapping.insert(text("id"), text(&meta.id));
    mapping.insert(text("title"), text(&meta.title));
    mapping.insert(
        text("tags"),
        serde_yaml::Value::Sequence(meta.tags.iter().map(|tag| text(tag)).collect()),
    );
    mapping.insert(text("pinned"), serde_yaml::Value::Bool(meta.pinned));
    mapping.insert(text("created"), text(&meta.created.to_rfc3339()));
    mapping.insert(text("modified"), text(&meta.modified.to_rfc3339()));
    let yaml = serde_yaml::to_string(&serde_yaml::Value::Mapping(mapping))
        .expect("strings, booleans and a list always serialize");
    format!("---\n{yaml}---\n")
}

pub fn update_note_raw(meta: &NoteMeta, body: &str) -> String {
    let fm = serialize_frontmatter(meta);
    let body_with_heading = ensure_title_heading(body, &meta.title);
    format!("{}{}", fm, body_with_heading)
}

/// Merge NoteMeta fields into existing frontmatter, preserving unknown YAML keys.
/// Falls back to serialize_frontmatter() if the original has no frontmatter.
pub fn merge_frontmatter(original_raw: &str, meta: &NoteMeta, body: &str) -> String {
    let matter = Matter::<BoundedYaml>::new();
    let parsed = matter.parse(original_raw);

    // If original had no frontmatter, use the simple serializer
    if parsed.matter.trim().is_empty() {
        return update_note_raw(meta, body);
    }

    // Parse existing YAML into a Mapping to preserve all keys
    let mut mapping: serde_yaml::Mapping = match serde_yaml::from_str(&parsed.matter) {
        Ok(m) => m,
        Err(_) => return update_note_raw(meta, body), // unparseable YAML, fall back
    };

    // Update only our known fields
    if !meta.id.is_empty() {
        mapping.insert(
            serde_yaml::Value::String("id".into()),
            serde_yaml::Value::String(meta.id.clone()),
        );
    }
    mapping.insert(
        serde_yaml::Value::String("title".into()),
        serde_yaml::Value::String(meta.title.clone()),
    );
    mapping.insert(
        serde_yaml::Value::String("tags".into()),
        serde_yaml::Value::Sequence(
            meta.tags
                .iter()
                .map(|t| serde_yaml::Value::String(t.clone()))
                .collect(),
        ),
    );
    mapping.insert(
        serde_yaml::Value::String("pinned".into()),
        serde_yaml::Value::Bool(meta.pinned),
    );
    mapping.insert(
        serde_yaml::Value::String("created".into()),
        serde_yaml::Value::String(meta.created.to_rfc3339()),
    );
    mapping.insert(
        serde_yaml::Value::String("modified".into()),
        serde_yaml::Value::String(meta.modified.to_rfc3339()),
    );

    // Serialize the mapping back to YAML
    let yaml_str = match serde_yaml::to_string(&serde_yaml::Value::Mapping(mapping)) {
        Ok(s) => s,
        Err(_) => return update_note_raw(meta, body),
    };

    let body_with_heading = ensure_title_heading(body, &meta.title);
    format!("---\n{}---\n{}", yaml_str, body_with_heading)
}

pub fn filename_to_title(filename: &str) -> String {
    let stem = filename.trim_end_matches(".md");
    // Only replace dashes/underscores acting as word separators (between non-space chars).
    // Keep dashes that are surrounded by spaces (e.g. "Title - Subtitle").
    let mut result = String::with_capacity(stem.len());
    let chars: Vec<char> = stem.chars().collect();
    for (i, &ch) in chars.iter().enumerate() {
        if (ch == '-' || ch == '_')
            && i > 0
            && i < chars.len() - 1
            && chars[i - 1] != ' '
            && chars[i + 1] != ' '
        {
            result.push(' ');
        } else {
            result.push(ch);
        }
    }
    result
}

/// Quick title extraction from raw note content (frontmatter only, no full parse)
pub fn extract_title(raw: &str) -> Option<String> {
    let matter = Matter::<BoundedYaml>::new();
    let result = matter.parse(raw);
    read_frontmatter(result.data).title
}

pub fn extract_preview(content: &str, max_len: usize) -> String {
    // Filter out headings and empty lines BEFORE stripping markdown,
    // since strip_html_and_markdown collapses all whitespace into one line.
    let filtered: String = content
        .lines()
        .filter(|l| {
            let trimmed = l.trim();
            !trimmed.is_empty()
                && !trimmed.starts_with('#')
                && !trimmed.starts_with("---")
                && !trimmed.starts_with("```")
        })
        .take(5)
        .collect::<Vec<_>>()
        .join("\n");

    let text = strip_html_and_markdown(&filtered);
    let trimmed = text.trim().to_string();

    if trimmed.len() > max_len {
        let mut end = max_len;
        while end > 0 && !trimmed.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &trimmed[..end])
    } else {
        trimmed
    }
}

fn strip_html_and_markdown(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_tag = false;

    while let Some(ch) = chars.next() {
        if in_tag {
            if ch == '>' {
                in_tag = false;
                // Add a space after closing tags to separate words
                result.push(' ');
            }
            continue;
        }

        if ch == '<' {
            in_tag = true;
            continue;
        }

        // Skip markdown image syntax: ![alt](url)
        if ch == '!' && chars.peek() == Some(&'[') {
            // Consume ![...](...) entirely
            chars.next(); // skip '['
            let mut depth = 1;
            // Skip alt text
            for c in chars.by_ref() {
                if c == '[' {
                    depth += 1;
                }
                if c == ']' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
            }
            // Skip (url) if present
            if chars.peek() == Some(&'(') {
                chars.next();
                let mut depth = 1;
                for c in chars.by_ref() {
                    if c == '(' {
                        depth += 1;
                    }
                    if c == ')' {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
            }
            continue;
        }

        // Markdown links: [text](url) - keep the text, skip the URL
        if ch == '[' {
            let mut link_text = String::new();
            let mut depth = 1;
            for c in chars.by_ref() {
                if c == '[' {
                    depth += 1;
                }
                if c == ']' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                link_text.push(c);
            }
            // Skip (url) if present
            if chars.peek() == Some(&'(') {
                chars.next();
                let mut depth = 1;
                for c in chars.by_ref() {
                    if c == '(' {
                        depth += 1;
                    }
                    if c == ')' {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
                result.push_str(&link_text);
            } else {
                // Not a link, just brackets
                result.push('[');
                result.push_str(&link_text);
                result.push(']');
            }
            continue;
        }

        // Strip bold/italic markers
        if ch == '*' || ch == '_' {
            // Skip consecutive * or _
            while chars.peek() == Some(&ch) {
                chars.next();
            }
            continue;
        }

        // Strip strikethrough ~~
        if ch == '~' && chars.peek() == Some(&'~') {
            chars.next();
            continue;
        }

        // Strip inline code backticks
        if ch == '`' {
            while chars.peek() == Some(&'`') {
                chars.next();
            }
            continue;
        }

        result.push(ch);
    }

    // Collapse multiple spaces/newlines into single spaces
    let mut collapsed = String::with_capacity(result.len());
    let mut last_was_space = false;
    for ch in result.chars() {
        if ch.is_whitespace() {
            if !last_was_space {
                collapsed.push(' ');
                last_was_space = true;
            }
        } else {
            collapsed.push(ch);
            last_was_space = false;
        }
    }

    collapsed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_preview_basic() {
        let content =
            "## SIM Kaarten\n\n### Artjom\n\n**Telefoonnummer**\n\n**== 0456 74 49 91 ==**\n";
        let preview = extract_preview(content, 120);
        eprintln!("Preview: {:?}", preview);
        assert!(!preview.is_empty(), "Preview should not be empty");
    }

    #[test]
    fn test_extract_preview_no_frontmatter() {
        let content = "# Android flash\n\nAndroid flash\n\n[TWRP](https://twrp.me/)\n\nNotes;\n\n- Samsung S6\n";
        let preview = extract_preview(content, 120);
        eprintln!("Preview: {:?}", preview);
        assert!(!preview.is_empty(), "Preview should not be empty");
    }

    #[test]
    fn test_strip_html_markdown() {
        let input = "**Telefoonnummer**\n\n**== 0456 ==**";
        let stripped = strip_html_and_markdown(input);
        eprintln!("Stripped: {:?}", stripped);
        assert!(stripped.contains("Telefoonnummer"));
    }
    fn alias_bomb(levels: usize, width: usize) -> String {
        let mut note = format!(
            "---\ntitle: Bomb\na0: &a0 [{}]\n",
            vec!["1"; width].join(",")
        );
        for level in 1..levels {
            let alias = format!("*a{}", level - 1);
            note.push_str(&format!(
                "a{level}: &a{level} [{}]\n",
                vec![alias.as_str(); width].join(",")
            ));
        }
        note.push_str("---\nbody\n");
        note
    }

    /// Runs on a stack the size of the app's worker threads, where the unbounded loader overflowed.
    fn on_worker_stack<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
        std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(work)
            .unwrap()
            .join()
            .unwrap()
    }

    #[test]
    fn hostile_frontmatter_is_read_as_a_note_without_metadata() {
        let nested_on_one_line = format!("---\nt: {}x\n---\nbody\n", "- ".repeat(2400));
        let mut nested_by_indent = String::from("---\nt:\n");
        for depth in 0..400 {
            note_line(&mut nested_by_indent, depth);
        }
        nested_by_indent.push_str("---\nbody\n");
        let oversized = format!(
            "---\ntitle: Big\nfiller: \"{}\"\n---\nbody\n",
            "x".repeat(MAX_FRONTMATTER_BYTES)
        );
        for note in [
            alias_bomb(10, 8),
            alias_bomb(40, 2),
            nested_on_one_line,
            nested_by_indent,
            oversized,
        ] {
            let started = std::time::Instant::now();
            let (title, content, extracted, merged) = on_worker_stack(move || {
                let (meta, content) = parse_note(&note, "Fallback Name.md");
                let merged = merge_frontmatter(&note, &meta, &content);
                (meta.title, content, extract_title(&note), merged)
            });
            assert!(started.elapsed() < std::time::Duration::from_secs(2));
            // No metadata is taken from it, and the body is still split off as usual.
            assert_eq!(title, "Fallback Name");
            assert_eq!(content, "body");
            assert_eq!(extracted, None);
            assert!(
                merged.ends_with("body"),
                "{}",
                &merged[merged.len().saturating_sub(40)..]
            );
        }
    }

    fn note_line(note: &mut String, depth: usize) {
        note.push_str(&" ".repeat(depth));
        note.push_str("-\n");
    }

    #[test]
    fn ordinary_frontmatter_is_unaffected_by_the_bounds() {
        let (meta, content) = parse_note(
            "---\nid: abc\ntitle: \"*Starred* title: 5*3\"\ntags:\n  - one\n  - two\npinned: true\n---\n# Body\n",
            "x.md",
        );
        assert_eq!(meta.id, "abc");
        assert_eq!(meta.title, "*Starred* title: 5*3");
        assert_eq!(meta.tags, vec!["one", "two"]);
        assert!(meta.pinned);
        assert_eq!(content, "# Body");
        // A few aliases and modest nesting are ordinary YAML.
        let (meta, _) = parse_note(
            "---\ntitle: Shared\nbase: &base [a, b]\ntags: *base\nextra:\n  - - nested\n    - list\n---\nbody\n",
            "x.md",
        );
        assert_eq!(meta.title, "Shared");
        assert_eq!(meta.tags, vec!["a", "b"]);
        assert!(frontmatter_within_bounds(&format!(
            "title: {}",
            "x".repeat(MAX_FRONTMATTER_BYTES - 16)
        )));
    }
    #[test]
    fn one_field_of_another_shape_no_longer_drops_the_rest() {
        let note = |extra: &str| format!("---\nid: keep-me\ntitle: Kept\n{extra}\n---\nbody\n");
        let (meta, _) = parse_note(&note("tags: work"), "File.md");
        assert_eq!((meta.id.as_str(), meta.title.as_str()), ("keep-me", "Kept"));
        assert_eq!(meta.tags, vec!["work"]);
        let (meta, _) = parse_note(&note("tags: a, b ,c\npinned: yes"), "File.md");
        assert_eq!(meta.tags, vec!["a", "b", "c"]);
        assert!(meta.pinned);
        assert_eq!(meta.id, "keep-me");
        // Numbers where text is expected are read as text.
        let (meta, _) = parse_note(
            "---\nid: 20231014\ntitle: 2024\ntags: [1, two, true]\npinned: 0\n---\nbody\n",
            "File.md",
        );
        assert_eq!(
            (meta.id.as_str(), meta.title.as_str()),
            ("20231014", "2024")
        );
        assert_eq!(meta.tags, vec!["1", "two", "true"]);
        assert!(!meta.pinned);
        assert_eq!(
            extract_title("---\ntitle: 2024\ntags: work\n---\nbody\n").as_deref(),
            Some("2024")
        );
        // A field that makes no sense is skipped on its own.
        let (meta, _) = parse_note(&note("tags: {a: b}\npinned: maybe"), "File.md");
        assert_eq!(meta.id, "keep-me");
        assert!(meta.tags.is_empty());
        assert!(!meta.pinned);
        // Saving keeps the identity and writes the tags as a list.
        let (meta, content) = parse_note(&note("tags: work\naliases: [x]"), "File.md");
        let saved = merge_frontmatter(&note("tags: work\naliases: [x]"), &meta, &content);
        let (again, _) = parse_note(&saved, "File.md");
        assert_eq!(
            (again.id.as_str(), again.title.as_str()),
            ("keep-me", "Kept")
        );
        assert_eq!(again.tags, vec!["work"]);
        assert!(saved.contains("aliases:"));
    }

    #[test]
    fn new_frontmatter_reads_back_whatever_the_title_and_tags_contain() {
        for (title, tags) in [
            ("C:\\Users\\tom", vec!["2024", "true"]),
            ("Plain", vec!["foo: bar", "a,b"]),
            ("Say \"hi\" and 'bye'", vec!["#hash", "- dash"]),
            ("\\sum x", vec!["null", "~", "1.5"]),
            (
                "Tab\there: colon # hash",
                vec!["[x]", "{y}", "*star", "&amp"],
            ),
            ("- leading dash", vec![]),
            ("", vec!["yes"]),
        ] {
            let meta = NoteMeta {
                id: "the-id".into(),
                title: title.into(),
                tags: tags.iter().map(|tag| tag.to_string()).collect(),
                pinned: true,
                created: Utc::now(),
                modified: Utc::now(),
            };
            let raw = update_note_raw(&meta, "body\n");
            let (back, content) = parse_note(&raw, "Fallback.md");
            assert_eq!(back.id, "the-id", "{raw}");
            assert_eq!(back.title, title, "{raw}");
            assert_eq!(back.tags, meta.tags, "{raw}");
            assert!(back.pinned);
            assert_eq!(back.created.timestamp(), meta.created.timestamp());
            assert!(content.ends_with("body"));
        }
    }

    #[test]
    fn a_title_heading_is_added_only_when_the_body_has_no_top_level_heading() {
        let meta = |title: &str| NoteMeta {
            id: "id".into(),
            title: title.into(),
            tags: vec![],
            pinned: false,
            created: Utc::now(),
            modified: Utc::now(),
        };
        let body_of = |raw: String| parse_note(&raw, "x.md").1;
        // No heading yet: the title becomes one.
        assert_eq!(
            body_of(update_note_raw(&meta("New"), "Text.\n")),
            "# New\n\nText."
        );
        // A heading that differs from the title is not buried under a second one.
        assert_eq!(
            body_of(update_note_raw(&meta("New"), "# Old\n\nText.\n")),
            "# Old\n\nText."
        );
        let original = "---\nid: id\ntitle: Old\n---\n# Old\n\nText.\n";
        assert_eq!(
            body_of(merge_frontmatter(
                original,
                &meta("New"),
                "# Old\n\nText.\n"
            )),
            "# Old\n\nText."
        );
        // Saving again and again never grows the note.
        let once = merge_frontmatter(original, &meta("New"), "# Old\n\nText.\n");
        let (_, body) = parse_note(&once, "x.md");
        assert_eq!(
            body_of(merge_frontmatter(&once, &meta("Newer"), &body)),
            "# Old\n\nText."
        );
        // Lower-level headings and empty notes behave as before.
        assert_eq!(
            body_of(update_note_raw(&meta("New"), "## Section\n")),
            "# New\n\n## Section"
        );
        assert!(update_note_raw(&meta("New"), "\n").ends_with("---\n\n"));
    }
}
