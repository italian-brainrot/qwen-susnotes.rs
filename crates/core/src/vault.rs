//! The vault: an unlocked folder of encrypted notes and assets.
//!
//! Folder layout, which is also the portable contract of a vault:
//!
//! ```text
//! <root>/
//!   pages/     one encrypted markdown file per note, named after the note
//!   assets/    encrypted images and attachments
//!   config/    vault.json (wrapped master key), settings.json, logs/
//! ```
//!
//! A `Vault` value only exists while unlocked: it holds the master key and the
//! [change bus](crate::change::ChangeBus). Locking is dropping the value.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use base64::prelude::{BASE64_STANDARD as B64, Engine as _};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::assets::{self, ASSETS_DIR, Asset, AssetRef, now_millis};
use crate::change::{Change, ChangeBus};
use crate::crypto::{KdfParams, MasterKey, PasswordKey, RecordKind, SALT_LEN, WrappedKey};
use crate::error::{Error, Result};
use crate::fs;
use crate::links::{LinkSyntax, parse_links};
use crate::names::{NoteName, file_name_for, is_note_file_name, note_name_for_file_name};
use crate::search::{self, SearchHit};

/// Folder holding the markdown notes.
pub const PAGES_DIR: &str = "pages";
/// Folder holding app configuration.
pub const CONFIG_DIR: &str = "config";
/// File holding the vault's crypto envelope.
pub const VAULT_META_FILE: &str = "vault.json";
/// File holding user settings.
pub const SETTINGS_FILE: &str = "settings.json";
/// Folder holding log files.
pub const LOGS_DIR: &str = "logs";
/// Format version of the on-disk vault layout.
pub const VAULT_VERSION: u32 = 1;

/// Contents of `config/vault.json`: everything needed to try an unlock, none of
/// it secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultMeta {
    /// Layout version, so a future release can migrate old vaults.
    pub version: u32,
    /// When the vault was created, in milliseconds since the epoch.
    pub created_at: i64,
    /// Parameters used to stretch the password.
    pub kdf: KdfParams,
    /// Base64 encoded Argon2id salt.
    pub salt: String,
    /// The vault master key, encrypted under the stretched password.
    pub wrapped_key: WrappedKey,
}

impl VaultMeta {
    /// Read and validate `config/vault.json`.
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join(CONFIG_DIR).join(VAULT_META_FILE);
        let bytes = fs::read_optional(&path)?.ok_or_else(|| {
            if root.exists() {
                Error::NotAVault {
                    path: root.to_path_buf(),
                }
            } else {
                Error::NotAVault { path: path.clone() }
            }
        })?;

        let meta: Self = serde_json::from_slice(&bytes).map_err(|err| Error::Config {
            file: path.clone(),
            reason: err.to_string(),
        })?;
        if meta.version != VAULT_VERSION {
            return Err(Error::Config {
                file: path,
                reason: format!(
                    "unsupported vault version {} (this build understands {VAULT_VERSION})",
                    meta.version
                ),
            });
        }
        let salt = B64.decode(&meta.salt).map_err(|err| Error::Config {
            file: path.clone(),
            reason: format!("salt is not base64: {err}"),
        })?;
        if salt.len() != SALT_LEN {
            return Err(Error::Config {
                file: path,
                reason: format!("salt must be {SALT_LEN} bytes, found {}", salt.len()),
            });
        }
        Ok(meta)
    }

    /// Write `config/vault.json` atomically.
    pub fn store(&self, root: &Path) -> Result<()> {
        let path = root.join(CONFIG_DIR).join(VAULT_META_FILE);
        let json = serde_json::to_vec_pretty(self).map_err(|err| Error::Config {
            file: path.clone(),
            reason: err.to_string(),
        })?;
        fs::write_atomic(&path, &json)
    }

    fn salt_bytes(&self) -> Result<Vec<u8>> {
        B64.decode(&self.salt).map_err(|err| Error::Config {
            file: PathBuf::from(CONFIG_DIR).join(VAULT_META_FILE),
            reason: format!("salt is not base64: {err}"),
        })
    }
}

/// A vault that was found on disk but is still locked.
///
/// Unlocking is deliberately a separate, expensive step so the UI can do it on
/// a background thread.
#[derive(Debug, Clone)]
pub struct UnlockRequest {
    root: PathBuf,
    meta: VaultMeta,
}

impl UnlockRequest {
    /// Read the vault envelope at `root`.
    pub fn discover(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let meta = VaultMeta::load(&root)?;
        debug!(?root, version = meta.version, "found a susnotes vault");
        Ok(Self { root, meta })
    }

    /// Whether a vault envelope exists at `root`.
    #[must_use]
    pub fn is_vault(root: impl AsRef<Path>) -> bool {
        root.as_ref()
            .join(CONFIG_DIR)
            .join(VAULT_META_FILE)
            .is_file()
    }

    /// The folder holding the vault.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The folder name, used as the vault's title before it is unlocked.
    #[must_use]
    pub fn title(&self) -> String {
        self.root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.root.display().to_string())
    }

    /// Cost parameters that will be used to stretch the password.
    #[must_use]
    pub fn kdf(&self) -> KdfParams {
        self.meta.kdf
    }

    /// Stretch the password and unwrap the master key.
    ///
    /// This is the expensive part of opening a vault: run it off the UI thread.
    pub fn unlock(&self, password: &[u8]) -> Result<Vault> {
        let salt = self.meta.salt_bytes()?;
        let password_key: PasswordKey = self.meta.kdf.derive_password_key(password, &salt)?;
        let master = crate::crypto::unwrap_master_key(&password_key, &self.meta.wrapped_key)?;
        info!(root = %self.root.display(), "vault unlocked");
        let vault = Vault {
            root: self.root.clone(),
            meta: self.meta.clone(),
            master,
            bus: ChangeBus::new(),
        };
        vault.bus.emit(Change::Unlocked);
        Ok(vault)
    }
}

