//! Assets: images and attachments stored next to the notes.
//!
//! Asset file names carry a short content tag (`diagram-1a2b3c4d.png`), which
//! is what makes repeated pastes of the same image collapse into one file and
//! keeps links stable when a file is replaced by a different image.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// Folder holding images and attachments.
pub const ASSETS_DIR: &str = "assets";

/// How an attachment behaves when it is opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    /// Rendered inline in the note.
    Image,
    /// Opened by the operating system's viewer.
    Pdf,
    /// Opened by the operating system's player.
    Audio,
    /// Opened by the operating system's player.
    Video,
    /// Opened by the operating system's archive tool.
    Archive,
    /// Plain text or source; safe to show in the app.
    Text,
    /// Anything else: handed to the operating system.
    Other,
}

impl AssetKind {
    /// The name used in the API and in `susnotes-cli assets --json`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Pdf => "pdf",
            Self::Audio => "audio",
            Self::Video => "video",
            Self::Archive => "archive",
            Self::Text => "text",
            Self::Other => "other",
        }
    }

    /// Whether the app should try to open this itself instead of rendering it.
    #[must_use]
    pub fn opens_externally(self) -> bool {
        !matches!(self, Self::Image)
    }
}

/// A file in the assets folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetRef {
    /// Path relative to the vault root, e.g. `assets/diagram-1a2b3c4d.png`.
    pub path: String,
    /// File name without the folder.
    pub name: String,
    /// MIME type guessed from the extension.
    pub mime: String,
    /// What kind of handling the file needs.
    pub kind: AssetKind,
    /// Size of the encrypted file on disk, in bytes.
    pub size: u64,
    /// Short content tag embedded in the file name.
    pub hash_tag: String,
    /// Last modification time, in milliseconds since the epoch.
    pub modified_at: i64,
    /// Creation time when the file system reports one.
    pub created_at: Option<i64>,
}

/// An asset together with its decrypted bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    /// Metadata of the asset.
    pub reference: AssetRef,
    /// Decrypted contents.
    pub bytes: Vec<u8>,
}

impl Asset {
    /// Whether the asset is an image.
    #[must_use]
    pub fn is_image(&self) -> bool {
        self.reference.kind == AssetKind::Image
    }
}

/// Convert a system time into milliseconds since the epoch.
#[must_use]
pub fn time_millis(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as i64)
        .unwrap_or_default()
}

/// Milliseconds since the epoch, right now.
#[must_use]
pub fn now_millis() -> i64 {
    time_millis(SystemTime::now())
}

/// First eight hex digits of the SHA-256 of the contents.
#[must_use]
pub fn hash_tag(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest
        .iter()
        .take(4)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Sanitise a file name coming from outside the vault (a pasted screenshot, a
/// dropped PDF) so it is safe and readable inside `assets/`.
#[must_use]
pub fn sanitize_file_name(original: &str) -> String {
    let original = original
        .trim()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(original);
    let (stem, extension) = match original.rsplit_once('.') {
        Some((stem, extension))
            if !stem.is_empty()
                && !extension.is_empty()
                && extension.len() <= 8
                && extension.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            (stem, extension.to_ascii_lowercase())
        }
        _ => (original, String::new()),
    };

    let mut stem: String = stem
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | ' ' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    while stem.contains("  ") {
        stem = stem.replace("  ", " ");
    }
    let stem = stem
        .trim()
        .trim_matches(['.', '-'])
        .chars()
        .take(60)
        .collect::<String>();
    let stem = if stem.is_empty() {
        "file".to_owned()
    } else {
        stem
    };

    if extension.is_empty() {
        stem
    } else {
        format!("{stem}.{extension}")
    }
}

/// The file name for a new asset: sanitized name plus its content tag.
#[must_use]
pub fn asset_file_name(original: &str, tag: &str) -> String {
    let sanitized = sanitize_file_name(original);
    match sanitized.rsplit_once('.') {
        Some((stem, extension)) => format!("{stem}-{tag}.{extension}"),
        None => format!("{sanitized}-{tag}"),
    }
}

/// Guess a MIME type from a file name, with a few explicit overrides for types
/// `mime_guess` gets wrong or leaves empty.
#[must_use]
pub fn mime_for(file_name: &str) -> String {
    let guessed = mime_guess::from_path(file_name)
        .first()
        .map(|mime| mime.essence_str().to_owned())
        .unwrap_or_default();
    if !guessed.is_empty() {
        return guessed;
    }
    match file_name
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "md" => "text/markdown".to_owned(),
        "svg" => "image/svg+xml".to_owned(),
        "avif" => "image/avif".to_owned(),
        "heic" => "image/heic".to_owned(),
        "webm" => "video/webm".to_owned(),
        _ => "application/octet-stream".to_owned(),
    }
}

