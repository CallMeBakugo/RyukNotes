use crate::types::{
    NoteContent, NoteEntry, NoteMeta, NoteTitleEntry, NotebookEntry, TagStyle, TagStyleChange,
    TrashContents, TrashNotebookEntry, VaultState,
};
use crate::vault::frontmatter;
use chrono::{DateTime, Local, Locale, Utc};
use rayon::prelude::*;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use uuid::Uuid;
use walkdir::WalkDir;

pub fn helixnotes_dir(vault_path: &str) -> PathBuf {
    Path::new(vault_path).join(".helixnotes")
}

fn canonicalize_path(path: &Path, label: &str) -> Result<PathBuf, String> {
    fs::canonicalize(path).map_err(|error| format!("Invalid {label}: {error}"))
}

fn ensure_vault_content_path(
    vault_path: &str,
    requested_path: &Path,
    allow_root: bool,
) -> Result<PathBuf, String> {
    let vault = canonicalize_path(Path::new(vault_path), "vault path")?;
    let requested = canonicalize_path(requested_path, "vault item path")?;
    let metadata = vault.join(".helixnotes");

    if !requested.starts_with(&vault)
        || requested.starts_with(&metadata)
        || (!allow_root && requested == vault)
    {
        return Err("Path must stay inside the active vault".to_string());
    }

    Ok(requested_path.to_path_buf())
}

fn ensure_vault_content_dir(vault_path: &str, requested_path: &Path) -> Result<PathBuf, String> {
    let requested = ensure_vault_content_path(vault_path, requested_path, true)?;
    if !requested.is_dir() {
        return Err("Vault destination is not a directory".to_string());
    }
    Ok(requested)
}

fn ensure_note_path(vault_path: &str, requested_path: &Path) -> Result<PathBuf, String> {
    let requested = ensure_vault_content_path(vault_path, requested_path, false)?;
    if !requested.is_file()
        || requested
            .extension()
            .and_then(|extension| extension.to_str())
            != Some("md")
    {
        return Err("Note path must point to a Markdown file".to_string());
    }
    Ok(requested)
}

fn ensure_readable_note_path(vault_path: &str, requested_path: &Path) -> Result<PathBuf, String> {
    if let Ok(note) = ensure_note_path(vault_path, requested_path) {
        return Ok(note);
    }

    let trashed_note = ensure_trash_entry(vault_path, requested_path)?;
    if !trashed_note.is_file()
        || trashed_note
            .extension()
            .and_then(|extension| extension.to_str())
            != Some("md")
    {
        return Err("Note path must point to a Markdown file".to_string());
    }
    Ok(trashed_note)
}

fn ensure_notebook_path(vault_path: &str, requested_path: &Path) -> Result<PathBuf, String> {
    let requested = ensure_vault_content_path(vault_path, requested_path, false)?;
    if !requested.is_dir() {
        return Err("Notebook path must point to a directory".to_string());
    }
    Ok(requested)
}

fn ensure_trash_entry(vault_path: &str, requested_path: &Path) -> Result<PathBuf, String> {
    let trash = canonicalize_path(&helixnotes_dir(vault_path).join("trash"), "trash path")?;
    let requested = canonicalize_path(requested_path, "trash item path")?;
    if requested == trash || !requested.starts_with(&trash) {
        return Err("Path must be an item inside the active vault trash".to_string());
    }
    Ok(requested_path.to_path_buf())
}

fn safe_relative_path(path: &str) -> Result<&Path, String> {
    let relative = Path::new(path);
    if relative.as_os_str().is_empty()
        || !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err("Path must be a safe vault-relative path".to_string());
    }
    Ok(relative)
}

fn safe_child_name(name: &str) -> Result<&str, String> {
    let mut components = Path::new(name).components();
    if name.trim().is_empty()
        || !matches!(components.next(), Some(Component::Normal(_)))
        || components.next().is_some()
    {
        return Err("Name must not contain path separators".to_string());
    }
    Ok(name)
}

// A normal rename can replace an existing file (or an empty directory).
// Use the OS's exclusive rename primitive instead; checking exists first is racy.
#[cfg(target_os = "linux")]
fn rename_exclusive(src: &Path, dest: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    unsafe extern "C" {
        fn renameat2(
            olddirfd: i32,
            oldpath: *const std::ffi::c_char,
            newdirfd: i32,
            newpath: *const std::ffi::c_char,
            flags: u32,
        ) -> i32;
    }
    let old = CString::new(src.as_os_str().as_bytes())
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    let new = CString::new(dest.as_os_str().as_bytes())
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    // AT_FDCWD and RENAME_NOREPLACE
    if unsafe { renameat2(-100, old.as_ptr(), -100, new.as_ptr(), 1) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(target_os = "android")]
fn rename_exclusive(src: &Path, dest: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let old = CString::new(src.as_os_str().as_bytes())
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    let new = CString::new(dest.as_os_str().as_bytes())
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    // Bionic exports renameat2 only on newer Android releases (minSdk is 24).
    // The kernel syscall is available independently; ENOSYS safely rejects
    // the move on old kernels rather than falling back to a clobbering rename.
    if unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            old.as_ptr(),
            libc::AT_FDCWD,
            new.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } == 0
    {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn rename_exclusive(src: &Path, dest: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    unsafe extern "C" {
        fn renameatx_np(
            olddirfd: i32,
            oldpath: *const std::ffi::c_char,
            newdirfd: i32,
            newpath: *const std::ffi::c_char,
            flags: u32,
        ) -> i32;
    }
    let old = CString::new(src.as_os_str().as_bytes())
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    let new = CString::new(dest.as_os_str().as_bytes())
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    // AT_FDCWD and RENAME_EXCL
    if unsafe { renameatx_np(-2, old.as_ptr(), -2, new.as_ptr(), 4) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn rename_exclusive(src: &Path, dest: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn MoveFileW(old: *const u16, new: *const u16) -> i32;
    }
    let old: Vec<u16> = src.as_os_str().encode_wide().chain(Some(0)).collect();
    let new: Vec<u16> = dest.as_os_str().encode_wide().chain(Some(0)).collect();
    if unsafe { MoveFileW(old.as_ptr(), new.as_ptr()) } != 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn move_without_replacing(src: &Path, dest: &Path) -> Result<(), String> {
    if src == dest {
        return Ok(());
    }
    // Case-insensitive volumes resolve two differently-spelled paths to the
    // same object. Move via a unique sibling to permit a case-only rename.
    let same_object = fs::canonicalize(dest)
        .and_then(|target| fs::canonicalize(src).map(|source| source == target))
        .unwrap_or(false);
    if same_object {
        let parent = src.parent().ok_or("Item has no parent directory")?;
        let staging = loop {
            let candidate = parent.join(format!(".helixnotes-rename-{}", Uuid::new_v4()));
            match rename_exclusive(src, &candidate) {
                Ok(()) => break candidate,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.to_string()),
            }
        };
        if let Err(error) = rename_exclusive(&staging, dest) {
            return Err(match rename_exclusive(&staging, src) {
                Ok(()) => error.to_string(),
                Err(rollback) => {
                    format!("{error}; item remains at {}: {rollback}", staging.display())
                }
            });
        }
        return Ok(());
    }
    rename_exclusive(src, dest).map_err(|error| error.to_string())
}

fn trash_name(original: &str) -> String {
    format!(
        "{}_{}_{}",
        Utc::now().format("%Y%m%d%H%M%S%3f"),
        Uuid::new_v4(),
        original
    )
}

fn original_trash_name(name: &str) -> &str {
    let (prefix, rest) = if name.len() > 18
        && name.as_bytes()[..17].iter().all(u8::is_ascii_digit)
        && name.as_bytes()[17] == b'_'
    {
        (18, &name[18..])
    } else if name.len() > 15
        && name.as_bytes()[..14].iter().all(u8::is_ascii_digit)
        && name.as_bytes()[14] == b'_'
    {
        (15, &name[15..])
    } else {
        return name;
    };
    if prefix == 18
        && rest.len() > 37
        && rest.as_bytes()[36] == b'_'
        && Uuid::parse_str(&rest[..36]).is_ok()
    {
        &rest[37..]
    } else {
        rest
    }
}

pub fn ensure_vault_structure(vault_path: &str) -> Result<(), String> {
    let hn_dir = helixnotes_dir(vault_path);
    fs::create_dir_all(hn_dir.join("trash")).map_err(|e| e.to_string())?;
    fs::create_dir_all(hn_dir.join("attachments")).map_err(|e| e.to_string())?;

    let config_path = hn_dir.join("config.json");
    if !config_path.exists() {
        let config = serde_json::json!({
            "version": "0.1.0",
            "created": Utc::now().to_rfc3339()
        });
        fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap())
            .map_err(|e| e.to_string())?;
    }

    let state_path = hn_dir.join("state.json");
    if !state_path.exists() {
        let state = VaultState::default();
        fs::write(&state_path, serde_json::to_string_pretty(&state).unwrap())
            .map_err(|e| e.to_string())?;
    }

    let gitignore_path = hn_dir.join(".gitignore");
    if !gitignore_path.exists() {
        fs::write(&gitignore_path, "trash/\nindex.json\nstate.json\n")
            .map_err(|e| e.to_string())?;
    }

    Ok(())
}

fn digit_run(value: &[u8], start: usize) -> (usize, &[u8]) {
    let mut end = start;
    while end < value.len() && value[end].is_ascii_digit() {
        end += 1;
    }
    let mut significant = start;
    while significant + 1 < end && value[significant] == b'0' {
        significant += 1;
    }
    (end, &value[significant..end])
}

fn compare_natural_names(left: &str, right: &str) -> Ordering {
    let left_lower = left.to_lowercase();
    let right_lower = right.to_lowercase();
    let left = left_lower.as_bytes();
    let right = right_lower.as_bytes();
    let (mut left_index, mut right_index) = (0, 0);
    let mut leading_zero_order = Ordering::Equal;

    while left_index < left.len() && right_index < right.len() {
        if left[left_index].is_ascii_digit() && right[right_index].is_ascii_digit() {
            let (left_end, left_number) = digit_run(left, left_index);
            let (right_end, right_number) = digit_run(right, right_index);
            let number_order = left_number
                .len()
                .cmp(&right_number.len())
                .then_with(|| left_number.cmp(right_number));
            if number_order != Ordering::Equal {
                return number_order;
            }
            if leading_zero_order == Ordering::Equal {
                leading_zero_order = (left_end - left_index).cmp(&(right_end - right_index));
            }
            left_index = left_end;
            right_index = right_end;
        } else {
            let character_order = left[left_index].cmp(&right[right_index]);
            if character_order != Ordering::Equal {
                return character_order;
            }
            left_index += 1;
            right_index += 1;
        }
    }

    match (left_index == left.len(), right_index == right.len()) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        _ => leading_zero_order,
    }
}

pub fn scan_notebooks(vault_path: &str) -> Result<Vec<NotebookEntry>, String> {
    let root = Path::new(vault_path);
    if !root.exists() {
        return Err("Vault path does not exist".to_string());
    }
    Ok(scan_dir_recursive(root, vault_path))
}

fn scan_dir_recursive(dir: &Path, vault_root: &str) -> Vec<NotebookEntry> {
    let root = Path::new(vault_root);

    let Ok(read_dir) = fs::read_dir(dir) else {
        return Vec::new();
    };

    // Collect subdirs (root level only lists directories, note counts come from scan_dir_with_count)
    let mut dirs: Vec<_> = read_dir
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_type().map(|ft| ft.is_dir()).unwrap_or(false)
                && !is_hidden(&e.path())
                && e.file_name().to_string_lossy() != "Daily"
        })
        .collect();

    dirs.sort_by(|a, b| {
        compare_natural_names(
            &a.file_name().to_string_lossy(),
            &b.file_name().to_string_lossy(),
        )
    });

    // Scan sibling notebooks in parallel: each subtree is independent, so
    // overlapping the per-directory read_dir calls hides FUSE latency on Android.
    // par_iter().collect() preserves the (already-sorted) order.
    let paths: Vec<PathBuf> = dirs.iter().map(|e| e.path()).collect();
    paths
        .par_iter()
        .map(|path| {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let relative = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .to_string();
            let (children, child_note_count) = scan_dir_with_count(path, vault_root);
            NotebookEntry {
                name,
                path: path.to_string_lossy().to_string(),
                relative_path: relative,
                children,
                note_count: child_note_count,
            }
        })
        .collect()
}

/// Combined scan: returns (children, note_count) in a single read_dir pass.
fn scan_dir_with_count(dir: &Path, vault_root: &str) -> (Vec<NotebookEntry>, usize) {
    let root = Path::new(vault_root);

    let Ok(read_dir) = fs::read_dir(dir) else {
        return (Vec::new(), 0);
    };

    let mut subdirs = Vec::new();
    let mut note_count = 0usize;
    for entry in read_dir.filter_map(|e| e.ok()) {
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if ft.is_dir() && !is_hidden(&entry.path()) {
            subdirs.push(entry);
        } else if ft.is_file() && entry.path().extension().and_then(|x| x.to_str()) == Some("md") {
            note_count += 1;
        }
    }

    subdirs.sort_by(|a, b| {
        compare_natural_names(
            &a.file_name().to_string_lossy(),
            &b.file_name().to_string_lossy(),
        )
    });

    let paths: Vec<PathBuf> = subdirs.iter().map(|e| e.path()).collect();
    let entries: Vec<NotebookEntry> = paths
        .par_iter()
        .map(|path| {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let relative = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .to_string();
            let (children, child_note_count) = scan_dir_with_count(path, vault_root);
            NotebookEntry {
                name,
                path: path.to_string_lossy().to_string(),
                relative_path: relative,
                children,
                note_count: child_note_count,
            }
        })
        .collect();

    (entries, note_count)
}

pub(crate) fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| {
            n.starts_with('.')
                || matches!(
                    n,
                    "_res" | "_resources" | "_attachments" | "_assets" | "assets" | "node_modules"
                )
        })
        .unwrap_or(false)
}

pub fn count_root_notes(vault_path: &str) -> Result<usize, String> {
    let root = Path::new(vault_path);
    if !root.exists() {
        return Err("Vault path does not exist".to_string());
    }
    let count = fs::read_dir(root)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_type().map(|ft| ft.is_file()).unwrap_or(false)
                && e.path().extension().and_then(|x| x.to_str()) == Some("md")
        })
        .count();
    Ok(count)
}

