//! CLI regressions use disposable vaults and a fake Hermes, never user data.
#![cfg(unix)]

use app_lib::vault::db::{NewNote, VaultDb};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

struct Fixture {
    dir: PathBuf,
    vault: PathBuf,
    home: String,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("ryuk-cli-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let vault = dir.join("displayed vault.rn");
        let home = format!("home-{}", uuid::Uuid::new_v4());
        let db = VaultDb::open(&vault).unwrap();
        for (id, body) in [
            (&home[..], "live home"),
            ("extra-a", "live extra A"),
            ("extra-b", "live extra B"),
        ] {
            db.upsert_note(&NewNote {
                id: id.into(),
                title: id.into(),
                content: body.into(),
                pinned: false,
                tags: vec![],
                created: "2026-10-15T00:00:00Z".into(),
                modified: "2026-10-15T00:00:00Z".into(),
            })
            .unwrap();
        }
        let program = dir.join("hermes");
        std::fs::write(
            &program,
            r#"#!/bin/sh
if test "$1" = sessions; then
    printf '%s\n' "$4" >> "$HERMES_TEST_DIR/workspaces"
    if test -f "$HERMES_TEST_DIR/started"; then
        printf '%s' '[{"session_id":"20261015_120000_abcdef"}]'
    else
        printf '%s' '[]'
    fi
    exit 0
fi
printf '%s\n' "$@" > "$HERMES_TEST_DIR/args"
printf started > "$HERMES_TEST_DIR/started"
"#,
        )
        .unwrap();
        std::fs::set_permissions(program, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self { dir, vault, home }
    }

    fn context(&self) -> PathBuf {
        std::env::temp_dir().join("ryuk-sessions").join(&self.home)
    }

    fn run(&self, args: &[&str]) {
        let output = Command::new(env!("CARGO_BIN_EXE_ryuk"))
            .arg("--vault")
            .arg(&self.vault)
            .args(args)
            .env("PATH", &self.dir)
            .env("HERMES_TEST_DIR", &self.dir)
            // The GUI flag must override a deliberately conflicting CLI env.
            .env("RYUK_VAULT", self.dir.join("wrong-vault.rn"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn assert_grounding(&self) {
        let ctx = std::fs::read_to_string(self.context().join("context.md")).unwrap();
        assert!(ctx.contains("live home"));
        assert!(ctx.contains("# Additional context: extra-a"));
        assert!(ctx.contains("live extra A"));
        assert!(ctx.contains("# Additional context: extra-b"));
        assert!(ctx.contains("live extra B"));
        let rules = std::fs::read_to_string(self.context().join(".hermes.md")).unwrap();
        assert!(rules.contains(self.vault.to_str().unwrap()));
        let args = std::fs::read_to_string(self.dir.join("args")).unwrap();
        assert!(args.contains("--resume\n20261015_120000_abcdef\n"));
        assert!(args.contains("-z\nnew prompt with spaces\n"));
        assert!(args.contains(&format!("--in\n{}\n", self.context().display())));
        assert!(!self.dir.join("wrong-vault.rn").exists());
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.context());
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn linked_start_and_explicit_resume_routes_keep_extras_prompt_and_displayed_vault() {
    let f = Fixture::new();
    VaultDb::open(&f.vault)
        .unwrap()
        .set_session_link(&f.home, "20261015_120000_abcdef")
        .unwrap();
    // Both established forms of extra notes survive the linked-session branch.
    f.run(&[
        "session",
        "start",
        "--notes",
        &f.home,
        "extra-a",
        "--notes",
        "extra-b",
        "--prompt",
        "new prompt with spaces",
    ]);
    f.assert_grounding();
    for args in [
        vec!["session", "restore", "20261015_120000_abcdef"],
        vec!["session", "resume-for-note", &f.home],
    ] {
        let mut args = args;
        args.extend([
            "--notes",
            "extra-a",
            "--notes",
            "extra-b",
            "--prompt",
            "new prompt with spaces",
        ]);
        f.run(&args);
        f.assert_grounding();
    }
}

#[test]
fn new_session_discovers_and_links_by_full_workspace() {
    let f = Fixture::new();
    f.run(&["session", "start", "--notes", &f.home]);
    assert_eq!(
        VaultDb::open(&f.vault)
            .unwrap()
            .get_session_link(&f.home)
            .unwrap(),
        Some("20261015_120000_abcdef".into())
    );
    let workspaces = std::fs::read_to_string(f.dir.join("workspaces")).unwrap();
    let context = f.context();
    assert_eq!(
        workspaces.lines().collect::<Vec<_>>(),
        vec![context.to_str().unwrap(); 2]
    );
}

#[test]
fn failed_before_discovery_never_records_an_unverified_session_link() {
    let f = Fixture::new();
    let program = f.dir.join("hermes");
    let script = std::fs::read_to_string(&program).unwrap();
    std::fs::write(
        &program,
        script.replace("printf '%s' '[]'", "printf '%s' '[]'; exit 1"),
    )
    .unwrap();
    f.run(&["session", "start", "--notes", &f.home]);
    assert_eq!(
        VaultDb::open(&f.vault)
            .unwrap()
            .get_session_link(&f.home)
            .unwrap(),
        None
    );
}
