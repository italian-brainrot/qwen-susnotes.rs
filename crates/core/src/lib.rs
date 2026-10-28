//! susnotes-core: the encrypted vault, its notes, links and settings.
//!
//! This crate has no UI dependency of any kind. The desktop app, the CLI and
//! the HTTP API are all thin layers over it, which is what makes the vault an
//! honest, scriptable data store rather than a private format inside a GUI.
//!
//! ## Reading order
//!
//! * [`vault`] — opening, listing and editing notes and assets
//! * [`crypto`] — the on-disk record format and the password envelope
//! * [`names`] and [`journal`] — how a note name becomes a file name
//! * [`links`] — wiki links, backlinks, and the rewrite the renderer needs
//! * [`blocks`] — splitting a note into the pieces the renderer shows
//! * [`config`] — settings, recent vaults
//! * [`api_types`] — the shapes the local HTTP API speaks
//!
//! ## A small tour
//!
//! ```no_run
//! use susnotes_core::crypto::KdfParams;
//! use susnotes_core::vault::Vault;
//!
//! let vault = Vault::create("/tmp/my-vault", b"a strong password", KdfParams::default())?;
//! vault.append_to_note("2026-10-23", "pasted a thought")?;
//! let journal = vault.read_note("2026-10-23")?;
//! assert!(journal.content.contains("pasted a thought"));
//! # Ok::<(), susnotes_core::Error>(())
//! ```

pub mod api_types;
pub mod assets;
pub mod blocks;
pub mod change;
pub mod config;
pub mod crypto;
pub mod error;
pub mod fs;
pub mod journal;
pub mod links;
pub mod logging;
pub mod names;
pub mod search;
pub mod vault;

pub use change::{Change, ChangeBus};
pub use config::Settings;
pub use error::{Error, Result};
pub use names::NoteName;
pub use vault::{Note, NoteSummary, UnlockRequest, Vault, VaultStats};

/// The name every susnotes binary reports in `--version`.
pub const APP_NAME: &str = "susnotes";

/// Version of this crate, which doubles as the vault format's producer tag.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where a note's markdown lives, given its display name.
#[must_use]
pub fn note_path_for(name: &str) -> String {
    format!("{}/{}", vault::PAGES_DIR, names::file_name_for(name))
}