pub fn scan_notes(vault_path: &str, notebook_path: Option<&str>) -> Result<Vec<NoteEntry>, String> {
    let scan_path = notebook_path.unwrap_or(vault_path);
    let root = Path::new(scan_path);
    ensure_vault_content_dir(vault_path, root)?;
    let vault_root = Path::new(vault_path);

    log::info!(
        "scan_notes: vault={}, scan={}, exists={}",
        vault_path,
        scan_path,
        root.exists()
    );

    if !root.exists() {
        return Err("Path does not exist".to_string());
    }

    // On mobile, use metadata-only scan (no file reads) for fast listing on sandboxed FS
    #[cfg(mobile)]
    {
        // Collect candidate .md paths first, then read their metadata in parallel.
        // Overlapping the per-file reads hides FUSE latency on Android (matches the
        // desktop path below). The previous version also did an extra read_dir purely
        // for debug logging, which doubled the directory I/O on the sandboxed FS.
        let paths: Vec<PathBuf> = if notebook_path.is_some() {
            match fs::read_dir(root) {
                Ok(rd) => rd
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.is_file() && p.extension().and_then(|x| x.to_str()) == Some("md"))
                    .collect(),
                Err(_) => Vec::new(),
            }
        } else {
            let hn_dir = helixnotes_dir(vault_path);
            WalkDir::new(root)
                .into_iter()
                // filter_entry skips descending into hidden dirs (unlike filter)
                .filter_entry(|e| {
                    e.depth() == 0 || (!is_hidden(e.path()) && !e.path().starts_with(&hn_dir))
                })
                .filter_map(|e| e.ok())
                .map(|e| e.path().to_path_buf())
                .filter(|p| p.is_file() && p.extension().and_then(|x| x.to_str()) == Some("md"))
                .collect()
        };

        let mut notes: Vec<NoteEntry> = paths
            .par_iter()
            .filter_map(|path| read_note_entry_metadata_only(path, vault_root).ok())
            .collect();

        log::info!("scan_notes: mobile scan found {} notes", notes.len());
        notes.sort_by_key(|note| std::cmp::Reverse(note.meta.modified));
        return Ok(notes);
    }

    #[cfg(desktop)]
    {
        let md_files: Vec<PathBuf> = if notebook_path.is_some() {
            fs::read_dir(root)
                .map_err(|e| e.to_string())?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_file() && p.extension().and_then(|x| x.to_str()) == Some("md"))
                .collect()
        } else {
            WalkDir::new(root)
                .into_iter()
                .filter_entry(|e| {
                    e.depth() == 0
                        || (!is_hidden(e.path())
                            && !e.path().starts_with(helixnotes_dir(vault_path)))
                })
                .filter_map(|e| e.ok())
                .map(|e| e.path().to_path_buf())
                .filter(|p| p.is_file() && p.extension().and_then(|x| x.to_str()) == Some("md"))
                .collect()
        };

        let mut notes: Vec<NoteEntry> = md_files
            .par_iter()
            .filter_map(|path| read_note_entry_fast(path, vault_root).ok())
            .collect();

        notes.sort_by_key(|note| std::cmp::Reverse(note.meta.modified));
        Ok(notes)
    }
}

fn read_note_entry(path: &Path, vault_root: &Path) -> Result<NoteEntry, String> {
    let raw = fs::read_to_string(path).map_err(|e| e.to_string())?;
    read_note_entry_from_str(&raw, path, vault_root)
}

/// Mobile listing reads the first 2KB and uses the filesystem modification time.
/// Persisted creation dates survive atomic file replacement.
#[cfg(any(mobile, test))]
fn read_note_entry_metadata_only(path: &Path, vault_root: &Path) -> Result<NoteEntry, String> {
    let raw = read_note_head(path)?;

    let filename = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let (mut meta, content) = frontmatter::parse_note(&raw, &filename);

    // Creation time belongs to the note, not the replacement file's new inode.
    if let Ok(fs_meta) = fs::metadata(path) {
        if let Ok(m) = fs_meta.modified() {
            meta.modified = m.into();
        }
        if !raw.contains("\ncreated:") && !raw.starts_with("created:") {
            if let Ok(c) = fs_meta.created() {
                meta.created = c.into();
            }
        }
    }

    let relative = path
        .strip_prefix(vault_root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();

    let preview = frontmatter::extract_preview(&content, 120);

    Ok(NoteEntry {
        path: path.to_string_lossy().to_string(),
        relative_path: relative,
        meta,
        preview,
    })
}

const NOTE_HEAD_BLOCK: usize = 2048;
const NOTE_HEAD_LIMIT: usize = 256 * 1024;

/// Where the YAML frontmatter stands within a partially read note: `None` when the note has
/// none, `Some(None)` while the closing delimiter is still unread, else the offset after it.
fn frontmatter_state(head: &[u8]) -> Option<Option<usize>> {
    let rest = head.strip_prefix(b"---")?;
    if !matches!(rest.first(), None | Some(b'\n' | b'\r')) {
        return None;
    }
    let mut offset = 3;
    for line in head[3..].split_inclusive(|byte| *byte == b'\n') {
        offset += line.len();
        if line.ends_with(b"\n") && line.trim_ascii_end() == b"---" {
            return Some(Some(offset));
        }
    }
    Some(None)
}

/// Reads the start of a note: its whole frontmatter plus a block of body for previews, instead
/// of a fixed prefix that truncates long frontmatter and loses its title and tags.
pub(crate) fn read_note_head(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut head = Vec::with_capacity(NOTE_HEAD_BLOCK);
    let mut target = NOTE_HEAD_BLOCK;
    loop {
        let wanted = target - head.len();
        (&mut file)
            .take(wanted as u64)
            .read_to_end(&mut head)
            .map_err(|e| e.to_string())?;
        if head.len() < target || head.len() >= NOTE_HEAD_LIMIT {
            break;
        }
        target = match frontmatter_state(&head) {
            None => break,
            Some(None) => (head.len() * 2).min(NOTE_HEAD_LIMIT),
            Some(Some(end)) if head.len() >= end + NOTE_HEAD_BLOCK => break,
            Some(Some(end)) => end + NOTE_HEAD_BLOCK,
        };
    }
    Ok(String::from_utf8_lossy(&head).into_owned())
}

/// Fast version: reads the frontmatter and the start of the body (enough for a preview).
pub(crate) fn read_note_entry_fast(path: &Path, vault_root: &Path) -> Result<NoteEntry, String> {
    let raw = read_note_head(path)?;
    read_note_entry_from_str(&raw, path, vault_root)
}

fn read_note_entry_from_str(
    raw: &str,
    path: &Path,
    vault_root: &Path,
) -> Result<NoteEntry, String> {
    let filename = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let (mut meta, content) = frontmatter::parse_note(raw, &filename);

    // Use filesystem times only if frontmatter didn't have created/modified fields
    let has_created = raw.contains("\ncreated:") || raw.starts_with("created:");
    let has_modified = raw.contains("\nmodified:") || raw.starts_with("modified:");
    if let Ok(fs_meta) = fs::metadata(path) {
        if !has_modified {
            if let Ok(modified) = fs_meta.modified() {
                meta.modified = modified.into();
            }
        }
        if !has_created {
            if let Ok(created) = fs_meta.created() {
                meta.created = created.into();
            }
        }
    }

    let relative = path
        .strip_prefix(vault_root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();

    let preview = frontmatter::extract_preview(&content, 120);

    Ok(NoteEntry {
        path: path.to_string_lossy().to_string(),
        relative_path: relative,
        meta,
        preview,
    })
}

pub fn read_note(vault_path: &str, path: &str) -> Result<NoteContent, String> {
    let validated = ensure_readable_note_path(vault_path, Path::new(path))?;
    read_note_content(&validated, path)
}

pub fn read_vault_note(vault_path: &str, path: &str) -> Result<NoteContent, String> {
    let validated = ensure_note_path(vault_path, Path::new(path))?;
    read_note_content(&validated, path)
}

pub(crate) fn read_note_content(
    validated: &Path,
    reported_path: &str,
) -> Result<NoteContent, String> {
    let p = validated;
    let raw = fs::read_to_string(p).map_err(|e| e.to_string())?;
    let filename = p
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let (mut meta, content) = frontmatter::parse_note(&raw, &filename);

    // Fall back to filesystem times when the frontmatter has no created/modified, matching the
    // note-list scan (read_note_entry_from_str). Without this, a missing `created:` defaults to
    // Utc::now() here and then gets written to disk on the next save (e.g. a title change),
    // resetting the create date of imported/frontmatter-less notes. (issue #139)
    let has_created = raw.contains("\ncreated:") || raw.starts_with("created:");
    let has_modified = raw.contains("\nmodified:") || raw.starts_with("modified:");
    if let Ok(fs_meta) = fs::metadata(p) {
        if !has_modified {
            if let Ok(modified) = fs_meta.modified() {
                meta.modified = modified.into();
            }
        }
        if !has_created {
            if let Ok(created) = fs_meta.created() {
                meta.created = created.into();
            }
        }
    }

    Ok(NoteContent {
        path: reported_path.to_string(),
        meta,
        content,
        raw,
    })
}

pub struct SavedNote {
    pub id: String,
    pub previous_raw: String,
}

/// Returns the pre-save content only when the note actually changed.
pub fn save_note(
    vault_path: &str,
    path: &str,
    meta: &NoteMeta,
    body: &str,
    expected_raw: &str,
) -> Result<Option<SavedNote>, String> {
    let validated = ensure_note_path(vault_path, Path::new(path))?;
    // Replace the actual file, not a symlink pointing to it.
    let target = canonicalize_path(&validated, "note path")?;
    let existing = read_note_content(&target, path)?;
    if existing.raw != expected_raw {
        return Err(
            "Save conflict: note changed on disk. Your edits have not been written.".to_string(),
        );
    }
    let mut updated_meta = meta.clone();

    // A stale editor may still have an empty ID after the first save.
    if !existing.meta.id.is_empty() {
        updated_meta.id.clone_from(&existing.meta.id);
    } else if updated_meta.id.is_empty() {
        updated_meta.id = Uuid::new_v4().to_string();
    }

    // The editor appends one terminal LF; the note parser omits it. Do not
    // trim spaces here: Markdown hard breaks and indentation are meaningful.
    let comparable_body = body.strip_suffix('\n').unwrap_or(body);
    if existing.content == comparable_body
        && existing.meta.id == updated_meta.id
        && existing.meta.title == updated_meta.title
        && existing.meta.tags == updated_meta.tags
        && existing.meta.pinned == updated_meta.pinned
        && existing.meta.created == updated_meta.created
    {
        return Ok(None);
    }

    updated_meta.modified = Utc::now();
    let raw = frontmatter::merge_frontmatter(&existing.raw, &updated_meta, body);
    replace_file_atomically(&target, raw.as_bytes())?;

    Ok(Some(SavedNote {
        id: updated_meta.id,
        previous_raw: existing.raw,
    }))
}

pub(crate) fn replace_file_atomically(path: &Path, content: &[u8]) -> Result<(), String> {
    let permissions = match fs::metadata(path) {
        Ok(metadata) => {
            let permissions = metadata.permissions();
            if permissions.readonly() {
                return Err("File is read-only".to_string());
            }
            Some(permissions)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.to_string()),
    };
    let parent = path.parent().ok_or("File has no parent directory")?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".helixnotes-save-")
        .tempfile_in(parent)
        .map_err(|error| error.to_string())?;
    temporary
        .write_all(content)
        .map_err(|error| error.to_string())?;
    temporary.flush().map_err(|error| error.to_string())?;
    if let Some(permissions) = permissions {
        temporary
            .as_file()
            .set_permissions(permissions)
            .map_err(|error| error.to_string())?;
    }
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    // tempfile uses an atomic replacement on Unix and Windows, and owns cleanup
    // on every failure before the replacement succeeds.
    temporary.persist(path).map_err(|error| error.to_string())?;
    Ok(())
}

pub fn create_note(
    vault_path: &str,
    notebook_relative: Option<&str>,
    title: &str,
) -> Result<NoteEntry, String> {
    let requested_dir = match notebook_relative {
        Some(rel) => Path::new(vault_path).join(safe_relative_path(rel)?),
        None => PathBuf::from(vault_path),
    };
    let dir = ensure_vault_content_dir(vault_path, &requested_dir)?;

    let filename = sanitize_filename(title);
    let mut file_path = dir.join(format!("{}.md", filename));

    // Deduplicate filename
    let mut counter = 1;
    while file_path.exists() {
        file_path = dir.join(format!("{} {}.md", filename, counter));
        counter += 1;
    }

    let now = Utc::now();
    let meta = NoteMeta {
        id: Uuid::new_v4().to_string(),
        title: title.to_string(),
        tags: Vec::new(),
        pinned: false,
        created: now,
        modified: now,
    };

    let raw = frontmatter::update_note_raw(&meta, "\n");
    fs::write(&file_path, raw).map_err(|e| e.to_string())?;

    let vault_root = Path::new(vault_path);
    let relative = file_path
        .strip_prefix(vault_root)
        .unwrap_or(&file_path)
        .to_string_lossy()
        .to_string();

    Ok(NoteEntry {
        path: file_path.to_string_lossy().to_string(),
        relative_path: relative,
        meta,
        preview: String::new(),
    })
}

pub fn duplicate_note(path: &str, vault_path: &str) -> Result<NoteEntry, String> {
    let validated = ensure_note_path(vault_path, Path::new(path))?;
    let src = validated.as_path();

    let parent = src
        .parent()
        .ok_or_else(|| "Note has no parent directory".to_string())?;
    let raw = fs::read_to_string(src).map_err(|e| e.to_string())?;
    let filename = src
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let (mut meta, body) = frontmatter::parse_note(&raw, &filename);
    let source_title = meta.title.clone();
    let base_title = format!("{} copy", source_title.trim_end());
    let mut copy_title = base_title.clone();
    let mut copy_number = 2;
    let mut copy_path = parent.join(format!("{}.md", sanitize_filename(&copy_title)));

    while copy_path.exists() {
        copy_title = format!("{} {}", base_title, copy_number);
        copy_number += 1;
        copy_path = parent.join(format!("{}.md", sanitize_filename(&copy_title)));
    }

    let now = Utc::now();
    meta.id = Uuid::new_v4().to_string();
    meta.title = copy_title;
    meta.pinned = false;
    meta.created = now;
    meta.modified = now;

    let body = retitle_leading_heading(&body, &source_title, &meta.title);
    let copy_raw = frontmatter::merge_frontmatter(&raw, &meta, &body);
    fs::write(&copy_path, copy_raw).map_err(|e| e.to_string())?;

    read_note_entry(&copy_path, Path::new(vault_path))
}

fn retitle_leading_heading(body: &str, old_title: &str, new_title: &str) -> String {
    let trimmed = body.trim_start();
    let leading_len = body.len() - trimmed.len();
    let first_line_end = trimmed.find('\n').unwrap_or(trimmed.len());
    let first_line = &trimmed[..first_line_end];
    let heading_len = first_line.chars().take_while(|&c| c == '#').count();

    if !(1..=6).contains(&heading_len) {
        return body.to_string();
    }

    let Some(heading_title) = first_line[heading_len..].strip_prefix(' ') else {
        return body.to_string();
    };
    if !heading_title.trim().eq_ignore_ascii_case(old_title.trim()) {
        return body.to_string();
    }

    let mut retitled = String::with_capacity(body.len() + new_title.len());
    retitled.push_str(&body[..leading_len]);
    retitled.push_str(&"#".repeat(heading_len));
    retitled.push(' ');
    retitled.push_str(new_title);
    retitled.push_str(&trimmed[first_line_end..]);
    retitled
}

fn get_system_locale() -> Locale {
    let sys = sys_locale::get_locale().unwrap_or_else(|| "en-US".to_string());
    let lang = sys.split(&['-', '_', '.'][..]).next().unwrap_or("en");
    match lang {
        "af" => Locale::af_ZA,
        "ar" => Locale::ar_SA,
        "be" => Locale::be_BY,
        "bg" => Locale::bg_BG,
        "ca" => Locale::ca_ES,
        "cs" => Locale::cs_CZ,
        "da" => Locale::da_DK,
        "de" => Locale::de_DE,
        "el" => Locale::el_GR,
        "es" => Locale::es_ES,
        "et" => Locale::et_EE,
        "fi" => Locale::fi_FI,
        "fr" => Locale::fr_FR,
        "he" => Locale::he_IL,
        "hi" => Locale::hi_IN,
        "hr" => Locale::hr_HR,
        "hu" => Locale::hu_HU,
        "id" => Locale::id_ID,
        "is" => Locale::is_IS,
        "it" => Locale::it_IT,
        "ja" => Locale::ja_JP,
        "ka" => Locale::ka_GE,
        "ko" => Locale::ko_KR,
        "lt" => Locale::lt_LT,
        "lv" => Locale::lv_LV,
        "mk" => Locale::mk_MK,
        "nb" | "no" => Locale::nb_NO,
        "nl" => Locale::nl_NL,
        "nn" => Locale::nn_NO,
        "pl" => Locale::pl_PL,
        "pt" => Locale::pt_BR,
        "ro" => Locale::ro_RO,
        "ru" => Locale::ru_RU,
        "sk" => Locale::sk_SK,
        "sl" => Locale::sl_SI,
        "sq" => Locale::sq_AL,
        "sr" => Locale::sr_RS,
        "sv" => Locale::sv_SE,
        "th" => Locale::th_TH,
        "tr" => Locale::tr_TR,
        "uk" => Locale::uk_UA,
        "vi" => Locale::vi_VN,
        "zh" => Locale::zh_CN,
        _ => Locale::en_US,
    }
}

pub fn create_daily_note(
    vault_path: &str,
    date: Option<&str>,
    format: &str,
) -> Result<NoteEntry, String> {
    let target_date = match date {
        Some(d) => chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d")
            .map_err(|e| format!("Invalid date: {}", e))?,
        None => Local::now().date_naive(),
    };
    let date_str = target_date.format("%Y-%m-%d").to_string();
    let title = match format {
        "iso" => target_date.format("%Y-%m-%d").to_string(),
        "long" => target_date.format("%B %-d, %Y").to_string(),
        "us" => target_date.format("%m/%d/%Y").to_string(),
        "eu" => target_date.format("%d/%m/%Y").to_string(),
        _ => {
            let locale = get_system_locale();
            target_date
                .format_localized("%B %d, %Y", locale)
                .to_string()
        }
    };

    let dir = Path::new(vault_path).join("Daily");
    if fs::symlink_metadata(&dir).is_err() {
        fs::create_dir(&dir).map_err(|e| e.to_string())?;
    }
    let dir = ensure_vault_content_dir(vault_path, &dir)?;
    let file_path = dir.join(format!("{}.md", date_str));
    let vault_root = Path::new(vault_path);

    // Return a genuine existing daily note, but never follow an outside link.
    if fs::symlink_metadata(&file_path).is_ok() {
        if fs::symlink_metadata(&file_path)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_symlink()
        {
            return Err("Daily note destination is a symlink".to_string());
        }
        ensure_note_path(vault_path, &file_path)?;
        return read_note_entry(&file_path, vault_root);
    }

    let now = Utc::now();
    let meta = NoteMeta {
        id: Uuid::new_v4().to_string(),
        title,
        tags: vec!["daily".to_string()],
        pinned: false,
        created: now,
        modified: now,
    };

    let raw = frontmatter::update_note_raw(&meta, "\n");
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&file_path)
    {
        Ok(mut file) => file.write_all(raw.as_bytes()).map_err(|e| e.to_string())?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if fs::symlink_metadata(&file_path)
                .map_err(|e| e.to_string())?
                .file_type()
                .is_symlink()
            {
                return Err("Daily note destination is a symlink".to_string());
            }
            ensure_note_path(vault_path, &file_path)?;
            return read_note_entry(&file_path, vault_root);
        }
        Err(error) => return Err(error.to_string()),
    }

    let relative = file_path
        .strip_prefix(vault_root)
        .unwrap_or(&file_path)
        .to_string_lossy()
        .to_string();

    Ok(NoteEntry {
        path: file_path.to_string_lossy().to_string(),
        relative_path: relative,
        meta,
        preview: String::new(),
    })
}

