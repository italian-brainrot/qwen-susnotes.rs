//! Note names, and the mapping between a note name and its file name.
//!
//! A note's name *is* its file name (`pages/<name>.md`), which is what makes
//! a susnotes vault portable and greppable. File systems and note names do
//! not share a character set, so the file name is a reversible escaping of
//! the name: every character that a file system forbids becomes `%XX`, and
//! `%` itself becomes `%25`. Because the escaping is reversible, the name
//! shown in the app is always exactly the name the user typed.

use crate::error::{Error, Result};

/// Longest accepted note name, in characters.
///
/// The limit keeps the escaped file name comfortably inside the 255 byte
/// limit most file systems enforce, so a vault stays portable.
pub const MAX_NAME_CHARS: usize = 100;

/// Extensions reserved by the vault: only `.md` files are notes.
pub const NOTE_EXTENSION: &str = "md";

/// Characters a note name may not contain literally; they get escaped in the
/// file name. `/` and `\` are rejected outright instead, because they would
/// silently move a note into a sub folder.
const ILLEGAL_CHARS: [char; 9] = ['<', '>', ':', '"', '|', '?', '*', '%', '\\'];

/// Windows device names that cannot be file names even with an extension.
const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// The name of a note, exactly as the user wrote it.
///
/// Comparison is case insensitive (so `[[roam]]` finds the note `Roam`), while
/// display keeps the original spelling.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct NoteName(String);

impl std::fmt::Debug for NoteName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("NoteName").field(&self.0).finish()
    }
}

impl std::fmt::Display for NoteName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl NoteName {
    /// Accept user input as a note name.
    ///
    /// Trims surrounding whitespace, then rejects anything that cannot be
    /// turned into a portable file name.
    pub fn parse(input: &str) -> Result<Self> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(Error::InvalidName {
                name: input.to_owned(),
                reason: "the name is empty".into(),
            });
        }
        if trimmed == "." || trimmed == ".." || trimmed.starts_with('.') {
            return Err(Error::InvalidName {
                name: input.to_owned(),
                reason: "a note name may not start with a dot".into(),
            });
        }
        if trimmed.chars().count() > MAX_NAME_CHARS {
            return Err(Error::InvalidName {
                name: input.to_owned(),
                reason: format!("at most {MAX_NAME_CHARS} characters are allowed"),
            });
        }
        if trimmed.ends_with('.') {
            return Err(Error::InvalidName {
                name: input.to_owned(),
                reason: "a note name may not end with a dot".into(),
            });
        }
        for ch in trimmed.chars() {
            if ch == '/' || ch == '\\' || ch.is_control() {
                return Err(Error::InvalidName {
                    name: input.to_owned(),
                    reason: format!("{ch:?} is not allowed in a note name"),
                });
            }
        }

        Ok(Self(trimmed.to_owned()))
    }

    /// Wrap an already validated name.
    #[must_use]
    pub fn trusted(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// The name as displayed.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The case folded form used to compare and to index links.
    #[must_use]
    pub fn key(&self) -> String {
        self.0.to_lowercase()
    }

    /// The file name for this note, without the folder.
    #[must_use]
    pub fn file_name(&self) -> String {
        file_name_for(&self.0)
    }

    /// The path of this note inside the vault, e.g. `pages/Roam basics.md`.
    #[must_use]
    pub fn vault_path(&self) -> String {
        format!("pages/{}", self.file_name())
    }

    /// Whether this note is a journal page for a day.
    #[must_use]
    pub fn is_journal(&self) -> bool {
        crate::journal::is_journal_name(&self.0)
    }
}

impl PartialOrd for NoteName {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for NoteName {
    /// Case insensitive ordering, falling back to the exact name so the order
    /// is total.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.key()
            .cmp(&other.key())
            .then_with(|| self.0.cmp(&other.0))
    }
}

