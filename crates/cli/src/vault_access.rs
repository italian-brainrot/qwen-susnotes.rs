//! Finding the vault and getting it unlocked.
//!
//! The resolution order is written down once, here, and every command uses it:
//!
//! 1. `--vault <folder>`
//! 2. `SUSNOTS_VAULT`
//! 3. the most recently opened vault
//!
//! Passwords, the other half of the same problem:
//!
//! 1. `--password`
//! 2. `SUSNOTS_PASSWORD`
//! 3. a prompt, and only when stdin is a terminal
//!
//! A password on the command line is visible to every process on the machine, so
//! it is documented as the last resort rather than the convenient one.

use std::path::{Path, PathBuf};

use susnotes_core::{
    Error, Result, UnlockRequest, Vault,
    config::{RecentVault, forget_vault_in, recent_vaults_in, remember_vault_in},
};

use crate::{
    args::Cli,
    context::{Context, prompt_password},
};

/// An unlocked vault plus the folder it came from.
pub struct Session {
    pub vault: Vault,
    pub path: PathBuf,
}

impl Session {
    #[must_use]
    pub fn vault(&self) -> &Vault {
        &self.vault
    }
}

/// Where the app keeps its own state, honouring `--state-dir` and
/// `SUSNOTS_STATE_DIR` so tests can point it at a temporary folder.
fn state_dir(context: &Context) -> PathBuf {
    context
        .state_dir
        .clone()
        .unwrap_or_else(susnotes_core::config::state_dir)
}

/// The folder to work on, following the order above.
pub fn resolve_vault_path(cli: &Cli, context: &Context) -> Result<PathBuf> {
    if let Some(path) = &cli.vault {
        return Ok(path.clone());
    }
    if let Some(from_env) = std::env::var_os("SUSNOTS_VAULT") {
        return Ok(PathBuf::from(from_env));
    }
    let recent = recent_vaults_in(&state_dir(context));
    if let Some(vault) = recent.first() {
        return Ok(PathBuf::from(&vault.path));
    }
    Err(Error::NotAVault {
        path: state_dir(context),
    })
}

/// Open and unlock the vault the user meant.
pub fn open(cli: &Cli, context: &mut Context) -> Result<Session> {
    let path = resolve_vault_path(cli, context)?;
    if !path.is_dir() {
        // The folder is gone: a moved or deleted vault. Drop it from the recent
        // list so the next command does not keep resolving to the same dead path.
        forget(context, &path);
        return Err(Error::NotAVault { path });
    }
    if !Vault::is_vault(&path) {
        return Err(Error::NotAVault { path });
    }
    let request = Vault::open(&path)?;
    let password = resolve_password(cli, context, &request, None)?;
    let vault = request.unlock(password.as_bytes())?;

    // Remember the folder only after a successful unlock, so a typo does not
    // become the "most recent vault" the next command picks up.
    remember(context, &vault);
    Ok(Session { vault, path })
}

/// Resolve the password, or ask for it.
///
/// `second_prompt_for` is used by `password change`, which must ask twice for the
/// new password.
pub fn resolve_password(
    cli: &Cli,
    context: &mut Context,
    request: &UnlockRequest,
    second_prompt_for: Option<&str>,
) -> Result<String> {
    if let Some(password) = &cli.password {
        return Ok(password.clone());
    }
    if let Some(from_env) = std::env::var_os("SUSNOTS_PASSWORD") {
        let password = from_env.to_string_lossy().into_owned();
        if password.is_empty() {
            return Err(Error::BadRequest(
                "SUSNOTS_PASSWORD is set but empty".into(),
            ));
        }
        return Ok(password);
    }
    // `second_prompt_for` only changes how the prompt reads, e.g. "new password".
    let label = match second_prompt_for {
        Some(what) => format!("{what} for {}", request.title()),
        None => format!("password for {}", request.title()),
    };
    prompt_password(context, &label)
}

/// Ask for a new password twice, so a mistyped one cannot lock a vault.
pub fn prompt_new_password(context: &mut Context, title: &str) -> Result<String> {
    let first = prompt_password(context, &format!("new password for {title}"))?;
    let again = prompt_password(context, &format!("new password for {title} (again)"))?;
    if first != again {
        return Err(Error::BadRequest("the two passwords did not match".into()));
    }
    Ok(first)
}

/// Record the vault as recently opened, so the next command can find it without
/// `--vault`.
pub fn remember(context: &Context, vault: &Vault) {
    let dir = state_dir(context);
    if let Err(err) = remember_vault_in(&dir, vault.root()) {
        // Forgetting to remember a vault is never a reason to fail a command.
        tracing::warn!(error = %err, "could not update the recent vault list");
    }
}

/// Drop a vault from the recent list, e.g. after its folder disappeared.
pub fn forget(context: &Context, path: &Path) {
    let dir = state_dir(context);
    if let Err(err) = forget_vault_in(&dir, path) {
        tracing::warn!(error = %err, "could not update the recent vault list");
    }
}

#[must_use]
pub fn recent(context: &Context) -> Vec<RecentVault> {
    recent_vaults_in(&state_dir(context))
}