pub fn create_notebook(
    vault_path: &str,
    parent_relative: Option<&str>,
    name: &str,
) -> Result<NotebookEntry, String> {
    let requested_parent = match parent_relative {
        Some(rel) => Path::new(vault_path).join(safe_relative_path(rel)?),
        None => PathBuf::from(vault_path),
    };
    let parent = ensure_vault_content_dir(vault_path, &requested_parent)?;
    let dir_path = parent.join(safe_child_name(name)?);
    if dir_path.exists() {
        return Err("Notebook already exists".to_string());
    }

    fs::create_dir_all(&dir_path).map_err(|e| e.to_string())?;

    let vault_root = Path::new(vault_path);
    let relative = dir_path
        .strip_prefix(vault_root)
        .unwrap_or(&dir_path)
        .to_string_lossy()
        .to_string();

    Ok(NotebookEntry {
        name: name.to_string(),
        path: dir_path.to_string_lossy().to_string(),
        relative_path: relative,
        children: Vec::new(),
        note_count: 0,
    })
}

pub fn delete_note(vault_path: &str, note_path: &str) -> Result<(), String> {
    let validated = ensure_note_path(vault_path, Path::new(note_path))?;
    let src = validated.as_path();

    let trash_dir = helixnotes_dir(vault_path).join("trash");
    fs::create_dir_all(&trash_dir).map_err(|e| e.to_string())?;

    let filename = src
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    loop {
        let candidate = trash_dir.join(trash_name(&filename));
        match rename_exclusive(src, &candidate) {
            Ok(()) => break,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

pub fn delete_notebook(vault_path: &str, notebook_path: &str) -> Result<(), String> {
    let validated = ensure_notebook_path(vault_path, Path::new(notebook_path))?;
    let src = validated.as_path();

    let trash_dir = helixnotes_dir(vault_path).join("trash");
    fs::create_dir_all(&trash_dir).map_err(|e| e.to_string())?;

    let dirname = src
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let relative = src
        .strip_prefix(vault_path)
        .unwrap_or(src)
        .to_string_lossy()
        .to_string();
    loop {
        let trash_name = trash_name(&dirname);
        let dest = trash_dir.join(&trash_name);
        let meta_path = trash_dir.join(format!("{trash_name}.meta"));
        let mut sidecar = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&meta_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.to_string()),
        };
        if let Err(error) = sidecar.write_all(relative.as_bytes()) {
            drop(sidecar);
            let _ = fs::remove_file(&meta_path);
            return Err(error.to_string());
        }
        drop(sidecar);
        match rename_exclusive(src, &dest) {
            Ok(()) => return Ok(()),
            Err(error) => {
                let _ = fs::remove_file(&meta_path);
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    continue;
                }
                return Err(error.to_string());
            }
        }
    }
}

pub struct RenamedNote {
    pub path: String,
    pub old_raw: String,
    pub new_raw: String,
    pub old_title: String,
    pub new_title: String,
}

pub fn rename_note(path: &str, new_title: &str, vault_path: &str) -> Result<RenamedNote, String> {
    let validated = ensure_note_path(vault_path, Path::new(path))?;
    let src = validated.as_path();

    // Read old title before renaming
    let raw = fs::read_to_string(src).map_err(|e| e.to_string())?;
    let filename = src
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let (mut meta, content) = frontmatter::parse_note(&raw, &filename);
    let old_title = meta.title.clone();
    let old_path_str = src.to_string_lossy().to_string();

    // Update frontmatter
    meta.title = new_title.to_string();
    meta.modified = Utc::now();
    if meta.id.is_empty() {
        meta.id = Uuid::new_v4().to_string();
    }
    let updated = frontmatter::merge_frontmatter(&raw, &meta, &content);

    // Rename file
    let new_filename = sanitize_filename(new_title);
    let new_path = src.parent().unwrap().join(format!("{}.md", new_filename));
    let previous_quick_access = move_note_with_quick_access(vault_path, src, &new_path)?;
    if let Err(error) = replace_file_atomically(&new_path, updated.as_bytes()) {
        if new_path != src {
            return Err(match move_without_replacing(&new_path, src) {
                Ok(()) => {
                    if let Some(previous) = previous_quick_access {
                        match save_quick_access(vault_path, &previous) {
                            Ok(()) => error,
                            Err(rollback) => {
                                format!("{error}; Quick Access rollback failed: {rollback}")
                            }
                        }
                    } else {
                        error
                    }
                }
                Err(rollback) => format!(
                    "{error}; note remains at {}: {rollback}",
                    new_path.display()
                ),
            });
        }
        return Err(error);
    }

    let new_path_str = new_path.to_string_lossy().to_string();

    // Update wikilinks in other notes that reference this note
    update_wikilinks_after_rename(
        vault_path,
        &old_path_str,
        &new_path_str,
        &old_title,
        new_title,
    );

    Ok(RenamedNote {
        path: new_path_str,
        old_raw: raw,
        new_raw: updated,
        old_title,
        new_title: meta.title,
    })
}

/// Walk all .md files in the vault and update wikilink references after a note rename.
/// Updates both HTML data-attributes (data-path, data-title) and raw [[old_title]] references.
fn update_wikilinks_after_rename(
    vault_path: &str,
    old_path: &str,
    new_path: &str,
    old_title: &str,
    new_title: &str,
) {
    let vault = Path::new(vault_path);
    let hn_dir = helixnotes_dir(vault_path);

    // Vault-relative path ref (without .md), normalized to forward slashes so it
    // matches [[folder/note]] wikilinks on Windows (OS paths use backslashes there).
    let rel_ref = |p: &str| -> String {
        let rel = Path::new(p)
            .strip_prefix(vault)
            .map(|r| r.to_string_lossy().into_owned())
            .unwrap_or_else(|_| p.to_string())
            .replace('\\', "/");
        rel.strip_suffix(".md").unwrap_or(&rel).to_string()
    };
    let old_rel_ref = rel_ref(old_path);
    let new_rel_ref = rel_ref(new_path);

    // Check if another note with the same old_title exists in the vault.
    // If so, title-based replacements (rules 1-3) are ambiguous and must be skipped,
    // because we can't tell which [[Old Title]] ref points to the renamed note
    // vs. the other note with the same title.
    let title_is_unique = !WalkDir::new(vault)
        .into_iter()
        .filter_entry(|e| {
            e.depth() == 0 || (!is_hidden(e.path()) && !e.path().starts_with(&hn_dir))
        })
        .filter_map(|e| e.ok())
        .any(|e| {
            let p = e.path();
            if !p.is_file() || p.to_string_lossy().as_ref() == new_path {
                return false;
            }
            if p.extension().and_then(|ext| ext.to_str()) != Some("md") {
                return false;
            }
            // Check if this note's filename (without .md) matches the old title
            p.file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case(old_title))
                .unwrap_or(false)
        });

    for entry in WalkDir::new(vault)
        .into_iter()
        .filter_entry(|e| {
            e.depth() == 0 || (!is_hidden(e.path()) && !e.path().starts_with(&hn_dir))
        })
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let path_str = path.to_string_lossy();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        // Skip the renamed note itself
        if *path_str == *new_path {
            continue;
        }

        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let mut result = content.clone();

        // Notes are saved as markdown with [[ref]] or [[ref|display]] syntax.
        // Update all wikilink forms that reference the renamed note:

        // Title-based rules only apply when the old title is unique in the vault.
        // If another note shares the same title, these would be ambiguous.
        if old_title != new_title && title_is_unique {
            // 1. Short title ref: [[Old Title]] → [[New Title]]
            result = result.replace(&format!("[[{}]]", old_title), &format!("[[{}]]", new_title));

            // 2. Short title with alias: [[Old Title|display]] → [[New Title|display]]
            result = result.replace(&format!("[[{}|", old_title), &format!("[[{}|", new_title));

            // 3. Short title as alias display: [[ref|Old Title]] → [[ref|New Title]]
            result = result.replace(&format!("|{}]]", old_title), &format!("|{}]]", new_title));
        }

        // Path-based rules are always safe (paths are unique).
        if old_rel_ref != new_rel_ref {
            // 4. Path-based ref: [[folder/Old Name]] → [[folder/New Name]]
            result = result.replace(
                &format!("[[{}]]", old_rel_ref),
                &format!("[[{}]]", new_rel_ref),
            );

            // 5. Path-based ref with alias: [[folder/Old Name|display]] → [[folder/New Name|display]]
            //    Also update the alias if it matches the old title.
            //    e.g. [[folder/Old Name|Old Title]] → [[folder/New Name|New Title]]
            if old_title != new_title {
                result = result.replace(
                    &format!("[[{}|{}]]", old_rel_ref, old_title),
                    &format!("[[{}|{}]]", new_rel_ref, new_title),
                );
            }
            // For aliases that DON'T match the old title, just update the ref part
            result = result.replace(
                &format!("[[{}|", old_rel_ref),
                &format!("[[{}|", new_rel_ref),
            );
        }

        if result != content {
            let _ = fs::write(path, &result);
        }
    }
}

pub fn rename_notebook(vault_path: &str, path: &str, new_name: &str) -> Result<String, String> {
    let validated = ensure_notebook_path(vault_path, Path::new(path))?;
    let src = validated.as_path();

    let new_path = src.parent().unwrap().join(safe_child_name(new_name)?);
    if src == new_path {
        return Ok(path.to_string());
    }

    move_notebook_with_icons(vault_path, src, &new_path)?;
    Ok(new_path.to_string_lossy().to_string())
}

pub fn move_note(vault_path: &str, note_path: &str, dest_notebook: &str) -> Result<String, String> {
    let validated = ensure_note_path(vault_path, Path::new(note_path))?;
    let src = validated.as_path();

    let validated_dest = ensure_vault_content_dir(vault_path, Path::new(dest_notebook))?;
    let dest_dir = validated_dest.as_path();
    if !dest_dir.is_dir() {
        return Err("Destination notebook does not exist".to_string());
    }

    let filename = src.file_name().unwrap_or_default();
    let dest = dest_dir.join(filename);

    move_note_with_quick_access(vault_path, src, &dest)?;
    Ok(dest.to_string_lossy().to_string())
}

pub fn move_notebook(
    vault_path: &str,
    notebook_path: &str,
    dest_parent: &str,
) -> Result<String, String> {
    let validated = ensure_notebook_path(vault_path, Path::new(notebook_path))?;
    let src = validated.as_path();

    let validated_dest = ensure_vault_content_dir(vault_path, Path::new(dest_parent))?;
    let dest_parent_path = validated_dest.as_path();
    if !dest_parent_path.is_dir() {
        return Err("Destination does not exist".to_string());
    }

    // No-op if already in that parent
    if src.parent() == Some(dest_parent_path) {
        return Err("Notebook is already in that location".to_string());
    }

    let dir_name = src.file_name().unwrap_or_default();
    let dest = dest_parent_path.join(dir_name);

    // Prevent moving into itself or a descendant
    if dest.starts_with(src) {
        return Err("Cannot move a notebook into itself or its descendants".to_string());
    }

    // Destination reservation is performed atomically by move_notebook_with_icons.

    move_notebook_with_icons(vault_path, src, &dest)?;
    Ok(dest.to_string_lossy().to_string())
}

/// Remove a directory inside trash if it's empty (after restoring/deleting its last note).
fn cleanup_empty_trash_dir(vault_path: &str, dir: Option<&Path>) {
    let trash_dir = helixnotes_dir(vault_path).join("trash");
    if let Some(d) = dir {
        if d != trash_dir && d.starts_with(&trash_dir) {
            if let Ok(mut entries) = fs::read_dir(d) {
                if entries.next().is_none() {
                    let _ = fs::remove_dir(d);
                }
            }
        }
    }
}

pub fn get_trash_contents(vault_path: &str) -> Result<TrashContents, String> {
    let trash_dir = helixnotes_dir(vault_path).join("trash");
    if !trash_dir.exists() {
        return Ok(TrashContents {
            notes: Vec::new(),
            notebooks: Vec::new(),
        });
    }

    let vault_root = Path::new(vault_path);
    let mut notes = Vec::new();
    let mut notebooks = Vec::new();

    for entry in fs::read_dir(&trash_dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|x| x.to_str()) == Some("md") {
            if let Ok(note) = read_note_entry(&path, vault_root) {
                notes.push(note);
            }
        } else if path.is_dir() {
            let note_count = WalkDir::new(&path)
                .min_depth(1)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.path().is_file()
                        && e.path().extension().and_then(|x| x.to_str()) == Some("md")
                })
                .count();
            let dirname = path.file_name().unwrap_or_default().to_string_lossy();
            let name = original_trash_name(&dirname).to_string();
            let modified = fs::metadata(&path)
                .and_then(|m| m.modified())
                .map(DateTime::<Utc>::from)
                .unwrap_or_else(|_| Utc::now());
            notebooks.push(TrashNotebookEntry {
                name,
                path: path.to_string_lossy().to_string(),
                note_count,
                modified,
            });
        }
    }

    notes.sort_by_key(|note| std::cmp::Reverse(note.meta.modified));
    notebooks.sort_by_key(|notebook| std::cmp::Reverse(notebook.modified));
    Ok(TrashContents { notes, notebooks })
}

