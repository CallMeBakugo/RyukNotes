//! Disposable-vault executable tests for the authoritative CLI writer contract.
use app_lib::vault::db::{NewNote, VaultDb};
use app_lib::vault::writer::{NoteWrite, WritePolicy};
use serde_json::{json, Value};
use std::io::Write;
use std::process::{Command, Output, Stdio};

struct Fixture {
    dir: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }
    fn db(&self) -> VaultDb {
        VaultDb::open_with_report(
            &self.dir.path().join("test.rn"),
            &self.dir.path().join("host-data/ryuknotes/revision-ledger"),
        )
        .unwrap()
        .0
    }
    fn run(&self, args: &[&str], input: &str) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_ryuk"))
            .arg("--vault")
            .arg(self.dir.path().join("test.rn"))
            .env(
                "RYUK_V3_LEDGER",
                self.dir.path().join("host-data/ryuknotes/revision-ledger"),
            )
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }
    fn ok(&self, args: &[&str], input: &str) -> String {
        let out = self.run(args, input);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }
    fn summary(&self, changed: &str) -> String {
        let path = self.dir.path().join("summary.json");
        std::fs::write(
            &path,
            json!({"session_id":"test-session", "title":"Summary", "changed":[changed]})
                .to_string(),
        )
        .unwrap();
        path.to_string_lossy().into_owned()
    }
}
fn token(note: &Value) -> &str {
    note["accepted"]["token"].as_str().unwrap()
}
fn gui_write(db: &VaultDb, base: &Value, body: &str) -> Value {
    let note = NewNote {
        id: base["id"].as_str().unwrap().into(),
        title: base["title"].as_str().unwrap().into(),
        content: body.into(),
        pinned: base["pinned"].as_bool().unwrap(),
        tags: serde_json::from_value(base["tags"].clone()).unwrap(),
        created: base["created"].as_str().unwrap().into(),
        modified: "unused".into(),
    };
    db.write_authoritative(
        &NoteWrite::from_note(
            db,
            &note,
            WritePolicy::Update {
                expected_token: token(base).into(),
            },
        ),
        20,
    )
    .unwrap()
}
#[test]
fn cli_tokens_tags_and_gui_saves_preserve_the_winning_state_in_both_orders() {
    let f = Fixture::new();
    let id = f
        .ok(&["notes", "new", "Title", "--stdin"], "base")
        .trim()
        .to_owned();
    let db = f.db();
    let base: Value = serde_json::from_str(&f.ok(&["notes", "show", &id, "--json"], "")).unwrap();
    let gui = gui_write(&db, &base, "GUI winner");
    f.ok(
        &[
            "notes",
            "tag",
            &id,
            "--add",
            "cli",
            "--expected-token",
            token(&gui),
        ],
        "",
    );
    let tagged = db.accepted_note(&id).unwrap().unwrap();
    assert_eq!(tagged["content"], "GUI winner");
    assert_eq!(tagged["tags"], json!(["cli"]));
    let stale = f.run(
        &[
            "notes",
            "update",
            &id,
            "--expected-token",
            token(&gui),
            "--stdin",
        ],
        "stale",
    );
    assert_eq!(stale.status.code(), Some(3));
    assert_eq!(
        serde_json::from_slice::<Value>(&stale.stderr).unwrap()["kind"],
        "changed"
    );
    let cli: Value = serde_json::from_str(&f.ok(
        &[
            "notes",
            "update",
            &id,
            "--expected-token",
            token(&tagged),
            "--stdin",
        ],
        "CLI winner",
    ))
    .unwrap();
    assert_eq!(cli["content"], "CLI winner");
    let note = NewNote {
        id: id.clone(),
        title: "Title".into(),
        content: "stale GUI".into(),
        pinned: false,
        tags: vec![],
        created: "unused".into(),
        modified: "unused".into(),
    };
    assert!(db
        .write_authoritative(
            &NoteWrite::from_note(
                &db,
                &note,
                WritePolicy::Update {
                    expected_token: token(&tagged).into()
                }
            ),
            20
        )
        .is_err());
    assert_eq!(db.get_note_content(&id).unwrap().unwrap(), "CLI winner");
    let other = f.ok(&["notes", "new", "Title", "--stdin"], "base");
    assert_ne!(id, other.trim());
    assert_eq!(db.get_note_content(other.trim()).unwrap().unwrap(), "base");
}
#[test]
fn summary_repeats_replace_generated_state_but_manual_state_preserves_pending_input() {
    for field in ["content", "title", "tags", "pinned"] {
        let f = Fixture::new();
        let file = f.summary("first");
        f.ok(&["session", "import", "--file", &file], "");
        let db = f.db();
        let id = "hermes-session-test-session";
        let first = db.accepted_note(id).unwrap().unwrap();
        f.ok(&["session", "import", "--file", &file], "");
        assert_eq!(first, db.accepted_note(id).unwrap().unwrap());
        f.summary("second");
        f.ok(&["session", "import", "--file", &file], "");
        assert!(db.get_note_content(id).unwrap().unwrap().contains("second"));
        assert_eq!(db.list_versions(id).unwrap().len(), 1);
        match field {
            "content" => {
                let base = db.accepted_note(id).unwrap().unwrap();
                gui_write(&db, &base, "manual");
            }
            "title" | "pinned" => {
                let current = db.accepted_note(id).unwrap().unwrap();
                let note = NewNote {
                    id: id.into(),
                    title: if field == "title" {
                        "manual".into()
                    } else {
                        current["title"].as_str().unwrap().into()
                    },
                    content: current["content"].as_str().unwrap().into(),
                    tags: serde_json::from_value(current["tags"].clone()).unwrap(),
                    pinned: field == "pinned",
                    created: current["created"].as_str().unwrap().into(),
                    modified: "unused".into(),
                };
                db.write_authoritative(
                    &NoteWrite::from_note(
                        &db,
                        &note,
                        WritePolicy::Update {
                            expected_token: token(&current).into(),
                        },
                    ),
                    20,
                )
                .unwrap();
            }
            "tags" => {
                let current = db.accepted_note(id).unwrap().unwrap();
                f.ok(
                    &[
                        "notes",
                        "tag",
                        id,
                        "--add",
                        "manual",
                        "--expected-token",
                        token(&current),
                    ],
                    "",
                );
            }
            _ => unreachable!(),
        }
        let manual = db.accepted_note(id).unwrap().unwrap();
        f.summary("incoming third");
        for _ in 0..2 {
            let out = f.run(&["session", "import", "--file", &file], "");
            assert_eq!(out.status.code(), Some(3));
            let error: Value = serde_json::from_slice(&out.stderr).unwrap();
            assert_eq!(error["kind"], "changed");
            assert!(error["artifact"]
                .as_str()
                .unwrap()
                .starts_with("pending_summary:"));
        }
        assert_eq!(manual, db.accepted_note(id).unwrap().unwrap());
        let pending: Value =
            serde_json::from_str(&f.ok(&["session", "pending", "test-session"], "")).unwrap();
        assert_eq!(pending.as_array().unwrap().len(), 1);
        assert_eq!(pending[0]["value"]["label"], "Pending summary: Summary");
        assert!(pending[0]["value"]["incoming"]["content"]
            .as_str()
            .unwrap()
            .contains("incoming third"));
    }
}
