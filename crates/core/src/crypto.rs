//! The encrypted on-disk record format used for every note and asset.
//!
//! # Format (`SUSNVAUL` v1)
//!
//! ```text
//! offset  size  field
//! 0       8     magic  b"SUSNVAUL"
//! 8       1     format version (1)
//! 9       1     record kind (0 = note, 1 = asset)
//! 10      1     key-derivation algorithm id (1 = argon2id)
//! 11      2     path length, little endian u16
//! 13      n     record path, relative to the vault root, '/' separated
//! 13+n    24    XChaCha20-Poly1305 nonce (fresh random per write)
//! 37+n    ...   ciphertext with trailing 16 byte Poly1305 tag
//! ```
//!
//! Everything before the nonce is also the AEAD additional authenticated
//! data, so a record cannot be moved to another path, converted between a
//! note and an asset, or have its version downgraded without detection. The
//! per-record encryption key is derived from the vault's master key with
//! HKDF-SHA256 over the record path, which means two files never share a key
//! even though the nonce is only random, and a rename forces a re-encrypt.
//!
//! The password never encrypts files directly. Unlock derives a password key
//! with Argon2id (salt and costs stored in the clear in `config/vault.json`),
//! uses it to decrypt the randomly generated master key that is stored wrapped
//! next to it, and only that master key feeds the per-record HKDF. Changing the
//! password therefore rewrites one small file instead of re-encrypting the
//! whole vault, and an interrupted password change can never lose notes.

use std::fmt;

use base64::prelude::{BASE64_STANDARD as B64, Engine as _};
use chacha20poly1305::{
    XChaCha20Poly1305,
    aead::{Aead, KeyInit, Payload},
};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::error::{Error, Result};

/// Magic prefix of every encrypted record.
pub const MAGIC: [u8; 8] = *b"SUSNVAUL";
/// Current on-disk record format version.
pub const FORMAT_VERSION: u8 = 1;
/// Length of the master key, in bytes.
pub const KEY_LEN: usize = 32;
/// Length of the Argon2id salt, in bytes.
pub const SALT_LEN: usize = 16;

const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
const HEADER_MIN_LEN: usize = 13;

const HKDF_FILE_SALT: &[u8] = b"susnotes-file-key-v1";
const KEY_WRAPPING_AAD: &[u8] = b"susnotes-wrapped-master-key-v1";

/// Whether a record is a note or an asset. Stored in the authenticated
/// header, so the two kinds of file are not interchangeable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    /// A markdown note under `pages/`.
    Note,
    /// An image or attachment under `assets/`.
    Asset,
}

impl RecordKind {
    /// The single byte written into the header.
    #[must_use]
    pub fn as_byte(self) -> u8 {
        match self {
            Self::Note => 0,
            Self::Asset => 1,
        }
    }

    /// Decode the header byte.
    pub fn from_byte(byte: u8) -> Result<Self> {
        match byte {
            0 => Ok(Self::Note),
            1 => Ok(Self::Asset),
            other => Err(Error::Format {
                what: "record header".into(),
                reason: format!("unknown record kind {other}"),
            }),
        }
    }

    /// Whether a record's path is expected to be UTF-8 text.
    #[must_use]
    pub fn is_text(self) -> bool {
        matches!(self, Self::Note)
    }
}

impl fmt::Display for RecordKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Note => f.write_str("note"),
            Self::Asset => f.write_str("asset"),
        }
    }
}

/// Password hashing algorithm ids, written into every record header so a
/// future version can migrate without ambiguity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KdfAlgorithm {
    /// Argon2id, the only algorithm this version writes.
    Argon2id,
}

impl KdfAlgorithm {
    fn as_byte(self) -> u8 {
        match self {
            Self::Argon2id => 1,
        }
    }

    fn from_byte(byte: u8) -> Result<Self> {
        match byte {
            1 => Ok(Self::Argon2id),
            other => Err(Error::Format {
                what: "key derivation".into(),
                reason: format!("unsupported algorithm id {other}"),
            }),
        }
    }
}

