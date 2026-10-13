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
  ryuk [--vault PATH] session start --notes <id> [<id> ...] [--prompt <text>]

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
    let title = positional(tail, &[]).first().cloned().ok_or("notes new <title>")?;
    let tags = flag_values(tail, "--tag");
    let content = if tail.iter().any(|a| a == "--stdin") {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf).map_err(|e| e.to_string())?;
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
    let id = positional(tail, &[]).first().cloned().ok_or("notes tag <id>")?;
    let add = flag_values(tail, "--add");
    let remove = flag_values(tail, "--remove");
    let db = open_vault(vault_flag.as_deref())?;
    let Some(mut note) = db.get_note_full(&id)? else {
        return Err(format!("no note with id {id}"));
    };
    let mut tags: Vec<String> = note["tags"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
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
    let query = positional(tail, &[]).first().cloned().ok_or("notes search <query>")?;
    let db = open_vault(vault_flag.as_deref())?;
    for note in db.search(&query)? {
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
    let file = flag_values(tail, "--file").first().cloned().ok_or("session import --file <json>")?;
    let raw = std::fs::read_to_string(&file).map_err(|e| format!("reading {file}: {e}"))?;
    let v: serde_json::Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    let session_id = v["session_id"].as_str().ok_or("summary json needs session_id")?.to_string();
    let title = v["title"]
        .as_str()
        .map(String::from)
        .unwrap_or_else(|| format!("Session — {}", session_id));
    let lines_of = |key: &str| -> Vec<String> {
        v[key]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
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
    let session_id = positional(tail, &[]).first().cloned().ok_or("session restore <session-id>")?;
    let _ = vault_flag; // restoring does not need the vault; keep the flag accepted
    let status = Command::new("hermes")
        .arg("--resume")
        .arg(&session_id)
        .status()
        .map_err(|e| format!("launching hermes: {e}"))?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

fn session_start(vault_flag: &Option<String>, tail: &[String]) -> Result<(), String> {
    let ids = flag_values(tail, "--notes");
    let prompt = flag_values(tail, "--prompt").first().cloned();
    if ids.is_empty() {
        return Err("session start --notes <id> [<id> ...]".into());
    }
    let db = open_vault(vault_flag.as_deref())?;
    let mut context = String::new();
    for id in &ids {
        let Some(note) = db.get_note_full(id)? else {
            return Err(format!("no note with id {id}"));
        };
        context.push_str(&format!(
            "# {}\n\n{}\n\n---\n\n",
            note["title"].as_str().unwrap_or(""),
            note["content"].as_str().unwrap_or("")
        ));
    }
    let context_dir = std::env::temp_dir().join("ryuk-sessions");
    std::fs::create_dir_all(&context_dir).map_err(|e| e.to_string())?;
    let context_file = context_dir.join(format!("{}.md", uuid::Uuid::new_v4()));
    std::fs::write(&context_file, &context).map_err(|e| e.to_string())?;
    println!("context: {}", context_file.display());

    let mut cmd = Command::new("hermes");
    cmd.arg("--in").arg(context_dir.to_string_lossy().as_ref());
    if let Some(p) = prompt {
        cmd.arg("-z").arg(p);
    } else {
        cmd.arg("chat");
    }
    let status = cmd
        .status()
        .map_err(|e| format!("launching hermes: {e}"))?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}