/// Classify an asset by MIME type and extension.
#[must_use]
pub fn classify(file_name: &str, mime: &str) -> AssetKind {
    let extension = file_name
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if mime.starts_with("image/")
        || matches!(
            extension.as_str(),
            "png"
                | "jpg"
                | "jpeg"
                | "gif"
                | "webp"
                | "svg"
                | "bmp"
                | "avif"
                | "tif"
                | "tiff"
                | "ico"
                | "heic"
        )
    {
        return AssetKind::Image;
    }
    if extension == "pdf" || mime == "application/pdf" {
        return AssetKind::Pdf;
    }
    if mime.starts_with("audio/") {
        return AssetKind::Audio;
    }
    if mime.starts_with("video/") {
        return AssetKind::Video;
    }
    if mime.starts_with("text/")
        || matches!(
            extension.as_str(),
            "txt" | "csv" | "json" | "rs" | "py" | "js" | "toml" | "log" | "html"
        )
    {
        return AssetKind::Text;
    }
    if matches!(
        extension.as_str(),
        "zip" | "tar" | "gz" | "bz2" | "xz" | "7z" | "rar"
    ) || mime.ends_with("zip")
        || mime.ends_with("gzip")
    {
        return AssetKind::Archive;
    }
    AssetKind::Other
}

/// Build an [`AssetRef`] from a file that already exists under `assets/`.
pub fn asset_ref(root: &Path, rel_path: &str) -> Result<AssetRef> {
    let rel_path = crate::names::normalize_vault_path(rel_path)?;
    if !rel_path.starts_with(&format!("{ASSETS_DIR}/")) {
        return Err(Error::BadRequest(format!(
            "{rel_path} is not inside {ASSETS_DIR}/"
        )));
    }
    let path = root.join(&rel_path);
    let metadata = std::fs::metadata(&path).map_err(|err| Error::io("stat asset", &path, err))?;
    if !metadata.is_file() {
        return Err(Error::AssetNotFound(rel_path));
    }

    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mime = mime_for(&name);
    let hash_tag = name
        .rsplit_once('-')
        .and_then(|(_, tag)| tag.split('.').next())
        .filter(|tag| tag.len() == 8 && tag.chars().all(|c| c.is_ascii_hexdigit()))
        .unwrap_or_default()
        .to_owned();

    Ok(AssetRef {
        path: rel_path,
        modified_at: metadata.modified().map(time_millis).unwrap_or_default(),
        created_at: metadata.created().ok().map(time_millis),
        hash_tag,
        kind: classify(&name, &mime),
        mime: mime.clone(),
        name,
        size: metadata.len(),
    })
}

/// Every file under `assets/`, sorted by path.
pub fn list(assets_dir: &Path) -> Result<Vec<String>> {
    if !assets_dir.exists() {
        return Ok(Vec::new());
    }
    let mut paths: Vec<String> = walkdir::WalkDir::new(assets_dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with(['.', '_']))
        .filter_map(|entry| {
            let relative = entry
                .path()
                .strip_prefix(assets_dir.parent().unwrap_or(assets_dir))
                .ok()?
                .to_string_lossy()
                .replace('\\', "/");
            Some(relative)
        })
        .collect();
    paths.sort();
    Ok(paths)
}