/// Argon2id parameters for the vault master key.
///
/// Defaults to 32 MiB, three passes, one lane: strong enough to make an
/// offline attack expensive while keeping unlock under a second on modest
/// hardware, which matters because unlock happens on every app start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    /// Algorithm, written into each record header.
    pub algorithm: KdfAlgorithm,
    /// Memory cost in KiB.
    pub memory_kib: u32,
    /// Number of passes over the memory.
    pub iterations: u32,
    /// Degree of parallelism.
    pub parallelism: u32,
}

impl Default for KdfParams {
    fn default() -> Self {
        Self {
            algorithm: KdfAlgorithm::Argon2id,
            memory_kib: 32 * 1024,
            iterations: 3,
            parallelism: 1,
        }
    }
}

impl KdfParams {
    /// Parameters for tests: fast enough to keep the suite quick, structurally
    /// identical to the production parameters.
    #[must_use]
    pub fn test_defaults() -> Self {
        Self {
            algorithm: KdfAlgorithm::Argon2id,
            memory_kib: 64,
            iterations: 2,
            parallelism: 1,
        }
    }

    /// Derive the password key from a password and the vault's salt.
    ///
    /// This is intentionally expensive; call it off the main thread.
    pub fn derive_password_key(&self, password: &[u8], salt: &[u8]) -> Result<PasswordKey> {
        if salt.len() != SALT_LEN {
            return Err(Error::Format {
                what: "vault salt".into(),
                reason: format!("expected {SALT_LEN} bytes, got {}", salt.len()),
            });
        }

        let params = argon2::Params::new(
            self.memory_kib,
            self.iterations,
            self.parallelism,
            Some(KEY_LEN),
        )
        .map_err(|err| Error::Crypto(format!("invalid Argon2id parameters: {err}")))?;
        let argon =
            argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);

        let mut key = [0u8; KEY_LEN];
        argon
            .hash_password_into(password, salt, &mut key)
            .map_err(|err| Error::Crypto(format!("Argon2id failed: {err}")))?;
        Ok(PasswordKey(Zeroizing::new(key)))
    }
}

/// A key stretched from the password with [`KdfParams`].
///
/// Only ever used to unwrap the master key; never used for file content.
#[derive(Clone)]
pub struct PasswordKey(Zeroizing<[u8; KEY_LEN]>);

impl fmt::Debug for PasswordKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PasswordKey(<redacted>)")
    }
}

/// The master key of a vault: random bytes, stored wrapped in
/// `config/vault.json`, and the thing every record key is derived from.
#[derive(Clone)]
pub struct MasterKey(Zeroizing<[u8; KEY_LEN]>);

impl MasterKey {
    /// Generate a fresh master key.
    pub fn generate() -> Result<Self> {
        Ok(Self(Zeroizing::new(random_bytes::<KEY_LEN>()?)))
    }

    /// The raw bytes, for comparisons inside the crate.
    pub(crate) fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }

    fn hkdf(&self, salt: &[u8], info: &[u8]) -> [u8; KEY_LEN] {
        let hk = Hkdf::<Sha256>::new(Some(salt), self.0.as_slice());
        let mut out = [0u8; KEY_LEN];
        // `expand` only fails when the requested length is absurd; KEY_LEN is
        // far below the HKDF limit.
        hk.expand(info, &mut out)
            .expect("HKDF expansion of 32 bytes always succeeds");
        out
    }

    /// The key for one record, bound to its kind and path.
    fn record_key(&self, kind: RecordKind, rel_path: &str) -> Zeroizing<[u8; KEY_LEN]> {
        let info = format!("{}:{rel_path}", kind.as_byte());
        Zeroizing::new(self.hkdf(HKDF_FILE_SALT, info.as_bytes()))
    }
}

impl fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MasterKey(<redacted>)")
    }
}

