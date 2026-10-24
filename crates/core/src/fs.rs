//! Filesystem helpers: atomic writes, restrictive permissions, scanning.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Write `bytes` to `path` atomically and create parent directories.
///
/// A crash or a full disk leaves either the previous content or the new
/// content in place, never a truncated file, which matters because an
/// encrypted record cannot be repaired by hand.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|e| Error::io("create folder", parent, e))?;

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
    // Sync the directory entry so the rename itself survives a power loss.
    if let Some(parent) = parent
        .parent()
        .or(Some(parent))
        .filter(|p| !p.as_os_str().is_empty())
    {
        let _ = sync_dir(parent);
    }
    Ok(())
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
    let tmp_dir = path.parent().unwrap_or_else(|| Path::new("."));
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    tmp_dir.join(format!(".{name}.{unique:08x}.tmp"))
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
