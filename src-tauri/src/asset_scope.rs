use std::path::Path;
use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_fs::FsExt;

/// Only configured user-selected vaults may provide an external note's vault-relative root.
pub fn external_note_root<R: Runtime>(
    app: &AppHandle<R>,
    note_path: &Path,
) -> Result<std::path::PathBuf, String> {
    let note = std::fs::canonicalize(note_path).map_err(|error| error.to_string())?;
    if let Some(state) = app.try_state::<crate::state::AppState>() {
        let (active, mut roots) = {
            let config = state.config.lock().map_err(|error| error.to_string())?;
            (
                config.active_vault.clone(),
                config
                    .vaults
                    .iter()
                    .map(|vault| vault.path.clone())
                    .collect::<Vec<_>>(),
            )
        };
        roots.sort_by_key(|root| std::cmp::Reverse(root.len()));
        for root in active.into_iter().chain(roots) {
            if let Ok(root) = std::fs::canonicalize(root) {
                if note.starts_with(&root) {
                    return Ok(root);
                }
            }
        }
    }
    note.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "External note has no parent directory".into())
}

/// Grants asset-protocol access only to a vault selected by the user.
/// `.helixnotes` is added explicitly because Unix glob matching excludes hidden paths.
pub fn allow_vault_assets<R: Runtime>(app: &AppHandle<R>, vault_path: &Path) -> Result<(), String> {
    // Keep the canonical native path: Tauri's scope normalizes Unix paths plus
    // Windows drive, verbatim, and UNC prefixes when matching asset requests.
    let vault = std::fs::canonicalize(vault_path).map_err(|error| {
        format!(
            "Failed to resolve vault asset path '{}': {error}",
            vault_path.display()
        )
    })?;
    let scope = app.asset_protocol_scope();
    scope
        .allow_directory(&vault, true)
        .map_err(|error| error.to_string())?;
    scope
        .allow_directory(vault.join(".helixnotes"), true)
        .map_err(|error| error.to_string())
}