impl zeroize::ZeroizeOnDrop for MasterKey {}
impl zeroize::ZeroizeOnDrop for PasswordKey {}

/// Wrap the master key under a password key.
pub fn wrap_master_key(password_key: &PasswordKey, master: &MasterKey) -> Result<WrappedKey> {
    let nonce = random_bytes::<NONCE_LEN>()?;
    let cipher = cipher(&password_key.0);
    let ciphertext = cipher
        .encrypt(
            (&nonce).into(),
            Payload {
                msg: master.0.as_slice(),
                aad: KEY_WRAPPING_AAD,
            },
        )
        .map_err(|err| Error::Crypto(format!("could not wrap the master key: {err}")))?;
    Ok(WrappedKey {
        nonce: B64.encode(nonce),
        ciphertext: B64.encode(ciphertext),
    })
}

/// Recover the master key, or report a wrong password.
pub fn unwrap_master_key(password_key: &PasswordKey, wrapped: &WrappedKey) -> Result<MasterKey> {
    let nonce = B64.decode(&wrapped.nonce).map_err(|err| Error::Format {
        what: "wrapped key".into(),
        reason: err.to_string(),
    })?;
    let ciphertext = B64
        .decode(&wrapped.ciphertext)
        .map_err(|err| Error::Format {
            what: "wrapped key".into(),
            reason: err.to_string(),
        })?;
    let nonce: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| Error::Format {
        what: "wrapped key".into(),
        reason: "nonce has the wrong length".into(),
    })?;

    let cipher = cipher(&password_key.0);
    let key = cipher
        .decrypt(
            (&nonce).into(),
            Payload {
                msg: ciphertext.as_slice(),
                aad: KEY_WRAPPING_AAD,
            },
        )
        .map_err(|_| Error::WrongPassword)?;
    let key: [u8; KEY_LEN] = key.try_into().map_err(|_| Error::Format {
        what: "wrapped key".into(),
        reason: "key has the wrong length".into(),
    })?;
    Ok(MasterKey(Zeroizing::new(key)))
}

/// Build the AEAD cipher for a 32 byte key.
fn cipher(key: &[u8; KEY_LEN]) -> XChaCha20Poly1305 {
    XChaCha20Poly1305::new(key.into())
}

fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut out = [0u8; N];
    getrandom::fill(&mut out)
        .map_err(|err| Error::Crypto(format!("operating system randomness failed: {err}")))?;
    Ok(out)
}

/// Build the authenticated header of a record.
fn header(kind: RecordKind, kdf: KdfAlgorithm, rel_path: &str) -> Vec<u8> {
    let path = rel_path.as_bytes();
    let mut header = Vec::with_capacity(HEADER_MIN_LEN + path.len());
    header.extend_from_slice(&MAGIC);
    header.push(FORMAT_VERSION);
    header.push(kind.as_byte());
    header.push(kdf.as_byte());
    header.extend_from_slice(&(path.len() as u16).to_le_bytes());
    header.extend_from_slice(path);
    header
}

