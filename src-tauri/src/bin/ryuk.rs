// ryuk — the Shinigami's hands on the vault.
//
// A small CLI over the RyukNotes SQLite vault, sharing vault::db with the app.
// Vault resolution order: --vault flag, RYUK_VAULT env, ~/Notes/ryuknotes.rn.
//
// The hermes integration lives in the `session` subcommands:
//   ryuk session import --file summary.json   (summary in, note out)
//   ryuk session restore <session-id>         (execs `hermes --resume <id>`)
//   ryuk session start --notes <id> [...]      (notes -> context file -> hermes)

use app_lib::vault::db::{NewNote, VaultDb};
use std::path::PathBuf;
use std::process::Command;

fn resolve_vault(flag: Option<&str>) -> Result<PathBuf, String> {
    if let Some(path) = flag {
        return Ok(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("RYUK_VAULT") {
        return Ok(PathBuf::from(path));
    }
    let path = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join("Notes/ryuknotes.rn");
    if path.is_file() {
        return Ok(path);
    }
    Err(format!(
        "no vault found at {}; pass --vault or set RYUK_VAULT",
        path.display()
    ))
}

fn open_vault(flag: Option<&str>) -> Result<VaultDb, String> {
    let path = resolve_vault(flag)?;
    VaultDb::open(&path)
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn usage() -> ! {
    eprintln!(
        "ryuk — Shinigami access to the RyukNotes vault

USAGE:
  ryuk [--vault PATH] notes list [--trashed]
  ryuk [--vault PATH] notes show <id>
  ryuk [--vault PATH] notes new <title> [--tag a --tag b] [--stdin]
  ryuk [--vault PATH] notes tag <id> [--add a] [--remove b]
  ryuk [--vault PATH] notes search <query>
  ryuk [--vault PATH] notes trash <id> | restore <id>
  ryuk [--vault PATH] session import --file <summary.json>
  ryuk [--vault PATH] session restore <session-id>
  ryuk [--vault PATH] session start --notes <id> [--prompt <text>]   (resumes linked session if any)
  ryuk [--vault PATH] session resume-for-note <note-id>
  ryuk [--vault PATH] session list-links

Vault resolution: --vault, then RYUK_VAULT, then ~/Notes/ryuknotes.rn."
    );
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut vault_flag: Option<String> = None;
    let mut rest: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--vault" => {
                i += 1;
                vault_flag = Some(match args.get(i) {
                    Some(v) => v.clone(),
                    None => usage(),
                });
            }
            _ => rest.push(args[i].clone()),
        }
        i += 1;
    }
    let cmd = match rest.first() {
        Some(c) => c.as_str(),
        None => usage(),
    };
    let sub = rest.get(1).map(String::as_str);
    let tail = &rest[2.min(rest.len())..];

    let result = match (cmd, sub) {
        ("notes", Some("list")) => notes_list(&vault_flag, tail),
        ("notes", Some("show")) => notes_show(&vault_flag, tail),
        ("notes", Some("new")) => notes_new(&vault_flag, tail),
        ("notes", Some("tag")) => notes_tag(&vault_flag, tail),
        ("notes", Some("search")) => notes_search(&vault_flag, tail),
        ("notes", Some("trash")) => notes_trash(&vault_flag, tail, true),
        ("notes", Some("restore")) => notes_trash(&vault_flag, tail, false),
        ("session", Some("import")) => session_import(&vault_flag, tail),
        ("session", Some("restore")) => session_restore(&vault_flag, tail),
        ("session", Some("start")) => session_start(&vault_flag, tail),
        ("session", Some("resume-for-note")) => session_resume_for_note(&vault_flag, tail),
        ("session", Some("list-links")) => session_list_links(&vault_flag),
        _ => usage(),
    };
    if let Err(e) = result {
        eprintln!("ryuk: {e}");
        std::process::exit(1);
    }
}

fn flag_values(tail: &[String], name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < tail.len() {
        if tail[i] == name {
            if let Some(v) = tail.get(i + 1) {
                out.push(v.clone());
                i += 1;
            }
        }
        i += 1;
    }
    out
}

