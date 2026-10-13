use crate::search::{SearchIndex, SearchIndexShutdown};
use crate::types::AppConfig;
use crate::vault::db::VaultDb;
use crate::vault::watcher::VaultWatcher;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, Weak};
use tauri::AppHandle;
#[cfg(desktop)]
use tauri::{Manager, Runtime, Url};

#[cfg(desktop)]
fn is_detached_window_for_vault(label: &str, url: &Url, target_path: &str) -> bool {
    label.starts_with("note-")
        && url
            .query_pairs()
            .any(|(key, value)| key == "vault" && value == target_path)
}

struct CachedSearchIndex {
    index: Weak<SearchIndex>,
    shutdown: Arc<SearchIndexShutdown>,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingOpenFile {
    pub id: u64,
    pub path: String,
}

#[derive(Default)]
struct PendingOpenFiles {
    next_id: u64,
    requests: VecDeque<PendingOpenFile>,
}

pub struct AppState {
    pub config: Mutex<AppConfig>,
    pub search_index: Mutex<Option<Arc<SearchIndex>>>,
    search_indexes: Mutex<HashMap<String, CachedSearchIndex>>,
    pub watchers: Mutex<HashMap<String, VaultWatcher>>,
    pub vault_transition: tokio::sync::Mutex<()>,
    pub vault_maintenance: Arc<tokio::sync::Mutex<()>>,
    pub note_save: Arc<tokio::sync::Mutex<()>>,
    pub importing: AtomicBool,
    /// The SQLite vault database, when the active vault is DB-backed.
    pub vault_db: Mutex<Option<VaultDb>>,
    #[cfg(desktop)]
    pub quitting: AtomicBool,
    pending_open_files: Mutex<PendingOpenFiles>,
}

impl AppState {
    pub fn enqueue_open_file(&self, path: String) -> Result<(), String> {
        let mut pending = self
            .pending_open_files
            .lock()
            .map_err(|error| error.to_string())?;
        pending.next_id += 1;
        let id = pending.next_id;
        pending.requests.push_back(PendingOpenFile { id, path });
        Ok(())
    }

    pub fn peek_pending_open_file(&self) -> Result<Option<PendingOpenFile>, String> {
        let pending = self
            .pending_open_files
            .lock()
            .map_err(|error| error.to_string())?;
        Ok(pending.requests.front().cloned())
    }