pub fn restore_note(
    vault_path: &str,
    trash_path: &str,
    dest_notebook: Option<&str>,
) -> Result<String, String> {
    let validated = ensure_trash_entry(vault_path, Path::new(trash_path))?;
    let src = validated.as_path();
    if !src.is_file() {
        return Err("Trashed note does not exist".to_string());
    }

    let requested_dest = match dest_notebook {
        Some(nb) => PathBuf::from(nb),
        None => PathBuf::from(vault_path),
    };
    let dest_dir = ensure_vault_content_dir(vault_path, &requested_dest)?;

    // Support both legacy timestamps and timestamp/UUID destinations.
    let filename = src.file_name().unwrap_or_default().to_string_lossy();
    let original_name = original_trash_name(&filename);

    let parent = src.parent().map(|p| p.to_path_buf());
    let dest = dest_dir.join(original_name);
    move_without_replacing(src, &dest)?;

    // Clean up empty parent directory (deleted notebook folder) in trash
    cleanup_empty_trash_dir(vault_path, parent.as_deref());

    Ok(dest.to_string_lossy().to_string())
}

pub fn restore_notebook(vault_path: &str, trash_path: &str) -> Result<String, String> {
    let validated = ensure_trash_entry(vault_path, Path::new(trash_path))?;
    let src = validated.as_path();
    if !src.is_dir() {
        return Err("Trashed notebook does not exist".to_string());
    }

    let dirname = src.file_name().unwrap_or_default().to_string_lossy();

    // The sidecar is paired with the exact trash entry name, including its UUID.
    let meta_path = src.with_file_name(format!("{}.meta", dirname));
    let relative = match fs::read_to_string(&meta_path) {
        Ok(original) => original,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Legacy notebooks without sidecars were restored to the vault root.
            original_trash_name(&dirname).to_string()
        }
        Err(error) => return Err(error.to_string()),
    };

    let relative = safe_relative_path(&relative)?;
    let dest = Path::new(vault_path).join(relative);
    // Validate every existing ancestor before creating children: a symlink
    // inside the vault may otherwise redirect this restore outside the vault.
    let vault = canonicalize_path(Path::new(vault_path), "vault path")?;
    let mut ancestor = PathBuf::from(vault_path);
    for component in relative
        .components()
        .take(relative.components().count() - 1)
    {
        ancestor.push(component);
        if fs::symlink_metadata(&ancestor).is_ok() {
            let canonical = canonicalize_path(&ancestor, "notebook destination")?;
            if !canonical.starts_with(&vault) || canonical.starts_with(vault.join(".helixnotes")) {
                return Err("Path must stay inside the active vault".to_string());
            }
            if !canonical.is_dir() {
                return Err("Notebook destination parent is not a directory".to_string());
            }
        } else {
            fs::create_dir(&ancestor).map_err(|e| e.to_string())?;
        }
    }
    let parent = dest.parent().ok_or("Notebook has no parent directory")?;
    ensure_vault_content_dir(vault_path, parent)?;
    move_without_replacing(src, &dest)?;
    // Clean up .meta file
    let _ = fs::remove_file(&meta_path);
    Ok(dest.to_string_lossy().to_string())
}

pub fn permanent_delete(vault_path: &str, path: &str) -> Result<(), String> {
    let validated = ensure_trash_entry(vault_path, Path::new(path))?;
    let p = validated.as_path();
    let parent = p.parent().map(|pp| pp.to_path_buf());
    if p.is_dir() {
        fs::remove_dir_all(p).map_err(|e| e.to_string())?;
        if let Some(name) = p.file_name() {
            let sidecar = p.with_file_name(format!("{}.meta", name.to_string_lossy()));
            let _ = fs::remove_file(sidecar);
        }
    } else {
        fs::remove_file(p).map_err(|e| e.to_string())?;
    }

    // Clean up empty parent directory (deleted notebook folder) in trash
    cleanup_empty_trash_dir(vault_path, parent.as_deref());

    Ok(())
}

pub fn empty_trash(vault_path: &str) -> Result<(), String> {
    let trash_dir = helixnotes_dir(vault_path).join("trash");
    if trash_dir.exists() {
        fs::remove_dir_all(&trash_dir).map_err(|e| e.to_string())?;
        fs::create_dir_all(&trash_dir).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn load_vault_state(vault_path: &str) -> Result<VaultState, String> {
    let state_path = helixnotes_dir(vault_path).join("state.json");
    if state_path.exists() {
        let data = fs::read_to_string(&state_path).map_err(|e| e.to_string())?;
        serde_json::from_str(&data).map_err(|e| e.to_string())
    } else {
        Ok(VaultState::default())
    }
}

pub fn save_vault_state(vault_path: &str, state: &VaultState) -> Result<(), String> {
    let state_path = helixnotes_dir(vault_path).join("state.json");
    let data = serde_json::to_string_pretty(state).map_err(|e| e.to_string())?;
    replace_file_atomically(&state_path, data.as_bytes())
}

pub fn get_all_tags(vault_path: &str) -> Result<Vec<(String, usize)>, String> {
    let hn_dir = helixnotes_dir(vault_path);
    let md_files: Vec<PathBuf> = WalkDir::new(vault_path)
        .into_iter()
        .filter_entry(|entry| entry.depth() == 0 || entry.path() != hn_dir.as_path())
        .filter_map(|e| e.ok())
        .filter(|e| {
            let p = e.path();
            p.is_file() && p.extension().and_then(|x| x.to_str()) == Some("md") && !is_hidden(p)
        })
        .map(|e| e.path().to_path_buf())
        .collect();

    // Read frontmatter in parallel (only need tags, so the body is not read)
    let all_tags: Vec<Vec<String>> = md_files
        .par_iter()
        .filter_map(|path| {
            let raw = read_note_head(path).ok()?;
            let filename = path.file_name()?.to_string_lossy().to_string();
            let (meta, _) = frontmatter::parse_note(&raw, &filename);
            Some(meta.tags)
        })
        .collect();

    let mut tag_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for tags in all_tags {
        for tag in tags {
            *tag_counts.entry(tag).or_insert(0) += 1;
        }
    }

    let mut tags: Vec<(String, usize)> = tag_counts.into_iter().collect();
    tags.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    Ok(tags)
}

pub fn save_image(vault_path: &str, name: &str, data: &[u8]) -> Result<String, String> {
    save_attachment(vault_path, name, data)
}

pub fn save_attachment(vault_path: &str, name: &str, data: &[u8]) -> Result<String, String> {
    let attachments_dir = helixnotes_dir(vault_path).join("attachments");
    fs::create_dir_all(&attachments_dir).map_err(|e| e.to_string())?;

    let safe_name = sanitize_filename(name);
    let mut temporary = tempfile::Builder::new()
        .prefix(".helixnotes-asset-")
        .tempfile_in(&attachments_dir)
        .map_err(|error| error.to_string())?;
    temporary
        .write_all(data)
        .map_err(|error| error.to_string())?;
    temporary.flush().map_err(|error| error.to_string())?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    loop {
        let filename = format!("{}_{}", Uuid::new_v4(), safe_name);
        match temporary.persist_noclobber(attachments_dir.join(&filename)) {
            Ok(_) => return Ok(format!(".helixnotes/attachments/{filename}")),
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                temporary = error.file;
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct ExternalNoteReference {
    pub from: usize,
    pub to: usize,
    pub href: String,
    pub html: bool,
    #[serde(default)]
    pub pdf: bool,
}

/// Import an accepted Markdown snapshot and its scoped relative attachments without overwriting files.
pub fn import_note(
    vault_path: &str,
    source_path: &str,
    source_root: &Path,
    notebook_relative: Option<&str>,
    expected_raw: &str,
    mut references: Vec<ExternalNoteReference>,
    authorize: impl Fn(&Path) -> Result<(), String>,
) -> Result<NoteContent, String> {
    let source = canonicalize_path(Path::new(source_path), "source note")?;
    authorize(&source)?;
    if !source.is_file() || source.extension().and_then(|ext| ext.to_str()) != Some("md") {
        return Err("Import source must be a Markdown file".into());
    }
    let source_root = canonicalize_path(source_root, "source attachment root")?;
    if !source.starts_with(&source_root) {
        return Err("Source attachment root does not own the note".into());
    }
    let source_attachments = source_root.join(".helixnotes/attachments");
    let raw = fs::read_to_string(&source).map_err(|error| error.to_string())?;
    if raw != expected_raw {
        return Err("The source note changed on disk. Reopen it before importing.".into());
    }
    let relative = notebook_relative.map(safe_relative_path).transpose()?;
    let directory = Path::new(vault_path).join(relative.unwrap_or(Path::new("")));
    ensure_vault_content_dir(vault_path, &directory)?;
    let vault = canonicalize_path(Path::new(vault_path), "vault path")?;
    let canonical_directory = canonicalize_path(&directory, "import destination")?;
    if canonical_directory != vault.join(relative.unwrap_or(Path::new(""))) {
        return Err("Import destination must not use symbolic links".into());
    }
    let attachments = helixnotes_dir(vault_path).join("attachments");
    references.sort_by_key(|reference| reference.from);
    let mut previous = 0;
    let mut targets = Vec::new();
    // Validate every range and permission before publishing any attachment.
    for reference in &references {
        if reference.from < previous
            || reference.from >= reference.to
            || raw.get(reference.from..reference.to).is_none()
        {
            return Err("Invalid attachment reference range".into());
        }
        previous = reference.to;
        let (path, suffix) = reference
            .href
            .find(['#', '?'])
            .map(|index| reference.href.split_at(index))
            .unwrap_or((&reference.href, ""));
        let decoded = urlencoding::decode(path)
            .map_err(|error| error.to_string())?
            .replace('\\', "/");
        let path = Path::new(&decoded);
        if decoded.is_empty()
            || decoded.contains(':')
            || path.is_absolute()
            || path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        {
            return Err("Import attachments must be relative non-Markdown files".into());
        }
        let base = if decoded.starts_with(".helixnotes/attachments/") || reference.pdf {
            source_root.as_path()
        } else {
            source.parent().ok_or("Source note has no parent")?
        };
        let target = canonicalize_path(&base.join(path), "attachment")?;
        authorize(&target)?;
        let relative_target = if let Ok(relative) = target.strip_prefix(&source_attachments) {
            relative
        } else {
            target.strip_prefix(source.parent().ok_or("Source note has no parent")?)
                .map_err(|_| "Attachments must stay inside the source note's folder or managed attachment root")?
        };
        let components: Vec<_> = relative_target.components().collect();
        let helix_attachment = components
            .first()
            .is_some_and(|part| part.as_os_str() == ".helixnotes")
            && components
                .get(1)
                .is_some_and(|part| part.as_os_str() == "attachments");
        if components.iter().enumerate().any(|(index, part)| {
            part.as_os_str().to_string_lossy().starts_with('.') && !(helix_attachment && index == 0)
        }) {
            return Err("Hidden metadata is not imported as an attachment".into());
        }
        if !target.is_file() {
            return Err("Attachment reference must point to a file".into());
        }
        targets.push((target, suffix.to_string()));
    }
    if !references.is_empty() {
        fs::create_dir_all(&attachments).map_err(|error| error.to_string())?;
        if canonicalize_path(&attachments, "attachment destination")?
            != vault.join(".helixnotes/attachments")
        {
            return Err("Attachment destination must not use symbolic links".into());
        }
    }
    let mut created = Vec::new();
    let result = (|| {
        let mut copied = std::collections::HashMap::<PathBuf, String>::new();
        let mut rewritten = String::with_capacity(raw.len());
        let mut previous = 0;
        for (reference, (target, suffix)) in references.iter().zip(targets) {
            let relative = if let Some(relative) = copied.get(&target) {
                relative.clone()
            } else {
                let name = sanitize_filename(
                    &target
                        .file_name()
                        .ok_or("Attachment has no filename")?
                        .to_string_lossy(),
                );
                let mut temporary = tempfile::Builder::new()
                    .prefix(".helixnotes-import-")
                    .tempfile_in(&attachments)
                    .map_err(|error| error.to_string())?;
                let mut file = fs::File::open(&target).map_err(|error| error.to_string())?;
                let before = file.metadata().map_err(|error| error.to_string())?;
                let bytes = std::io::copy(&mut file, temporary.as_file_mut())
                    .map_err(|error| error.to_string())?;
                let after = file.metadata().map_err(|error| error.to_string())?;
                if bytes != before.len()
                    || before.len() != after.len()
                    || before.modified().ok() != after.modified().ok()
                {
                    return Err("An attachment changed while importing. Retry the import.".into());
                }
                temporary
                    .as_file()
                    .sync_all()
                    .map_err(|error| error.to_string())?;
                let filename = loop {
                    let filename = format!("{}_{}", Uuid::new_v4(), name);
                    let destination = attachments.join(&filename);
                    match temporary.persist_noclobber(&destination) {
                        Ok(_) => {
                            created.push(destination);
                            break filename;
                        }
                        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                            temporary = error.file
                        }
                        Err(error) => return Err(error.error.to_string()),
                    }
                };
                let relative =
                    format!(".helixnotes/attachments/{}", urlencoding::encode(&filename));
                copied.insert(target, relative.clone());
                relative
            };
            // Use the same stable vault-root convention as normal attachment insertion/saving.
            let mut href = format!("{relative}{suffix}");
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
            rewritten.push_str(&raw[previous..reference.from]);
            rewritten.push_str(&href);
            previous = reference.to;
        }
        rewritten.push_str(&raw[previous..]);
        let filename = source.file_name().ok_or("Source note has no filename")?;
        let mut temporary = tempfile::Builder::new()
            .prefix(".helixnotes-import-")
            .tempfile_in(&directory)
            .map_err(|error| error.to_string())?;
        temporary
            .write_all(rewritten.as_bytes())
            .map_err(|error| error.to_string())?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|error| error.to_string())?;
        let mut destination = directory.join(filename);
        let mut suffix = 2;
        loop {
            match temporary.persist_noclobber(&destination) {
                Ok(_) => break,
                Err(error)
                    if error.error.kind() == std::io::ErrorKind::AlreadyExists && suffix <= 100 =>
                {
                    temporary = error.file;
                    destination = directory.join(format!(
                        "{} ({suffix}).md",
                        source.file_stem().unwrap_or_default().to_string_lossy()
                    ));
                    suffix += 1;
                }
                Err(error) => return Err(error.error.to_string()),
            }
        }
        let (meta, content) = frontmatter::parse_note(
            &rewritten,
            &destination
                .file_name()
                .unwrap_or_default()
                .to_string_lossy(),
        );
        Ok(NoteContent {
            path: destination.to_string_lossy().into_owned(),
            meta,
            content,
            raw: rewritten,
        })
    })();
    if let Err(error) = result {
        let mut failed_cleanup = Vec::new();
        for path in created {
            if let Err(cleanup) = fs::remove_file(&path) {
                if cleanup.kind() != std::io::ErrorKind::NotFound {
                    failed_cleanup.push(format!("{}: {cleanup}", path.display()));
                }
            }
        }
        if !failed_cleanup.is_empty() {
            return Err(format!(
                "{error}; could not remove unused attachments: {}",
                failed_cleanup.join("; ")
            ));
        }
        return Err(error);
    }
    result
}

pub(crate) fn normalize_notebook_icon_key(path: &str) -> String {
    path.replace('\\', "/")
}

pub fn load_notebook_icons(
    vault_path: &str,
) -> Result<std::collections::HashMap<String, String>, String> {
    let icons_path = helixnotes_dir(vault_path).join("notebook_icons.json");
    let data = match fs::read_to_string(&icons_path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(std::collections::HashMap::new())
        }
        Err(error) => return Err(error.to_string()),
    };
    let icons: std::collections::HashMap<String, String> =
        serde_json::from_str(&data).map_err(|e| e.to_string())?;
    Ok(icons
        .into_iter()
        .map(|(path, icon)| (normalize_notebook_icon_key(&path), icon))
        .collect())
}

pub fn set_notebook_icon(
    vault_path: &str,
    notebook_relative: &str,
    icon_relative: Option<&str>,
) -> Result<(), String> {
    let mut icons = load_notebook_icons(vault_path)?;
    let notebook_key = normalize_notebook_icon_key(notebook_relative);
    match icon_relative {
        Some(icon) => {
            icons.insert(notebook_key, icon.to_string());
        }
        None => {
            icons.remove(&notebook_key);
        }
    }
    save_notebook_icons(vault_path, &icons)
}

fn save_notebook_icons(
    vault_path: &str,
    icons: &std::collections::HashMap<String, String>,
) -> Result<(), String> {
    let dir = helixnotes_dir(vault_path);
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let data = serde_json::to_vec_pretty(icons).map_err(|error| error.to_string())?;
    replace_file_atomically(&dir.join("notebook_icons.json"), &data)
}

fn move_notebook_with_icons(vault_path: &str, src: &Path, dest: &Path) -> Result<(), String> {
    let icons = load_notebook_icons(vault_path)?;
    let old_key = normalize_notebook_icon_key(
        &src.strip_prefix(vault_path)
            .map_err(|error| error.to_string())?
            .to_string_lossy(),
    );
    let new_key = normalize_notebook_icon_key(
        &dest
            .strip_prefix(vault_path)
            .map_err(|error| error.to_string())?
            .to_string_lossy(),
    );
    let quick_access = quick_access_migration(vault_path, src, dest, true)?;
    let prefix = format!("{old_key}/");
    let mut migrated = None;
    for (key, value) in &icons {
        let new_icon_key = if key == &old_key {
            new_key.clone()
        } else if let Some(suffix) = key.strip_prefix(&prefix) {
            format!("{new_key}/{suffix}")
        } else {
            continue;
        };
        let mapping = migrated.get_or_insert_with(|| icons.clone());
        mapping.remove(key);
        mapping.insert(new_icon_key, value.clone());
    }
    move_without_replacing(src, dest)?;
    if let Some(migrated) = &migrated {
        if let Err(error) = save_notebook_icons(vault_path, migrated) {
            return Err(match move_without_replacing(dest, src) {
                Ok(()) => error,
                Err(rollback) => format!(
                    "{error}; notebook remains at {}: {rollback}",
                    dest.display()
                ),
            });
        }
    }
    if let Some((_, updated)) = quick_access {
        if let Err(error) = save_quick_access(vault_path, &updated) {
            return Err(match move_without_replacing(dest, src) {
                Ok(()) if migrated.is_some() => match save_notebook_icons(vault_path, &icons) {
                    Ok(()) => error,
                    Err(rollback) => format!("{error}; notebook icon rollback failed: {rollback}"),
                },
                Ok(()) => error,
                Err(rollback) => format!(
                    "{error}; notebook remains at {}: {rollback}",
                    dest.display()
                ),
            });
        }
    }
    Ok(())
}

fn rollback_import<T>(
    vault_path: &str,
    relative: &str,
    result: Result<T, String>,
) -> Result<T, String> {
    result.map_err(
        |error| match fs::remove_file(Path::new(vault_path).join(relative)) {
            Ok(()) => error,
            Err(rollback) => format!("{error}; imported attachment rollback failed: {rollback}"),
        },
    )
}

pub fn import_notebook_icon(
    vault_path: &str,
    notebook_relative: &str,
    name: &str,
    data: &[u8],
) -> Result<String, String> {
    let relative = save_attachment(vault_path, name, data)?;
    rollback_import(
        vault_path,
        &relative,
        set_notebook_icon(vault_path, notebook_relative, Some(&relative)),
    )?;
    Ok(relative)
}

pub fn import_tag_icon(
    vault_path: &str,
    tag: &str,
    name: &str,
    data: &[u8],
) -> Result<TagStyle, String> {
    let relative = save_attachment(vault_path, name, data)?;
    let result = patch_tag_style(
        vault_path,
        tag,
        TagStyleChange::Icon(Some(relative.clone())),
    )
    .and_then(|style| style.ok_or_else(|| "Imported tag icon was not registered".to_string()));
    rollback_import(vault_path, &relative, result)
}

fn tag_styles_path(vault_path: &str) -> PathBuf {
    helixnotes_dir(vault_path).join("tag_styles.json")
}

fn normalize_tag_style_key(tag: &str) -> String {
    tag.trim().to_string()
}

fn tag_style_key_fold(tag: &str) -> String {
    tag.to_ascii_lowercase()
}

fn remove_matching_tag_styles(styles: &mut std::collections::HashMap<String, TagStyle>, tag: &str) {
    let fold = tag_style_key_fold(tag);
    styles.retain(|existing, _| tag_style_key_fold(existing) != fold);
}

fn normalize_tag_color(color: Option<&str>) -> Result<Option<String>, String> {
    let Some(color) = color.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let hex = color
        .strip_prefix('#')
        .ok_or_else(|| "Tag color must be a #hex value".to_string())?;
    if !matches!(hex.len(), 3 | 6) || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("Tag color must be #RGB or #RRGGBB".to_string());
    }
    Ok(Some(format!("#{}", hex.to_ascii_lowercase())))
}

fn normalize_tag_icon(icon: Option<&str>) -> Option<String> {
    icon.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.replace('\\', "/"))
}

