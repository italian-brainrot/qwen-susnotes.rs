//! User settings, stored per vault in `config/settings.json`.
//!
//! Settings live inside the vault so a vault moved to another machine carries
//! its appearance and API configuration with it. Unknown fields are preserved
//! by ignoring them rather than rejecting the file, so an older build can open
//! a vault configured by a newer one.

use std::path::Path;

use base64::prelude::{BASE64_STANDARD as B64, Engine as _};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::fs;

/// File holding settings inside `config/`.
pub const SETTINGS_FILE: &str = "settings.json";

/// Which colour scheme the app follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    /// Follow the operating system.
    #[default]
    System,
    /// Always light.
    Light,
    /// Always dark.
    Dark,
}

impl std::fmt::Display for ThemeMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        })
    }
}

impl ThemeMode {
    /// Parse a theme name, ignoring case; unknown names fall back to system.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "light" => Self::Light,
            "dark" => Self::Dark,
            _ => Self::System,
        }
    }
}

/// Which page the app shows when a vault unlocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartPage {
    /// Today's journal page, the specification's default.
    #[default]
    Journal,
    /// The last note that was open.
    LastNote,
    /// The list of all notes.
    Notes,
}

/// How the note body opens when a note is clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenMode {
    /// Rendered markdown; clicking the body edits it.
    #[default]
    View,
    /// Markdown source straight into the editor.
    Edit,
}

/// Settings for the local, loopback-only HTTP API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiSettings {
    /// Whether the API listens at all.
    pub enabled: bool,
    /// Port to bind on 127.0.0.1; `0` asks the operating system for a free one.
    pub port: u16,
    /// Bearer token clients must present. Generated once and kept here.
    pub token: String,
}

impl Default for ApiSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            port: default_api_port(),
            token: String::new(),
        }
    }
}

impl ApiSettings {
    /// Whether the API needs a token at all.
    #[must_use]
    pub fn requires_token(&self) -> bool {
        !self.token.is_empty()
    }
}

/// The default port, chosen high enough to avoid clashing with common dev
/// servers and low enough to remember.
#[must_use]
pub fn default_api_port() -> u16 {
    47117
}

/// Everything a susnotes vault stores about how it should be shown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Colour scheme.
    pub theme: ThemeMode,
    /// Body text size in pixels.
    pub font_size: f32,
    /// Interface density: smaller paddings and controls.
    pub compact_ui: bool,
    /// Milliseconds of typing before the note is written to disk.
    pub autosave_ms: u64,
    /// Page shown when the vault unlocks.
    pub start_page: StartPage,
    /// How notes open when they are clicked.
    pub open_mode: OpenMode,
    /// Show the line of the note a link points to in the preview pane.
    pub link_previews: bool,
    /// Wrap code blocks instead of scrolling them sideways.
    pub wrap_code: bool,
    /// Keep a copy of every note as plain markdown under `backup/` inside the
    /// vault, so a lost password never means lost notes.
    pub plaintext_backup: bool,
    /// Onboarding screens the user has already dismissed.
    pub seen_tips: Vec<String>,
    /// The local HTTP API.
    pub api: ApiSettings,
    /// Settings that this build does not know about, kept verbatim so a newer
    /// build does not lose them.
    #[serde(flatten)]
    pub unknown: serde_json::Map<String, serde_json::Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeMode::default(),
            font_size: 15.5,
            compact_ui: false,
            autosave_ms: 800,
            start_page: StartPage::default(),
            open_mode: OpenMode::default(),
            link_previews: true,
            wrap_code: false,
            plaintext_backup: false,
            seen_tips: Vec::new(),
            api: ApiSettings::default(),
            unknown: serde_json::Map::new(),
        }
    }
}

impl Settings {
    /// Read `config/settings.json`, or defaults when it is missing or damaged.
    ///
    /// A damaged settings file must never stop a vault from opening, so the
    /// problem is logged and defaults are used.
    pub fn load(config_dir: &Path) -> Result<Self> {
        let path = config_dir.join(SETTINGS_FILE);
        let Some(bytes) = fs::read_optional(&path)? else {
            return Ok(Self::default());
        };
        match serde_json::from_slice(&bytes) {
            Ok(settings) => Ok(settings),
            Err(err) => {
                tracing::warn!(path = %path.display(), %err, "settings file is unreadable, using defaults");
                Ok(Self::default())
            }
        }
    }

