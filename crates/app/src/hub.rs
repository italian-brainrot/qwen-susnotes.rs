//! The unlocked vault, shared by the app's windows.
//!
//! This is the app's only owner of a [`Vault`]. Views get a clone of this handle
//! and never keep a vault of their own, so there is exactly one place that decides
//! what is open, and one place that starts and stops the HTTP API.
//!
//! The vault layer is synchronous and blocking: a note is a few kilobytes, so the
//! work is invisible next to a frame. The API server runs on its own thread,
//! because a long-lived connection from an agent must not be able to stall a
//! window.

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use susnotes_api::{AppState, Bind, OwnThreadHandle};
use susnotes_core::{
    Error, Note, Settings, Vault,
    config::{self, RecentVault},
    crypto::KdfParams,
    journal,
    logging::{self, LoggingGuard},
};

/// What a window holds to reach the current vault.
///
/// Cheap to clone: the state behind it is shared, so every window sees the same
/// vault. `Default` is a hub with nothing open, which is what the app starts with.
#[derive(Clone, Default)]
pub struct Hub {
    inner: Rc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    /// The unlocked vault, or `None` while the unlock window is showing.
    vault: RefCell<Option<Arc<Vault>>>,
    /// The HTTP API, while it is running.
    api: RefCell<Option<OwnThreadHandle>>,
    /// The log file's guard, kept alive so the writer is not closed while the
    /// vault is open.
    logging: RefCell<Option<LoggingGuard>>,
    /// The current vault's settings, kept in memory so the UI does not re-read the
    /// file on every frame.
    settings: RefCell<Settings>,
    /// The vault folder asked for on the command line, used to prefill the unlock
    /// window.
    requested: RefCell<Option<PathBuf>>,
}

impl Hub {
    /// The vault folder requested with `--vault`, if any.
    pub fn requested_vault(&self) -> Option<PathBuf> {
        self.inner.requested.borrow().clone()
    }

    /// Remember the folder given on the command line.
    pub fn set_requested_vault(&self, path: PathBuf) {
        *self.inner.requested.borrow_mut() = Some(path);
    }

    /// Whether a vault is open.
    pub fn is_unlocked(&self) -> bool {
        self.inner.vault.borrow().is_some()
    }

    /// The current vault, when one is open.
    pub fn vault(&self) -> Option<Arc<Vault>> {
        self.inner.vault.borrow().clone()
    }

    /// The current vault's settings. While nothing is open these are the defaults,
    /// which is what the unlock window needs for its own appearance.
    pub fn settings(&self) -> Settings {
        self.inner.settings.borrow().clone()
    }

    /// Where susnotes keeps its own state: recent vaults, and settings before a
    /// vault has been chosen.
    pub fn state_dir() -> PathBuf {
        config::state_dir()
    }

    /// Vaults opened before, newest first.
    pub fn recent_vaults() -> Vec<RecentVault> {
        let mut recent = config::recent_vaults_in(&Self::state_dir());
        // A folder that no longer holds a vault is dropped rather than offered: a
        // list of dead entries is worse than a short list.
        recent.retain(|entry| Vault::is_vault(Path::new(&entry.path)));
        recent
    }

    /// Open a vault with a password and take it into this hub.
    ///
    /// The password is used once, by the vault layer, to unwrap the key; the hub
    /// never keeps it.
    pub fn unlock(&self, path: &Path, password: &str) -> Result<Arc<Vault>, Error> {
        let request = Vault::open(path)?;
        let vault = Arc::new(request.unlock(password.as_bytes())?);
        self.adopt(vault, path)
    }

    /// Create a vault and open it.
    pub fn create(&self, path: &Path, password: &str) -> Result<Arc<Vault>, Error> {
        let vault = Arc::new(Vault::create(
            path,
            password.as_bytes(),
            KdfParams::default(),
        )?);
        self.adopt(vault, path)
    }

    /// Close the vault and stop its API.
    pub fn lock(&self) {
        if let Some(handle) = self.inner.api.borrow_mut().take() {
            handle.shutdown();
            tracing::info!("the API was stopped");
        }
        // Dropping the guard here flushes and closes the vault's log file.
        drop(self.inner.logging.borrow_mut().take());
        if let Some(vault) = self.inner.vault.borrow_mut().take() {
            tracing::info!(vault = %vault.root().display(), "the vault was closed");
        }
    }