/// A note without its content: what a folder listing costs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteSummary {
    /// Display name of the note.
    pub name: NoteName,
    /// Path inside the vault, e.g. `pages/Roam.md`.
    pub path: String,
    /// Size of the encrypted file, in bytes.
    pub size: u64,
    /// Last modification time in milliseconds since the epoch.
    pub modified_at: i64,
    /// Creation time when the file system reports one.
    pub created_at: Option<i64>,
    /// Whether this note is a journal page.
    pub is_journal: bool,
}

/// A note with its decrypted markdown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    /// Display name of the note.
    pub name: NoteName,
    /// Path inside the vault.
    pub path: String,
    /// Markdown source.
    pub content: String,
    /// Last modification time in milliseconds since the epoch.
    pub modified_at: i64,
    /// Creation time when the file system reports one.
    pub created_at: Option<i64>,
}

impl Note {
    /// A note with nothing in it.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.content.trim().is_empty()
    }
}

/// One note pointing at another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkEdge {
    /// Name of the note holding the link.
    pub from: String,
    /// Name of the note the link resolves to.
    pub to: String,
    /// Syntax the link was written with.
    pub syntax: LinkSyntax,
    /// Byte range of the link in `from`.
    pub range: std::ops::Range<usize>,
    /// Text shown instead of the target name, when there was one.
    pub alias: Option<String>,
    /// The line of `from` that holds the link.
    pub snippet: String,
}

/// A link that points at a note which does not exist yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingLink {
    /// The note holding the link.
    pub from: String,
    /// The name the link asks for.
    pub target: String,
    /// Byte range of the link.
    pub range: std::ops::Range<usize>,
}

/// Every link in the vault, computed by one pass over the notes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkGraph {
    /// Links that resolved to an existing note.
    pub links: Vec<LinkEdge>,
    /// Links whose target does not exist yet, which the app can offer to create.
    pub missing: Vec<MissingLink>,
    /// Links to assets, as `(note, asset path)`.
    pub assets: Vec<(String, String)>,
    /// Links that leave the vault.
    pub external: Vec<(String, String)>,
}

impl LinkGraph {
    /// Links pointing at `name`.
    #[must_use]
    pub fn backlinks(&self, name: &str) -> Vec<&LinkEdge> {
        let key = name.to_lowercase();
        self.links
            .iter()
            .filter(|link| link.to.to_lowercase() == key)
            .collect()
    }

    /// Links written inside `name`.
    #[must_use]
    pub fn outgoing(&self, name: &str) -> Vec<&LinkEdge> {
        let key = name.to_lowercase();
        self.links
            .iter()
            .filter(|link| link.from.to_lowercase() == key)
            .collect()
    }

    /// Names that are linked but missing, with how many links ask for them.
    #[must_use]
    pub fn missing_counts(&self) -> Vec<(String, usize)> {
        let mut counts: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for missing in &self.missing {
            *counts.entry(missing.target.clone()).or_default() += 1;
        }
        counts.into_iter().collect()
    }
}

/// How full a vault is; shown in the status bar and reported by the CLI.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultStats {
    /// Number of notes.
    pub notes: usize,
    /// Notes that are not empty.
    pub notes_with_content: usize,
    /// Journal pages.
    pub journal_pages: usize,
    /// Assets in `assets/`.
    pub assets: usize,
    /// Total encrypted size, in bytes.
    pub bytes: u64,
    /// Modification time of the newest note, in milliseconds since the epoch.
    pub last_modified: Option<i64>,
}

/// An unlocked vault. Cheap to clone through an `Arc`, and the single place
/// that touches the vault's files.
#[derive(Debug)]
pub struct Vault {
    root: PathBuf,
    meta: VaultMeta,
    master: MasterKey,
    bus: ChangeBus,
}

impl Vault {
    /// Create a brand new vault in `root`, which must not hold one yet.
    ///
    /// The folders are created, a random master key is generated and wrapped
    /// under `password`, and default settings are written.
    pub fn create(root: impl AsRef<Path>, password: &[u8], kdf: KdfParams) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        if UnlockRequest::is_vault(&root) {
            return Err(Error::InvalidVault {
                path: root,
                reason: "this folder already holds a susnotes vault".into(),
            });
        }

        for dir in [PAGES_DIR, ASSETS_DIR, CONFIG_DIR] {
            std::fs::create_dir_all(root.join(dir))
                .map_err(|err| Error::io("create folder", root.join(dir), err))?;
        }

        let salt = crate::crypto::random_salt()?;
        let password_key = kdf.derive_password_key(password, &salt)?;
        let master = MasterKey::generate()?;
        let meta = VaultMeta {
            version: VAULT_VERSION,
            created_at: now_millis(),
            kdf,
            salt: B64.encode(salt),
            wrapped_key: crate::crypto::wrap_master_key(&password_key, &master)?,
        };
        meta.store(&root)?;

