//! Filesystem helpers: atomic writes, restrictive permissions, scanning.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use tracing::debug;

use crate::error::{Error, Result};

/// Write `bytes` to `path` atomically and create parent directories.
///
/// A crash or a full disk leaves either the previous content or the new
/// content in place, never a truncated file, which matters because an
/// encrypted record cannot be repaired by hand.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = folder_holding(path);
    fs::create_dir_all(&parent).map_err(|e| Error::io("create folder", &parent, e))?;

    let tmp = tmp_path(path);
    {
        let mut file = fs::File::create(&tmp).map_err(|e| Error::io("create", &tmp, e))?;
        #[cfg(unix)]
        {
            use std::fs::Permissions;
            use std::os::unix::fs::PermissionsExt as _;
            // Vault files are encrypted, but they still should not be world
            // readable on a shared machine.
            file.set_permissions(Permissions::from_mode(0o600))
                .map_err(|e| Error::io("set permissions", &tmp, e))?;
        }
        file.write_all(bytes)
            .map_err(|e| Error::io("write", &tmp, e))?;
        file.flush().map_err(|e| Error::io("flush", &tmp, e))?;
        file.sync_all().map_err(|e| Error::io("sync", &tmp, e))?;
    }

    fs::rename(&tmp, path).map_err(|e| Error::io("replace", path, e))?;
    // Sync the folder that holds the file, so the rename's directory entry
    // survives a power loss. Not its parent one level up, and not skipped
    // because the path happens to be relative: `pages/` is the entry that must
    // be durable, and that is the one folder the durability claim depends on.
    if let Err(err) = sync_dir(&parent) {
        // The write itself succeeded, so this only reports that the rename is
        // not known to be durable yet; it is not a failure of the vault.
        debug!(path = %path.display(), folder = %parent.display(), %err,
            "the folder holding a record could not be synced");
    }
    Ok(())
}

/// The folder whose directory entry has to be synced for `path` to be visible:
/// the folder the file sits in, never a folder above it.
///
/// Split out because the difference between `pages/` and its parent is exactly
/// the mistake this used to make, and a relative path has no parent to fall
/// back on.
fn folder_holding(path: &Path) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        // `file.md`, or a path that starts out as `.`, means the current folder.
        _ => PathBuf::from("."),
    }
}

/// Best-effort directory sync; ignored on platforms without a portable API.
pub fn sync_dir(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        fs::File::open(dir)
            .and_then(|file| file.sync_all())
            .map_err(|e| Error::io("sync folder", dir, e))
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

fn tmp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "record".to_owned());
    let tmp_dir = folder_holding(path);
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    tmp_dir.join(format!(".{name}.{unique:08x}.tmp"))
}

/// A temporary path in the same folder as `path`, named so that the vault's own
/// listings never count it as a note or an asset.
///
/// Used where a record has to be written before the entry it is going to
/// replace exists, which is what makes a rename safe on a case insensitive file
/// system (see `vault::rename_note`).
pub fn staging_path(path: &Path, tag: &str) -> PathBuf {
    let folder = folder_holding(path);
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "record".to_owned());
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    folder.join(format!(".{name}.susnotes-{tag}-{unique:08x}"))
}

/// Read a file, mapping a missing file to `None` instead of an error.
pub fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(Error::io("read", path, err)),
    }
}

/// Read a file or fail with a describing error.
pub fn read(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).map_err(|e| Error::io("read", path, e))
}

/// Remove a file, treating "already gone" as success.
pub fn remove_optional(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(Error::io("delete", path, err)),
    }
}

/// Milliseconds since the Unix epoch, used for log names and asset ids.
#[must_use]
pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_creates_parents_and_replaces_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deep/nested/file.md");
        write_atomic(&path, b"first").unwrap();
        assert_eq!(read(&path).unwrap(), b"first");
        write_atomic(&path, b"second").unwrap();
        assert_eq!(read(&path).unwrap(), b"second");

        let leftovers = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().starts_with('.'))
            .count();
        assert_eq!(leftovers, 0, "temporary files must not be left behind");
    }

    #[test]
    fn read_optional_reports_missing_files_as_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_optional(&dir.path().join("nope")).unwrap().is_none());
    }

    #[test]
    fn a_replace_syncs_the_folder_that_holds_the_file_not_the_one_above_it() {
        // This is the whole durability claim: `pages/x.md` becomes durable with
        // `pages/`, not with the vault root, and a relative path has no parent
        // one level up to reach for instead.
        assert_eq!(
            folder_holding(Path::new("/v/pages/x.md")),
            PathBuf::from("/v/pages")
        );
        assert_eq!(
            folder_holding(Path::new("pages/x.md")),
            PathBuf::from("pages")
        );
        assert_eq!(folder_holding(Path::new("x.md")), PathBuf::from("."));
        assert_eq!(folder_holding(Path::new("./x.md")), PathBuf::from("."));
    }

    #[test]
    fn a_staged_file_shares_the_folder_and_is_never_counted_as_a_record() {
        let staged = staging_path(Path::new("/v/pages/Hello World.md"), "new");
        let file_name = staged.file_name().unwrap().to_string_lossy().into_owned();
        // The staging file has to sit next to the record it replaces, or the
        // final rename would be a cross-device move rather than a rename.
        assert_eq!(staged.parent(), Some(Path::new("/v/pages")));
        assert!(file_name.starts_with('.'), "{file_name} is not hidden");
        assert!(
            !crate::names::is_note_file_name(&file_name),
            "{file_name} would be counted as a note"
        );
    }

    #[cfg(unix)]
    #[test]
    fn written_files_are_not_world_readable() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret");
        write_atomic(&path, b"x").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "permissions were {mode:#o}");
    }
}