pub fn load_tag_styles(
    vault_path: &str,
) -> Result<std::collections::HashMap<String, TagStyle>, String> {
    let styles_path = tag_styles_path(vault_path);
    let data = match fs::read_to_string(&styles_path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(std::collections::HashMap::new())
        }
        Err(error) => return Err(error.to_string()),
    };
    let styles: std::collections::HashMap<String, TagStyle> =
        serde_json::from_str(&data).map_err(|e| e.to_string())?;
    let mut collapsed = std::collections::HashMap::new();
    for (tag, style) in styles {
        normalize_tag_color(style.color.as_deref())?;
        let key = normalize_tag_style_key(&tag);
        if key.is_empty() || style.is_empty() {
            continue;
        }
        remove_matching_tag_styles(&mut collapsed, &key);
        collapsed.insert(key, style);
    }
    Ok(collapsed)
}

pub fn patch_tag_style(
    vault_path: &str,
    tag: &str,
    change: TagStyleChange,
) -> Result<Option<TagStyle>, String> {
    let key = normalize_tag_style_key(tag);
    if key.is_empty() {
        return Err("Tag name is required".to_string());
    }

    let mut styles = load_tag_styles(vault_path)?;
    let stored_key = styles
        .keys()
        .find(|existing| tag_style_key_fold(existing) == tag_style_key_fold(&key))
        .cloned()
        .unwrap_or(key);
    let mut next = styles.remove(&stored_key).unwrap_or_default();
    match change {
        TagStyleChange::Icon(value) => next.icon = normalize_tag_icon(value.as_deref()),
        TagStyleChange::Color(value) => next.color = normalize_tag_color(value.as_deref())?,
        TagStyleChange::Reset => next = TagStyle::default(),
    }
    let next = (!next.is_empty()).then_some(next);
    if let Some(style) = &next {
        styles.insert(stored_key, style.clone());
    }

    let dir = helixnotes_dir(vault_path);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let data = serde_json::to_string_pretty(&styles).map_err(|e| e.to_string())?;
    replace_file_atomically(&tag_styles_path(vault_path), data.as_bytes())?;
    Ok(next)
}

pub(crate) fn attachment_metadata_references(
    mapping: &str,
    content: &str,
) -> Result<Vec<String>, String> {
    if mapping == "notebook_icons.json" {
        let icons: std::collections::HashMap<String, String> =
            serde_json::from_str(content).map_err(|error| error.to_string())?;
        Ok(icons.into_values().collect())
    } else {
        let styles: std::collections::HashMap<String, TagStyle> =
            serde_json::from_str(content).map_err(|error| error.to_string())?;
        let mut references = Vec::new();
        for style in styles.into_values() {
            normalize_tag_color(style.color.as_deref())?;
            if let Some(icon) = style.icon {
                references.push(icon);
            }
        }
        Ok(references)
    }
}

type QuickAccessChange = (Vec<String>, Vec<String>);

fn quick_access_migration(
    vault_path: &str,
    src: &Path,
    dest: &Path,
    notebook: bool,
) -> Result<Option<QuickAccessChange>, String> {
    if src == dest {
        return Ok(None);
    }
    let previous = load_quick_access(vault_path)?;
    if previous.is_empty() {
        return Ok(None);
    }
    let old = normalize_notebook_icon_key(
        &src.strip_prefix(vault_path)
            .map_err(|error| error.to_string())?
            .to_string_lossy(),
    );
    let new = normalize_notebook_icon_key(
        &dest
            .strip_prefix(vault_path)
            .map_err(|error| error.to_string())?
            .to_string_lossy(),
    );
    let mut updated: Option<Vec<String>> = None;
    for (index, entry) in previous.iter().enumerate() {
        let normalized: std::borrow::Cow<'_, str> = if entry.contains('\\') {
            std::borrow::Cow::Owned(entry.replace('\\', "/"))
        } else {
            std::borrow::Cow::Borrowed(entry.as_str())
        };
        let mapped = if notebook {
            normalized
                .strip_prefix(&old)
                .and_then(|suffix| suffix.strip_prefix('/'))
                .map(|suffix| format!("{new}/{suffix}"))
        } else if normalized.as_ref() == old {
            Some(new.clone())
        } else {
            None
        };
        if let Some(mapped) = mapped {
            updated.get_or_insert_with(|| previous.clone())[index] = mapped;
        }
    }
    Ok(updated.map(|updated| (previous, updated)))
}

fn move_note_with_quick_access(
    vault_path: &str,
    src: &Path,
    dest: &Path,
) -> Result<Option<Vec<String>>, String> {
    let migration = quick_access_migration(vault_path, src, dest, false)?;
    move_without_replacing(src, dest)?;
    if let Some((previous, updated)) = migration {
        if let Err(error) = save_quick_access(vault_path, &updated) {
            return Err(match move_without_replacing(dest, src) {
                Ok(()) => error,
                Err(rollback) => format!("{error}; note remains at {}: {rollback}", dest.display()),
            });
        }
        Ok(Some(previous))
    } else {
        Ok(None)
    }
}

pub fn load_quick_access(vault_path: &str) -> Result<Vec<String>, String> {
    let qa_path = helixnotes_dir(vault_path).join("quick_access.json");
    if qa_path.exists() {
        let data = fs::read_to_string(&qa_path).map_err(|e| e.to_string())?;
        serde_json::from_str(&data).map_err(|e| e.to_string())
    } else {
        Ok(Vec::new())
    }
}

pub fn save_quick_access(vault_path: &str, paths: &[String]) -> Result<(), String> {
    let qa_path = helixnotes_dir(vault_path).join("quick_access.json");
    let data = serde_json::to_string_pretty(paths).map_err(|e| e.to_string())?;
    replace_file_atomically(&qa_path, data.as_bytes())
}

pub fn add_quick_access(vault_path: &str, note_relative: &str) -> Result<(), String> {
    let mut list = load_quick_access(vault_path)?;
    if !list.contains(&note_relative.to_string()) {
        list.push(note_relative.to_string());
        save_quick_access(vault_path, &list)?;
    }
    Ok(())
}

pub fn remove_quick_access(vault_path: &str, note_relative: &str) -> Result<(), String> {
    let mut list = load_quick_access(vault_path)?;
    list.retain(|p| p != note_relative);
    save_quick_access(vault_path, &list)?;
    Ok(())
}

const NOTE_SWITCHER_RECENT_LIMIT: usize = 6;

pub fn get_note_switcher_titles(
    vault_path: &str,
    recent_paths: &[String],
) -> Result<Vec<NoteTitleEntry>, String> {
    let vault_root = Path::new(vault_path);
    if !vault_root.is_dir() {
        return Err("Vault path does not exist".to_string());
    }

    let mut seen = HashSet::new();
    let mut titles = Vec::with_capacity(NOTE_SWITCHER_RECENT_LIMIT);

    for requested_path in recent_paths {
        if titles.len() >= NOTE_SWITCHER_RECENT_LIMIT {
            break;
        }

        let path = Path::new(requested_path);
        let Ok(relative) = path.strip_prefix(vault_root) else {
            continue;
        };
        let safe_relative = !relative.as_os_str().is_empty()
            && relative.components().all(|component| match component {
                Component::Normal(name) => !is_hidden(Path::new(name)),
                Component::CurDir => true,
                _ => false,
            });
        if !safe_relative
            || path.extension().and_then(|extension| extension.to_str()) != Some("md")
            || !path.is_file()
        {
            continue;
        }

        let relative_path = relative.to_path_buf();
        if !seen.insert(relative_path.clone()) {
            continue;
        }
        let Ok(entry) = read_note_entry_fast(path, vault_root) else {
            continue;
        };

        titles.push(NoteTitleEntry {
            title: entry.meta.title,
            path: relative_path.to_string_lossy().replace('\\', "/"),
        });
    }

    Ok(titles)
}

pub fn get_quick_access_notes(vault_path: &str) -> Result<Vec<NoteEntry>, String> {
    let list = load_quick_access(vault_path)?;
    let vault_root = Path::new(vault_path);
    let mut notes = Vec::new();

    for relative in &list {
        let full_path = vault_root.join(relative);
        if full_path.exists() {
            if let Ok(entry) = read_note_entry_fast(&full_path, vault_root) {
                notes.push(entry);
            }
        }
    }

    Ok(notes)
}