/// Parse and authenticate a record header, returning `(kind, path, header_len)`.
fn parse_header(bytes: &[u8], expect: RecordKind) -> Result<(RecordKind, String, usize)> {
    if bytes.len() < HEADER_MIN_LEN + NONCE_LEN {
        if bytes.starts_with(&MAGIC) {
            return Err(Error::Format {
                what: "record".into(),
                reason: "file is truncated".into(),
            });
        }
        return Err(Error::Format {
            what: "record".into(),
            reason: if bytes.iter().all(u8::is_ascii)
                && String::from_utf8_lossy(bytes).starts_with("---")
            {
                "file looks like plain text with frontmatter, not an encrypted susnotes record"
                    .into()
            } else {
                "file is not an encrypted susnotes record".into()
            },
        });
    }

    if !bytes.starts_with(&MAGIC) {
        return Err(Error::Format {
            what: "record".into(),
            reason: "missing SUSNVAUL magic; the file was written by another tool or is plain text"
                .into(),
        });
    }

    let version = bytes[8];
    if version != FORMAT_VERSION {
        return Err(Error::Format {
            what: "record".into(),
            reason: format!("unsupported format version {version}"),
        });
    }

    let kind = RecordKind::from_byte(bytes[9])?;
    if kind != expect {
        return Err(Error::Format {
            what: "record".into(),
            reason: format!("file holds {kind} data but was read as {expect}"),
        });
    }

    let kdf_id = bytes[10];
    KdfAlgorithm::from_byte(kdf_id)?;

    let path_len = u16::from_le_bytes([bytes[11], bytes[12]]) as usize;
    let header_len = HEADER_MIN_LEN + path_len;
    if bytes.len() < header_len + NONCE_LEN {
        return Err(Error::Format {
            what: "record".into(),
            reason: "file is truncated inside the header".into(),
        });
    }

    let stored_path = std::str::from_utf8(&bytes[HEADER_MIN_LEN..header_len])
        .map_err(|err| Error::Format {
            what: "record path".into(),
            reason: err.to_string(),
        })?
        .to_owned();

    // The caller asked for a record at a specific path; the header must agree.
    Ok((kind, stored_path, header_len))
}

