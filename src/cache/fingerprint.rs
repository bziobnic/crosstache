//! Account/config fingerprint for the cache path (v5 layout).
//!
//! Cache paths historically keyed on `(backend, vault)` only. Two different
//! accounts/tenants/configs reached through the same backend NAME (e.g. two
//! Azure tenants both using the `azure` backend, or a real and a LocalStack
//! AWS endpoint both using `aws`) therefore shared cache files — one account
//! could be served the other's cached listing.
//!
//! [`config_fingerprint`] derives a short, stable identifier from the resolved
//! config so the cache root can be split per identity
//! (`cache_dir/<fingerprint>/<backend>/<vault>/…`). The hash input is
//! deterministic and contains **no secret material, timestamps, or
//! randomness** — only the resolved global config path and the identity fields
//! of every built-in backend the config configures.
//!
//! Crucially the fingerprint is **config-level, not active-backend-level**: it
//! does NOT depend on which backend happens to be active for a given
//! invocation. This is load-bearing. A single command can read one backend's
//! listing while *writing* (and invalidating) a different backend's — a
//! workspace-qualified write (`xv set work:SECRET`) invalidates the entry a
//! plain `xv --backend <entry> ls` populated. Those two invocations run with
//! different *active* backends, so keying the fingerprint on the active backend
//! would send the write's invalidation to a different fingerprint directory
//! than the read's entry, leaving a stale hit. Because the cache key already
//! carries the backend NAME as its own path component, per-identity isolation
//! only needs the config-level account fields; the active selection must stay
//! out of the hash. It also means the background refresh child process
//! (`xv cache refresh --key …`), which re-resolves config independently, lands
//! on the exact same fingerprint.

use sha2::{Digest, Sha256};

use crate::config::Config;

/// Number of leading hex characters kept from the SHA-256 digest. 16 hex chars
/// = 64 bits, ample to separate a handful of local configs without bloating the
/// path. Never treated as a security boundary — only an isolation key.
const FINGERPRINT_HEX_LEN: usize = 16;

/// Compute the account/config fingerprint used as the top-level cache path
/// component in the v5 layout.
///
/// The result is a lowercase hex string of length [`FINGERPRINT_HEX_LEN`]. It is
/// a pure, config-level function of `config` (and the process's resolved global
/// config path) — it does not depend on which backend is active — so the read
/// path, the invalidating write path, and the `xv cache refresh` child process
/// all agree on it (see the module docs).
pub fn config_fingerprint(config: &Config) -> String {
    let mut hasher = Sha256::new();

    // Domain separator + layout version: if the fingerprint recipe ever changes
    // in a way that should force a miss, bump this string.
    hasher.update(b"xv-cache-fingerprint-v5\n");

    // Resolved global config path. Two stores driven by different config files
    // are distinct identities even if their fields happen to match.
    let config_path = Config::get_config_path()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    hasher.update(b"config_path=");
    hasher.update(config_path.as_bytes());
    hasher.update(b"\n");

    // Identity fields of every built-in backend the config carries, hashed
    // unconditionally so the fingerprint is stable no matter which one is
    // active. Env-var/CLI overrides fold into `config` before this runs, so a
    // different account reached through the same backend name yields a
    // different fingerprint. Named backends need no entry here: the cache key's
    // own backend-name path component already separates them, and two configs
    // that differ only in a named backend differ by `config_path`.
    let azure = config.azure_settings();
    hasher.update(b"azure.tenant_id=");
    hasher.update(azure.tenant_id.unwrap_or_default().as_bytes());
    hasher.update(b"\nazure.subscription_id=");
    hasher.update(azure.subscription_id.unwrap_or_default().as_bytes());
    hasher.update(b"\n");

    let aws = config.aws.clone().unwrap_or_default();
    hasher.update(b"aws.region=");
    hasher.update(aws.region.unwrap_or_default().as_bytes());
    hasher.update(b"\naws.profile=");
    hasher.update(aws.profile.unwrap_or_default().as_bytes());
    hasher.update(b"\naws.endpoint_url=");
    hasher.update(aws.endpoint_url.unwrap_or_default().as_bytes());
    hasher.update(b"\n");

    // Resolved local store path, so a config that omits `store_path` still
    // hashes to the concrete default location it actually uses.
    let local = crate::backend::local::config::ResolvedLocalConfig::from_raw(config.local.as_ref());
    hasher.update(b"local.store_path=");
    hasher.update(local.store_path.to_string_lossy().as_bytes());
    hasher.update(b"\n");

    let digest = hasher.finalize();
    let mut hex = hex::encode(digest);
    hex.truncate(FINGERPRINT_HEX_LEN);
    hex
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::settings::{AwsConfig, AzureConfig, LocalConfig};

    fn azure_config(tenant: &str, subscription: &str) -> Config {
        Config {
            backend: Some("azure".to_string()),
            azure: Some(AzureConfig {
                tenant_id: Some(tenant.to_string()),
                subscription_id: Some(subscription.to_string()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn fingerprint_is_deterministic_for_same_config() {
        let config = azure_config("tenant-a", "sub-a");
        assert_eq!(config_fingerprint(&config), config_fingerprint(&config));
    }

    #[test]
    fn fingerprint_has_expected_shape() {
        let fp = config_fingerprint(&azure_config("tenant-a", "sub-a"));
        assert_eq!(fp.len(), FINGERPRINT_HEX_LEN);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn fingerprint_differs_when_tenant_differs() {
        let a = config_fingerprint(&azure_config("tenant-a", "sub-shared"));
        let b = config_fingerprint(&azure_config("tenant-b", "sub-shared"));
        assert_ne!(a, b);
    }

    #[test]
    fn fingerprint_differs_when_subscription_differs() {
        let a = config_fingerprint(&azure_config("tenant-shared", "sub-a"));
        let b = config_fingerprint(&azure_config("tenant-shared", "sub-b"));
        assert_ne!(a, b);
    }

    #[test]
    fn fingerprint_differs_across_backends() {
        let aws = Config {
            backend: Some("aws".to_string()),
            aws: Some(AwsConfig {
                region: Some("us-east-1".to_string()),
                profile: Some("default".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let local = Config {
            backend: Some("local".to_string()),
            local: Some(LocalConfig {
                store_path: Some("/tmp/store-x".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let azure = azure_config("tenant-a", "sub-a");
        let fa = config_fingerprint(&aws);
        let fl = config_fingerprint(&local);
        let fz = config_fingerprint(&azure);
        assert_ne!(fa, fl);
        assert_ne!(fa, fz);
        assert_ne!(fl, fz);
    }

    #[test]
    fn fingerprint_differs_when_aws_endpoint_differs() {
        let real = Config {
            backend: Some("aws".to_string()),
            aws: Some(AwsConfig {
                region: Some("us-east-1".to_string()),
                profile: Some("default".to_string()),
                endpoint_url: None,
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut localstack = real.clone();
        localstack.aws = Some(AwsConfig {
            region: Some("us-east-1".to_string()),
            profile: Some("default".to_string()),
            endpoint_url: Some("http://localhost:4566".to_string()),
            ..Default::default()
        });
        assert_ne!(config_fingerprint(&real), config_fingerprint(&localstack));
    }
}