impl serde::Serialize for NoteName {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for NoteName {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// True when a character must be escaped in a file name.
fn needs_escape(ch: char) -> bool {
    ILLEGAL_CHARS.contains(&ch) || ch.is_control() || ch == '\u{7f}'
}

/// Percent-escape the characters a file system forbids.
fn escape_char(ch: char) -> String {
    let mut out = String::new();
    let mut buffer = [0u8; 4];
    for byte in ch.encode_utf8(&mut buffer).as_bytes() {
        out.push('%');
        out.push_str(&format!("{byte:02X}"));
    }
    out
}

/// The file name (no folder) that stores a note called `name`.
#[must_use]
pub fn file_name_for(name: &str) -> String {
    let mut escaped = String::with_capacity(name.len());
    for ch in name.chars() {
        if needs_escape(ch) {
            escaped.push_str(&escape_char(ch));
        } else {
            escaped.push(ch);
        }
    }

    // Windows refuses these names even with an extension. They are pure ASCII,
    // so escaping their first byte produces a valid, reversible file name that
    // no operating system reserves: the note keeps its real name.
    let stem_upper = escaped.to_ascii_uppercase();
    if RESERVED.contains(&stem_upper.as_str()) {
        let head = escaped.as_bytes()[0];
        escaped = format!("%{head:02X}{}", &escaped[1..]);
    }

    if escaped.is_empty() {
        escaped = "%20".to_owned();
    }

    format!("{escaped}.{NOTE_EXTENSION}")
}

/// Decode a file name back into the note name it stores.
pub fn note_name_for_file_name(file_name: &str) -> Result<String> {
    let stem = file_name
        .strip_suffix(&format!(".{NOTE_EXTENSION}"))
        .or_else(|| file_name.strip_suffix(&format!(".{}", NOTE_EXTENSION.to_uppercase())))
        .unwrap_or(file_name);

    let bytes = unhex(stem)?;
    String::from_utf8(bytes).map_err(|_| Error::Format {
        what: "file name".into(),
        reason: format!("{file_name:?} does not decode to a valid note name"),
    })
}

/// Reverse of [`file_name_for`]'s escaping, operating on bytes.
fn unhex(stem: &str) -> Result<Vec<u8>> {
    let bytes = stem.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = *bytes.get(i + 1).ok_or_else(|| Error::Format {
                what: "file name".into(),
                reason: "escape sequence is truncated".into(),
            })?;
            let lo = *bytes.get(i + 2).ok_or_else(|| Error::Format {
                what: "file name".into(),
                reason: "escape sequence is truncated".into(),
            })?;
            let (Ok(hi), Ok(lo)) = (hex_val(hi), hex_val(lo)) else {
                return Err(Error::Format {
                    what: "file name".into(),
                    reason: format!("invalid escape sequence %{}{}", hi as char, lo as char),
                });
            };
            out.push(hi * 16 + lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Ok(out)
}

fn hex_val(byte: u8) -> std::result::Result<u8, ()> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(()),
    }
}

/// Whether a file in `pages/` is a note (and not a stray file or a leftover
/// temporary file).
#[must_use]
pub fn is_note_file_name(file_name: &str) -> bool {
    file_name.ends_with(&format!(".{NOTE_EXTENSION}"))
        && !file_name.starts_with('.')
        && !file_name.ends_with(".tmp")
}

/// Normalise a path relative to the vault root: forward slashes, no leading
/// `./`, no `..` segments. Used as AEAD associated data, so it must be stable.
pub fn normalize_vault_path(path: &str) -> Result<String> {
    let path = path.replace('\\', "/");
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => continue,
            ".." => {
                return Err(Error::BadRequest(format!(
                    "{path:?} escapes the vault folder"
                )));
            }
            other => segments.push(other),
        }
    }
    if segments.is_empty() {
        return Err(Error::BadRequest("the path is empty".into()));
    }
    Ok(segments.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_through_file_names() {
        for name in [
            "Roam basics",
            "C# notes",
            "2026-10-23",
            "math: $\u{3c}\u{3e}",
            "quote \" and | and * and ?",
            "\u{4e2d}\u{6587}\u{b014}\u{ae80}",
            "percent %20 inside",
            "CON",
            "nul.txt",
            "\u{1f600} emoji",
        ] {
            let file = file_name_for(name);
            assert!(!file.contains('/'), "{name:?} produced a path {file:?}");
            let back = note_name_for_file_name(&file).unwrap();
            assert_eq!(back, name, "file name {file:?} did not decode back");
        }
    }

    #[test]
    fn file_names_are_unique_per_name() {
        let mut files: Vec<_> = ["a b", "a%20b", "a<b", "a%b"]
            .iter()
            .map(|n| file_name_for(n))
            .collect();
        let all = files.clone();
        files.sort();
        files.dedup();
        assert_eq!(files.len(), all.len(), "colliding file names from {all:?}");
    }

    #[test]
    fn rejected_names_explain_why() {
        for bad in [
            "",
            "   ",
            "a/b",
            "a\\b",
            "..",
            ".hidden",
            "trailing.",
            &"x".repeat(MAX_NAME_CHARS + 1),
        ] {
            let err = NoteName::parse(bad).unwrap_err();
            assert!(
                matches!(err, Error::InvalidName { .. }),
                "{bad:?} was accepted"
            );
        }
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        assert_eq!(
            NoteName::parse("  Hello World \n").unwrap().as_str(),
            "Hello World"
        );
    }

    #[test]
    fn comparison_is_case_insensitive_but_display_is_not() {
        let upper = NoteName::parse("Zettelkasten").unwrap();
        let lower = NoteName::parse("zettelkasten").unwrap();
        assert_eq!(upper.key(), lower.key());
        assert_ne!(upper, lower);
        assert_eq!(upper.vault_path(), "pages/Zettelkasten.md");
    }

    #[test]
    fn long_names_are_rejected_before_touching_the_file_system() {
        let cyrillic = "\u{0430}".repeat(MAX_NAME_CHARS + 1);
        assert!(NoteName::parse(&cyrillic).is_err());
    }

    #[test]
    fn vault_paths_are_normalised() {
        assert_eq!(
            normalize_vault_path("./assets//img.png").unwrap(),
            "assets/img.png"
        );
        assert_eq!(
            normalize_vault_path("pages\\note.md").unwrap(),
            "pages/note.md"
        );
        assert!(normalize_vault_path("../secret").is_err());
        assert!(normalize_vault_path("/").is_err());
    }

    #[test]
    fn only_md_files_are_notes() {
        assert!(is_note_file_name("Hello.md"));
        assert!(is_note_file_name("Hello%20World.md"));
        assert!(!is_note_file_name("image.png"));
        assert!(!is_note_file_name(".Hello.md.tmp"));
        assert!(!is_note_file_name("README.txt"));
    }
}