fn positional(tail: &[String], skip_flags: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < tail.len() {
        let is_flag = tail[i].starts_with("--") || skip_flags.contains(&tail[i].as_str());
        if is_flag {
            // skip the flag's value too when it's a --name value flag
            if tail[i].starts_with("--") && i + 1 < tail.len() && !tail[i + 1].starts_with("--") {
                i += 1;
            }
        } else {
            out.push(tail[i].clone());
        }
        i += 1;
    }
    out
}

fn notes_list(vault_flag: &Option<String>, tail: &[String]) -> Result<(), String> {
    let db = open_vault(vault_flag.as_deref())?;
    let trashed = tail.iter().any(|a| a == "--trashed");
    for note in db.list_notes(trashed)? {
        let pin = if note.pinned { "*" } else { " " };
        let trash = if note.trashed { " [trash]" } else { "" };
        let tags = if note.tags.is_empty() {
            String::new()
        } else {
            format!("  #{}", note.tags.join(" #"))
        };
        println!("{pin} {}  {}{trash}{tags}", note.id, note.title);
    }
    Ok(())
}

fn notes_show(vault_flag: &Option<String>, tail: &[String]) -> Result<(), String> {
    let id = tail.first().ok_or("notes show <id>")?;
    let db = open_vault(vault_flag.as_deref())?;
    let Some(note) = db.get_note_full(id)? else {
        return Err(format!("no note with id {id}"));
    };
    println!("# {}", note["title"].as_str().unwrap_or(""));
    println!();
    println!("{}", note["content"].as_str().unwrap_or(""));
    Ok(())
}

fn notes_new(vault_flag: &Option<String>, tail: &[String]) -> Result<(), String> {
    let title = positional(tail, &[])
        .first()
        .cloned()
        .ok_or("notes new <title>")?;
    let tags = flag_values(tail, "--tag");
    let content = if tail.iter().any(|a| a == "--stdin") {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| e.to_string())?;
        buf
    } else {
        String::new()
    };
    let db = open_vault(vault_flag.as_deref())?;
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_rfc3339();
    db.upsert_note(&NewNote {
        id: id.clone(),
        title,
        content,
        pinned: false,
        tags,
        created: now.clone(),
        modified: now,
    })?;
    println!("{id}");
    Ok(())
}

