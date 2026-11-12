//! Explicit opt-in CLI rollout; default builds still refuse writable v3.
#![cfg(feature = "revision-ancestry")]
use app_lib::vault::{
    ancestry,
    db::{NewNote, VaultDb},
    writer::{NoteWrite, WritePolicy},
};
use serde_json::{json, Value};
use std::{
    io::Write,
    process::{Command, Output, Stdio},
};
struct Fixture {
    dir: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        ancestry::migrate(&dir.path().join("v.rn"), &dir.path().join("ledger")).unwrap();
        Self { dir }
    }
    fn db(&self) -> VaultDb {
        VaultDb::open_v3(
            &self.dir.path().join("v.rn"),
            &self.dir.path().join("ledger"),
        )
        .unwrap()
    }
    fn run(&self, args: &[&str], body: &str) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_ryuk"))
            .arg("--vault")
            .arg(self.dir.path().join("v.rn"))
            .env("RYUK_V3_LEDGER", self.dir.path().join("ledger"))
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
            .write_all(body.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }
    fn ok(&self, args: &[&str], body: &str) -> String {
        let output = self.run(args, body);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
    fn show(&self, id: &str) -> Value {
        serde_json::from_str(&self.ok(&["notes", "show", id, "--json"], "")).unwrap()
    }
}
fn write(db: &VaultDb, base: &Value, body: &str) -> Value {
    let note = NewNote {
        id: base["id"].as_str().unwrap().into(),
        title: base["title"].as_str().unwrap().into(),
        content: body.into(),
        tags: serde_json::from_value(base["tags"].clone()).unwrap(),
        pinned: base["pinned"].as_bool().unwrap(),
        created: "ignored".into(),
        modified: "ignored".into(),
    };
    db.write_authoritative(
        &NoteWrite::from_note(
            db,
            &note,
            WritePolicy::Update {
                expected_token: base["accepted"]["token"].as_str().unwrap().into(),
            },
        ),
        0,
    )
    .unwrap()
}
#[test]
fn cli_creates_once_retries_updates_and_tags_guard_the_gui_in_both_orders() {
    let fixture = Fixture::new();
    let id = fixture
        .ok(
            &[
                "notes",
                "new",
                "Title",
                "--operation-id",
                "new-1",
                "--stdin",
            ],
            "A",
        )
        .trim()
        .to_string();
    assert_eq!(
        fixture
            .ok(
                &[
                    "notes",
                    "new",
                    "Title",
                    "--operation-id",
                    "new-1",
                    "--stdin"
                ],
                "A"
            )
            .trim(),
        id
    );
    assert!(!fixture
        .run(
            &[
                "notes",
                "new",
                "Title",
                "--operation-id",
                "new-1",
                "--stdin"
            ],
            "different"
        )
        .status
        .success());
    let gui = fixture.db();
    let a = fixture.show(&id);
    let a_token = a["accepted"]["token"].as_str().unwrap();
    let b: Value = serde_json::from_str(&fixture.ok(
        &[
            "notes",
            "update",
            &id,
            "--expected-token",
            a_token,
            "--operation-id",
            "update-1",
            "--stdin",
        ],
        "B",
    ))
    .unwrap();
    let c = write(&gui, &b, "C");
    let retry: Value = serde_json::from_str(&fixture.ok(
        &[
            "notes",
            "update",
            &id,
            "--expected-token",
            a_token,
            "--operation-id",
            "update-1",
            "--stdin",
        ],
        "B",
    ))
    .unwrap();
    assert_eq!(retry, b);
    assert_eq!(fixture.show(&id), c);
    let stale = fixture.run(
        &[
            "notes",
            "tag",
            &id,
            "--add",
            "stale",
            "--expected-token",
            a_token,
        ],
        "",
    );
    assert_eq!(stale.status.code(), Some(3));
    fixture.ok(
        &[
            "notes",
            "tag",
            &id,
            "--add",
            "tag",
            "--expected-token",
            c["accepted"]["token"].as_str().unwrap(),
            "--operation-id",
            "tag-1",
        ],
        "",
    );
    let tagged = fixture.show(&id);
    assert_eq!(tagged["content"], "C");
    assert_eq!(tagged["tags"], json!(["tag"]));
    let result = gui.write_scripted(&NoteWrite::from_note(
        &gui,
        &NewNote {
            id: id.clone(),
            title: "Title".into(),
            content: "stale GUI".into(),
            pinned: false,
            tags: vec![],
            created: "ignored".into(),
            modified: "ignored".into(),
        },
        WritePolicy::Update {
            expected_token: c["accepted"]["token"].as_str().unwrap().into(),
        },
    ));
    assert!(result.is_err());
    // The v2 default CLI path remains fail-closed even in this feature build.
    let default = Command::new(env!("CARGO_BIN_EXE_ryuk"))
        .arg("--vault")
        .arg(fixture.dir.path().join("v.rn"))
        .env_remove("RYUK_V3_LEDGER")
        .args(["notes", "list"])
        .output()
        .unwrap();
    assert!(!default.status.success());
}
#[test]
fn cli_summary_import_does_not_promote_manually_edited_then_restored_generated_text() {
    let fixture = Fixture::new();
    let summary = fixture.dir.path().join("summary.json");
    std::fs::write(
        &summary,
        json!({"session_id":"fixture","title":"Summary","changed":["generated"]}).to_string(),
    )
    .unwrap();
    let path = summary.to_string_lossy().into_owned();
    fixture.ok(&["session", "import", "--file", &path], "");
    let a = fixture.show("hermes-session-fixture");
    fixture.ok(&["session", "import", "--file", &path], "");
    assert_eq!(fixture.show("hermes-session-fixture"), a);
    let gui = fixture.db();
    let manual = write(&gui, &a, "manual");
    let back = write(&gui, &manual, a["content"].as_str().unwrap());
    let conflict = fixture.run(&["session", "import", "--file", &path], "");
    assert_eq!(conflict.status.code(), Some(3));
    let error: Value = serde_json::from_slice(&conflict.stderr).unwrap();
    assert!(error["artifact"].is_string());
    assert_eq!(fixture.show("hermes-session-fixture"), back);
    let pending: Value =
        serde_json::from_str(&fixture.ok(&["session", "pending", "fixture"], "")).unwrap();
    assert_eq!(pending.as_array().unwrap().len(), 1);
}