pub fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            _ => c,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        compare_natural_names, create_notebook, duplicate_note, get_note_switcher_titles,
        helixnotes_dir, load_notebook_icons, load_tag_styles, patch_tag_style, permanent_delete,
        read_note, replace_file_atomically, restore_notebook, save_note, scan_notebooks,
        set_notebook_icon,
    };
    use crate::types::{TagStyle, TagStyleChange};
    use std::fs;
    use std::io::Read;

    #[test]
    fn all_tags_excludes_only_root_metadata_subtree_and_hidden_filenames() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join(".helixnotes");
        let fixtures = [
            ("Root.md", "root"),
            (".helixnotes/history/Stored.md", "excluded"),
            ("Notebook/.helixnotes/Nested.md", "nested"),
            (".helixnotes-other/Visible.md", "similar"),
            (".obsidian/Visible.md", "obsidian"),
            ("assets/Visible.md", "assets"),
            (".other/Visible.md", "other"),
            ("Notebook/.Hidden.md", "hidden"),
            ("Notebook/Upper.MD", "uppercase"),
            ("Notebook/other.txt", "text"),
        ];
        for (relative, tag) in fixtures {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, format!("---\ntags: [{tag}]\n---\n# Note")).unwrap();
        }

        assert_eq!(
            super::get_all_tags(root.to_str().unwrap()).unwrap(),
            vec![
                ("assets".to_string(), 1),
                ("nested".to_string(), 1),
                ("obsidian".to_string(), 1),
                ("other".to_string(), 1),
                ("root".to_string(), 1),
                ("similar".to_string(), 1),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn all_tags_keeps_file_symlinks_without_following_directory_symlinks() {
        use std::os::unix::fs::symlink;
        let vault = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(
            outside.path().join("External.md"),
            "---\ntags: [linked]\n---\n# Note",
        )
        .unwrap();
        symlink(
            outside.path().join("External.md"),
            vault.path().join("Linked.md"),
        )
        .unwrap();
        symlink(outside.path(), vault.path().join("Directory")).unwrap();
        fs::create_dir(vault.path().join(".helixnotes")).unwrap();
        symlink(
            outside.path().join("External.md"),
            vault.path().join(".helixnotes/Excluded.md"),
        )
        .unwrap();

        assert_eq!(
            super::get_all_tags(vault.path().to_str().unwrap()).unwrap(),
            vec![("linked".to_string(), 1)]
        );
    }

    #[test]
    fn same_named_attachment_writes_never_replace_each_other() {
        let vault = tempfile::tempdir().unwrap();
        let path = vault.path().to_str().unwrap();
        super::ensure_vault_structure(path).unwrap();
        let assets = std::thread::scope(|scope| {
            let writes: Vec<_> = (0..16u8)
                .map(|byte| {
                    scope.spawn(move || {
                        let relative =
                            super::save_attachment(path, "same.png", &[byte; 8]).unwrap();
                        (relative, byte)
                    })
                })
                .collect();
            writes
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect::<Vec<_>>()
        });
        let mut names = std::collections::HashSet::new();
        for (relative, byte) in assets {
            assert!(names.insert(relative.clone()));
            assert_eq!(fs::read(vault.path().join(relative)).unwrap(), [byte; 8]);
        }
    }

    #[test]
    fn metadata_setters_atomically_replace_without_truncating_open_readers() {
        let vault = tempfile::tempdir().unwrap();
        let path = vault.path().to_str().unwrap();
        super::ensure_vault_structure(path).unwrap();
        set_notebook_icon(path, "Book", Some("builtin:folder")).unwrap();
        patch_tag_style(path, "Work", TagStyleChange::Color(Some("#123456".into()))).unwrap();
        let icons_path = helixnotes_dir(path).join("notebook_icons.json");
        let styles_path = helixnotes_dir(path).join("tag_styles.json");
        let icons_before = fs::read(&icons_path).unwrap();
        let styles_before = fs::read(&styles_path).unwrap();
        let mut icons_reader = fs::File::open(&icons_path).unwrap();
        let mut styles_reader = fs::File::open(&styles_path).unwrap();
        set_notebook_icon(path, "Book", Some("builtin:star")).unwrap();
        patch_tag_style(
            path,
            "Work",
            TagStyleChange::Icon(Some("builtin:briefcase".into())),
        )
        .unwrap();
        let mut icons_old = Vec::new();
        let mut styles_old = Vec::new();
        icons_reader.read_to_end(&mut icons_old).unwrap();
        styles_reader.read_to_end(&mut styles_old).unwrap();
        assert_eq!(icons_old, icons_before);
        assert_eq!(styles_old, styles_before);
        assert_eq!(load_notebook_icons(path).unwrap()["Book"], "builtin:star");
        assert_eq!(
            load_tag_styles(path).unwrap()["Work"],
            TagStyle {
                icon: Some("builtin:briefcase".into()),
                color: Some("#123456".into()),
            }
        );
    }

    #[test]
    fn failed_icon_registration_rolls_back_only_the_new_unique_attachment() {
        for mapping in ["notebook_icons.json", "tag_styles.json"] {
            let vault = tempfile::tempdir().unwrap();
            let path = vault.path().to_str().unwrap();
            super::ensure_vault_structure(path).unwrap();
            let original = super::save_attachment(path, "same.png", b"keep").unwrap();
            fs::write(helixnotes_dir(path).join(mapping), b"{broken").unwrap();
            let result = if mapping == "notebook_icons.json" {
                super::import_notebook_icon(path, "Book", "same.png", b"reject")
            } else {
                super::import_tag_icon(path, "Work", "same.png", b"reject")
                    .map(|style| style.icon.unwrap())
            };
            assert!(result.is_err());
            assert_eq!(fs::read(vault.path().join(&original)).unwrap(), b"keep");
            let remaining: Vec<_> = fs::read_dir(helixnotes_dir(path).join("attachments"))
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect();
            assert_eq!(remaining, vec![vault.path().join(original)]);
            assert_eq!(
                fs::read(helixnotes_dir(path).join(mapping)).unwrap(),
                b"{broken"
            );
        }
    }

    #[test]
    fn invalid_icon_mapping_prevents_notebook_move_before_renaming_files() {
        let vault = tempfile::tempdir().unwrap();
        let path = vault.path().to_str().unwrap();
        super::ensure_vault_structure(path).unwrap();
        let notebook = create_notebook(path, None, "Book").unwrap();
        let note = super::create_note(path, Some("Book"), "Keep").unwrap();
        let before = fs::read(&note.path).unwrap();
        let destination = create_notebook(path, None, "Archive").unwrap();
        fs::write(helixnotes_dir(path).join("notebook_icons.json"), b"{broken").unwrap();
        assert!(super::rename_notebook(path, &notebook.path, "Renamed").is_err());
        assert!(super::move_notebook(path, &notebook.path, &destination.path).is_err());
        assert_eq!(fs::read(&note.path).unwrap(), before);
        assert!(!vault.path().join("Renamed").exists());
        assert!(!vault.path().join("Archive/Book").exists());
    }

    #[cfg(unix)]
    #[test]
    fn failed_metadata_replacement_keeps_registration_and_rolls_back_notebook_move() {
        use std::os::unix::fs::PermissionsExt;
        let vault = tempfile::tempdir().unwrap();
        let path = vault.path().to_str().unwrap();
        super::ensure_vault_structure(path).unwrap();
        let notebook = create_notebook(path, None, "Book").unwrap();
        set_notebook_icon(path, "Book", Some("builtin:folder")).unwrap();
        let mapping = helixnotes_dir(path).join("notebook_icons.json");
        let before = fs::read(&mapping).unwrap();
        fs::set_permissions(&mapping, fs::Permissions::from_mode(0o440)).unwrap();
        assert!(set_notebook_icon(path, "Book", Some("builtin:star")).is_err());
        assert!(super::rename_notebook(path, &notebook.path, "Renamed").is_err());
        assert!(std::path::Path::new(&notebook.path).is_dir());
        assert!(!vault.path().join("Renamed").exists());
        assert_eq!(fs::read(&mapping).unwrap(), before);
    }
    use uuid::Uuid;

    #[test]
    fn compares_numeric_segments_anywhere_in_names() {
        let mut names = ["Class 10b", "Class 2b", "Class 10a", "Class 2a"];
        names.sort_by(|left, right| compare_natural_names(left, right));
        assert_eq!(names, ["Class 2a", "Class 2b", "Class 10a", "Class 10b"]);
        assert_eq!(
            compare_natural_names("Class 02", "Class 2b"),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn sorts_notebooks_with_numeric_names_naturally() {
        let vault =
            std::env::temp_dir().join(format!("helixnotes-natural-sort-test-{}", Uuid::new_v4()));
        let nested = vault.join("Classes");
        fs::create_dir_all(&nested).unwrap();
        let expected = ["5g", "6g", "7g", "8g", "9g", "10g", "11g", "12g"];
        for name in ["10g", "11g", "12g", "5g", "6g", "7g", "8g", "9g"] {
            fs::create_dir(vault.join(name)).unwrap();
            fs::create_dir(nested.join(name)).unwrap();
        }

        let notebooks = scan_notebooks(&vault.to_string_lossy()).unwrap();
        let root_names: Vec<_> = notebooks
            .iter()
            .filter(|notebook| notebook.name != "Classes")
            .map(|notebook| notebook.name.as_str())
            .collect();
        assert_eq!(root_names, expected);
        let nested_names: Vec<_> = notebooks
            .iter()
            .find(|notebook| notebook.name == "Classes")
            .unwrap()
            .children
            .iter()
            .map(|notebook| notebook.name.as_str())
            .collect();
        assert_eq!(nested_names, expected);

        fs::remove_dir_all(vault).unwrap();
    }

    #[test]
    fn persists_and_removes_builtin_notebook_icons() {
        let vault =
            std::env::temp_dir().join(format!("helixnotes-notebook-icon-test-{}", Uuid::new_v4()));
        let vault_path = vault.to_string_lossy();
        fs::create_dir_all(helixnotes_dir(&vault_path)).unwrap();

        set_notebook_icon(&vault_path, r"Projects\Client", Some("builtin:briefcase")).unwrap();
        let stored: std::collections::HashMap<String, String> = serde_json::from_str(
            &fs::read_to_string(helixnotes_dir(&vault_path).join("notebook_icons.json")).unwrap(),
        )
        .unwrap();
        assert!(stored.contains_key("Projects/Client"));
        assert!(!stored.contains_key(r"Projects\Client"));

        let icons = load_notebook_icons(&vault_path).unwrap();
        assert_eq!(
            icons.get("Projects/Client").map(String::as_str),
            Some("builtin:briefcase")
        );

        set_notebook_icon(&vault_path, "Projects/Client", None).unwrap();
        assert!(load_notebook_icons(&vault_path).unwrap().is_empty());

        fs::remove_dir_all(vault).unwrap();
    }

    #[test]
    fn normalizes_legacy_notebook_icon_keys_when_loading() {
        let vault =
            std::env::temp_dir().join(format!("helixnotes-notebook-icon-test-{}", Uuid::new_v4()));
        let vault_path = vault.to_string_lossy();
        let icons_path = helixnotes_dir(&vault_path).join("notebook_icons.json");
        fs::create_dir_all(helixnotes_dir(&vault_path)).unwrap();
        fs::write(&icons_path, r#"{"Projects\\Client":"builtin:folder"}"#).unwrap();

        let icons = load_notebook_icons(&vault_path).unwrap();
        assert_eq!(
            icons.get("Projects/Client").map(String::as_str),
            Some("builtin:folder")
        );

        fs::remove_dir_all(vault).unwrap();
    }

    #[test]
    fn persists_and_removes_tag_styles() {
        let vault =
            std::env::temp_dir().join(format!("helixnotes-tag-style-test-{}", Uuid::new_v4()));
        let vault_path = vault.to_string_lossy();

        patch_tag_style(
            &vault_path,
            " Work ",
            TagStyleChange::Icon(Some("builtin:briefcase".into())),
        )
        .unwrap();
        patch_tag_style(
            &vault_path,
            "Work",
            TagStyleChange::Color(Some("#E11D48".into())),
        )
        .unwrap();

        let stored: std::collections::HashMap<String, TagStyle> = serde_json::from_str(
            &fs::read_to_string(helixnotes_dir(&vault_path).join("tag_styles.json")).unwrap(),
        )
        .unwrap();
        assert!(stored.contains_key("Work"));
        assert!(!stored.contains_key(" Work "));

        let styles = load_tag_styles(&vault_path).unwrap();
        let work = styles.get("Work").expect("work tag style");
        assert_eq!(work.icon.as_deref(), Some("builtin:briefcase"));
        assert_eq!(work.color.as_deref(), Some("#e11d48"));

        patch_tag_style(&vault_path, "Work", TagStyleChange::Reset).unwrap();
        assert!(load_tag_styles(&vault_path).unwrap().is_empty());

        fs::remove_dir_all(vault).unwrap();
    }

    #[test]
    fn resetting_tag_style_is_case_insensitive() {
        let vault =
            std::env::temp_dir().join(format!("helixnotes-tag-style-case-test-{}", Uuid::new_v4()));
        let vault_path = vault.to_string_lossy();

        patch_tag_style(
            &vault_path,
            "Work",
            TagStyleChange::Icon(Some("builtin:briefcase".into())),
        )
        .unwrap();
        patch_tag_style(
            &vault_path,
            "Work",
            TagStyleChange::Color(Some("#e11d48".into())),
        )
        .unwrap();

        patch_tag_style(&vault_path, "work", TagStyleChange::Reset).unwrap();

        let stored: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(helixnotes_dir(&vault_path).join("tag_styles.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(stored, serde_json::json!({}));
        assert!(load_tag_styles(&vault_path).unwrap().is_empty());

        fs::remove_dir_all(vault).unwrap();
    }

    #[test]
    fn load_collapses_case_variant_tag_style_keys() {
        let vault = std::env::temp_dir().join(format!(
            "helixnotes-tag-style-collapse-test-{}",
            Uuid::new_v4()
        ));
        let vault_path = vault.to_string_lossy();
        fs::create_dir_all(helixnotes_dir(&vault_path)).unwrap();
        fs::write(
            helixnotes_dir(&vault_path).join("tag_styles.json"),
            r##"{"Work":{"icon":"builtin:briefcase"},"work":{"color":"#e11d48"}}"##,
        )
        .unwrap();

        let styles = load_tag_styles(&vault_path).unwrap();
        assert_eq!(styles.len(), 1);
        let (key, style) = styles.iter().next().unwrap();
        assert_eq!(key.to_ascii_lowercase(), "work");
        assert!(style.icon.is_some() || style.color.is_some());

        fs::remove_dir_all(vault).unwrap();
    }

    #[test]
    fn rejects_invalid_tag_color() {
        let vault = std::env::temp_dir().join(format!(
            "helixnotes-tag-style-color-test-{}",
            Uuid::new_v4()
        ));
        let vault_path = vault.to_string_lossy();

        let err = patch_tag_style(
            &vault_path,
            "daily",
            TagStyleChange::Color(Some("red".into())),
        )
        .unwrap_err();
        assert!(err.contains("hex"));

        fs::remove_dir_all(&vault).ok();
    }

    #[test]
    fn clearing_empty_tag_style_removes_entry() {
        let vault = std::env::temp_dir().join(format!(
            "helixnotes-tag-style-clear-test-{}",
            Uuid::new_v4()
        ));
        let vault_path = vault.to_string_lossy();

        patch_tag_style(
            &vault_path,
            "daily",
            TagStyleChange::Icon(Some("builtin:calendar".into())),
        )
        .unwrap();
        patch_tag_style(&vault_path, "daily", TagStyleChange::Icon(None)).unwrap();
        assert!(load_tag_styles(&vault_path).unwrap().is_empty());

        fs::remove_dir_all(vault).unwrap();
    }

    #[test]
    fn loads_only_requested_note_switcher_titles() {
        let vault =
            std::env::temp_dir().join(format!("helixnotes-note-switcher-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&vault).unwrap();

        let mut note_paths = Vec::new();
        for index in 0..8 {
            let path = vault.join(format!("Note {index}.md"));
            fs::write(
                &path,
                format!("---\ntitle: Note {index}\n---\n\nNote {index} body.\n"),
            )
            .unwrap();
            note_paths.push(path);
        }
        fs::write(
            vault.join("Unrelated.md"),
            "---\ntitle: Unrelated\n---\n\nMust not be loaded.\n",
        )
        .unwrap();

        let outside =
            std::env::temp_dir().join(format!("helixnotes-outside-note-{}.md", Uuid::new_v4()));
        fs::write(&outside, "---\ntitle: Outside\n---\n").unwrap();

        let requested = vec![
            note_paths[0].to_string_lossy().into_owned(),
            note_paths[0].to_string_lossy().into_owned(),
            vault.join("Missing.md").to_string_lossy().into_owned(),
            outside.to_string_lossy().into_owned(),
            note_paths[1].to_string_lossy().into_owned(),
            note_paths[2].to_string_lossy().into_owned(),
            note_paths[3].to_string_lossy().into_owned(),
            note_paths[4].to_string_lossy().into_owned(),
            note_paths[5].to_string_lossy().into_owned(),
            note_paths[6].to_string_lossy().into_owned(),
            note_paths[7].to_string_lossy().into_owned(),
        ];

        let vault_path = vault.to_string_lossy();
        let titles = get_note_switcher_titles(&vault_path, &requested).unwrap();
        assert_eq!(titles.len(), 6);
        assert_eq!(
            titles
                .iter()
                .map(|entry| entry.path.as_str())
                .collect::<Vec<_>>(),
            vec![
                "Note 0.md",
                "Note 1.md",
                "Note 2.md",
                "Note 3.md",
                "Note 4.md",
                "Note 5.md",
            ]
        );
        assert!(titles.iter().all(|entry| entry.title != "Unrelated"));
        assert!(titles.iter().all(|entry| entry.title != "Outside"));

        fs::remove_dir_all(vault).unwrap();
        fs::remove_file(outside).unwrap();
    }

    #[test]
    fn reads_markdown_notes_from_trash_without_allowing_external_files() {
        let test_root =
            std::env::temp_dir().join(format!("helixnotes-path-security-test-{}", Uuid::new_v4()));
        let vault = test_root.join("vault");
        let trash = helixnotes_dir(&vault.to_string_lossy()).join("trash");
        let trashed_note = trash.join("20240101000000000_Note.md");
        let outside = test_root.join("outside.md");
        fs::create_dir_all(&trash).unwrap();
        fs::write(&trashed_note, "---\ntitle: Note\n---\n\ntrashed").unwrap();
        fs::write(&outside, "outside").unwrap();

        assert!(read_note(&vault.to_string_lossy(), &trashed_note.to_string_lossy()).is_ok());
        assert!(read_note(&vault.to_string_lossy(), &outside.to_string_lossy()).is_err());

        fs::remove_dir_all(test_root).unwrap();
    }

    #[test]
    fn rejects_permanent_deletion_outside_trash() {
        let test_root =
            std::env::temp_dir().join(format!("helixnotes-path-security-test-{}", Uuid::new_v4()));
        let vault = test_root.join("vault");
        let outside = test_root.join("outside.md");
        fs::create_dir_all(helixnotes_dir(&vault.to_string_lossy()).join("trash")).unwrap();
        fs::write(&outside, "must survive").unwrap();

        let result = permanent_delete(&vault.to_string_lossy(), &outside.to_string_lossy());

        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&outside).unwrap(), "must survive");
        fs::remove_dir_all(test_root).unwrap();
    }

    #[test]
    fn rejects_notebook_creation_outside_vault() {
        let test_root =
            std::env::temp_dir().join(format!("helixnotes-path-security-test-{}", Uuid::new_v4()));
        let vault = test_root.join("vault");
        fs::create_dir_all(&vault).unwrap();

        let result = create_notebook(&vault.to_string_lossy(), Some(".."), "escaped");

        assert!(result.is_err());
        assert!(!test_root.join("escaped").exists());
        fs::remove_dir_all(test_root).unwrap();
    }

    #[test]
    fn rejects_traversal_in_restored_notebook_metadata() {
        let test_root =
            std::env::temp_dir().join(format!("helixnotes-path-security-test-{}", Uuid::new_v4()));
        let vault = test_root.join("vault");
        let trash = helixnotes_dir(&vault.to_string_lossy()).join("trash");
        let trashed_notebook = trash.join("20240101000000000_Notebook");
        fs::create_dir_all(&trashed_notebook).unwrap();
        fs::write(trash.join("20240101000000000_Notebook.meta"), "../escaped").unwrap();

        let result = restore_notebook(
            &vault.to_string_lossy(),
            &trashed_notebook.to_string_lossy(),
        );

        assert!(result.is_err());
        assert!(trashed_notebook.exists());
        assert!(!test_root.join("escaped").exists());
        fs::remove_dir_all(test_root).unwrap();
    }

    #[test]
    fn duplicates_note_content_and_assigns_unique_identity_and_name() {
        let vault =
            std::env::temp_dir().join(format!("helixnotes-duplicate-note-test-{}", Uuid::new_v4()));
        let notebook = vault.join("Projects");
        fs::create_dir_all(&notebook).unwrap();
        let source_path = notebook.join("Project.md");
        let source_raw = "---\nid: source-id\ntitle: Project\ntags:\n  - work\npinned: true\ncreated: 2020-01-01T00:00:00Z\nmodified: 2020-01-02T00:00:00Z\naliases:\n  - Plan\n---\n# Project\n\nOriginal body.\n";
        fs::write(&source_path, source_raw).unwrap();

        let vault_path = vault.to_string_lossy();
        let first = duplicate_note(&source_path.to_string_lossy(), &vault_path).unwrap();
        let second = duplicate_note(&source_path.to_string_lossy(), &vault_path).unwrap();

        assert_eq!(first.meta.title, "Project copy");
        assert_eq!(second.meta.title, "Project copy 2");
        assert_eq!(first.meta.tags, vec!["work"]);
        assert!(!first.meta.pinned);
        assert_ne!(first.meta.id, "source-id");
        assert_ne!(first.meta.id, second.meta.id);
        assert_eq!(first.relative_path, "Projects/Project copy.md");
        assert_eq!(second.relative_path, "Projects/Project copy 2.md");

        let first_raw = fs::read_to_string(&first.path).unwrap();
        assert!(first_raw.contains("aliases:\n- Plan"));
        assert!(first_raw.contains("# Project copy\n\nOriginal body."));
        assert!(!first_raw.contains("\n# Project\n"));
        assert_eq!(fs::read_to_string(&source_path).unwrap(), source_raw);

        fs::remove_dir_all(vault).unwrap();
    }

    #[test]
    fn mobile_listing_preserves_creation_date_after_atomic_save() {
        let vault = tempfile::tempdir().unwrap();
        let path = vault.path().join("Note.md");
        fs::write(&path, "---\nid: existing\ntitle: Note\ncreated: 2020-01-01T00:00:00Z\n---\n# Note\n\nOld body.\n").unwrap();
        let note = read_note(vault.path().to_str().unwrap(), path.to_str().unwrap()).unwrap();
        save_note(
            vault.path().to_str().unwrap(),
            path.to_str().unwrap(),
            &note.meta,
            "New body.",
            &note.raw,
        )
        .unwrap();
        let mobile = super::read_note_entry_metadata_only(&path, vault.path()).unwrap();
        let desktop = super::read_note_entry_fast(&path, vault.path()).unwrap();
        assert_eq!(mobile.meta.created, note.meta.created);
        assert_eq!(mobile.meta.created, desktop.meta.created);
        assert_eq!(mobile.preview, "New body.");
    }

    #[test]
    fn unchanged_serialized_save_ignores_modified_without_rewriting_the_file() {
        let vault = tempfile::tempdir().unwrap();
        let path = vault.path().join("Note.md");
        let original = "---\nid: existing\ntitle: Note\ncreated: 2020-01-01T00:00:00Z\nmodified: 2020-01-02T00:00:00Z\naliases: [Legacy]\n---\n# Note\n\nUnchanged body.\n";
        fs::write(&path, original).unwrap();
        let vault_path = vault.path().to_string_lossy();
        let note_path = path.to_string_lossy();
        let note = read_note(&vault_path, &note_path).unwrap();
        let before = fs::metadata(&path).unwrap().modified().unwrap();
        let mut meta = note.meta;
        meta.modified = chrono::Utc::now();
        let serialized_body = format!("{}\n", note.content);

        assert!(
            save_note(&vault_path, &note_path, &meta, &serialized_body, original)
                .unwrap()
                .is_none()
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), before);
    }

    #[test]
    fn save_keeps_authored_terminal_spaces_before_the_serializer_newline() {
        let vault = tempfile::tempdir().unwrap();
        let path = vault.path().join("Note.md");
        let original = "---\nid: existing\ntitle: Note\ncreated: 2020-01-01T00:00:00Z\n---\n# Note\n\nFinal line.\n";
        fs::write(&path, original).unwrap();
        let vault_path = vault.path().to_string_lossy();
        let note_path = path.to_string_lossy();
        let note = read_note(&vault_path, &note_path).unwrap();
        let body = format!("{}  \n", note.content);

        assert!(
            save_note(&vault_path, &note_path, &note.meta, &body, &note.raw)
                .unwrap()
                .is_some()
        );
        assert!(fs::read_to_string(&path)
            .unwrap()
            .ends_with("Final line.  \n"));
    }

    #[test]
    fn saves_keep_first_identity_unknown_fields_and_previous_content() {
        let vault = tempfile::tempdir().unwrap();
        let path = vault.path().join("Note.md");
        let original = "---\ntitle: Note\ncreated: 2020-01-01T00:00:00Z\naliases: [Legacy]\n---\nOriginal body.\n";
        fs::write(&path, original).unwrap();
        let vault_path = vault.path().to_string_lossy();
        let note_path = path.to_string_lossy();
        let meta = read_note(&vault_path, &note_path).unwrap().meta;
        assert!(meta.id.is_empty());

        let first = save_note(
            &vault_path,
            &note_path,
            &meta,
            "# Note\n\nFirst change.\n",
            original,
        )
        .unwrap()
        .unwrap();
        Uuid::parse_str(&first.id).unwrap();
        assert_eq!(first.previous_raw, original);
        let first_raw = fs::read_to_string(&path).unwrap();
        let second = save_note(
            &vault_path,
            &note_path,
            &meta,
            "# Note\n\nSecond change.\n",
            &first_raw,
        )
        .unwrap()
        .unwrap();
        assert_eq!(second.id, first.id);
        assert_eq!(second.previous_raw, first_raw);

        let saved = read_note(&vault_path, &note_path).unwrap();
        assert_eq!(saved.meta.id, first.id);
        assert_eq!(saved.meta.created, meta.created);
        assert!(saved.content.contains("Second change."));
        let parsed = gray_matter::Matter::<gray_matter::engine::YAML>::new().parse(&saved.raw);
        let fields: serde_yaml::Value = parsed.data.unwrap().deserialize().unwrap();
        assert_eq!(fields["aliases"][0].as_str(), Some("Legacy"));
    }

    #[test]
    fn atomic_replacement_does_not_truncate_an_open_original() {
        let vault = tempfile::tempdir().unwrap();
        let path = vault.path().join("Note.md");
        fs::write(&path, "original contents").unwrap();
        let mut original = fs::File::open(&path).unwrap();

        replace_file_atomically(&path, b"replacement contents").unwrap();

        let mut old_contents = String::new();
        original.read_to_string(&mut old_contents).unwrap();
        assert_eq!(old_contents, "original contents");
        assert_eq!(fs::read_to_string(&path).unwrap(), "replacement contents");
    }

    #[test]
    fn failed_atomic_replacement_cleans_up_without_removing_destination() {
        let vault = tempfile::tempdir().unwrap();
        let destination = vault.path().join("Note.md");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("original"), "keep me").unwrap();

        assert!(replace_file_atomically(&destination, b"replacement").is_err());

        assert_eq!(
            fs::read_to_string(destination.join("original")).unwrap(),
            "keep me"
        );
        let remaining: Vec<_> = fs::read_dir(vault.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(remaining, vec![std::ffi::OsString::from("Note.md")]);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_save_preserves_permissions_and_refuses_read_only_notes() {
        use std::os::unix::fs::PermissionsExt;

        let vault = tempfile::tempdir().unwrap();
        let path = vault.path().join("Note.md");
        fs::write(&path, "original").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        replace_file_atomically(&path, b"saved").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );

        fs::set_permissions(&path, fs::Permissions::from_mode(0o440)).unwrap();
        assert!(replace_file_atomically(&path, b"must not save").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "saved");
    }
    #[test]
    fn stale_save_does_not_overwrite_external_edits_even_when_requested_body_is_unchanged() {
        let vault = tempfile::tempdir().unwrap();
        let path = vault.path().join("Note.md");
        fs::write(&path, "---\nid: original\ntitle: Note\n---\nOriginal\n").unwrap();
        let vault_path = vault.path().to_str().unwrap();
        let note = read_note(vault_path, path.to_str().unwrap()).unwrap();
        let external = "---\nid: original\ntitle: Note\naliases: [External]\n---\nOriginal\n";
        fs::write(&path, external).unwrap();
        let error = save_note(
            vault_path,
            path.to_str().unwrap(),
            &note.meta,
            &note.content,
            &note.raw,
        )
        .err()
        .unwrap();
        assert!(error.starts_with("Save conflict:"));
        assert_eq!(fs::read_to_string(&path).unwrap(), external);
    }

    #[test]
    fn rename_reports_exact_old_and_saved_baselines_to_open_editors() {
        let vault = tempfile::tempdir().unwrap();
        let root = vault.path();
        let old_path = root.join("Before.md");
        let old_raw = "---\nid: stable\ntitle: Before\naliases: [Legacy]\n---\nBody.\n";
        fs::write(&old_path, old_raw).unwrap();
        let result =
            super::rename_note(old_path.to_str().unwrap(), "After", root.to_str().unwrap())
                .unwrap();
        assert_eq!(result.path, root.join("After.md").to_string_lossy());
        assert_eq!(result.old_raw, old_raw);
        assert_eq!(result.old_title, "Before");
        assert_eq!(result.new_title, "After");
        assert_eq!(result.new_raw, fs::read_to_string(&result.path).unwrap());
        assert!(result.new_raw.contains("aliases:"));
        assert!(!old_path.exists());
    }

    #[test]
    fn favorites_follow_note_and_notebook_renames_and_moves_with_mixed_separators() {
        let vault = tempfile::tempdir().unwrap();
        let root = vault.path();
        let vault_path = root.to_str().unwrap();
        super::ensure_vault_structure(vault_path).unwrap();
        fs::create_dir(root.join("Drafts")).unwrap();
        fs::create_dir(root.join("DraftsExtra")).unwrap();
        fs::create_dir(root.join("Archive")).unwrap();
        fs::create_dir(root.join("Group")).unwrap();
        fs::write(root.join("Drafts/First.md"), "---\ntitle: First\n---\nbody").unwrap();
        fs::write(root.join("DraftsExtra/Keep.md"), "other").unwrap();
        super::save_quick_access(
            vault_path,
            &["Drafts\\First.md".into(), "DraftsExtra/Keep.md".into()],
        )
        .unwrap();
        let renamed = super::rename_note(
            root.join("Drafts/First.md").to_str().unwrap(),
            "Second",
            vault_path,
        )
        .unwrap();
        assert_eq!(
            super::load_quick_access(vault_path).unwrap(),
            ["Drafts/Second.md", "DraftsExtra/Keep.md",]
        );
        let moved = super::move_note(
            vault_path,
            &renamed.path,
            root.join("Archive").to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(
            super::load_quick_access(vault_path).unwrap()[0],
            "Archive/Second.md"
        );
        let note_after_move = fs::read_to_string(&moved).unwrap();

        super::save_quick_access(
            vault_path,
            &[
                "Archive\\Second.md".into(),
                "Archive/Sub\\Other.md".into(),
                "DraftsExtra/Keep.md".into(),
            ],
        )
        .unwrap();
        set_notebook_icon(vault_path, "Archive", Some("builtin:folder")).unwrap();
        let renamed_book = super::rename_notebook(
            vault_path,
            root.join("Archive").to_str().unwrap(),
            "Journal",
        )
        .unwrap();
        assert_eq!(
            super::load_quick_access(vault_path).unwrap(),
            [
                "Journal/Second.md",
                "Journal/Sub/Other.md",
                "DraftsExtra/Keep.md",
            ]
        );
        super::move_notebook(
            vault_path,
            &renamed_book,
            root.join("Group").to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(
            super::load_quick_access(vault_path).unwrap(),
            [
                "Group/Journal/Second.md",
                "Group/Journal/Sub/Other.md",
                "DraftsExtra/Keep.md",
            ]
        );
        assert_eq!(
            fs::read_to_string(root.join("Group/Journal/Second.md")).unwrap(),
            note_after_move
        );
        assert_eq!(
            load_notebook_icons(vault_path).unwrap()["Group/Journal"],
            "builtin:folder"
        );
    }

    #[cfg(unix)]
    #[test]
    fn failed_favorites_save_rolls_back_note_and_notebook_paths() {
        use std::os::unix::fs::PermissionsExt;
        let vault = tempfile::tempdir().unwrap();
        let root = vault.path();
        let vault_path = root.to_str().unwrap();
        super::ensure_vault_structure(vault_path).unwrap();
        fs::create_dir(root.join("Book")).unwrap();
        fs::create_dir(root.join("Target")).unwrap();
        let note = root.join("Book/Note.md");
        let original = "---\ntitle: Note\n---\noriginal";
        fs::write(&note, original).unwrap();
        super::save_quick_access(vault_path, &["Book/Note.md".into()]).unwrap();
        let favorites = helixnotes_dir(vault_path).join("quick_access.json");
        let before = fs::read(&favorites).unwrap();
        fs::set_permissions(&favorites, fs::Permissions::from_mode(0o440)).unwrap();

        assert!(super::rename_note(note.to_str().unwrap(), "Changed", vault_path).is_err());
        assert!(super::move_note(
            vault_path,
            note.to_str().unwrap(),
            root.join("Target").to_str().unwrap()
        )
        .is_err());
        assert!(
            super::rename_notebook(vault_path, root.join("Book").to_str().unwrap(), "Renamed")
                .is_err()
        );
        assert!(super::move_notebook(
            vault_path,
            root.join("Book").to_str().unwrap(),
            root.join("Target").to_str().unwrap()
        )
        .is_err());
        assert_eq!(fs::read_to_string(&note).unwrap(), original);
        assert!(!root.join("Book/Changed.md").exists());
        assert!(!root.join("Target/Note.md").exists());
        assert!(!root.join("Renamed").exists());
        assert!(!root.join("Target/Book").exists());
        assert_eq!(fs::read(&favorites).unwrap(), before);
    }

    #[test]
    fn rename_move_and_restore_reject_collisions_without_changing_either_note() {
        let vault = tempfile::tempdir().unwrap();
        let root = vault.path();
        let vault_path = root.to_str().unwrap();
        super::ensure_vault_structure(vault_path).unwrap();
        let src = root.join("Source.md");
        let existing = root.join("Taken.md");
        fs::write(&src, "---\ntitle: Source\n---\nSOURCE").unwrap();
        fs::write(&existing, "DESTINATION").unwrap();
        assert!(super::rename_note(src.to_str().unwrap(), "Taken", vault_path).is_err());
        assert_eq!(
            fs::read_to_string(&src).unwrap(),
            "---\ntitle: Source\n---\nSOURCE"
        );
        assert_eq!(fs::read_to_string(&existing).unwrap(), "DESTINATION");

        let target_dir = root.join("Target");
        fs::create_dir(&target_dir).unwrap();
        fs::write(target_dir.join("Source.md"), "TARGET").unwrap();
        assert!(super::move_note(
            vault_path,
            src.to_str().unwrap(),
            target_dir.to_str().unwrap()
        )
        .is_err());
        assert_eq!(
            fs::read_to_string(&src).unwrap(),
            "---\ntitle: Source\n---\nSOURCE"
        );
        assert_eq!(
            fs::read_to_string(target_dir.join("Source.md")).unwrap(),
            "TARGET"
        );

        super::delete_note(vault_path, src.to_str().unwrap()).unwrap();
        let trash = super::get_trash_contents(vault_path).unwrap().notes;
        assert_eq!(trash.len(), 1);
        fs::write(&src, "REPLACEMENT").unwrap();
        assert!(super::restore_note(vault_path, &trash[0].path, None).is_err());
        assert_eq!(fs::read_to_string(&src).unwrap(), "REPLACEMENT");
        assert_eq!(
            fs::read_to_string(&trash[0].path).unwrap(),
            "---\ntitle: Source\n---\nSOURCE"
        );
    }

    #[test]
    fn rapid_repeated_trash_preserves_all_same_named_notes_and_restores_original_name() {
        let vault = tempfile::tempdir().unwrap();
        let vault_path = vault.path().to_str().unwrap();
        super::ensure_vault_structure(vault_path).unwrap();
        let note = vault.path().join("Repeat.md");
        for index in 0..200 {
            fs::write(&note, format!("---\ntitle: Repeat\n---\nCopy {index}")).unwrap();
            super::delete_note(vault_path, note.to_str().unwrap()).unwrap();
        }
        let trash = super::get_trash_contents(vault_path).unwrap();
        assert_eq!(trash.notes.len(), 200);
        let contents: std::collections::HashSet<_> = trash
            .notes
            .iter()
            .map(|entry| fs::read_to_string(&entry.path).unwrap())
            .collect();
        assert_eq!(contents.len(), 200);
        let restored = super::restore_note(vault_path, &trash.notes[0].path, None).unwrap();
        assert_eq!(restored, note.to_string_lossy());
        assert!(fs::read_to_string(&restored).unwrap().contains("Copy "));
    }

    #[test]
    fn notebook_trash_sidecars_survive_repeated_deletion_and_restore_collisions() {
        let vault = tempfile::tempdir().unwrap();
        let root = vault.path();
        let vault_path = root.to_str().unwrap();
        super::ensure_vault_structure(vault_path).unwrap();
        fs::create_dir(root.join("Parent")).unwrap();
        for index in 0..30 {
            fs::create_dir(root.join("Parent/Book")).unwrap();
            fs::write(root.join("Parent/Book/Note.md"), format!("Book {index}")).unwrap();
            super::delete_notebook(vault_path, root.join("Parent/Book").to_str().unwrap()).unwrap();
        }
        let trash = super::get_trash_contents(vault_path).unwrap();
        assert_eq!(trash.notebooks.len(), 30);
        assert!(trash
            .notebooks
            .iter()
            .all(|entry| entry.name == "Book" && entry.note_count == 1));
        fs::create_dir(root.join("Parent/Book")).unwrap();
        fs::write(root.join("Parent/Book/Note.md"), "current").unwrap();
        let old = &trash.notebooks[0].path;
        assert!(super::restore_notebook(vault_path, old).is_err());
        assert_eq!(
            fs::read_to_string(root.join("Parent/Book/Note.md")).unwrap(),
            "current"
        );
        assert!(std::path::Path::new(old).exists());
        fs::remove_dir_all(root.join("Parent/Book")).unwrap();
        let restored = super::restore_notebook(vault_path, old).unwrap();
        assert_eq!(restored, root.join("Parent/Book").to_string_lossy());
        assert!(fs::read_to_string(root.join("Parent/Book/Note.md"))
            .unwrap()
            .starts_with("Book "));
    }

    #[test]
    fn notebook_rename_and_move_reject_existing_directories_without_losing_icons() {
        let vault = tempfile::tempdir().unwrap();
        let root = vault.path();
        let vault_path = root.to_str().unwrap();
        super::ensure_vault_structure(vault_path).unwrap();
        let book = create_notebook(vault_path, None, "Book").unwrap();
        fs::write(root.join("Book/Only.md"), "original").unwrap();
        set_notebook_icon(vault_path, "Book", Some("builtin:star")).unwrap();
        fs::create_dir(root.join("Taken")).unwrap();
        assert!(super::rename_notebook(vault_path, &book.path, "Taken").is_err());
        fs::create_dir(root.join("Target")).unwrap();
        fs::create_dir(root.join("Target/Book")).unwrap();
        assert!(super::move_notebook(
            vault_path,
            &book.path,
            root.join("Target").to_str().unwrap()
        )
        .is_err());
        assert_eq!(
            fs::read_to_string(root.join("Book/Only.md")).unwrap(),
            "original"
        );
        assert_eq!(
            load_notebook_icons(vault_path).unwrap()["Book"],
            "builtin:star"
        );
    }

    #[test]
    fn legacy_trash_names_restore_without_a_sidecar() {
        let vault = tempfile::tempdir().unwrap();
        let vault_path = vault.path().to_str().unwrap();
        super::ensure_vault_structure(vault_path).unwrap();
        let trash = helixnotes_dir(vault_path).join("trash");
        let old_note = trash.join("20260101000000_Old.md");
        fs::write(&old_note, "---\ntitle: Old\n---\nlegacy note").unwrap();
        let restored_note =
            super::restore_note(vault_path, old_note.to_str().unwrap(), None).unwrap();
        assert_eq!(restored_note, vault.path().join("Old.md").to_string_lossy());
        assert!(fs::read_to_string(&restored_note)
            .unwrap()
            .contains("legacy note"));

        let old_notebook = trash.join("20260101000000000_Old Book");
        fs::create_dir(&old_notebook).unwrap();
        fs::write(old_notebook.join("Keep.md"), "legacy book").unwrap();
        let restored_book = restore_notebook(vault_path, old_notebook.to_str().unwrap()).unwrap();
        assert_eq!(
            restored_book,
            vault.path().join("Old Book").to_string_lossy()
        );
        assert_eq!(
            fs::read_to_string(vault.path().join("Old Book/Keep.md")).unwrap(),
            "legacy book"
        );
    }

    #[cfg(unix)]
    #[test]
    fn daily_and_restored_notebook_cannot_follow_symlinks_out_of_vault() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let vault = root.path().join("vault");
        let outside = root.path().join("outside");
        fs::create_dir(&vault).unwrap();
        fs::create_dir(&outside).unwrap();
        let vault_path = vault.to_str().unwrap();
        super::ensure_vault_structure(vault_path).unwrap();
        symlink(&outside, vault.join("Daily")).unwrap();
        assert!(super::create_daily_note(vault_path, Some("2026-01-01"), "iso").is_err());
        assert!(!outside.join("2026-01-01.md").exists());
        fs::remove_file(vault.join("Daily")).unwrap();
        let existing = vault.join("Daily/2026-01-01.md");
        fs::create_dir(vault.join("Daily")).unwrap();
        fs::write(outside.join("2026-01-01.md"), "outside").unwrap();
        symlink(outside.join("2026-01-01.md"), &existing).unwrap();
        assert!(super::create_daily_note(vault_path, Some("2026-01-01"), "iso").is_err());
        assert_eq!(
            fs::read_to_string(outside.join("2026-01-01.md")).unwrap(),
            "outside"
        );

        let trash = helixnotes_dir(vault_path).join("trash");
        let notebook = trash.join("20260101000000000_Stashed");
        fs::create_dir(&notebook).unwrap();
        fs::write(notebook.join("Keep.md"), "safe").unwrap();
        fs::write(
            trash.join("20260101000000000_Stashed.meta"),
            "Escape/Stashed",
        )
        .unwrap();
        symlink(&outside, vault.join("Escape")).unwrap();
        assert!(restore_notebook(vault_path, notebook.to_str().unwrap()).is_err());
        assert!(notebook.join("Keep.md").exists());
        assert!(!outside.join("Stashed").exists());
    }

    #[test]
    fn header_reads_keep_metadata_when_frontmatter_exceeds_the_first_block() {
        let vault = tempfile::tempdir().unwrap();
        let tags: Vec<String> = (0..120)
            .map(|index| format!("long-tag-name-{index}"))
            .collect();
        let raw = format!(
            "---\nid: existing\ntitle: Long Frontmatter\ntags: [{}]\n---\n# Long Frontmatter\n\nBody.\n",
            tags.join(", ")
        );
        assert!(raw.find("\n---\n#").unwrap() > 2048);
        fs::write(vault.path().join("Long.md"), raw).unwrap();

        let entry =
            super::read_note_entry_fast(&vault.path().join("Long.md"), vault.path()).unwrap();
        assert_eq!(entry.meta.title, "Long Frontmatter");
        assert_eq!(entry.meta.tags, tags);
        let counted = super::get_all_tags(vault.path().to_str().unwrap()).unwrap();
        assert_eq!(counted.len(), tags.len());
    }

    #[test]
    fn note_head_stops_after_frontmatter_and_one_body_block() {
        let vault = tempfile::tempdir().unwrap();
        let path = vault.path().join("Note.md");
        let tags = (0..400)
            .map(|i| format!("tag-{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        for newline in ["\n", "\r\n"] {
            let body = "x".repeat(100_000);
            let raw = format!("---{newline}tags: [{tags}]{newline}---{newline}{body}");
            fs::write(&path, &raw).unwrap();
            let head = super::read_note_head(&path).unwrap();
            let end = raw.find(&format!("{newline}---{newline}")).unwrap() + newline.len() * 2 + 3;
            assert!(head.len() >= end + 2048 && head.len() < end + 2 * 2048);
            assert!(raw.starts_with(&head));
        }
    }

    #[test]
    fn note_head_without_frontmatter_reads_one_block_and_unterminated_is_capped() {
        let vault = tempfile::tempdir().unwrap();
        let path = vault.path().join("Note.md");
        fs::write(&path, "# Title\n".to_string() + &"y".repeat(50_000)).unwrap();
        assert_eq!(super::read_note_head(&path).unwrap().len(), 2048);
        fs::write(
            &path,
            "---\ntitle: Open\n".to_string() + &"z\n".repeat(300_000),
        )
        .unwrap();
        assert_eq!(super::read_note_head(&path).unwrap().len(), 256 * 1024);
        fs::write(
            &path,
            "---not frontmatter\n".to_string() + &"y".repeat(50_000),
        )
        .unwrap();
        assert_eq!(super::read_note_head(&path).unwrap().len(), 2048);
        fs::write(&path, "---\n---\nShort").unwrap();
        assert_eq!(super::read_note_head(&path).unwrap(), "---\n---\nShort");
    }

    #[test]
    fn dot_named_vault_folder_is_still_scanned_and_renames_update_links() {
        // tempdir() names its folder ".tmpXXXX", like a vault kept in ~/.notes.
        let vault = tempfile::tempdir().unwrap();
        assert!(vault
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with('.'));
        let vault_path = vault.path().to_str().unwrap();
        fs::write(vault.path().join("Old.md"), "---\ntitle: Old\n---\n# Old\n").unwrap();
        let other = vault.path().join("Other.md");
        fs::write(&other, "---\ntitle: Other\n---\nSee [[Old]].\n").unwrap();

        assert_eq!(super::scan_notes(vault_path, None).unwrap().len(), 2);
        fs::create_dir(vault.path().join("Notebook")).unwrap();
        fs::write(
            vault.path().join("Notebook/Tagged.md"),
            "---\ntags: [work]\n---\n# Tagged\n",
        )
        .unwrap();
        assert_eq!(super::scan_notebooks(vault_path).unwrap().len(), 1);
        assert_eq!(
            super::get_all_tags(vault_path).unwrap(),
            [("work".to_string(), 1)]
        );
        assert_eq!(
            super::scan_notes(
                vault_path,
                Some(vault.path().join("Notebook").to_str().unwrap())
            )
            .unwrap()
            .len(),
            1
        );

        super::rename_note(
            vault.path().join("Old.md").to_str().unwrap(),
            "Renamed",
            vault_path,
        )
        .unwrap();
        assert!(fs::read_to_string(other).unwrap().contains("Renamed"));
    }

    #[test]
    fn vault_state_save_replaces_file_without_truncating_open_reader() {
        let vault = tempfile::tempdir().unwrap();
        let vault_path = vault.path().to_str().unwrap();
        super::ensure_vault_structure(vault_path).unwrap();
        let state = helixnotes_dir(vault_path).join("state.json");
        fs::write(&state, b"old vault state").unwrap();
        let mut reader = fs::File::open(&state).unwrap();
        let new_state = crate::types::VaultState {
            last_open_note: Some("Daily/2026-01-01.md".into()),
            ..Default::default()
        };
        super::save_vault_state(vault_path, &new_state).unwrap();
        let mut old = String::new();
        reader.read_to_string(&mut old).unwrap();
        assert_eq!(old, "old vault state");
        let saved = super::load_vault_state(vault_path).unwrap();
        assert_eq!(saved.last_open_note, new_state.last_open_note);
    }
}
