//! Client-side cache for expensive listing operations.
//!
//! Caches responses from `xv ls`, `xv vault list`, and `xv file list`
//! as private (0600) JSON files organized by config-identity fingerprint,
//! backend, and vault. Supports configurable TTL, background refresh, and
//! eager, mutation-driven invalidation on writes.
//!
//! Security posture (v5 hardening):
//! - All cache I/O uses private modes (0600 files / 0700 dirs) and no-follow
//!   atomic writes (see `manager`).
//! - Entries are isolated per account/config via a path fingerprint so two
//!   identities reached through the same backend NAME never share files
//!   (see `fingerprint`).
//! - Mutation-driven invalidation is centralized behind `invalidation`.
//! - Unparseable entries are quarantined to `<name>.corrupt` rather than
//!   silently rewritten forever.
//! - `XV_CACHE_STRICT` promotes cache-failure logs from `debug!` to `warn!`
//!   without ever making a cache failure fatal — see [`strict_mode`].
//!
//! This module is deliberately a *listing metadata* cache only. It never
//! stores secret values, and must not be widened to.

pub mod fingerprint;
pub mod invalidation;
pub mod manager;
pub mod models;
pub mod refresh;

#[allow(unused_imports)]
pub use fingerprint::config_fingerprint;
pub use manager::CacheManager;
#[allow(unused_imports)]
pub use models::{CacheEntry, CacheEntryType, CacheKey, CacheStatus};

/// Whether the cache is in strict/fail-loud mode, requested via the
/// `XV_CACHE_STRICT` environment variable (`1` or `true`, case-insensitive).
///
/// Strict mode promotes cache-failure logs (write errors, quarantine events,
/// permission-tightening failures) from `debug!` to `warn!` so CI can see cache
/// trouble. It NEVER changes the return contract: `get` still returns `Option`,
/// and commands still succeed with a broken cache. Loud, not fatal.
pub fn strict_mode() -> bool {
    matches!(
        std::env::var("XV_CACHE_STRICT")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true"
    )
}