    /// Write `config/settings.json` atomically with owner-only permissions.
    pub fn store(&self, config_dir: &Path) -> Result<()> {
        let path = config_dir.join(SETTINGS_FILE);
        let json = serde_json::to_vec_pretty(self).map_err(|err| Error::Config {
            file: path.clone(),
            reason: err.to_string(),
        })?;
        fs::write_atomic(&path, &json)
    }

    /// Produce an API token when there is none, so the first unlock of a vault
    /// has a working API without any setup.
    pub fn ensure_api_token(&mut self) -> Result<bool> {
        if !self.api.token.is_empty() {
            return Ok(false);
        }
        let mut bytes = [0u8; 24];
        getrandom::fill(&mut bytes)
            .map_err(|err| Error::Crypto(format!("randomness failed: {err}")))?;
        self.api.token = B64.encode(bytes);
        Ok(true)
    }

    /// Clamp values that a hand-edited settings file could push out of range.
    pub fn normalize(&mut self) {
        self.font_size = self.font_size.clamp(10.0, 32.0);
        self.autosave_ms = self.autosave_ms.clamp(100, 10_000);
        if self.api.port == 0 {
            self.api.port = default_api_port();
        }
    }
}

/// Recently opened vaults, remembered outside the vault so the app can offer
/// them before any folder is chosen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentVault {
    /// Absolute path of the vault folder.
    pub path: String,
    /// Folder name, for display.
    pub title: String,
    /// When it was last opened, in milliseconds since the epoch.
    pub opened_at: i64,
}

/// Where the app keeps its own state (a small folder in the user config dir).
#[must_use]
pub fn state_dir() -> std::path::PathBuf {
    match std::env::var_os("SUSNOTS_STATE_DIR") {
        Some(dir) => std::path::PathBuf::from(dir),
        None => {
            let config = std::env::var_os("XDG_CONFIG_HOME")
                .map(std::path::PathBuf::from)
                .filter(|path| path.is_absolute())
                .or_else(|| {
                    std::env::var_os("HOME")
                        .map(|home| std::path::PathBuf::from(home).join(".config"))
                })
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            config.join("susnotes")
        }
    }
}

/// Read the recent vault list, newest first, from the default state folder.
#[must_use]
pub fn recent_vaults() -> Vec<RecentVault> {
    recent_vaults_in(&state_dir())
}

/// Read the recent vault list from a specific state folder.
#[must_use]
pub fn recent_vaults_in(dir: &Path) -> Vec<RecentVault> {
    let path = dir.join("recent-vaults.json");
    fs::read_optional(&path)
        .ok()
        .flatten()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Remember that a vault was opened, in the default state folder.
pub fn remember_vault(vault: &Path) -> Result<()> {
    remember_vault_in(&state_dir(), vault)
}

/// Remember that a vault was opened, keeping at most ten entries.
pub fn remember_vault_in(dir: &Path, vault: &Path) -> Result<()> {
    let mut recent = recent_vaults_in(dir);
    let entry_path = vault.to_string_lossy().into_owned();
    recent.retain(|vault| vault.path != entry_path);
    recent.insert(
        0,
        RecentVault {
            title: vault
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| entry_path.clone()),
            path: entry_path,
            opened_at: crate::assets::now_millis(),
        },
    );
    recent.truncate(10);

    std::fs::create_dir_all(dir).map_err(|err| Error::io("create folder", dir, err))?;
    let json = serde_json::to_vec_pretty(&recent).map_err(|err| Error::Config {
        file: dir.to_path_buf(),
        reason: err.to_string(),
    })?;
    fs::write_atomic(&dir.join("recent-vaults.json"), &json)
}

/// Remove a vault from the recent list, in the default state folder.
pub fn forget_vault(vault: &Path) -> Result<()> {
    forget_vault_in(&state_dir(), vault)
}