        info!(root = %root.display(), "created a new vault");
        let vault = Self {
            root,
            meta,
            master,
            bus: ChangeBus::new(),
        };
        vault.write_settings(&crate::config::Settings::default())?;
        vault.bus.emit(Change::Unlocked);
        Ok(vault)
    }

    /// Whether a folder is a susnotes vault, without reading its contents.
    #[must_use]
    pub fn is_vault(root: impl AsRef<Path>) -> bool {
        UnlockRequest::is_vault(root)
    }

    /// Locate a vault at `root` and prepare to unlock it.
    pub fn open(root: impl AsRef<Path>) -> Result<UnlockRequest> {
        UnlockRequest::discover(root)
    }

    /// The folder holding the vault.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The vault's title: the folder name.
    #[must_use]
    pub fn title(&self) -> String {
        self.root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.root.display().to_string())
    }

    /// Parameters used to stretch this vault's password.
    #[must_use]
    pub fn kdf(&self) -> KdfParams {
        self.meta.kdf
    }

    /// The change bus, for the UI, the API and the CLI to observe.
    #[must_use]
    pub fn changes(&self) -> &ChangeBus {
        &self.bus
    }

    /// Absolute path of `pages/`.
    #[must_use]
    pub fn pages_dir(&self) -> PathBuf {
        self.root.join(PAGES_DIR)
    }

    /// Absolute path of `assets/`.
    #[must_use]
    pub fn assets_dir(&self) -> PathBuf {
        self.root.join(ASSETS_DIR)
    }

    /// Absolute path of `config/`.
    #[must_use]
    pub fn config_dir(&self) -> PathBuf {
        self.root.join(CONFIG_DIR)
    }

    /// Absolute path of `config/logs/`.
    #[must_use]
    pub fn log_dir(&self) -> PathBuf {
        self.config_dir().join(LOGS_DIR)
    }

    /// Read `config/settings.json`, falling back to defaults.
    pub fn settings(&self) -> Result<crate::config::Settings> {
        crate::config::Settings::load(&self.config_dir())
    }

    /// Replace `config/settings.json`.
    pub fn write_settings(&self, settings: &crate::config::Settings) -> Result<()> {
        settings.store(&self.config_dir())
    }

    fn record_path(&self, rel_path: &str) -> Result<PathBuf> {
        let normalized = crate::names::normalize_vault_path(rel_path)?;
        Ok(self.root.join(normalized))
    }

    fn read_record(
        &self,
        rel_path: &str,
        kind: RecordKind,
    ) -> Result<(Vec<u8>, std::fs::Metadata)> {
        let path = self.record_path(rel_path)?;
        let bytes = fs::read(&path)?;
        let metadata = std::fs::metadata(&path).map_err(|err| Error::io("stat", &path, err))?;
        let plaintext = crate::crypto::decrypt(&self.master, kind, rel_path, &bytes)?;
        Ok((plaintext, metadata))
    }

    fn write_record(
        &self,
        rel_path: &str,
        kind: RecordKind,
        plaintext: &[u8],
    ) -> Result<std::fs::Metadata> {
        let path = self.record_path(rel_path)?;
        let bytes = crate::crypto::encrypt(
            &self.master,
            kind,
            self.meta.kdf.algorithm,
            rel_path,
            plaintext,
        )?;
        fs::write_atomic(&path, &bytes)?;
        std::fs::metadata(&path).map_err(|err| Error::io("stat", &path, err))
    }

    /// Put a new record where `old_path` is, without ever writing there before
    /// the old entry has been moved out of the way.
    ///
    /// The order is the reason this exists: a case only rename (`hello` to
    /// `Hello`) names one and the same directory entry on APFS, NTFS and FAT,
    /// so writing the target first, the way an ordinary rename does, would land
    /// on the entry that is then moved aside and deleted, and the note would be
    /// gone. Neither copy of either record is ever missing: the new content goes
    /// to a temporary name in the same folder, the old entry is moved aside,
    /// the staged record is renamed to its final path, and if any step fails the
    /// old entry is put back where it was.
    fn replace_in_place(
        &self,
        old_path: &Path,
        rel_target: &str,
        kind: RecordKind,
        plaintext: &[u8],
    ) -> Result<()> {
        let final_path = self.record_path(rel_target)?;
        // The record is encrypted under the path it is going to live at, while
        // it is still sitting in a temporary file: the header binds it to the
        // place it is meant to be, exactly as a single write would.
        let sealed = crate::crypto::encrypt(
            &self.master,
            kind,
            self.meta.kdf.algorithm,
            rel_target,
            plaintext,
        )?;
        let staged = fs::staging_path(&final_path, "staged");
        fs::write_atomic(&staged, &sealed)?;

        let aside = fs::staging_path(old_path, "aside");
        if let Err(err) = std::fs::rename(old_path, &aside)
            .map_err(|err| Error::io("move the note aside", old_path, err))
        {
            // Nothing was written at the final path, and the staged record is
            // the note's only copy besides the entry still in place: leave no
            // file behind that the vault's listings would never read.
            let _ = fs::remove_optional(&staged);
            return Err(err);
        }
        if let Err(err) = std::fs::rename(&staged, &final_path)
            .map_err(|err| Error::io("replace", &final_path, err))
        {
            // The staged record never made it, so the entry that was moved aside
            // is the only copy of the note: put it back instead of leaving the
            // note in a temporary name the listings do not read.
            if let Err(restore) = std::fs::rename(&aside, old_path) {
                warn!(
                    note = %old_path.display(),
                    %restore,
                    "a replacement failed and the note could not be put back where it was"
                );
            }
            let _ = fs::remove_optional(&staged);
            return Err(err);
        }
        // The replacement is done; a leftover temporary file is not a reason to
        // report the rename as having failed, so it is reported on its own.
        if let Err(err) = fs::remove_optional(&aside) {
            warn!(
                file = %aside.display(),
                %err,
                "the note was replaced, and the entry it was moved aside to stayed behind"
            );
        }
        // The renames above changed the folder's entries; syncing that folder is
        // what makes the replacement survive a power loss.
        let _ = fs::sync_dir(final_path.parent().unwrap_or(Path::new(".")));
        Ok(())
    }

    /// Every note in `pages/`, newest first.
    ///
    /// This only reads file names and metadata; nothing is decrypted.
    pub fn list_notes(&self) -> Result<Vec<NoteSummary>> {
        let dir = self.pages_dir();
        let entries = std::fs::read_dir(&dir).map_err(|err| Error::io("list notes", &dir, err))?;

        let mut notes: Vec<NoteSummary> = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|err| Error::io("list notes", &dir, err))?;
            let file_name = entry.file_name().to_string_lossy().into_owned();
            if !is_note_file_name(&file_name) {
                continue;
            }
            let display = match note_name_for_file_name(&file_name) {
                Ok(name) => name,
                Err(err) => {
                    warn!(file = %entry.path().display(), %err, "skipping a note file whose name cannot be decoded");
                    continue;
                }
            };
            let metadata = entry
                .metadata()
                .map_err(|err| Error::io("stat", entry.path(), err))?;
            if !metadata.is_file() {
                continue;
            }
            notes.push(NoteSummary {
                modified_at: metadata
                    .modified()
                    .map(crate::assets::time_millis)
                    .unwrap_or_default(),
                created_at: metadata.created().ok().map(crate::assets::time_millis),
                size: metadata.len(),
                is_journal: crate::journal::is_journal_name(&display),
                name: NoteName::trusted(display),
                path: format!("{PAGES_DIR}/{file_name}"),
            });
        }

        notes.sort_by_key(|note| std::cmp::Reverse(note.modified_at));
        Ok(notes)
    }

    /// Find the note whose name matches `query`, ignoring case.
    ///
    /// An exact match wins; otherwise the most recently modified candidate
    /// does, which is what makes `[[roam]]` open the note called `Roam`.
    pub fn resolve_name(&self, query: &str) -> Result<Option<NoteSummary>> {
        Ok(Self::resolve_in(&self.name_index()?, query)?.cloned())
    }

    /// Group notes by their case folded name, so a single pass answers every
    /// lookup while walking a whole vault.
    fn name_index(&self) -> Result<HashMap<String, Vec<NoteSummary>>> {
        let mut index: HashMap<String, Vec<NoteSummary>> = HashMap::new();
        for note in self.list_notes()? {
            index.entry(note.name.key()).or_default().push(note);
        }
        for notes in index.values_mut() {
            notes.sort_by_key(|note| std::cmp::Reverse(note.modified_at));
        }
        Ok(index)
    }

    fn resolve_in<'a>(
        index: &'a HashMap<String, Vec<NoteSummary>>,
        query: &str,
    ) -> Result<Option<&'a NoteSummary>> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(None);
        }
        let name = NoteName::parse(query)?;
        if let Some(exact) = index
            .get(&name.key())
            .and_then(|notes| notes.iter().find(|note| note.name == name))
        {
            return Ok(Some(exact));
        }
        Ok(index.get(&name.key()).and_then(|notes| notes.first()))
    }

    /// Whether a note exists under any spelling.
    #[must_use]
    pub fn note_exists(&self, query: &str) -> bool {
        self.resolve_name(query).is_ok_and(|found| found.is_some())
    }

    /// The file that stores the note called `name`, relative to the vault.
    #[must_use]
    pub fn note_path_for_name(&self, name: &str) -> String {
        format!("{PAGES_DIR}/{}", file_name_for(name))
    }

    /// Read a note, decrypting it.
    pub fn read_note(&self, query: &str) -> Result<Note> {
        let summary = self
            .resolve_name(query)?
            .ok_or_else(|| Error::NoteNotFound(query.to_owned()))?;
        self.read_summary(&summary)
    }

    fn read_summary(&self, summary: &NoteSummary) -> Result<Note> {
        let (bytes, metadata) = self.read_record(&summary.path, RecordKind::Note)?;
        let content = String::from_utf8(bytes).map_err(|err| Error::Format {
            what: summary.path.clone(),
            reason: format!("the note is not valid UTF-8: {err}"),
        })?;
        Ok(Note {
            name: summary.name.clone(),
            path: summary.path.clone(),
            content,
            modified_at: metadata
                .modified()
                .map(crate::assets::time_millis)
                .unwrap_or(summary.modified_at),
            created_at: summary.created_at,
        })
    }

    /// Read a note, creating an empty one when it does not exist.
    ///
    /// This is what "opening a link to a note that is not there yet" does.
    pub fn open_note(&self, query: &str) -> Result<Note> {
        match self.resolve_name(query)? {
            Some(summary) => self.read_summary(&summary),
            None => {
                let name = NoteName::parse(query)?;
                info!(note = %name, "creating a note that was missing");
                let note = self.write_note_content(&name, "", true)?;
                self.bus.emit(Change::NoteCreated {
                    name: note.name.to_string(),
                });
                Ok(note)
            }
        }
    }

    /// Create or replace a note's content.
    pub fn write_note(&self, query: &str, content: &str) -> Result<Note> {
        let name = self
            .resolve_name(query)?
            .map(|summary| summary.name)
            .unwrap_or(NoteName::parse(query)?);
        let existed = self.resolve_name(query)?.is_some();
        let note = self.write_note_content(&name, content, existed)?;
        self.bus.emit(if existed {
            Change::NoteUpdated {
                name: note.name.to_string(),
            }
        } else {
            Change::NoteCreated {
                name: note.name.to_string(),
            }
        });
        Ok(note)
    }

    fn write_note_content(&self, name: &NoteName, content: &str, _existed: bool) -> Result<Note> {
        let path = name.vault_path();
        let metadata = self.write_record(&path, RecordKind::Note, content.as_bytes())?;
        Ok(Note {
            name: name.clone(),
            created_at: metadata.created().ok().map(crate::assets::time_millis),
            modified_at: metadata
                .modified()
                .map(crate::assets::time_millis)
                .unwrap_or_else(|_| now_millis()),
            content: content.to_owned(),
            path,
        })
    }

    /// Add text to the end of a note, creating it when needed.
    ///
    /// The workhorse of "open susnotes and paste something into today".
    pub fn append_to_note(&self, query: &str, text: &str) -> Result<Note> {
        let existing = self.resolve_name(query)?;
        let name = existing
            .as_ref()
            .map(|summary| summary.name.clone())
            .unwrap_or(NoteName::parse(query)?);
        let mut content = match existing {
            Some(summary) => self.read_summary(&summary)?.content,
            None => String::new(),
        };
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        if !content.is_empty() && !content.ends_with("\n\n") {
            content.push('\n');
        }
        content.push_str(text);
        // Keep the file ending with exactly one newline.
        let content = format!("{}\n", content.trim_end_matches('\n'));

        let note = self.write_note_content(&name, &content, false)?;
        self.bus.emit(Change::NoteUpdated {
            name: name.to_string(),
        });
        Ok(note)
    }

    /// Delete a note.
    pub fn delete_note(&self, query: &str) -> Result<String> {
        let summary = self
            .resolve_name(query)?
            .ok_or_else(|| Error::NoteNotFound(query.to_owned()))?;
        fs::remove_optional(&self.record_path(&summary.path)?)?;
        self.bus.emit(Change::NoteDeleted {
            name: summary.name.to_string(),
        });
        info!(note = %summary.name, "deleted a note");
        Ok(summary.name.to_string())
    }

    /// Rename a note, re-encrypting it under its new path.
    ///
    /// Because a record's encryption key is bound to its path, a rename is a
    /// decrypt-and-write rather than a file system move: the bytes are unreadable
    /// under the old name afterwards. When `rewrite_links` is set, other notes
    /// that link to the old name are updated in place.
    pub fn rename_note(&self, old: &str, new: &str, rewrite_links: bool) -> Result<RenameOutcome> {
        let target = NoteName::parse(new)?;
        let source = self
            .resolve_name(old)?
            .ok_or_else(|| Error::NoteNotFound(old.to_owned()))?;

        if target == source.name {
            return Ok(RenameOutcome {
                note: self.read_summary(&source)?,
                old: source.name.to_string(),
                new: source.name.to_string(),
                links_rewritten: 0,
            });
        }

        // A name belonging to a *different* note is taken. If the resolved name
        // is the source itself this is a case-only rename, which case insensitive
        // file systems would otherwise swallow.
        if let Some(clash) = self.resolve_name(target.as_str())?
            && clash.name != source.name
        {
            return Err(Error::AlreadyExists(clash.name.to_string()));
        }

        // Collect the links before anything moves: once the old file is gone, its
        // incoming links resolve as missing and would be invisible here.
        let graph = if rewrite_links {
            self.link_graph()?
        } else {
            LinkGraph::default()
        };

        let content = self.read_summary(&source)?.content;
        if source.name.key() == target.key() {
            // The same name, spelled differently: on a case insensitive file
            // system the two spellings are one directory entry, so the order
            // here matters more than it looks. Writing the target first, the way
            // an ordinary rename does, would put the new record on top of the
            // entry that is then moved aside and deleted, taking the note with
            // it. `replace_in_place` stages the new record first.
            let old_path = self.record_path(&source.path)?;
            self.replace_in_place(
                &old_path,
                &target.vault_path(),
                RecordKind::Note,
                content.as_bytes(),
            )?;
        } else {
            self.write_record(&target.vault_path(), RecordKind::Note, content.as_bytes())?;
            fs::remove_optional(&self.record_path(&source.path)?)?;
        }

        let links_rewritten = if rewrite_links {
            self.rewrite_links(&graph, &source.name, &target)?
        } else {
            0
        };

        self.bus.emit(Change::NoteRenamed {
            old: source.name.to_string(),
            new: target.to_string(),
            links_rewritten,
        });
        info!(from = %source.name, to = %target, links_rewritten, "renamed a note");

        Ok(RenameOutcome {
            note: self.read_note(target.as_str())?,
            old: source.name.to_string(),
            new: target.to_string(),
            links_rewritten,
        })
    }

    /// Rewrite every link that resolves to `old` so it points at `target`.
    ///
    /// `graph` must have been built while the old name still existed.
    fn rewrite_links(&self, graph: &LinkGraph, old: &NoteName, target: &NoteName) -> Result<usize> {
        let mut changed = 0;
        for note in self.list_notes()? {
            let relevant: Vec<&LinkEdge> = graph
                .links
                .iter()
                .filter(|link| link.from == note.name.as_str() && link.to == old.as_str())
                .collect();
            if relevant.is_empty() {
                continue;
            }
            let current = self.read_summary(&note)?;
            let mut content = current.content.clone();
            // The text that replaces a link and the range it replaces belong in
            // one package: two vectors sorted separately pair every replacement
            // with a different link, which writes the new text into the wrong
            // place. Rewriting from the back keeps earlier offsets valid, and
            // sorting the pairs together keeps each text with its own range.
            let mut rewrites: Vec<(std::ops::Range<usize>, String)> = relevant
                .iter()
                .map(|link| (link.range.clone(), new_link_text(link, target)))
                .collect();
            rewrites.sort_by_key(|rewrite| std::cmp::Reverse(rewrite.0.start));
            for (range, replacement) in rewrites {
                if range.end <= content.len() {
                    content.replace_range(range, &replacement);
                }
            }
            if content != current.content {
                self.write_record(&note.path, RecordKind::Note, content.as_bytes())?;
                changed += 1;
            }
        }
        Ok(changed)
    }

    /// Every link in the vault, in one pass over every note.
    pub fn link_graph(&self) -> Result<LinkGraph> {
        let notes = self.list_notes()?;
        let index = self.name_index()?;
        let mut graph = LinkGraph::default();

        for note in &notes {
            let content = match self.read_summary(note) {
                Ok(loaded) => loaded.content,
                Err(err) => {
                    warn!(note = %note.name, %err, "skipped a note while collecting links");
                    continue;
                }
            };
            let links = match parse_links(&content) {
                Ok(links) => links,
                Err(err) => {
                    warn!(note = %note.name, %err, "could not parse links");
                    continue;
                }
            };

            for link in links.notes {
                match Self::resolve_in(&index, &link.target)? {
                    Some(found) => graph.links.push(LinkEdge {
                        from: note.name.to_string(),
                        to: found.name.to_string(),
                        syntax: link.syntax,
                        snippet: line_at(&content, link.range.start),
                        range: link.range,
                        alias: link.alias,
                    }),
                    None => graph.missing.push(MissingLink {
                        from: note.name.to_string(),
                        target: link.target,
                        range: link.range,
                    }),
                }
            }
            for asset in links.assets {
                graph.assets.push((note.name.to_string(), asset.path));
            }
            for url in links.external {
                graph.external.push((note.name.to_string(), url));
            }
        }

        debug!(
            links = graph.links.len(),
            missing = graph.missing.len(),
            "link graph built"
        );
        Ok(graph)
    }

    /// Notes that link to `query`, with the line each link sits on.
    pub fn backlinks(&self, query: &str) -> Result<Vec<LinkEdge>> {
        let Some(found) = self.resolve_name(query)? else {
            return Ok(Vec::new());
        };
        let graph = self.link_graph()?;
        Ok(graph
            .links
            .iter()
            .filter(|link| link.to == found.name.as_str() && link.from != found.name.as_str())
            .cloned()
            .collect())
    }

    /// Links written inside `query`.
    pub fn outgoing_links(&self, query: &str) -> Result<Vec<LinkEdge>> {
        let Some(found) = self.resolve_name(query)? else {
            return Ok(Vec::new());
        };
        let graph = self.link_graph()?;
        Ok(graph
            .links
            .iter()
            .filter(|link| link.from == found.name.as_str())
            .cloned()
            .collect())
    }

    /// Search every note for `query`, best hits first.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let mut hits = Vec::new();
        for note in self.list_notes()? {
            let content = match self.read_summary(&note) {
                Ok(loaded) => loaded.content,
                Err(err) => {
                    warn!(note = %note.name, %err, "skipped a note while searching");
                    continue;
                }
            };
            if let Some(hit) = search::search_note(note.name.as_str(), &content, query) {
                hits.push(hit);
            }
        }
        hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.note.cmp(&b.note)));
        hits.truncate(limit);
        Ok(hits)
    }

    /// Journal pages, newest first, saying whether each one has content.
    ///
    /// Every journal page is decrypted to make that judgement: it is cheap
    /// (per note it is one HKDF and one AEAD, no password stretching) and it is
    /// exact, which matters because the journal is the app's home page.
    pub fn journal_pages(&self) -> Result<Vec<JournalPage>> {
        let mut pages: Vec<JournalPage> = Vec::new();
        for note in self
            .list_notes()?
            .into_iter()
            .filter(|note| note.is_journal)
        {
            let empty = match self.read_summary(&note) {
                Ok(content) => content.content.trim().is_empty(),
                Err(err) => {
                    warn!(note = %note.name, %err, "could not read a journal page");
                    true
                }
            };
            pages.push(JournalPage {
                name: note.name.to_string(),
                modified_at: note.modified_at,
                size: note.size,
                empty,
            });
        }
        pages.sort_by(|a, b| {
            crate::journal::parse_journal_name(&b.name)
                .cmp(&crate::journal::parse_journal_name(&a.name))
        });
        Ok(pages)
    }

    /// The journal page for today, created when it does not exist yet.
    pub fn open_today_journal(&self) -> Result<Note> {
        self.open_note(&crate::journal::journal_name(crate::journal::today()))
    }

    /// Store an image or attachment, returning the reference to use in a note.
    ///
    /// The file name carries a content tag, so pasting the same bytes twice
    /// reuses the existing file instead of growing the vault.
    pub fn add_asset(&self, original_name: &str, bytes: &[u8]) -> Result<AssetRef> {
        if bytes.is_empty() {
            return Err(Error::BadRequest("the file is empty".into()));
        }
        let tag = assets::hash_tag(bytes);
        let sanitized = assets::sanitize_file_name(original_name);
        let extension = sanitized
            .rsplit_once('.')
            .map(|(_, extension)| extension.to_owned())
            .unwrap_or_default();

        if let Some(existing) = assets::find_by_hash(&self.assets_dir(), &tag, &extension) {
            debug!(path = %existing, "asset already present, reusing it");
            return assets::asset_ref(&self.root, &existing);
        }

        let file_name = assets::asset_file_name(original_name, &tag);
        let rel_path = format!("{ASSETS_DIR}/{file_name}");
        self.write_record(&rel_path, RecordKind::Asset, bytes)?;
        self.bus.emit(Change::AssetAdded {
            path: rel_path.clone(),
        });
        info!(path = %rel_path, size = bytes.len(), "stored an asset");
        assets::asset_ref(&self.root, &rel_path)
    }

    /// Read an asset, decrypting it.
    pub fn read_asset(&self, rel_path: &str) -> Result<Asset> {
        let reference = assets::asset_ref(&self.root, rel_path)?;
        let (bytes, _) = self.read_record(&reference.path, RecordKind::Asset)?;
        Ok(Asset { reference, bytes })
    }

    /// Whether an asset path exists.
    #[must_use]
    pub fn asset_exists(&self, rel_path: &str) -> bool {
        assets::asset_ref(&self.root, rel_path).is_ok_and(|found| found.path == rel_path)
            || self.record_path(rel_path).is_ok_and(|path| path.is_file())
    }

    /// Every asset in the vault.
    pub fn list_assets(&self) -> Result<Vec<AssetRef>> {
        assets::list(&self.assets_dir())?
            .into_iter()
            .map(|path| assets::asset_ref(&self.root, &path))
            .collect()
    }

    /// Delete an asset.
    pub fn delete_asset(&self, rel_path: &str) -> Result<String> {
        let reference = assets::asset_ref(&self.root, rel_path)?;
        fs::remove_optional(&self.record_path(&reference.path)?)?;
        self.bus.emit(Change::AssetDeleted {
            path: reference.path.clone(),
        });
        Ok(reference.path)
    }

    /// Assets that no note links to, for a tidy-up pass.
    pub fn unused_assets(&self) -> Result<Vec<AssetRef>> {
        let graph = self.link_graph()?;
        let referenced: std::collections::HashSet<String> = graph
            .assets
            .iter()
            .map(|(_, path)| path.to_lowercase())
            .collect();
        Ok(self
            .list_assets()?
            .into_iter()
            .filter(|asset| !referenced.contains(&asset.path.to_lowercase()))
            .collect())
    }

    /// Change the vault password: rewrap the master key and write one file.
    ///
    /// Notes and assets keep their encryption, so this is safe to interrupt and
    /// instant even for a large vault.
    pub fn change_password(&self, current: &[u8], next: &[u8]) -> Result<()> {
        if next.is_empty() {
            return Err(Error::BadRequest("the new password is empty".into()));
        }
        let salt = self.meta.salt_bytes()?;
        let current_key = self.meta.kdf.derive_password_key(current, &salt)?;
        let master = crate::crypto::unwrap_master_key(&current_key, &self.meta.wrapped_key)?;
        if master.as_bytes() != self.master.as_bytes() {
            return Err(Error::WrongPassword);
        }

        let new_salt = crate::crypto::random_salt()?;
        let new_key = self.meta.kdf.derive_password_key(next, &new_salt)?;
        let mut meta = self.meta.clone();
        meta.salt = B64.encode(new_salt);
        meta.wrapped_key = crate::crypto::wrap_master_key(&new_key, &master)?;
        meta.store(&self.root)?;
        info!("changed the vault password");
        Ok(())
    }

    /// Write every note and asset out to `target` as plain files, keeping the
    /// folder layout. Used by the CLI's `export` for backups.
    pub fn export_plaintext(&self, target: &Path) -> Result<ExportSummary> {
        std::fs::create_dir_all(target).map_err(|err| Error::io("create folder", target, err))?;
        let mut summary = ExportSummary::default();

        // One set per export run: two notes whose names turn into the same file
        // name (`A:B` and `A-B`, or names that share their first 80 characters)
        // must not share a file, because an export is the one copy a user is
        // meant to be able to read without the vault, and silently writing one
        // note over another is data loss.
        let mut names = std::collections::HashSet::new();
        for note in self.list_notes()? {
            let content = self.read_summary(&note)?;
            let file_name = export_file_name(content.name.as_str(), &mut names);
            let out = target.join(PAGES_DIR).join(&file_name);
            fs::write_atomic(&out, content.content.as_bytes())?;
            summary.notes += 1;
        }
        for asset in self.list_assets()? {
            let decrypted = self.read_asset(&asset.path)?;
            let out = target.join(&asset.path);
            fs::write_atomic(&out, &decrypted.bytes)?;
            summary.assets += 1;
        }

        info!(
            notes = summary.notes,
            assets = summary.assets,
            "exported a plaintext copy"
        );
        Ok(summary)
    }

    /// Count notes, assets and bytes; shown in the status bar.
    pub fn stats(&self) -> Result<VaultStats> {
        let notes = self.list_notes()?;
        let assets = self.list_assets()?;
        let journal_pages = notes.iter().filter(|note| note.is_journal).count();
        let bytes: u64 = notes.iter().map(|note| note.size).sum::<u64>()
            + assets.iter().map(|asset| asset.size).sum::<u64>();
        Ok(VaultStats {
            notes_with_content: notes
                .iter()
                .filter(|note| {
                    note.size
                        > u64::from(
                            u32::try_from(crate::crypto::record_overhead(note.path.len()))
                                .unwrap_or(u32::MAX),
                        )
                })
                .count(),
            last_modified: notes.iter().map(|note| note.modified_at).max(),
            notes: notes.len(),
            journal_pages,
            assets: assets.len(),
            bytes,
        })
    }

    /// Report notes whose file cannot be decrypted with this key.
    ///
    /// A healthy vault has an empty list; anything else means files were moved
    /// in from another vault, and the app surfaces that instead of failing when
    /// the user opens one.
    pub fn verify(&self) -> Result<Vec<UnreadableRecord>> {
        let mut problems = Vec::new();
        for note in self.list_notes()? {
            if let Err(err) = self.read_summary(&note) {
                problems.push(UnreadableRecord {
                    path: note.path,
                    reason: err.to_string(),
                });
            }
        }
        for asset in self.list_assets()? {
            if let Err(err) = self.read_asset(&asset.path) {
                problems.push(UnreadableRecord {
                    path: asset.path,
                    reason: err.to_string(),
                });
            }
        }
        Ok(problems)
    }
}

