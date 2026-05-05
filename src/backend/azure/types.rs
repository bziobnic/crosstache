//! Azure-specific validated types.

use std::fmt;

use url::Url;

use crate::error::CrosstacheError;

/// Validated Azure Key Vault name.
///
/// Azure Key Vault names must be 3-24 characters, start with a letter, end
/// with a letter or number, contain only letters, numbers, and hyphens, and
/// must not contain consecutive hyphens.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AzureVaultName(String);

impl AzureVaultName {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Build the Key Vault data-plane base URL for this vault.
    pub fn key_vault_url(&self) -> Result<Url, CrosstacheError> {
        let mut url = Url::parse("https://vault.azure.net/").map_err(|e| {
            CrosstacheError::invalid_url(format!("Invalid Key Vault base URL: {e}"))
        })?;
        url.set_host(Some(&format!("{}.vault.azure.net", self.0)))
            .map_err(|_| {
                CrosstacheError::invalid_url(format!(
                    "Invalid Azure Key Vault host for vault '{}'",
                    self.0
                ))
            })?;
        Ok(url)
    }
}

impl fmt::Display for AzureVaultName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<&str> for AzureVaultName {
    type Error = CrosstacheError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        validate_vault_name(value)?;
        Ok(Self(value.to_string()))
    }
}

impl TryFrom<String> for AzureVaultName {
    type Error = CrosstacheError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        validate_vault_name(&value)?;
        Ok(Self(value))
    }
}

fn validate_vault_name(value: &str) -> Result<(), CrosstacheError> {
    let invalid = || {
        CrosstacheError::invalid_argument(format!(
            "Invalid Azure Key Vault name '{value}'. Vault names must match \
             ^[a-zA-Z][a-zA-Z0-9-]{{1,22}}[a-zA-Z0-9]$, be 3-24 characters, \
             and not contain consecutive hyphens."
        ))
    };

    let len = value.len();
    if !(3..=24).contains(&len) {
        return Err(invalid());
    }

    let bytes = value.as_bytes();
    if !bytes[0].is_ascii_alphabetic() || !bytes[len - 1].is_ascii_alphanumeric() {
        return Err(invalid());
    }

    if value.contains("--") {
        return Err(invalid());
    }

    if !bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
    {
        return Err(invalid());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_vault_names() {
        for name in [
            "abc",
            "a1b",
            "vault-name",
            "MyVault123",
            "a-1234567890123456789012",
        ] {
            let vault = AzureVaultName::try_from(name).expect(name);
            assert_eq!(vault.as_str(), name);
            assert_eq!(
                vault.key_vault_url().unwrap().as_str(),
                format!("https://{}.vault.azure.net/", name.to_ascii_lowercase())
            );
        }
    }

    #[test]
    fn rejects_path_traversal_characters() {
        for name in ["ab/../evil", "ab..", "ab\\evil"] {
            assert!(AzureVaultName::try_from(name).is_err(), "{name}");
        }
    }

    #[test]
    fn rejects_url_delimiters() {
        for name in ["ab@evil", "ab/evil", "ab?evil", "ab#evil", "ab:443"] {
            assert!(AzureVaultName::try_from(name).is_err(), "{name}");
        }
    }

    #[test]
    fn rejects_consecutive_hyphens() {
        assert!(AzureVaultName::try_from("ab--cd").is_err());
    }

    #[test]
    fn rejects_too_short_or_too_long_names() {
        assert!(AzureVaultName::try_from("ab").is_err());
        assert!(AzureVaultName::try_from("a234567890123456789012345").is_err());
    }
}