/// Grants access to relative assets next to a Markdown file explicitly opened by the user.
pub fn allow_external_note_assets<R: Runtime>(
    app: &AppHandle<R>,
    note_path: &Path,
) -> Result<(), String> {
    let note = std::fs::canonicalize(note_path).map_err(|error| {
        format!(
            "Failed to resolve external note path '{}': {error}",
            note_path.display()
        )
    })?;
    let parent = note
        .parent()
        .ok_or_else(|| "External note has no parent directory".to_string())?;
    app.asset_protocol_scope()
        .allow_directory(parent, true)
        .map_err(|error| error.to_string())?;
    let root = external_note_root(app, &note)?;
    let attachments = root.join(".helixnotes/attachments");
    // Never follow a hidden attachment-directory symlink when extending either scope.
    if std::fs::canonicalize(&attachments)
        .is_ok_and(|canonical| canonical == attachments && canonical.is_dir())
    {
        app.asset_protocol_scope()
            .allow_directory(&attachments, true)
            .map_err(|error| error.to_string())?;
        if let Some(scope) = app.try_fs_scope() {
            scope
                .allow_directory(&attachments, true)
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn test_directory(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("ryuknotes-{name}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn static_asset_scope_does_not_expose_the_filesystem() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let scope = &config["app"]["security"]["assetProtocol"]["scope"];

        assert_eq!(scope["allow"], serde_json::json!([]));
        assert_eq!(scope["deny"], serde_json::json!([]));
        assert_eq!(scope["requireLiteralLeadingDot"], true);
    }

    #[test]
    fn runtime_scope_allows_only_the_selected_vault() {
        let root = test_directory("asset-scope");
        let vault = root.join("Vault [Cross Platform]");
        let attachments = vault.join(".helixnotes").join("attachments");
        fs::create_dir_all(&attachments).unwrap();
        let note_image = vault.join("images").join("note image.png");
        fs::create_dir_all(note_image.parent().unwrap()).unwrap();
        fs::write(&note_image, b"image").unwrap();
        let attachment = attachments.join("attachment.png");
        fs::write(&attachment, b"attachment").unwrap();
        let outside = root.join("outside.png");
        fs::write(&outside, b"outside").unwrap();

        let app = tauri::test::mock_app();
        allow_vault_assets(app.handle(), &vault).unwrap();
        let scope = app.asset_protocol_scope();

        assert!(scope.is_allowed(&note_image));
        assert!(scope.is_allowed(&attachment));
        assert!(!scope.is_allowed(&outside));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn external_note_scopes_include_its_attachments_but_not_hidden_private_metadata() {
        use tauri_plugin_fs::FsExt;
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("external");
        let attachments = parent.join(".helixnotes/attachments");
        fs::create_dir_all(&attachments).unwrap();
        fs::create_dir_all(parent.join(".helixnotes/history")).unwrap();
        fs::create_dir_all(parent.join(".ssh")).unwrap();
        let note = parent.join("Note.md");
        let image = attachments.join("image.png");
        let history = parent.join(".helixnotes/history/snapshot.md");
        let private = parent.join(".ssh/private.bin");
        for file in [&note, &image, &history, &private] {
            fs::write(file, b"fixture").unwrap();
        }
        let app = tauri::test::mock_builder()
            .plugin(tauri_plugin_fs::init())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.fs_scope().allow_file(&note).unwrap();
        app.fs_scope().allow_directory(&parent, true).unwrap();
        let before_private = [
            app.fs_scope().is_allowed(&history),
            app.fs_scope().is_allowed(&private),
        ];
        allow_external_note_assets(app.handle(), &note).unwrap();
        assert!(
            app.fs_scope().is_allowed(&image),
            "import must be able to read its managed attachments"
        );
        assert!(
            app.asset_protocol_scope().is_allowed(&image),
            "the viewer must display managed attachments"
        );
        for (file, previous) in [&history, &private].into_iter().zip(before_private) {
            assert_eq!(
                app.fs_scope().is_allowed(file),
                previous,
                "attachment grants must not widen private metadata access"
            );
            assert!(!app.asset_protocol_scope().is_allowed(file));
        }
    }

    #[test]
    fn external_notebook_uses_its_registered_vault_root_for_attachment_scopes() {
        use tauri_plugin_fs::FsExt;
        let root = tempfile::tempdir().unwrap();
        let vault = root.path().join("source");
        fs::create_dir_all(vault.join("Notebook")).unwrap();
        fs::create_dir_all(vault.join(".helixnotes/attachments")).unwrap();
        let note = vault.join("Notebook/Note.md");
        let image = vault.join(".helixnotes/attachments/image.png");
        fs::write(&note, b"Note").unwrap();
        fs::write(&image, b"Image").unwrap();
        let app = tauri::test::mock_builder()
            .plugin(tauri_plugin_fs::init())
            .manage(crate::state::AppState::new(crate::types::AppConfig {
                vaults: vec![crate::types::VaultConfig {
                    path: vault.to_string_lossy().into_owned(),
                    ..Default::default()
                }],
                ..Default::default()
            }))
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        allow_external_note_assets(app.handle(), &note).unwrap();
        assert!(app.asset_protocol_scope().is_allowed(&image));
        assert!(app.fs_scope().is_allowed(&image));
    }

    #[cfg(unix)]
    #[test]
    fn external_attachment_scope_does_not_follow_hidden_directory_symlinks() {
        use tauri_plugin_fs::FsExt;
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("external");
        let private = root.path().join("private");
        fs::create_dir_all(parent.join(".helixnotes")).unwrap();
        fs::create_dir(&private).unwrap();
        let note = parent.join("Note.md");
        let file = private.join("private.bin");
        fs::write(&note, b"Note").unwrap();
        fs::write(&file, b"private fixture").unwrap();
        std::os::unix::fs::symlink(&private, parent.join(".helixnotes/attachments")).unwrap();
        let app = tauri::test::mock_builder()
            .plugin(tauri_plugin_fs::init())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        allow_external_note_assets(app.handle(), &note).unwrap();
        assert!(!app.fs_scope().is_allowed(&file));
        assert!(!app.asset_protocol_scope().is_allowed(&file));
    }

    #[test]
    fn external_note_scope_allows_relative_assets_only_below_its_directory() {
        let root = test_directory("external-note-assets");
        let note_dir = root.join("shared note");
        let assets = note_dir.join("images");
        fs::create_dir_all(&assets).unwrap();
        let note = note_dir.join("note.md");
        let image = assets.join("image.png");
        let outside = root.join("outside.png");
        fs::write(&note, b"note").unwrap();
        fs::write(&image, b"image").unwrap();
        fs::write(&outside, b"outside").unwrap();

        let app = tauri::test::mock_app();
        allow_external_note_assets(app.handle(), &note).unwrap();
        let scope = app.asset_protocol_scope();

        assert!(scope.is_allowed(&note));
        assert!(scope.is_allowed(&image));
        assert!(!scope.is_allowed(&outside));

        fs::remove_dir_all(root).unwrap();
    }
}