fn notes_tag(vault_flag: &Option<String>, tail: &[String]) -> Result<(), String> {
    let id = positional(tail, &[])
        .first()
        .cloned()
        .ok_or("notes tag <id>")?;
    let add = flag_values(tail, "--add");
    let remove = flag_values(tail, "--remove");
    let db = open_vault(vault_flag.as_deref())?;
    let Some(note) = db.get_note_full(&id)? else {
        return Err(format!("no note with id {id}"));
    };
    let mut tags: Vec<String> = note["tags"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    for tag in add {
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    tags.retain(|t| !remove.contains(t));
    db.upsert_note(&NewNote {
        id: id.clone(),
        title: note["title"].as_str().unwrap_or("").to_string(),
        content: note["content"].as_str().unwrap_or("").to_string(),
        pinned: note["pinned"].as_bool().unwrap_or(false),
        tags,
        created: note["created"].as_str().unwrap_or("").to_string(),
        modified: now_rfc3339(),
    })?;
    Ok(())
}

fn notes_search(vault_flag: &Option<String>, tail: &[String]) -> Result<(), String> {
    let query = positional(tail, &[])
        .first()
        .cloned()
        .ok_or("notes search <query>")?;
    // FTS5 raw queries break on '/' and other syntax; wrap as a quoted phrase.
    let quoted = format!("\"{}\"", query.replace('"', "\"\""));
    let db = open_vault(vault_flag.as_deref())?;
    for note in db.search(&quoted)? {
        println!("{}  {}", note.id, note.title);
    }
    Ok(())
}

fn notes_trash(vault_flag: &Option<String>, tail: &[String], trash: bool) -> Result<(), String> {
    let id = tail.first().ok_or("notes trash|restore <id>")?;
    let db = open_vault(vault_flag.as_deref())?;
    db.set_note_trashed(id, trash)?;
    println!("{} {}", id, if trash { "trashed" } else { "restored" });
    Ok(())
}

/// summary.json shape:
/// { "session_id": "...", "title": "...", "changed": [...], "decisions": [...],
///   "todos": [...], "topics": [...], "tags": [...] }
fn session_import(vault_flag: &Option<String>, tail: &[String]) -> Result<(), String> {
    let file = flag_values(tail, "--file")
        .first()
        .cloned()
        .ok_or("session import --file <json>")?;
    let raw = std::fs::read_to_string(&file).map_err(|e| format!("reading {file}: {e}"))?;
    let v: serde_json::Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    let session_id = v["session_id"]
        .as_str()
        .ok_or("summary json needs session_id")?
        .to_string();
    let title = v["title"]
        .as_str()
        .map(String::from)
        .unwrap_or_else(|| format!("Session — {}", session_id));
    let lines_of = |key: &str| -> Vec<String> {
        v[key]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut body = String::new();
    for (key, heading) in [
        ("changed", "What changed"),
        ("decisions", "Decisions"),
        ("todos", "Todos"),
    ] {
        let items = lines_of(key);
        if !items.is_empty() {
            body.push_str(&format!("\n## {heading}\n"));
            for item in items {
                body.push_str(&format!("- {item}\n"));
            }
        }
    }
    if body.is_empty() {
        return Err("summary has no changed/decisions/todos; not worth a note".into());
    }
    body.push_str(&format!("\n<!-- hermes-session-id: {session_id} -->\n"));

    let mut tags = lines_of("tags");
    for topic in lines_of("topics") {
        if !tags.contains(&topic) {
            tags.push(topic);
        }
    }
    if !tags.iter().any(|t| t == "hermes/session") {
        tags.push("hermes/session".to_string());
    }

    let db = open_vault(vault_flag.as_deref())?;
    // One note per session: deterministic id makes re-import an update.
    let id = format!("hermes-session-{session_id}");
    let now = now_rfc3339();
    let created = db.get_note_created(&id)?.unwrap_or_else(|| now.clone());
    db.upsert_note(&NewNote {
        id,
        title,
        content: body.trim_start().to_string(),
        pinned: false,
        tags,
        created,
        modified: now,
    })?;
    println!("session note written");
    Ok(())
}

fn session_restore(vault_flag: &Option<String>, tail: &[String]) -> Result<(), String> {
    let extra_ids = flag_values(tail, "--notes");
    let prompt = flag_values(tail, "--prompt").first().cloned();
    let session_id = positional(tail, &[])
        .first()
        .cloned()
        .ok_or("session restore <session-id>")?;
    // Restoring by session id: refresh the grounding of the note this session
    // is linked to (if any) so the resumed session re-reads the live note.
    let db = match open_vault(vault_flag.as_deref()) {
        Ok(db) => Some(db),
        Err(error) if vault_flag.is_some() => return Err(error),
        Err(_) => None,
    };
    let note = db
        .as_ref()
        .map(|db| db.get_note_for_session(&session_id))
        .transpose()?
        .flatten();
    let exit = match (db.as_ref(), note) {
        (Some(db), Some(note_id)) => resume_with_fresh_grounding(
            db,
            &note_id,
            &session_id,
            vault_flag.as_deref(),
            &extra_ids,
            prompt.as_deref(),
        )?,
        _ => {
            // Without a linked home there is nowhere to ground supplied notes.
            if !extra_ids.is_empty() {
                return Err("cannot ground extra notes without a linked home note; use session start --notes".into());
            }
            // No vault or no linked note: plain resume retains a supplied prompt.
            let status = resume_command(&session_id, prompt.as_deref())
                .status()
                .map_err(|e| format!("launching hermes: {e}"))?;
            status.code().unwrap_or(1)
        }
    };
    std::process::exit(exit);
}

/// Writes the session grounding files into a per-note context directory.
///
/// `context.md` carries the home note's full content; `.hermes.md` is Hermes's
/// auto-loaded project context file (cwd-only), so a session launched with
/// `hermes --in <dir>` reads these rules from the system prompt at startup —
/// the note grounds the session without any manual step. Paths inside
/// `.hermes.md` are absolute: on `--resume <id> --in <dir>` Hermes injects
/// the dir's `.hermes.md` even though the process cwd may not move there.
///
/// `extras` are additional notes folded into the grounding: each lands in the
/// context file as its own `# Additional context: <title>` section (skipped when unreadable —
/// the home note is the session's home; extras are best-effort context).
fn write_session_context(
    dir: &std::path::Path,
    note_id: &str,
    title: &str,
    content: &str,
    vault_path: &std::path::Path,
    extras: &[(String, String, String)], // (id, title, content)
) -> Result<std::path::PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let context_file = dir.join("context.md");
    let mut ctx = format!(
        "# {title}\n\n{content}\n\n---\n\n*(This file grounds the session in a Ryuk's Notebook note titled \"{title}\". The note id is {note_id}.)*\n"
    );
    for (extra_id, extra_title, extra_content) in extras {
        ctx.push_str(&format!(
            "\n\n# Additional context: {extra_title}\n\n*Note id: `{extra_id}`*\n\n{extra_content}\n"
        ));
    }
    std::fs::write(&context_file, ctx).map_err(|e| e.to_string())?;

    // Hermes loads this file automatically (cwd-only project context) and pins
    // it in the system prompt: the note IS the basis for the session.
    let hermes_file = dir.join(".hermes.md");
    std::fs::write(
        &hermes_file,
        format!(
            "# RyukNotes Session Grounding\n\n\
             This Hermes session was launched from Ryuk's Notebook for the note \
             \"{title}\" (note id: `{note_id}`).\n\n\
             ## Rules\n\n\
             - `{ctx}` holds the full content of that note. It is the \
             authoritative grounding for this session: read it before \
             answering, and treat it as the basis for every response in this \
             session.\n\
             - The note id is `{note_id}`. When the task refers to \"the note\", \
             that is the note.\n\
             - `context.md` is a launch-time snapshot. If freshness matters, \
             check the live note in the vault: \
             `ryuk --vault \"{vault}\" notes show {note_id}`.\n\
             - Output destined for this note (summaries, findings, artifacts) \
             should be written so `ryuk session import` can file it back into \
             the vault.\n",
            ctx = context_file.display(),
            vault = vault_path.display(),
        ),
    )
    .map_err(|e| e.to_string())?;
    Ok(context_file)
}

/// Read best-effort extra grounding from the same live vault as the home note.
fn read_extra_context(db: &VaultDb, ids: &[String]) -> Vec<(String, String, String)> {
    ids.iter()
        .filter_map(|id| match db.get_note_full(id) {
            Ok(Some(note)) => Some((
                id.clone(),
                note["title"].as_str().unwrap_or("").to_string(),
                note["content"].as_str().unwrap_or("").to_string(),
            )),
            _ => {
                eprintln!("warning: no readable note with id {id}; skipped");
                None
            }
        })
        .collect()
}

// Shared by all resume entry points. Prepare before launching so grounding
// errors cannot silently send Hermes stale context. Extras remain best-effort.
fn grounded_resume_command(
    db: &VaultDb,
    note_id: &str,
    session_id: &str,
    vault_path: &std::path::Path,
    context_dir: &std::path::Path,
    extra_ids: &[String],
    prompt: Option<&str>,
) -> Result<Command, String> {
    let note = db
        .get_note_full(note_id)?
        .ok_or_else(|| format!("no note with id {note_id}"))?;
    let extras = read_extra_context(db, extra_ids);
    write_session_context(
        context_dir,
        note_id,
        note["title"].as_str().unwrap_or(""),
        note["content"].as_str().unwrap_or(""),
        vault_path,
        &extras,
    )?;
    let mut cmd = resume_command(session_id, prompt);
    cmd.arg("--in").arg(context_dir);
    Ok(cmd)
}

fn resume_command(session_id: &str, prompt: Option<&str>) -> Command {
    let mut cmd = Command::new("hermes");
    cmd.arg("--resume").arg(session_id);
    if let Some(prompt) = prompt {
        cmd.arg("-z").arg(prompt);
    }
    cmd
}

fn resume_with_fresh_grounding(
    db: &VaultDb,
    note_id: &str,
    session_id: &str,
    vault_flag: Option<&str>,
    extra_ids: &[String],
    prompt: Option<&str>,
) -> Result<i32, String> {
    let vault_path = resolve_vault(vault_flag)?;
    let context_dir = std::env::temp_dir().join("ryuk-sessions").join(note_id);
    let status = grounded_resume_command(
        db,
        note_id,
        session_id,
        &vault_path,
        &context_dir,
        extra_ids,
        prompt,
    )?
    .status()
    .map_err(|e| format!("launching hermes: {e}"))?;
    Ok(status.code().unwrap_or(1))
}

fn session_start(vault_flag: &Option<String>, tail: &[String]) -> Result<(), String> {
    let ids = flag_values(tail, "--notes");
    let prompt = flag_values(tail, "--prompt").first().cloned();
    if ids.is_empty() {
        return Err("session start --notes <id> [<id> ...]".into());
    }
    let note_id = ids[0].clone();
    // Extra notes may follow the first --notes value as bare positionals
    // (`session start --notes home extra1 extra2 --prompt ...`) or as repeated
    // `--notes` flags; both forms collect into extra_ids.
    let mut extra_ids: Vec<String> = ids[1..].to_vec();
    for bare in positional(tail, &["--notes", "--prompt"]) {
        extra_ids.push(bare);
    }
    let db = open_vault(vault_flag.as_deref())?;

    // Linked session? Resume it — the note IS the session's home. Refresh the
    // grounding from the live note so the resumed session reads the current
    // content, not the launch-time snapshot.
    if let Some(session_id) = db.get_session_link(&note_id)? {
        println!("resuming linked session {session_id}");
        let exit = resume_with_fresh_grounding(
            &db,
            &note_id,
            &session_id,
            vault_flag.as_deref(),
            &extra_ids,
            prompt.as_deref(),
        )?;
        std::process::exit(exit);
    }

    // New session: hermes runs inside a per-note workspace so the session can
    // be found again by workspace (hermes --pass-session-id puts the id in
    // the agent's prompt; sessions list --workspace finds it regardless).
    let Some(note) = db.get_note_full(&note_id)? else {
        return Err(format!("no note with id {note_id}"));
    };
    let title = String::from(note["title"].as_str().unwrap_or(""));
    let content = String::from(note["content"].as_str().unwrap_or(""));
    let context_dir = std::env::temp_dir().join("ryuk-sessions").join(&note_id);
    let vault_path = resolve_vault(vault_flag.as_deref())?;
    // Additional notes: best-effort extra grounding, skipped when unreadable.
    let extras = read_extra_context(&db, &extra_ids);
    let context_file = write_session_context(
        &context_dir,
        &note_id,
        &title,
        &content,
        &vault_path,
        &extras,
    )?;
    println!("context: {}", context_file.display());
    println!("grounding: {}", context_dir.join(".hermes.md").display());

    let before = newest_session_in_workspace(&context_dir);
    let mut cmd = Command::new("hermes");
    cmd.arg("--in").arg(context_dir.to_string_lossy().as_ref());
    cmd.arg("--pass-session-id");
    if let Some(p) = prompt {
        cmd.arg("-z").arg(p);
    } else {
        cmd.arg("chat");
    }
    let status = cmd
        .env("HERMES_CONTEXT_NOTE", &note_id)
        .env("HERMES_CONTEXT_NOTE_TITLE", &title)
        .status()
        .map_err(|e| format!("launching hermes: {e}"))?;
    let exit = status.code().unwrap_or(1);

    // Record the linkage: the newest session in this note's workspace that
    // did not exist before we launched (before=None means a brand-new home).
    match (before, newest_session_in_workspace(&context_dir)) {
        (Ok(before), Ok(Some(after))) if Some(&after) != before.as_ref() => {
            db.set_session_link(&note_id, &after)?;
            println!("session linked: {note_id} -> {after}");
        }
        (Err(error), _) | (_, Err(error)) => {
            eprintln!("warning: session linkage not recorded: {error}");
        }
        _ => {}
    }
    std::process::exit(exit);
}

/// Prefer machine-readable discovery when supported. Older Hermes releases
/// expose only a table: accept a single strictly shaped session ID, regardless
/// of column position, and never interpret output from failed commands.
fn valid_session_id(id: &str) -> bool {
    let parts: Vec<&str> = id.split('_').collect();
    parts.len() == 3
        && parts[0].len() == 8
        && parts[0].bytes().all(|b| b.is_ascii_digit())
        && parts[1].len() == 6
        && parts[1].bytes().all(|b| b.is_ascii_digit())
        && parts[2].len() >= 4
        && parts[2].bytes().all(|b| b.is_ascii_hexdigit())
}

fn json_session_id(text: &str) -> Result<Option<String>, ()> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|_| ())?;
    let rows = value
        .as_array()
        .or_else(|| value.get("sessions").and_then(|v| v.as_array()))
        .ok_or(())?;
    if rows.is_empty() {
        return Ok(None);
    }
    if rows.len() != 1 {
        return Err(());
    }
    let id = rows[0]
        .get("session_id")
        .or_else(|| rows[0].get("id"))
        .and_then(|v| v.as_str())
        .filter(|id| valid_session_id(id))
        .ok_or(())?;
    Ok(Some(id.to_string()))
}

