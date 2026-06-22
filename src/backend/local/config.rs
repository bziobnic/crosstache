//! Local backend configuration.
//!
//! [`ResolvedLocalConfig`] resolves raw [`LocalConfig`] values from the config
//! file into concrete [`PathBuf`]s with sane defaults.

use std::path::PathBuf;

use crate::config::settings::LocalConfig;

/// Fully resolved configuration for the local age-encrypted backend.
#[derive(Debug, Clone)]
pub struct ResolvedLocalConfig {
    /// Root directory for the encrypted secret store (contains `vaults/`).
    pub store_path: PathBuf,
    /// Path to the age identity (private key) file.
    pub key_file: PathBuf,
    /// Path to the age recipients (public key) file.
    pub recipients_file: PathBuf,
    /// Default vault name used when no `--vault` flag is given.
    pub default_vault: String,
    /// Whether to encrypt secret metadata (`.meta.json`) at rest. Defaults to
    /// `false` for backward compatibility with existing plaintext stores.
    pub encrypt_metadata: bool,
    /// Whether on-disk filenames are opaque (keyed-hash stems + encrypted
    /// index). Defaults to `false`, leaving existing stores byte-for-byte
    /// unchanged until migrated.
    pub opaque_filenames: bool,
}

impl ResolvedLocalConfig {
    /// Resolve the raw [`LocalConfig`] into concrete paths.
    ///
    /// Defaults:
    /// - `store_path`: `~/.xv/store`
    /// - `key_file`:   `~/.xv/key.txt`
    /// - `default_vault`: `"default"`
    pub fn from_raw(raw: Option<&LocalConfig>) -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let xv_dir = home.join(".xv");

        let store_path = raw
            .and_then(|c| c.store_path.as_deref())
            .map(PathBuf::from)
            .unwrap_or_else(|| xv_dir.join("store"));

        let key_file = raw
            .and_then(|c| c.key_file.as_deref())
            .map(PathBuf::from)
            .unwrap_or_else(|| xv_dir.join("key.txt"));

        let recipients_file = key_file.parent().unwrap_or(&xv_dir).join("recipients.txt");

        let default_vault = raw
            .and_then(|c| c.default_vault.as_deref())
            .unwrap_or("default")
            .to_string();

        let encrypt_metadata = raw.and_then(|c| c.encrypt_metadata).unwrap_or(false);
        let opaque_filenames = raw.and_then(|c| c.opaque_filenames).unwrap_or(false);

        Self {
            store_path,
            key_file,
            recipients_file,
            default_vault,
            encrypt_metadata,
            opaque_filenames,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let cfg = ResolvedLocalConfig::from_raw(None);
        assert!(cfg.store_path.to_string_lossy().contains(".xv"));
        assert!(cfg.key_file.to_string_lossy().contains("key.txt"));
        assert_eq!(cfg.default_vault, "default");
    }

    #[test]
    fn overrides_take_effect() {
        let raw = LocalConfig {
            store_path: Some("/tmp/my-store".into()),
            key_file: Some("/tmp/my-key.txt".into()),
            default_vault: Some("staging".into()),
            encrypt_metadata: None,
            opaque_filenames: None,
        };
        let cfg = ResolvedLocalConfig::from_raw(Some(&raw));
        assert_eq!(cfg.store_path, PathBuf::from("/tmp/my-store"));
        assert_eq!(cfg.key_file, PathBuf::from("/tmp/my-key.txt"));
        assert_eq!(cfg.default_vault, "staging");
        assert!(!cfg.encrypt_metadata);
    }

    #[test]
    fn encrypt_metadata_opt_in() {
        let raw = LocalConfig {
            store_path: None,
            key_file: None,
            default_vault: None,
            encrypt_metadata: Some(true),
            opaque_filenames: None,
        };
        let cfg = ResolvedLocalConfig::from_raw(Some(&raw));
        assert!(cfg.encrypt_metadata);
        assert!(!cfg.opaque_filenames);
    }
}
