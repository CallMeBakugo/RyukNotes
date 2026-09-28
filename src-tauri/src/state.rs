use crate::search::{SearchIndex, SearchIndexShutdown};
use crate::types::AppConfig;
use crate::vault::watcher::VaultWatcher;
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, Weak};

struct CachedSearchIndex {
    index: Weak<SearchIndex>,
    shutdown: Arc<SearchIndexShutdown>,
}

pub struct AppState {
    pub config: Mutex<AppConfig>,
    pub search_index: Mutex<Option<Arc<SearchIndex>>>,
    search_indexes: Mutex<HashMap<String, CachedSearchIndex>>,
    pub watcher: Mutex<Option<VaultWatcher>>,
    pub vault_transition: tokio::sync::Mutex<()>,
    pub note_save: Arc<tokio::sync::Mutex<()>>,
    pub importing: AtomicBool,
    pub syncing: AtomicBool,
    #[cfg(desktop)]
    pub quitting: AtomicBool,
    pub pending_open_file: Mutex<Option<String>>,
}

impl AppState {
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
            watcher: Mutex::new(None),
            vault_transition: tokio::sync::Mutex::new(()),
            note_save: Arc::new(tokio::sync::Mutex::new(())),
            importing: AtomicBool::new(false),
            syncing: AtomicBool::new(false),
            #[cfg(desktop)]
            quitting: AtomicBool::new(false),
            pending_open_file: Mutex::new(None),
        }
    }
}

#[cfg(all(test, desktop))]
mod tests {
    use super::*;

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
