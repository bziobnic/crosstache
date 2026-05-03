//! Age encryption helpers for the local backend.
//!
//! All functions in this module are synchronous (they operate on
//! `std::fs::File`), because the age crate is synchronous. Callers use
//! `tokio::task::spawn_blocking` when called from async contexts.

use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use age::secrecy::ExposeSecret;
use zeroize::Zeroizing;

use crate::backend::error::BackendError;

/// Encrypt `plaintext` and write the ciphertext to `path`.
pub fn encrypt_to_file(
    path: &Path,
    plaintext: &[u8],
    recipients: &[age::x25519::Recipient],
) -> Result<(), BackendError> {
    let boxed_recipients: Vec<Box<dyn age::Recipient + Send>> = recipients
        .iter()
        .map(|r| Box::new(r.clone()) as Box<dyn age::Recipient + Send>)
        .collect();

    let encryptor = age::Encryptor::with_recipients(boxed_recipients)
        .ok_or_else(|| BackendError::Internal("no recipients provided".into()))?;

    let file = File::create(path)
        .map_err(|e| BackendError::Internal(format!("create {}: {e}", path.display())))?;

    let mut writer = encryptor
        .wrap_output(file)
        .map_err(|e| BackendError::Internal(format!("encrypt init: {e}")))?;

    writer
        .write_all(plaintext)
        .map_err(|e| BackendError::Internal(format!("encrypt write: {e}")))?;

    writer
        .finish()
        .map_err(|e| BackendError::Internal(format!("encrypt finish: {e}")))?;

    Ok(())
}

/// Read and decrypt the age file at `path`, returning the plaintext bytes.
///
/// Used for file/blob storage where the content may not be valid UTF-8.
pub fn decrypt_bytes_from_file(
    path: &Path,
    identity: &age::x25519::Identity,
) -> Result<Vec<u8>, BackendError> {
    let file = File::open(path)
        .map_err(|e| BackendError::Internal(format!("open {}: {e}", path.display())))?;
    let reader = BufReader::new(file);

    let decryptor = match age::Decryptor::new_buffered(reader)
        .map_err(|e| BackendError::Internal(format!("decrypt header: {e}")))?
    {
        age::Decryptor::Recipients(d) => d,
        _ => {
            return Err(BackendError::Internal(
                "unexpected passphrase-encrypted file".into(),
            ));
        }
    };

    let mut decrypted = decryptor
        .decrypt(std::iter::once(identity as &dyn age::Identity))
        .map_err(|e| BackendError::Internal(format!("decrypt: {e}")))?;

    let mut buf = Vec::new();
    decrypted
        .read_to_end(&mut buf)
        .map_err(|e| BackendError::Internal(format!("read plaintext: {e}")))?;

    Ok(buf)
}

/// Read and decrypt the age file at `path`, returning the plaintext as a
/// `Zeroizing<String>`.
pub fn decrypt_from_file(
    path: &Path,
    identity: &age::x25519::Identity,
) -> Result<Zeroizing<String>, BackendError> {
    let file = File::open(path)
        .map_err(|e| BackendError::Internal(format!("open {}: {e}", path.display())))?;
    let reader = BufReader::new(file);

    let decryptor = match age::Decryptor::new_buffered(reader)
        .map_err(|e| BackendError::Internal(format!("decrypt header: {e}")))?
    {
        age::Decryptor::Recipients(d) => d,
        _ => {
            return Err(BackendError::Internal(
                "unexpected passphrase-encrypted file".into(),
            ));
        }
    };

    let mut decrypted = decryptor
        .decrypt(std::iter::once(identity as &dyn age::Identity))
        .map_err(|e| BackendError::Internal(format!("decrypt: {e}")))?;

    let mut buf = String::new();
    decrypted
        .read_to_string(&mut buf)
        .map_err(|e| BackendError::Internal(format!("read plaintext: {e}")))?;

    Ok(Zeroizing::new(buf))
}

/// Load an age x25519 identity from a key file.
///
/// The file may contain comments (lines starting with `#`) and blank lines;
/// the first line matching the `AGE-SECRET-KEY-*` pattern is used.
pub fn load_identity(key_path: &Path) -> Result<age::x25519::Identity, BackendError> {
    let contents = fs::read_to_string(key_path)
        .map_err(|e| BackendError::Internal(format!("read key {}: {e}", key_path.display())))?;

    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Ok(id) = line.parse::<age::x25519::Identity>() {
            return Ok(id);
        }
    }

    Err(BackendError::Internal(format!(
        "no valid age identity found in {}",
        key_path.display()
    )))
}