fn text_session_id(text: &str) -> Option<String> {
    // Strip ANSI control sequences before tokenizing a colored table.
    let mut plain = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.next() != Some('[') {
                return None;
            }
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        } else {
            plain.push(c);
        }
    }
    let mut ids = plain
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|id| valid_session_id(id));
    let id = ids.next()?;
    if ids.next().is_some() {
        return None;
    }
    Some(id.to_string())
}

fn newest_session_in_workspace(workspace: &std::path::Path) -> Result<Option<String>, String> {
    discover_session(std::path::Path::new("hermes"), workspace)
}

fn discover_session(
    program: &std::path::Path,
    workspace: &std::path::Path,
) -> Result<Option<String>, String> {
    let run = |json: bool| {
        let mut cmd = Command::new(program);
        cmd.args(["sessions", "list", "--workspace"])
            .arg(workspace)
            .args(["--limit", "1"]);
        if json {
            cmd.arg("--json");
        }
        cmd.output().ok()
    };
    if let Some(out) = run(true) {
        if out.status.success() {
            if let Ok(id) = json_session_id(&String::from_utf8_lossy(&out.stdout)) {
                return Ok(id);
            }
        }
    }
    let out = run(false).ok_or("could not run Hermes session discovery")?;
    if !out.status.success() {
        return Err("Hermes session discovery failed".into());
    }
    let text = std::str::from_utf8(&out.stdout).map_err(|e| e.to_string())?;
    match text_session_id(text) {
        Some(id) => Ok(Some(id)),
        // Recognize an empty table response; unknown or ambiguous output must
        // not masquerade as a successful empty before-list.
        None if text.trim().is_empty()
            || text.trim().to_ascii_lowercase().starts_with("no sessions") =>
        {
            Ok(None)
        }
        None => Err("unrecognized or ambiguous Hermes session listing".into()),
    }
}

