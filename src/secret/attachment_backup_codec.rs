//! Private, bounded and authenticated attachment-key recovery format.
use super::attachment_key::{self, AttachmentKeyId};
use crate::error::{CrosstacheError, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use zeroize::Zeroizing;

pub(crate) const MAX_BUNDLE_BYTES: usize = 16 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Bundle {
    pub format: String,
    pub schema_version: u32,
    pub source_backend: String,
    pub source_vault: String,
    pub created_at: String,
    pub active_key_id: String,
    pub legacy_key_id: Option<String>,
    pub identities: Vec<IdentityRecord>,
    pub references: Vec<SourceRef>,
    pub files: Vec<ManifestFile>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IdentityRecord {
    pub key_id: String,
    pub identity: Zeroizing<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceRef {
    pub key_id: String,
    pub slot: String,
    pub provider_version: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManifestFile {
    pub name: String,
    pub ciphertext_sha256: String,
    pub key_id: String,
    pub source_ref: Option<SourceRef>,
}
fn invalid() -> CrosstacheError {
    CrosstacheError::InvalidArgument("Invalid attachment key backup bundle".into())
}
/// Validate all portable relationships before any provider access.
pub(crate) fn validate(bundle: &Bundle) -> Result<()> {
    if bundle.format != "xv-attachment-key-backup"
        || bundle.schema_version != 1
        || bundle.identities.is_empty()
        || bundle.identities.len() > 10_000
        || bundle.files.len() > 100_000
    {
        return Err(invalid());
    }
    let mut identities = HashSet::new();
    for record in &bundle.identities {
        let id = AttachmentKeyId::parse(&record.key_id).ok_or_else(invalid)?;
        let identity: age::x25519::Identity =
            record.identity.trim().parse().map_err(|_| invalid())?;
        if id != AttachmentKeyId::derive(&identity.to_public().to_string())
            || !identities.insert(record.key_id.as_str())
        {
            return Err(invalid());
        }
    }
    if !identities.contains(bundle.active_key_id.as_str())
        || bundle
            .legacy_key_id
            .as_ref()
            .is_some_and(|id| !identities.contains(id.as_str()))
    {
        return Err(invalid());
    }
    let mut references = HashSet::new();
    let mut bindings = HashMap::new();
    for reference in &bundle.references {
        let metadata = HashMap::from([
            (attachment_key::META_KEY_ID.into(), reference.key_id.clone()),
            (attachment_key::META_KEY_SLOT.into(), reference.slot.clone()),
            (
                attachment_key::META_KEY_VERSION.into(),
                reference.provider_version.clone(),
            ),
        ]);
        if attachment_key::parse_key_ref_from_metadata(&metadata).is_none()
            || reference.provider_version.chars().any(char::is_control)
            || !identities.contains(reference.key_id.as_str())
            || !references.insert(reference)
        {
            return Err(invalid());
        }
        // The legacy slot resolves one pointer record, so its exact version
        // cannot name two identities. Retained versions belong to distinct
        // record names derived from each key ID and may reuse version tokens.
        let binding = (reference.slot.as_str(), reference.provider_version.as_str());
        if reference.slot == "legacy"
            && bindings
                .insert(binding, reference.key_id.as_str())
                .is_some_and(|prior| prior != reference.key_id)
        {
            return Err(invalid());
        }
    }
    let mut names = HashSet::new();
    for file in &bundle.files {
        if !safe_name(&file.name)
            || !names.insert(file.name.as_str())
            || !identities.contains(file.key_id.as_str())
            || file.ciphertext_sha256.len() != 64
            || !file
                .ciphertext_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(invalid());
        }
        match &file.source_ref {
            Some(reference)
                if reference.key_id == file.key_id && references.contains(reference) => {}
            None if bundle.legacy_key_id.as_deref() == Some(file.key_id.as_str()) => {}
            _ => return Err(invalid()),
        }
    }
    // Also bound caller-created DTOs, not only documents read from ciphertext.
    serde_json::to_writer(
        BoundedWriter {
            buffer: None,
            written: 0,
        },
        bundle,
    )
    .map_err(|_| size_error())?;
    Ok(())
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('\\')
        && !name.chars().any(char::is_control)
        && !name
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        && !name
            .as_bytes()
            .get(1)
            .is_some_and(|b| *b == b':' && name.as_bytes()[0].is_ascii_alphabetic())
}
fn size_error() -> CrosstacheError {
    CrosstacheError::InvalidArgument("Attachment key backup exceeds the 16 MiB size limit".into())
}
/// A capped sink keeps both serialization allocation and size validation bounded.
struct BoundedWriter<'a> {
    buffer: Option<&'a mut Vec<u8>>,
    written: usize,
}
impl Write for BoundedWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_BUNDLE_BYTES - self.written {
            return Err(std::io::Error::other("bundle size limit"));
        }
        if let Some(buffer) = &mut self.buffer {
            buffer.extend_from_slice(bytes);
        }
        self.written += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(crate) fn encrypt(bundle: &Bundle, recipient: &age::x25519::Recipient) -> Result<Vec<u8>> {
    validate(bundle)?;
    let mut plaintext = Zeroizing::new(Vec::new());
    serde_json::to_writer(
        BoundedWriter {
            buffer: Some(&mut plaintext),
            written: 0,
        },
        bundle,
    )
    .map_err(|_| size_error())?;
    let encryptor =
        age::Encryptor::with_recipients(vec![Box::new(recipient.clone())]).ok_or_else(invalid)?;
    let mut ciphertext = Vec::new();
    let mut writer = encryptor
        .wrap_output(BoundedWriter {
            buffer: Some(&mut ciphertext),
            written: 0,
        })
        .map_err(|_| invalid())?;
    writer.write_all(&plaintext).map_err(|_| size_error())?;
    writer.finish().map_err(|_| size_error())?;
    Ok(ciphertext)
}

pub(crate) fn decrypt(bytes: &[u8], identity: &age::x25519::Identity) -> Result<Bundle> {
    if bytes.len() > MAX_BUNDLE_BYTES {
        return Err(size_error());
    }
    if bytes.is_empty() {
        return Err(invalid());
    }
    // Reject scrypt envelopes before processing a password work factor.
    let decryptor = match age::Decryptor::new_buffered(bytes).map_err(|_| invalid())? {
        age::Decryptor::Recipients(recipients) => recipients,
        age::Decryptor::Passphrase(_) => return Err(invalid()),
    };
    let reader = decryptor
        .decrypt(std::iter::once(identity as &dyn age::Identity))
        .map_err(|_| invalid())?;
    let mut plaintext = Zeroizing::new(Vec::new());
    reader
        .take((MAX_BUNDLE_BYTES + 1) as u64)
        .read_to_end(&mut plaintext)
        .map_err(|_| invalid())?;
    if plaintext.len() > MAX_BUNDLE_BYTES {
        return Err(size_error());
    }
    // from_slice rejects trailing JSON values and serde's struct visitors reject
    // repeated fields. Never propagate parser text containing private input.
    let bundle: Bundle = serde_json::from_slice(&plaintext).map_err(|_| invalid())?;
    validate(&bundle)?;
    Ok(bundle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use age::secrecy::ExposeSecret;
    fn fixture() -> Bundle {
        let identity = age::x25519::Identity::generate();
        let id = AttachmentKeyId::derive(&identity.to_public().to_string())
            .as_str()
            .to_owned();
        let reference = SourceRef {
            key_id: id.clone(),
            slot: "retained".into(),
            provider_version: "opaque-version_1".into(),
        };
        Bundle {
            format: "xv-attachment-key-backup".into(),
            schema_version: 1,
            source_backend: "local".into(),
            source_vault: "source".into(),
            created_at: "2026-09-06T12:00:00Z".into(),
            active_key_id: id.clone(),
            legacy_key_id: Some(id.clone()),
            identities: vec![IdentityRecord {
                key_id: id.clone(),
                identity: Zeroizing::new(identity.to_string().expose_secret().clone()),
            }],
            references: vec![reference.clone()],
            files: vec![ManifestFile {
                name: "attachments/example/photo.png".into(),
                ciphertext_sha256: "ab".repeat(32),
                key_id: id,
                source_ref: Some(reference),
            }],
        }
    }
    fn raw_encrypt(bytes: &[u8], recipient: &age::x25519::Recipient) -> Vec<u8> {
        let encryptor = age::Encryptor::with_recipients(vec![Box::new(recipient.clone())]).unwrap();
        let mut bytes_out = Vec::new();
        let mut writer = encryptor.wrap_output(&mut bytes_out).unwrap();
        writer.write_all(bytes).unwrap();
        writer.finish().unwrap();
        bytes_out
    }
    #[test]
    fn accepts_identity_whitespace_supported_by_custody() {
        let mut bundle = fixture();
        bundle.identities[0].identity =
            Zeroizing::new(format!(" \n{}\r\n ", *bundle.identities[0].identity));
        assert!(validate(&bundle).is_ok());
        let recovery = age::x25519::Identity::generate();
        let encrypted = encrypt(&bundle, &recovery.to_public()).unwrap();
        let restored = decrypt(&encrypted, &recovery).unwrap();
        assert_eq!(
            *restored.identities[0].identity,
            *bundle.identities[0].identity
        );
    }
    #[test]
    fn round_trip_and_wrong_recipient() {
        let bundle = fixture();
        let recovery = age::x25519::Identity::generate();
        let bytes = encrypt(&bundle, &recovery.to_public()).unwrap();
        assert!(!bytes
            .windows(bundle.identities[0].identity.len())
            .any(|w| w == bundle.identities[0].identity.as_bytes()));
        let restored = decrypt(&bytes, &recovery).unwrap();
        assert_eq!(
            *restored.identities[0].identity,
            *bundle.identities[0].identity
        );
        assert!(decrypt(&bytes, &age::x25519::Identity::generate()).is_err());
    }
    #[test]
    fn rejects_tampering_truncation_and_extra_ciphertext() {
        let recovery = age::x25519::Identity::generate();
        let bytes = raw_encrypt(
            &serde_json::to_vec(&fixture()).unwrap(),
            &recovery.to_public(),
        );
        for len in [0, 1, bytes.len() / 2, bytes.len() - 1] {
            assert!(decrypt(&bytes[..len], &recovery).is_err());
        }
        let mut modified = bytes.clone();
        *modified.last_mut().unwrap() ^= 1;
        assert!(decrypt(&modified, &recovery).is_err());
        let mut appended = bytes;
        appended.push(0);
        assert!(decrypt(&appended, &recovery).is_err());
    }
    #[test]
    fn rejects_invalid_structure() {
        type Mutation = Box<dyn Fn(&mut Bundle)>;
        let cases: Vec<Mutation> = vec![
            Box::new(|b| b.schema_version = 2),
            Box::new(|b| b.format.clear()),
            Box::new(|b| b.active_key_id = "ak1-invalid".into()),
            Box::new(|b| b.legacy_key_id = Some("ak1-invalid".into())),
            Box::new(|b| {
                b.identities[0].identity = Zeroizing::new("private-invalid-canary".into())
            }),
            Box::new(|b| b.identities[0].key_id = format!("ak1-{}", "0".repeat(64))),
            Box::new(|b| b.references.push(b.references[0].clone())),
            Box::new(|b| b.references[0].slot = "unsupported".into()),
            Box::new(|b| b.references[0].provider_version.clear()),
            Box::new(|b| b.references.clear()),
            Box::new(|b| b.files[0].ciphertext_sha256 = "AB".repeat(32)),
            Box::new(|b| b.files[0].source_ref = None),
            Box::new(|b| b.files[0].name = "../escape".into()),
        ];
        for (index, mutate) in cases.into_iter().enumerate() {
            let mut b = fixture();
            if index == 11 {
                b.legacy_key_id = None;
            }
            mutate(&mut b);
            assert!(validate(&b).is_err(), "case {index}");
        }
    }
    #[test]
    fn rejects_untrusted_json_without_leaking_it() {
        let recovery = age::x25519::Identity::generate();
        let document = serde_json::to_string(&fixture()).unwrap();
        for json in [
            format!("{document} private-canary"),
            document.replacen("{", "{\"schema_version\":1,", 1),
            document.replacen("{", "{\"private-canary\":true,", 1),
            "private-canary".into(),
        ] {
            let bytes = raw_encrypt(json.as_bytes(), &recovery.to_public());
            let error = decrypt(&bytes, &recovery).err().unwrap();
            assert!(!format!("{error:?}").contains("private-canary"));
        }
    }
    #[test]
    fn rejects_duplicates_and_ambiguous_bindings() {
        let mut b = fixture();
        b.identities.push(IdentityRecord {
            key_id: b.identities[0].key_id.clone(),
            identity: b.identities[0].identity.clone(),
        });
        assert!(validate(&b).is_err());
        let mut b = fixture();
        b.files.push(ManifestFile {
            name: b.files[0].name.clone(),
            ciphertext_sha256: b.files[0].ciphertext_sha256.clone(),
            key_id: b.files[0].key_id.clone(),
            source_ref: b.files[0].source_ref.clone(),
        });
        assert!(validate(&b).is_err());
        let mut b = fixture();
        b.references[0].slot = "legacy".into();
        b.files[0].source_ref = Some(b.references[0].clone());
        let second = fixture().identities.remove(0);
        b.references.push(SourceRef {
            key_id: second.key_id.clone(),
            ..b.references[0].clone()
        });
        b.identities.push(second);
        assert!(validate(&b).is_err());
        let mut b = fixture();
        b.files[0].source_ref.as_mut().unwrap().key_id = format!("ak1-{}", "0".repeat(64));
        assert!(validate(&b).is_err());
    }
    #[test]
    fn retained_versions_are_scoped_by_record_name() {
        let mut b = fixture();
        let second = fixture().identities.remove(0);
        b.references.push(SourceRef {
            key_id: second.key_id.clone(),
            ..b.references[0].clone()
        });
        b.identities.push(second);
        assert!(validate(&b).is_ok());
    }
    #[test]
    fn accepts_explicit_legacy_and_opaque_versions() {
        let mut b = fixture();
        b.files[0].source_ref = None;
        assert!(validate(&b).is_ok());
        let mut b = fixture();
        b.references[0].provider_version = "opaque:/version+==".into();
        b.files[0].source_ref = Some(b.references[0].clone());
        assert!(validate(&b).is_ok());
    }
    #[test]
    fn rejects_passphrase_without_work_factor_processing() {
        let recovery = age::x25519::Identity::generate();
        // A valid scrypt stanza with an excessive work factor: decoding the
        // envelope is cheap; attempting password recovery would be expensive.
        let bytes = b"age-encryption.org/v1\n-> scrypt AAAAAAAAAAAAAAAAAAAAAA 99\nAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n--- AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n";
        let mut envelope = bytes.to_vec();
        envelope.extend_from_slice(&[0; 16]);
        assert!(matches!(
            age::Decryptor::new_buffered(envelope.as_slice()).unwrap(),
            age::Decryptor::Passphrase(_)
        ));
        assert!(decrypt(&envelope, &recovery).is_err());
    }
    #[test]
    fn rejects_size_limits() {
        let recovery = age::x25519::Identity::generate();
        assert!(decrypt(&vec![0; MAX_BUNDLE_BYTES + 1], &recovery).is_err());
        let mut bundle = fixture();
        bundle.source_vault = "x".repeat(MAX_BUNDLE_BYTES);
        assert!(encrypt(&bundle, &recovery.to_public()).is_err());
        let bytes = raw_encrypt(&vec![b' '; MAX_BUNDLE_BYTES + 1], &recovery.to_public());
        assert!(decrypt(&bytes, &recovery).is_err());
    }
}