/// Remove a vault from the recent list, e.g. after a folder was deleted.
pub fn forget_vault_in(dir: &Path, vault: &Path) -> Result<()> {
    let entry_path = vault.to_string_lossy().into_owned();
    let mut recent = recent_vaults_in(dir);
    recent.retain(|vault| vault.path != entry_path);
    if std::fs::create_dir_all(dir).is_err() {
        return Ok(());
    }
    let json = serde_json::to_vec_pretty(&recent).map_err(|err| Error::Config {
        file: dir.to_path_buf(),
        reason: err.to_string(),
    })?;
    fs::write_atomic(&dir.join("recent-vaults.json"), &json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_keep_unknown_fields() {
        let dir = tempfile::tempdir().unwrap();
        let settings = Settings {
            theme: ThemeMode::Dark,
            font_size: 17.0,
            api: ApiSettings {
                token: "tok".into(),
                ..ApiSettings::default()
            },
            ..Settings::default()
        };
        settings.store(dir.path()).unwrap();

        let raw = std::fs::read_to_string(dir.path().join(SETTINGS_FILE)).unwrap();
        assert!(raw.contains("\"theme\": \"dark\""));

        // A field this build does not know about must not break loading.
        let mut value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        value["future_option"] = serde_json::json!(42);
        std::fs::write(dir.path().join(SETTINGS_FILE), value.to_string()).unwrap();
        let loaded = Settings::load(dir.path()).unwrap();
        assert_eq!(loaded.theme, ThemeMode::Dark);
        assert_eq!(loaded.font_size, 17.0);
        assert_eq!(
            loaded.unknown.get("future_option").and_then(|v| v.as_i64()),
            Some(42)
        );
    }

    #[test]
    fn a_broken_settings_file_still_opens_the_vault() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(SETTINGS_FILE), b"{ not json").unwrap();
        let loaded = Settings::load(dir.path()).unwrap();
        assert_eq!(loaded, Settings::default());
    }

    #[test]
    fn missing_settings_give_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Settings::load(dir.path()).unwrap(), Settings::default());
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let mut settings = Settings {
            font_size: 400.0,
            autosave_ms: 1,
            ..Settings::default()
        };
        settings.normalize();
        assert!((10.0..=32.0).contains(&settings.font_size));
        assert!(settings.autosave_ms >= 100);
    }

    #[test]
    fn api_tokens_are_generated_once_and_random() {
        let mut settings = Settings::default();
        assert!(settings.api.token.is_empty());
        assert!(settings.ensure_api_token().unwrap());
        let first = settings.api.token.clone();
        assert!(!first.is_empty());
        assert!(!settings.ensure_api_token().unwrap());
        assert_eq!(settings.api.token, first);

        let mut other = Settings::default();
        other.ensure_api_token().unwrap();
        assert_ne!(other.api.token, first);
    }

    #[test]
    fn theme_names_round_trip() {
        for (name, mode) in [
            ("light", ThemeMode::Light),
            ("Dark", ThemeMode::Dark),
            ("anything", ThemeMode::System),
        ] {
            assert_eq!(ThemeMode::parse(name), mode);
        }
    }

    #[test]
    fn recent_vaults_are_deduped_and_capped() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..14 {
            remember_vault_in(
                dir.path(),
                std::path::Path::new(&format!("/tmp/vault-{index}")),
            )
            .unwrap();
        }
        let recent = recent_vaults_in(dir.path());
        assert_eq!(recent.len(), 10);
        assert_eq!(recent[0].path, "/tmp/vault-13");

        remember_vault_in(dir.path(), std::path::Path::new("/tmp/vault-10")).unwrap();
        let recent = recent_vaults_in(dir.path());
        assert_eq!(recent[0].path, "/tmp/vault-10");
        assert_eq!(
            recent
                .iter()
                .filter(|vault| vault.path == "/tmp/vault-10")
                .count(),
            1
        );

        forget_vault_in(dir.path(), std::path::Path::new("/tmp/vault-10")).unwrap();
        assert_eq!(recent_vaults_in(dir.path()).len(), 9);
    }
}