fn session_resume_for_note(vault_flag: &Option<String>, tail: &[String]) -> Result<(), String> {
    let note_id = positional(tail, &[])
        .first()
        .cloned()
        .ok_or("session resume-for-note <note-id>")?;
    let db = open_vault(vault_flag.as_deref())?;
    let session_id = db
        .get_session_link(&note_id)?
        .ok_or_else(|| format!("note {note_id} has no linked session yet"))?;
    let extra_ids = flag_values(tail, "--notes");
    let prompt = flag_values(tail, "--prompt").first().cloned();
    let exit = resume_with_fresh_grounding(
        &db,
        &note_id,
        &session_id,
        vault_flag.as_deref(),
        &extra_ids,
        prompt.as_deref(),
    )?;
    std::process::exit(exit);
}

fn session_list_links(vault_flag: &Option<String>) -> Result<(), String> {
    let db = open_vault(vault_flag.as_deref())?;
    let mut stmt = db
        .conn
        .prepare("SELECT l.note_id, n.title, l.session_id FROM session_links l JOIN notes n ON n.id = l.note_id ORDER BY l.created")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    for row in rows {
        let (note_id, title, session_id) = row.map_err(|e| e.to_string())?;
        println!("{note_id}  {title}  ->  {session_id}");
    }
    Ok(())
}