/// What a rename changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameOutcome {
    /// The note under its new name.
    pub note: Note,
    /// Name before.
    pub old: String,
    /// Name after.
    pub new: String,
    /// How many notes had their links rewritten.
    pub links_rewritten: usize,
}

/// A journal page in a listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalPage {
    /// The date name, e.g. `2026-10-23`.
    pub name: String,
    /// Last modification time in milliseconds since the epoch.
    pub modified_at: i64,
    /// Size of the encrypted file.
    pub size: u64,
    /// Whether the page holds nothing.
    pub empty: bool,
}

/// A note or asset the current password cannot open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnreadableRecord {
    /// Path inside the vault.
    pub path: String,
    /// Why it failed.
    pub reason: String,
}

/// What an export produced.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportSummary {
    /// Notes written.
    pub notes: usize,
    /// Assets written.
    pub assets: usize,
}

/// A readable file name stem for an exported note: exported copies are plain
/// markdown, so they no longer need the vault's reversible escaping.
fn slug_file_name(name: &str) -> String {
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug
        .trim()
        .trim_matches(['.', '-'])
        .chars()
        .take(80)
        .collect::<String>();
    if slug.is_empty() {
        "note".to_owned()
    } else {
        slug
    }
}

/// A readable file name for an exported note, unique within one export run.
///
/// The note's own name is not usable outside the vault as it stands (it keeps
/// the vault's escaping), and two different names can collapse into the same
/// file name when the slug replaces characters or cuts the length at 80: `A:B`
/// and `A-B` are the same file. The first note to claim a slug keeps the plain
/// spelling, and every note after it gets the slug plus a hash of its own name;
/// the collision is logged, because an export is the copy a user is meant to
/// read without the vault, so silently overwriting a note would be data loss.
fn export_file_name(name: &str, taken: &mut std::collections::HashSet<String>) -> String {
    let stem = slug_file_name(name);
    let base = format!("{stem}.md");
    if taken.insert(base.clone()) {
        return base;
    }

    // A note whose slug is already claimed gets the slug plus a hash of its own
    // name, which is unique wherever the slug was not.
    let tag = crate::assets::hash_tag(name.as_bytes());
    let mut file_name = format!("{stem}-{tag}.md");
    if !taken.insert(file_name.clone()) {
        // The same slug and the same hash of the same name can only happen when
        // the names differ in something the slug drops, so a number is the only
        // thing left that can still be unique.
        let mut index = 1;
        loop {
            let candidate = format!("{stem}-{tag}-{index}.md");
            if taken.insert(candidate.clone()) {
                file_name = candidate;
                break;
            }
            index += 1;
        }
    }
    warn!(
        note = %name,
        file = %file_name.as_str(),
        "the export name was already used, wrote the note under another one"
    );
    file_name
}

