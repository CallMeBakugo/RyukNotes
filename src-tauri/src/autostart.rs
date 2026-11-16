//! Platform startup registration, kept separate so fixtures never change host settings.
#[cfg(target_os = "linux")]
fn write_entry(
    root: &std::path::Path,
    executable: &std::path::Path,
    enabled: bool,
) -> Result<(), String> {
    let path = root.join("autostart/ryuknotes.desktop");
    if !enabled {
        return match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        };
    }
    let executable = executable.to_str().ok_or("Executable path is not UTF-8")?;
    if executable.contains(['\n', '\r', '\0']) {
        return Err("Invalid executable path".into());
    }
    let escaped = executable
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('`', "\\`")
        .replace('$', "\\$")
        .replace('%', "%%");
    std::fs::create_dir_all(path.parent().ok_or("Missing startup directory")?)
        .map_err(|e| e.to_string())?;
    crate::vault::operations::replace_file_atomically(&path, format!("[Desktop Entry]\nType=Application\nName=Ryuk's Notebook\nExec=\"{escaped}\"\nTerminal=false\n").as_bytes())
}

pub fn apply(enabled: bool) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        write_entry(
            &dirs::config_dir().ok_or("No configuration directory")?,
            &std::env::current_exe().map_err(|e| e.to_string())?,
            enabled,
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        if enabled {
            Err("Startup registration is supported on Linux only".into())
        } else {
            Ok(())
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #[test]
    fn registration_quotes_executable_and_removal_is_idempotent() {
        let root = tempfile::tempdir().unwrap();
        super::write_entry(
            root.path(),
            std::path::Path::new("/tmp/Notes $x 20%.app"),
            true,
        )
        .unwrap();
        let text =
            std::fs::read_to_string(root.path().join("autostart/ryuknotes.desktop")).unwrap();
        assert!(text.contains("Exec=\"/tmp/Notes \\$x 20%%.app\""));
        super::write_entry(root.path(), std::path::Path::new("unused"), false).unwrap();
        super::write_entry(root.path(), std::path::Path::new("unused"), false).unwrap();
        assert!(!root.path().join("autostart/ryuknotes.desktop").exists());
    }
}