    pub fn ack_pending_open_file(&self, id: u64) -> Result<bool, String> {
        let mut pending = self
            .pending_open_files
            .lock()
            .map_err(|error| error.to_string())?;
        if pending
            .requests
            .front()
            .is_some_and(|request| request.id == id)
        {
            pending.requests.pop_front();
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn ensure_vault_watcher(&self, app: &AppHandle, vault_path: &str) -> Result<(), String> {
        let mut watchers = self.watchers.lock().map_err(|error| error.to_string())?;
        if !watchers.contains_key(vault_path) {
            let watcher = crate::vault::watcher::start_watcher(app.clone(), vault_path.to_owned())?;
            watchers.insert(vault_path.to_owned(), watcher);
        }
        Ok(())
    }

    pub fn retain_vault_watchers(&self) -> Result<(), String> {
        let configured: HashSet<String> = {
            let config = self.config.lock().map_err(|error| error.to_string())?;
            config
                .vaults
                .iter()
                .map(|vault| vault.path.clone())
                .chain(config.active_vault.clone())
                .collect()
        };
        let removed = {
            let mut watchers = self.watchers.lock().map_err(|error| error.to_string())?;
            let removed: Vec<String> = watchers
                .keys()
                .filter(|path| !configured.contains(*path))
                .cloned()
                .collect();
            removed
                .into_iter()
                .filter_map(|path| watchers.remove(&path))
                .collect::<Vec<_>>()
        };
        drop(removed);
        Ok(())
    }

    #[cfg(desktop)]
    pub fn ensure_no_detached_windows<R: Runtime>(
        app: &AppHandle<R>,
        target_path: &str,
    ) -> Result<(), String> {
        for (label, window) in app.webview_windows() {
            if is_detached_window_for_vault(
                &label,
                &window.url().map_err(|error| error.to_string())?,
                target_path,
            ) {
                return Err(
                    "Close this vault's detached note windows before forgetting it.".into(),
                );
            }
        }
        Ok(())
    }

    // A refresh worker may outlive a vault switch. Reuse its writer when opening
    // that vault again instead of competing for Tantivy's exclusive writer lock.
    pub fn search_index_for_vault(&self, vault_path: &str) -> Result<Arc<SearchIndex>, String> {
        let mut indexes = self
            .search_indexes
            .lock()
            .map_err(|error| error.to_string())?;
        indexes
            .retain(|_, cached| cached.index.strong_count() > 0 || !cached.shutdown.is_finished());
        if let Some(cached) = indexes.get(vault_path) {
            if let Some(index) = cached.index.upgrade() {
                return Ok(index);
            }
            // The last Arc is gone, but its writer may still be shutting down.
            cached.shutdown.wait();
        }
        let index = Arc::new(SearchIndex::new(vault_path)?);
        indexes.insert(
            vault_path.to_owned(),
            CachedSearchIndex {
                index: Arc::downgrade(&index),
                shutdown: index.shutdown_signal(),
            },
        );
        Ok(index)
    }

    pub fn validate_vault(&self, vault_path: &str) -> Result<(), String> {
        let config = self.config.lock().map_err(|error| error.to_string())?;
        if config.active_vault.as_deref() == Some(vault_path)
            || config.vaults.iter().any(|vault| vault.path == vault_path)
        {
            Ok(())
        } else {
            Err("Unknown vault".to_string())
        }
    }

    pub fn vault_snapshot(&self, vault_path: &str) -> Result<Option<Arc<SearchIndex>>, String> {
        let search = self
            .search_index
            .lock()
            .map_err(|error| error.to_string())?;
        let config = self.config.lock().map_err(|error| error.to_string())?;
        if config.active_vault.as_deref() == Some(vault_path) {
            return Ok(search.clone());
        }
        if !config.vaults.iter().any(|vault| vault.path == vault_path) {
            return Err("Unknown vault".to_string());
        }
        drop(config);
        drop(search);
        // Detached windows retain access to configured vaults. Never pair their
        // writes with the main window's index, nor hold config during disk I/O.
        self.search_index_for_vault(vault_path).map(Some)
    }

    pub fn active_vault_snapshot(&self) -> Result<(String, Option<Arc<SearchIndex>>), String> {
        // Match vault transition and watcher lock order. The vault and its
        // optional index must remain paired even if a switch follows the read.
        let search = self
            .search_index
            .lock()
            .map_err(|error| error.to_string())?;
        let config = self.config.lock().map_err(|error| error.to_string())?;
        Ok((
            config.active_vault.clone().ok_or("No active vault")?,
            search.clone(),
        ))
    }

    pub fn active_search(&self) -> Result<(Arc<SearchIndex>, String), String> {
        let (vault_path, search) = self.active_vault_snapshot()?;
        Ok((search.ok_or("Search index not initialized")?, vault_path))
    }

    #[cfg(desktop)]
    pub fn should_hide_to_tray(&self, label: &str, tray_available: bool) -> Result<bool, String> {
        if label != "main"
            || !tray_available
            || self.quitting.load(std::sync::atomic::Ordering::Relaxed)
        {
            return Ok(false);
        }
        let config = self.config.lock().map_err(|error| error.to_string())?;
        Ok(config.show_tray_icon && config.close_to_tray)
    }

    pub fn new(config: AppConfig) -> Self {
        Self {
            config: Mutex::new(config),
            search_index: Mutex::new(None),
            search_indexes: Mutex::new(HashMap::new()),
            watchers: Mutex::new(HashMap::new()),
            vault_transition: tokio::sync::Mutex::new(()),
            vault_maintenance: Arc::new(tokio::sync::Mutex::new(())),
            note_save: Arc::new(tokio::sync::Mutex::new(())),
            importing: AtomicBool::new(false),
            vault_db: Mutex::new(None),
            #[cfg(desktop)]
            quitting: AtomicBool::new(false),
            pending_open_files: Mutex::new(PendingOpenFiles::default()),
        }
    }
}

#[cfg(all(test, desktop))]
mod tests {
    use super::*;

    #[test]
    fn pending_open_requests_survive_reads_and_only_acknowledge_head() {
        let state = AppState::new(AppConfig::default());
        state.enqueue_open_file("one.md".into()).unwrap();
        state.enqueue_open_file("two.md".into()).unwrap();
        let first = state.peek_pending_open_file().unwrap().unwrap();
        assert_eq!(first.path, "one.md");
        assert_eq!(
            state.peek_pending_open_file().unwrap().unwrap().id,
            first.id
        );
        assert!(!state.ack_pending_open_file(first.id + 1).unwrap());
        assert_eq!(
            state.peek_pending_open_file().unwrap().unwrap().id,
            first.id
        );
        assert!(state.ack_pending_open_file(first.id).unwrap());
        let second = state.peek_pending_open_file().unwrap().unwrap();
        assert_eq!(second.path, "two.md");
        assert!(!state.ack_pending_open_file(first.id).unwrap());
        assert!(state.ack_pending_open_file(second.id).unwrap());
        assert!(state.peek_pending_open_file().unwrap().is_none());
    }

    #[test]
    fn detached_window_owner_matches_encoded_vault_path_only() {
        let url = Url::parse("tauri://localhost/?note=a.md&vault=%2Fvaults%2FA%26B").unwrap();
        assert!(is_detached_window_for_vault(
            "note-123",
            &url,
            "/vaults/A&B"
        ));
        assert!(!is_detached_window_for_vault("note-123", &url, "/vaults/A"));
        assert!(!is_detached_window_for_vault("main", &url, "/vaults/A&B"));
    }

    #[test]
    fn close_policy_respects_live_settings_window_type_and_explicit_quit() {
        let mut state = AppState::new(AppConfig {
            show_tray_icon: true,
            close_to_tray: true,
            ..AppConfig::default()
        });
        assert!(state.should_hide_to_tray("main", true).unwrap());
        assert!(!state.should_hide_to_tray("note-123", true).unwrap());
        assert!(!state.should_hide_to_tray("main", false).unwrap());
        state
            .quitting
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(!state.should_hide_to_tray("main", true).unwrap());
        state
            .quitting
            .store(false, std::sync::atomic::Ordering::Relaxed);
        state.config.get_mut().unwrap().close_to_tray = false;
        assert!(!state.should_hide_to_tray("main", true).unwrap());
        state.config.get_mut().unwrap().close_to_tray = true;
        state.config.get_mut().unwrap().show_tray_icon = false;
        assert!(!state.should_hide_to_tray("main", true).unwrap());
    }
}