/// Load age x25519 recipients from a recipients file.
///
/// Blank lines and comment lines (starting with `#`) are skipped.
pub fn load_recipients(
    recipients_path: &Path,
) -> Result<Vec<age::x25519::Recipient>, BackendError> {
    let contents = fs::read_to_string(recipients_path).map_err(|e| {
        BackendError::Internal(format!(
            "read recipients {}: {e}",
            recipients_path.display()
        ))
    })?;

    let mut recipients = Vec::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let r: age::x25519::Recipient = line
            .parse()
            .map_err(|e: &str| BackendError::Internal(format!("parse recipient '{line}': {e}")))?;
        recipients.push(r);
    }

    if recipients.is_empty() {
        return Err(BackendError::Internal(format!(
            "no recipients found in {}",
            recipients_path.display()
        )));
    }

    Ok(recipients)
}

/// Generate a new age x25519 keypair and write identity + recipient files.
///
/// - `key_path` receives the private key (permissions 0600 on Unix).
/// - `recipients_path` receives the public key.
pub fn generate_keypair(
    key_path: &Path,
    recipients_path: &Path,
) -> Result<(age::x25519::Identity, Vec<age::x25519::Recipient>), BackendError> {
    // Ensure parent directories exist with 0700 permissions.
    if let Some(parent) = key_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| BackendError::Internal(format!("mkdir {}: {e}", parent.display())))?;
        set_dir_permissions(parent)?;
    }

    let identity = age::x25519::Identity::generate();
    let recipient = identity.to_public();

    // Write private key
    let secret_str = identity.to_string();
    let key_content = format!(
        "# created: {}\n# public key: {}\n{}\n",
        chrono::Utc::now().to_rfc3339(),
        recipient,
        secret_str.expose_secret()
    );
    fs::write(key_path, key_content.as_bytes())
        .map_err(|e| BackendError::Internal(format!("write key: {e}")))?;

    // Set key file permissions to 0600
    #[cfg(unix)]
    {
        fs::set_permissions(key_path, fs::Permissions::from_mode(0o600))
            .map_err(|e| BackendError::Internal(format!("chmod key: {e}")))?;
    }

    // Write recipient (public key)
    let recipient_content = format!("{}\n", recipient);
    fs::write(recipients_path, recipient_content.as_bytes())
        .map_err(|e| BackendError::Internal(format!("write recipients: {e}")))?;

    Ok((identity, vec![recipient]))
}

/// Set directory permissions to 0700 on Unix.
#[allow(unused_variables)]
pub fn set_dir_permissions(dir: &Path) -> Result<(), BackendError> {
    #[cfg(unix)]
    {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .map_err(|e| BackendError::Internal(format!("chmod dir {}: {e}", dir.display())))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn generate_and_load_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let key_path = tmp.path().join("key.txt");
        let recipients_path = tmp.path().join("recipients.txt");

        let (id, recipients) = generate_keypair(&key_path, &recipients_path).unwrap();
        assert_eq!(recipients.len(), 1);
        assert_eq!(id.to_public().to_string(), recipients[0].to_string());

        // Reload
        let id2 = load_identity(&key_path).unwrap();
        assert_eq!(id.to_public().to_string(), id2.to_public().to_string());

        let r2 = load_recipients(&recipients_path).unwrap();
        assert_eq!(r2.len(), 1);
        assert_eq!(r2[0].to_string(), recipients[0].to_string());
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let key_path = tmp.path().join("key.txt");
        let recipients_path = tmp.path().join("recipients.txt");

        let (identity, recipients) = generate_keypair(&key_path, &recipients_path).unwrap();

        let secret_file = tmp.path().join("secret.age");
        let plaintext = "super-secret-value-42";

        encrypt_to_file(&secret_file, plaintext.as_bytes(), &recipients).unwrap();
        assert!(secret_file.exists());

        let decrypted = decrypt_from_file(&secret_file, &identity).unwrap();
        assert_eq!(&*decrypted, plaintext);
    }

    #[test]
    fn encrypt_decrypt_empty_value() {
        let tmp = TempDir::new().unwrap();
        let key_path = tmp.path().join("key.txt");
        let recipients_path = tmp.path().join("recipients.txt");

        let (identity, recipients) = generate_keypair(&key_path, &recipients_path).unwrap();

        let secret_file = tmp.path().join("empty.age");
        encrypt_to_file(&secret_file, b"", &recipients).unwrap();
        let decrypted = decrypt_from_file(&secret_file, &identity).unwrap();
        assert_eq!(&*decrypted, "");
    }

    #[cfg(unix)]
    #[test]
    fn key_file_has_correct_permissions() {
        let tmp = TempDir::new().unwrap();
        let key_path = tmp.path().join("key.txt");
        let recipients_path = tmp.path().join("recipients.txt");

        generate_keypair(&key_path, &recipients_path).unwrap();

        let meta = fs::metadata(&key_path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn load_identity_missing_file() {
        let result = load_identity(Path::new("/nonexistent/key.txt"));
        assert!(result.is_err());
    }
}