#[cfg(test)]
mod session_grounding_tests {
    use super::write_session_context;

    #[test]
    fn writes_context_and_hermes_grounding() {
        let dir = std::env::temp_dir().join("ryuk-grounding-test");
        let _ = std::fs::remove_dir_all(&dir);
        let context = write_session_context(
            &dir,
            "test-note-1",
            "Test Title",
            "Body of the note.",
            std::path::Path::new("/home/u/Notes/ryuknotes.rn"),
            &[],
        )
        .unwrap();
        assert!(context.is_file());

        let grounding = dir.join(".hermes.md");
        assert!(grounding.is_file(), ".hermes.md must exist for auto-load");
        let text = std::fs::read_to_string(&grounding).unwrap();
        assert!(text.contains("test-note-1"));
        assert!(text.contains("Test Title"));
        assert!(text.contains("context.md"));
        // The freshness pointer must carry the actual vault path.
        assert!(text.contains("/home/u/Notes/ryuknotes.rn"));

        let ctx = std::fs::read_to_string(&context).unwrap();
        assert!(ctx.contains("Body of the note."));
        assert!(ctx.contains("test-note-1"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn overwrites_stale_grounding_on_relaunch() {
        let dir = std::env::temp_dir().join("ryuk-grounding-test-2");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".hermes.md"), "stale").unwrap();
        write_session_context(&dir, "n", "t", "c", std::path::Path::new("/v.rn"), &[]).unwrap();
        let text = std::fs::read_to_string(dir.join(".hermes.md")).unwrap();
        assert!(
            !text.contains("stale"),
            "relaunch must replace stale grounding"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod session_link_tests {
    use app_lib::vault::db::{NewNote, VaultDb};

    fn mem_db() -> VaultDb {
        VaultDb::open_in_memory().expect("in-memory vault")
    }

    fn seed_note(db: &VaultDb, id: &str) {
        db.upsert_note(&NewNote {
            id: id.to_string(),
            title: id.to_string(),
            content: "x".to_string(),
            pinned: false,
            tags: vec![],
            created: "2026-10-15T00:00:00Z".to_string(),
            modified: "2026-10-15T00:00:00Z".to_string(),
        })
        .expect("seed note");
    }

    #[test]
    fn reverse_lookup_finds_note_for_session() {
        let db = mem_db();
        seed_note(&db, "note-a");
        db.set_session_link("note-a", "20261015_000000_aaaa")
            .unwrap();
        assert_eq!(
            db.get_note_for_session("20261015_000000_aaaa").unwrap(),
            Some("note-a".to_string())
        );
        assert_eq!(db.get_note_for_session("missing").unwrap(), None);
    }

    #[test]
    fn latest_link_wins_on_reverse_lookup() {
        let db = mem_db();
        seed_note(&db, "note-old");
        seed_note(&db, "note-new");
        db.set_session_link("note-old", "20261015_000000_aaaa")
            .unwrap();
        db.set_session_link("note-new", "20261015_000000_aaaa")
            .unwrap();
        assert_eq!(
            db.get_note_for_session("20261015_000000_aaaa").unwrap(),
            Some("note-new".to_string())
        );
    }
}

#[cfg(test)]
mod launch_context_tests {
    use super::*;

    fn seed(db: &VaultDb, id: &str, content: &str) {
        db.upsert_note(&NewNote {
            id: id.into(),
            title: format!("Title {id}"),
            content: content.into(),
            pinned: false,
            tags: vec![],
            created: "2026-10-15T00:00:00Z".into(),
            modified: "2026-10-15T00:00:00Z".into(),
        })
        .unwrap();
    }

    #[test]
    fn resume_refreshes_live_home_extras_and_preserves_prompt_and_vault() {
        let db = VaultDb::open_in_memory().unwrap();
        seed(&db, "home", "old home");
        seed(&db, "extra", "old extra");
        db.set_session_link("home", "20261015_120000_abcdef")
            .unwrap();
        let dir = std::env::temp_dir().join(format!("ryuk-resume-{}", uuid::Uuid::new_v4()));
        let vault = std::path::Path::new("/displayed vault.rn");
        let extras = vec!["extra".into(), "missing".into()];
        grounded_resume_command(
            &db,
            "home",
            "20261015_120000_abcdef",
            vault,
            &dir,
            &extras,
            None,
        )
        .unwrap();
        seed(&db, "home", "live home");
        seed(&db, "extra", "live extra");
        let cmd = grounded_resume_command(
            &db,
            "home",
            &db.get_session_link("home").unwrap().unwrap(),
            vault,
            &dir,
            &extras,
            Some("new prompt with spaces"),
        )
        .unwrap();
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            vec![
                "--resume",
                "20261015_120000_abcdef",
                "-z",
                "new prompt with spaces",
                "--in",
                dir.to_str().unwrap()
            ]
        );
        let context = std::fs::read_to_string(dir.join("context.md")).unwrap();
        assert!(context.contains("live home"));
        assert!(context.contains("# Additional context: Title extra"));
        assert!(context.contains("live extra"));
        assert!(!context.contains("old home") && !context.contains("old extra"));
        let grounding = std::fs::read_to_string(dir.join(".hermes.md")).unwrap();
        assert!(grounding.contains("/displayed vault.rn"));
        // Write errors must fail before a Hermes process is launched.
        assert!(grounded_resume_command(
            &db,
            "home",
            "s",
            vault,
            &dir.join("context.md"),
            &[],
            None
        )
        .is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn discovery_parses_structured_output_and_strict_legacy_tables() {
        let id = "20261015_120000_abcdef";
        assert_eq!(
            json_session_id(&format!(r#"[{{"session_id":"{id}"}}]"#)),
            Ok(Some(id.into()))
        );
        assert_eq!(
            json_session_id(&format!(r#"{{"sessions":[{{"id":"{id}"}}]}}"#)),
            Ok(Some(id.into()))
        );
        assert_eq!(json_session_id("[]"), Ok(None));
        assert!(json_session_id(r#"[{"id":"not_a_session"}]"#).is_err());
        assert_eq!(
            text_session_id(&format!(
                "ID | Workspace\n\x1b[32m{id}\x1b[0m | /tmp/note\n"
            )),
            Some(id.into())
        );
        assert_eq!(text_session_id("No sessions found\nfoo_bar_baz"), None);
        assert_eq!(
            text_session_id(&format!("{id}\n20261016_120000_abcdef")),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn discovery_probes_json_falls_back_and_ignores_failed_output() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("ryuk-discovery-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let program = dir.join("hermes");
        let workspace = dir.join("workspace with spaces");
        let install = |body: &str| {
            std::fs::write(&program, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        };
        install("test \"$1 $2 $3\" = 'sessions list --workspace' || exit 1\ntest \"$5 $6\" = '--limit 1' || exit 1\ntest \"$7\" = '--json' || exit 1\nprintf '%s' '[{\"session_id\":\"20261015_120000_abcdef\"}]'");
        assert_eq!(
            discover_session(&program, &workspace),
            Ok(Some("20261015_120000_abcdef".into()))
        );
        install(
            "test \"$7\" != '--json' || exit 2\nprintf '%s' '20261016_120000_abcdef | workspace'",
        );
        assert_eq!(
            discover_session(&program, &workspace),
            Ok(Some("20261016_120000_abcdef".into()))
        );
        install("printf '%s' '20261015_120000_abcdef'; exit 1");
        assert!(discover_session(&program, &workspace).is_err());
        install("test \"$7\" = '--json' || exit 1\nprintf '%s' '[]'");
        assert_eq!(discover_session(&program, &workspace), Ok(None));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