/// Find an existing file whose content tag matches, so an identical paste does
/// not create a second copy.
#[must_use]
pub fn find_by_hash(assets_dir: &Path, tag: &str, extension: &str) -> Option<String> {
    let suffix = if extension.is_empty() {
        format!("-{tag}")
    } else {
        format!("-{tag}.{extension}")
    };
    let entries = std::fs::read_dir(assets_dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.len() >= suffix.len() && name.ends_with(&suffix) {
            return Some(format!("{ASSETS_DIR}/{name}"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasted_names_become_safe_file_names() {
        let cases = [
            (
                "Screenshot from 2026-10-23 at 14.32.11.png",
                "Screenshot from 2026-10-23 at 14.32.11.png",
            ),
            (
                "/home/user/Downloads/Quarterly Report (final).PDF",
                "Quarterly Report -final.pdf",
            ),
            ("../../etc/passwd", "passwd"),
            ("   ", "file"),
            ("weird\u{1f600}name.png", "weird-name.png"),
            ("archive.tar.gz", "archive.tar.gz"),
        ];
        for (input, expected) in cases {
            assert_eq!(sanitize_file_name(input), expected, "sanitising {input:?}");
        }
    }

    #[test]
    fn content_tag_makes_names_unique_and_stable() {
        let a = asset_file_name("photo.png", &hash_tag(b"one".as_slice()));
        let b = asset_file_name("photo.png", &hash_tag(b"two".as_slice()));
        let same = asset_file_name("other name.png", &hash_tag(b"one".as_slice()));
        assert_ne!(a, b);
        assert!(same.starts_with("other name-"));
        assert_eq!(
            same.rsplit_once('-').unwrap().1,
            a.rsplit_once('-').unwrap().1
        );
    }

    #[test]
    fn classification_covers_the_common_kinds() {
        assert_eq!(classify("a.png", "image/png"), AssetKind::Image);
        assert_eq!(classify("a.SVG", ""), AssetKind::Image);
        assert_eq!(classify("contract.pdf", "application/pdf"), AssetKind::Pdf);
        assert_eq!(classify("clip.mp3", "audio/mpeg"), AssetKind::Audio);
        assert_eq!(classify("movie.mp4", "video/mp4"), AssetKind::Video);
        assert_eq!(classify("notes.zip", "application/zip"), AssetKind::Archive);
        assert_eq!(classify("data.csv", "text/csv"), AssetKind::Text);
        assert_eq!(
            classify("mystery.bin", "application/octet-stream"),
            AssetKind::Other
        );
        assert!(AssetKind::Pdf.opens_externally());
        assert!(!AssetKind::Image.opens_externally());
    }

    #[test]
    fn mime_guessing_has_fallbacks() {
        assert_eq!(mime_for("x.png"), "image/png");
        assert_eq!(mime_for("x.unknownext"), "application/octet-stream");
        assert_eq!(mime_for("no_extension"), "application/octet-stream");
    }

    #[test]
    fn duplicate_detection_finds_the_existing_copy() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("diagram-1a2b3c4d.png"), b"x").unwrap();
        assert_eq!(
            find_by_hash(dir.path(), "1a2b3c4d", "png").as_deref(),
            Some("assets/diagram-1a2b3c4d.png")
        );
        assert!(find_by_hash(dir.path(), "00000000", "png").is_none());
    }

    #[test]
    fn asset_paths_must_stay_in_assets() {
        let dir = tempfile::tempdir().unwrap();
        let err = asset_ref(dir.path(), "pages/note.md").unwrap_err();
        assert!(matches!(err, Error::BadRequest(_)));
    }
}