/// The whole line containing `offset`, trimmed.
fn line_at(source: &str, offset: usize) -> String {
    let offset = offset.min(source.len());
    let start = source[..offset].rfind('\n').map_or(0, |index| index + 1);
    let end = source[offset..]
        .find('\n')
        .map_or(source.len(), |index| offset + index);
    let line = source[start..end].trim();
    if line.chars().count() <= 120 {
        return line.to_owned();
    }
    let mut out: String = line.chars().take(120).collect();
    out.push('…');
    out
}

/// The text a link should be rewritten to when its target is renamed.
fn new_link_text(link: &LinkEdge, target: &NoteName) -> String {
    let target_name = target.as_str();
    match link.syntax {
        // A hash link can only hold a name without spaces.
        LinkSyntax::Hash if !target_name.contains(' ') => format!("#{target_name}"),
        LinkSyntax::Wiki | LinkSyntax::Hash => match &link.alias {
            Some(alias) => format!("[[{target_name}|{alias}]]"),
            None => format!("[[{target_name}]]"),
        },
        LinkSyntax::Markdown => {
            let label = link.alias.clone().unwrap_or_else(|| target_name.to_owned());
            format!("[{label}]({})", crate::links::note_link_url(target_name))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ordering rule of an in place replacement, checked on a file system
    /// that is case sensitive: the entry that holds the note is moved out of the
    /// way before anything is put at the final path, so a case only rename on
    /// APFS, NTFS or FAT cannot destroy the only copy of the note. That is the
    /// case the ordering protects, and it is only observable through what the
    /// sequence leaves behind when its last step fails.
    #[test]
    fn a_replacement_stops_without_touching_the_final_path_if_the_move_is_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let vault =
            Vault::create(dir.path(), b"pw", crate::crypto::KdfParams::test_defaults()).unwrap();
        let pages = vault.pages_dir();

        let old = pages.join("hello world.md");
        fs::write_atomic(&old, b"the only copy").unwrap();
        // Something in the way of the final path: the staged record exists, the
        // old entry has been moved aside, and putting it in place is what fails.
        let final_path = pages.join("Hello World.md");
        std::fs::create_dir(&final_path).unwrap();

        let err = vault
            .replace_in_place(
                &old,
                "pages/Hello World.md",
                RecordKind::Note,
                b"the new content",
            )
            .unwrap_err();
        assert!(matches!(err, Error::Io { .. }), "{err}");

        // Nothing reached the final path, and the entry that held the note was
        // put back rather than left in a name the vault's listings do not read.
        assert!(
            std::fs::metadata(&final_path)
                .map(|m| m.is_dir())
                .unwrap_or(false),
            "something was written at {final_path:?}"
        );
        assert_eq!(std::fs::read(&old).unwrap(), b"the only copy");

        let leftovers: Vec<_> = std::fs::read_dir(&pages)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path() != old && entry.path() != final_path)
            .collect();
        assert!(
            leftovers.is_empty(),
            "the staged files were left behind: {leftovers:?}"
        );
    }

    /// The shape a case insensitive file system actually gives the code: one
    /// entry, one path, holding the old record before the rename and the new one
    /// afterwards. ext4 cannot put two spellings on one entry, so the same path
    /// stands in for the pair, and it is exactly the input the ordering has to
    /// survive: replace the record where it already is without losing it.
    #[test]
    fn a_record_replaced_where_it_already_is_keeps_its_content_and_its_place() {
        let dir = tempfile::tempdir().unwrap();
        let vault =
            Vault::create(dir.path(), b"pw", crate::crypto::KdfParams::test_defaults()).unwrap();
        vault.write_note("hello world", "the note").unwrap();
        let path = dir.path().join("pages/hello world.md");

        vault
            .replace_in_place(
                &path,
                "pages/hello world.md",
                RecordKind::Note,
                b"the note, rewritten",
            )
            .unwrap();

        let note = vault.read_note("Hello World").unwrap();
        assert_eq!(note.content, "the note, rewritten");
        assert_eq!(vault.list_notes().unwrap().len(), 1);
        assert_eq!(
            std::fs::read_dir(dir.path().join("pages")).unwrap().count(),
            1,
            "the record was left in a staged name"
        );
        // A case only spelling of the same name is still the same note, which is
        // what the ordering promised to keep true.
        assert_eq!(
            vault.read_note("HELLO WORLD").unwrap().name.as_str(),
            "hello world"
        );
    }
}