    /// Write settings to the vault, or keep them in memory while nothing is open.
    pub fn save_settings(&self, settings: &Settings) -> Result<(), Error> {
        if let Some(vault) = self.vault() {
            vault.write_settings(settings)?;
        }
        *self.inner.settings.borrow_mut() = settings.clone();
        Ok(())
    }

    /// Today's journal page, created when it is missing.
    pub fn today(&self) -> Result<Note, Error> {
        self.needs_vault()?.open_today_journal()
    }

    /// Add a line to today's journal page.
    pub fn capture(&self, text: &str) -> Result<Note, Error> {
        let today = journal::today_name();
        self.needs_vault()?.append_to_note(&today, text)
    }

    /// Where the running API can be reached, for the status line and for telling a
    /// person how an agent should connect.
    pub fn api_url(&self) -> Option<String> {
        self.inner.api.borrow().as_ref().map(|handle| handle.url())
    }

    /// Journal pages before today, newest first, limited to `count`.
    pub fn recent_journal_pages(&self, count: usize) -> Vec<susnotes_core::vault::JournalPage> {
        let Some(vault) = self.vault() else {
            return Vec::new();
        };
        let today = journal::today_name();
        let mut pages: Vec<susnotes_core::vault::JournalPage> = vault
            .journal_pages()
            .unwrap_or_else(|err| {
                tracing::warn!(error = %err, "the journal could not be listed");
                Vec::new()
            })
            .into_iter()
            .filter(|page| page.name != today)
            .collect();
        pages.truncate(count);
        pages
    }

    /// A readable error for a command that needs an open vault, which the UI
    /// should not be able to reach in the first place.
    fn needs_vault(&self) -> Result<Arc<Vault>, Error> {
        self.vault()
            .ok_or_else(|| Error::Other("no vault is open".to_owned()))
    }

    /// Take a vault this hub has just opened: remember it, load its settings, move
    /// logging into the vault folder, and bring up its HTTP API.
    fn adopt(&self, vault: Arc<Vault>, path: &Path) -> Result<Arc<Vault>, Error> {
        let settings = vault.settings()?;
        *self.inner.settings.borrow_mut() = settings.clone();

        // The vault folder is where a bug report can find the logs without knowing
        // where susnotes is installed. A second `init` fails, because the process
        // already installed a logger at startup; that is fine and is logged.
        let logs = vault.log_dir();
        match logging::init(logging::LoggingConfig::with_dir(&logs)) {
            Ok(guard) => {
                *self.inner.logging.borrow_mut() = Some(guard);
                tracing::info!(logs = %logs.display(), "logging into the vault folder");
            }
            Err(err) => {
                tracing::info!(
                    logs = %logs.display(),
                    error = %err,
                    "keeping the log file opened at startup"
                );
            }
        }

        if settings.api.enabled {
            self.start_api(&vault, &settings);
        }

        if let Err(err) = config::remember_vault_in(&Self::state_dir(), path) {
            tracing::warn!(error = %err, "could not remember this vault as recent");
        }

        let stats = vault.stats().unwrap_or_default();
        tracing::info!(
            vault = %vault.root().display(),
            notes = stats.notes,
            "the vault is open"
        );
        *self.inner.vault.borrow_mut() = Some(Arc::clone(&vault));
        Ok(vault)
    }

    /// Start the HTTP API on its own thread.
    ///
    /// A port already in use does not fail the window: the API falls back to any
    /// free loopback port, and the vault settings record which port won, so
    /// `susnotes-cli` and an agent can still find it.
    fn start_api(&self, vault: &Arc<Vault>, settings: &Settings) {
        let state = match AppState::new(Arc::clone(vault)) {
            Ok(state) => state,
            Err(err) => {
                tracing::warn!(error = %err, "the API could not start");
                return;
            }
        };

        let handle = match susnotes_api::spawn_on_own_thread(
            state.clone(),
            Bind::from_settings(&settings.api),
        ) {
            Ok(handle) => Ok(handle),
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    port = settings.api.port,
                    "the configured API port is busy, trying any free loopback port"
                );
                susnotes_api::spawn_on_own_thread(state, Bind::loopback(0))
            }
        };

        match handle {
            Ok(handle) => {
                tracing::info!(url = %handle.url(), "the API is listening");
                *self.inner.api.borrow_mut() = Some(handle);
            }
            Err(err) => tracing::warn!(error = %err, "the API could not start"),
        }
    }
}
