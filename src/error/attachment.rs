//! Attachment failures carry only a typed reason, never key or provider data.
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[cfg_attr(not(feature = "file-ops"), allow(dead_code))]
pub enum AttachmentError {
    #[error("The attachment key or referenced key version is missing.")]
    KeyMissing,
    #[error("The attachment key record does not contain a valid age identity.")]
    KeyInvalid,
    #[error("The attachment key pointer is missing a value or is malformed.")]
    PointerInvalid,
    #[error("The attachment key does not match the expected key ID.")]
    KeyMismatch,
    #[error("The attachment key version is missing or failed exact-version verification.")]
    KeyVersionInvalid,
    #[error("The attachment key pointer publication could not be confirmed.")]
    CommitUnconfirmed,
    #[error("Could not initialize the attachment key ring because retained record names kept conflicting.")]
    InitializationConflict,
    #[error("The attachment key reference is missing, malformed, or uses an unsupported schema.")]
    ReferenceInvalid,
    #[error("The managed attachment is not age ciphertext; refusing to return it as plaintext.")]
    NotCiphertext,
    #[error("The attachment could not be decrypted with its required key.")]
    DecryptionFailed,
    #[error("The backend cannot provide a consistent attachment download snapshot.")]
    SnapshotUnsupported,
}

impl AttachmentError {
    pub fn code(self) -> &'static str {
        match self {
            Self::KeyMissing => "xv-attachment-key-missing",
            Self::KeyInvalid => "xv-attachment-key-invalid",
            Self::PointerInvalid => "xv-attachment-pointer-invalid",
            Self::KeyMismatch => "xv-attachment-key-mismatch",
            Self::KeyVersionInvalid => "xv-attachment-key-version-invalid",
            Self::CommitUnconfirmed => "xv-attachment-commit-unconfirmed",
            Self::InitializationConflict => "xv-attachment-initialization-conflict",
            Self::ReferenceInvalid => "xv-attachment-reference-invalid",
            Self::NotCiphertext => "xv-attachment-not-ciphertext",
            Self::DecryptionFailed => "xv-attachment-decryption-failed",
            Self::SnapshotUnsupported => "xv-attachment-snapshot-unsupported",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::KeyMissing => "Restore the original key record and exact version from backup; do not generate a replacement key.",
            Self::KeyInvalid => "Restore the original key record from backup; do not overwrite it with a new identity.",
            Self::PointerInvalid => "Inspect the key-ring backup and restore its valid active pointer before retrying.",
            Self::KeyMismatch => "Verify the vault and restore the key matching the attachment reference; do not substitute another key.",
            Self::KeyVersionInvalid => "Check provider version support and the original key version before retrying.",
            Self::CommitUnconfirmed => "Check the vault connection and active pointer, then retry; keep committed retained keys.",
            Self::InitializationConflict => "Inspect existing retained records without overwriting them, then retry initialization.",
            Self::ReferenceInvalid => "Use a compatible client and restore the original attachment metadata; no fallback key will be tried.",
            Self::NotCiphertext => "Restore the encrypted attachment bytes and metadata from a trusted backup.",
            Self::DecryptionFailed => "Verify the vault and restore the original attachment and key version from backup.",
            Self::SnapshotUnsupported => "Use a backend with consistent file snapshots; separate byte and metadata reads are unsafe.",
        }
    }
}