/// Encrypt `plaintext` for the record at `rel_path`.
///
/// `kdf` is recorded in the header for forward compatibility; it describes how
/// the master key was derived, not how this record is encrypted.
pub fn encrypt(
    master: &MasterKey,
    kind: RecordKind,
    kdf: KdfAlgorithm,
    rel_path: &str,
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    let header = header(kind, kdf, rel_path);
    let nonce = random_bytes::<NONCE_LEN>()?;
    let key = master.record_key(kind, rel_path);
    let cipher = cipher(&key);

    let ciphertext = cipher
        .encrypt(
            (&nonce).into(),
            Payload {
                msg: plaintext,
                aad: &header,
            },
        )
        .map_err(|err| Error::Crypto(format!("could not encrypt {rel_path}: {err}")))?;

    let mut out = header;
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt the record that is stored at `rel_path`.
///
/// The path inside the file must match `rel_path`, otherwise the record was
/// moved or copied and decryption is refused.
pub fn decrypt(
    master: &MasterKey,
    expect: RecordKind,
    rel_path: &str,
    bytes: &[u8],
) -> Result<Vec<u8>> {
    let (kind, stored_path, header_len) = parse_header(bytes, expect)?;
    if stored_path != rel_path {
        return Err(Error::Format {
            what: "record path".into(),
            reason: format!("file says it is {stored_path:?} but was read as {rel_path:?}"),
        });
    }

    let nonce_start = header_len;
    let nonce_end = header_len + NONCE_LEN;
    let nonce: &[u8; NONCE_LEN] =
        bytes[nonce_start..nonce_end]
            .try_into()
            .map_err(|_| Error::Format {
                what: "record".into(),
                reason: "the nonce is truncated".into(),
            })?;
    let key = master.record_key(kind, rel_path);
    let cipher = cipher(&key);

    cipher
        .decrypt(
            nonce.into(),
            Payload {
                msg: &bytes[nonce_end..],
                aad: &bytes[..header_len],
            },
        )
        .map_err(|err| Error::Format {
            what: "record".into(),
            reason: format!("could not decrypt {rel_path}: {err}"),
        })
}

/// Generate a fresh Argon2id salt for a vault.
pub fn random_salt() -> Result<[u8; SALT_LEN]> {
    random_bytes::<SALT_LEN>()
}

/// How many bytes an empty record occupies on disk: header, nonce and tag.
///
/// Comparing a file's size against this answers "does this note hold anything"
/// without decrypting it.
#[must_use]
pub fn record_overhead(path_len: usize) -> usize {
    HEADER_MIN_LEN + path_len + NONCE_LEN + TAG_LEN
}

/// Whether the bytes look like a susnotes record at all.
#[must_use]
pub fn looks_encrypted(bytes: &[u8]) -> bool {
    bytes.starts_with(&MAGIC)
}

/// The wrapped master key, as stored in `config/vault.json`.
///
/// Decrypting it *is* the password check: a wrong password makes the Poly1305
/// tag fail, and no vault content is touched before it succeeds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WrappedKey {
    /// Base64 encoded XChaCha20-Poly1305 nonce.
    pub nonce: String,
    /// Base64 encoded ciphertext of the master key (including the tag).
    pub ciphertext: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWORD: &[u8] = b"correct horse battery staple";
    const SALT: &[u8; SALT_LEN] = &[7u8; SALT_LEN];

    fn keys() -> (PasswordKey, MasterKey) {
        let password_key = KdfParams::test_defaults()
            .derive_password_key(PASSWORD, SALT)
            .unwrap();
        (password_key, MasterKey::generate().unwrap())
    }

    #[test]
    fn master_key_survives_the_wrapping_round_trip() {
        let (password_key, master) = keys();
        let wrapped = wrap_master_key(&password_key, &master).unwrap();
        let recovered = unwrap_master_key(&password_key, &wrapped).unwrap();
        assert_eq!(master.0.to_vec(), recovered.0.to_vec());
    }

    #[test]
    fn a_wrong_password_is_reported_as_such() {
        let (password_key, master) = keys();
        let wrapped = wrap_master_key(&password_key, &master).unwrap();
        let wrong = KdfParams::test_defaults()
            .derive_password_key(b"nope", SALT)
            .unwrap();
        let err = unwrap_master_key(&wrong, &wrapped).unwrap_err();
        assert!(matches!(err, Error::WrongPassword));

        let wrong_salt = KdfParams::test_defaults()
            .derive_password_key(PASSWORD, &[8u8; SALT_LEN])
            .unwrap();
        assert!(matches!(
            unwrap_master_key(&wrong_salt, &wrapped).unwrap_err(),
            Error::WrongPassword
        ));
    }

    #[test]
    fn wrapping_uses_a_fresh_nonce_each_time() {
        let (password_key, master) = keys();
        let a = wrap_master_key(&password_key, &master).unwrap();
        let b = wrap_master_key(&password_key, &master).unwrap();
        assert_ne!(a, b, "re-wrapping the same key must not repeat a nonce");
    }

    #[test]
    fn round_trip_recovers_plaintext() {
        let (_, key) = keys();
        let bytes = encrypt(
            &key,
            RecordKind::Note,
            KdfAlgorithm::Argon2id,
            "pages/Hello.md",
            b"# hi".as_slice(),
        )
        .unwrap();
        assert!(looks_encrypted(&bytes));
        assert!(!String::from_utf8_lossy(&bytes).contains("hi"));
        let plaintext = decrypt(&key, RecordKind::Note, "pages/Hello.md", &bytes).unwrap();
        assert_eq!(plaintext, b"# hi");
    }

    #[test]
    fn binary_assets_round_trip_byte_for_byte() {
        let (_, key) = keys();
        let payload: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
        let bytes = encrypt(
            &key,
            RecordKind::Asset,
            KdfAlgorithm::Argon2id,
            "assets/x-1a2b.png",
            &payload,
        )
        .unwrap();
        let back = decrypt(&key, RecordKind::Asset, "assets/x-1a2b.png", &bytes).unwrap();
        assert_eq!(back, payload);
    }

    #[test]
    fn every_write_uses_a_fresh_nonce() {
        let (_, key) = keys();
        let a = encrypt(
            &key,
            RecordKind::Note,
            KdfAlgorithm::Argon2id,
            "pages/A.md",
            b"same".as_slice(),
        )
        .unwrap();
        let b = encrypt(
            &key,
            RecordKind::Note,
            KdfAlgorithm::Argon2id,
            "pages/A.md",
            b"same".as_slice(),
        )
        .unwrap();
        assert_ne!(a, b, "re-encrypting the same note must not repeat a nonce");
    }

    #[test]
    fn a_record_does_not_decrypt_at_another_path() {
        let (_, key) = keys();
        let bytes = encrypt(
            &key,
            RecordKind::Note,
            KdfAlgorithm::Argon2id,
            "pages/Orig.md",
            b"secret".as_slice(),
        )
        .unwrap();
        let err = decrypt(&key, RecordKind::Note, "pages/Evil.md", &bytes).unwrap_err();
        assert!(matches!(err, Error::Format { .. }));
    }

    #[test]
    fn a_note_does_not_decrypt_as_an_asset() {
        let (_, key) = keys();
        let bytes = encrypt(
            &key,
            RecordKind::Note,
            KdfAlgorithm::Argon2id,
            "pages/A.md",
            b"secret".as_slice(),
        )
        .unwrap();
        let err = decrypt(&key, RecordKind::Asset, "pages/A.md", &bytes).unwrap_err();
        assert!(matches!(err, Error::Format { .. }));
    }

    #[test]
    fn another_master_key_cannot_decrypt() {
        let (password_key, key) = keys();
        let bytes = encrypt(
            &key,
            RecordKind::Note,
            KdfAlgorithm::Argon2id,
            "pages/A.md",
            b"secret".as_slice(),
        )
        .unwrap();
        let other = MasterKey::generate().unwrap();
        let err = decrypt(&other, RecordKind::Note, "pages/A.md", &bytes).unwrap_err();
        assert!(matches!(err, Error::Format { .. }));
        assert!(
            unwrap_master_key(
                &password_key,
                &wrap_master_key(&password_key, &other).unwrap()
            )
            .is_ok()
        );
    }

    #[test]
    fn tampering_with_the_header_is_detected() {
        let (_, key) = keys();
        let mut bytes = encrypt(
            &key,
            RecordKind::Note,
            KdfAlgorithm::Argon2id,
            "pages/A.md",
            b"secret".as_slice(),
        )
        .unwrap();
        // Flip a byte of the authenticated path.
        bytes[HEADER_MIN_LEN] ^= 0x20;
        let err = decrypt(&key, RecordKind::Note, "pages/A.md", &bytes).unwrap_err();
        assert!(matches!(err, Error::Format { .. }));
    }

    #[test]
    fn plain_text_files_are_reported_clearly() {
        let (_, key) = keys();
        let err = decrypt(&key, RecordKind::Note, "pages/A.md", b"# just markdown\n").unwrap_err();
        assert!(matches!(err, Error::Format { .. }));
        assert!(!looks_encrypted(b"# just markdown"));
        assert!(format!("{err}").contains("not an encrypted susnotes record"));
    }

    #[test]
    fn kdf_params_change_the_password_key() {
        let strong = KdfParams::test_defaults()
            .derive_password_key(b"pw", SALT)
            .unwrap();
        let mut tweaked = KdfParams::test_defaults();
        tweaked.iterations += 1;
        let tweaked = tweaked.derive_password_key(b"pw", SALT).unwrap();
        let master = MasterKey::generate().unwrap();
        let wrapped = wrap_master_key(&strong, &master).unwrap();
        assert!(matches!(
            unwrap_master_key(&tweaked, &wrapped).unwrap_err(),
            Error::WrongPassword
        ));
    }

    #[test]
    fn record_paths_are_bound_into_the_header() {
        let (_, key) = keys();
        let bytes = encrypt(
            &key,
            RecordKind::Note,
            KdfAlgorithm::Argon2id,
            "pages/Nested Name.md",
            b"x".as_slice(),
        )
        .unwrap();
        let (kind, path, _) = parse_header(&bytes, RecordKind::Note).unwrap();
        assert_eq!(kind, RecordKind::Note);
        assert_eq!(path, "pages/Nested Name.md");
    }
}
