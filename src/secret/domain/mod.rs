//! Backend-neutral secret domain model.
//!
//! Plaintext lives only in [`SecretValue`], which cannot be serialized,
//! displayed, or dereferenced; the single read is
//! [`SecretValue::expose_secret`], so `grep expose_secret` lists every
//! plaintext read in the crate. Metadata types (`SecretMetadata`,
//! `SecretSummary`, `DeletedSecretSummary`) are value-free and serializable.
//! Value-bearing composites (`SecretProperties`, `SecretRequest`,
//! `SecretUpdateRequest`, `SecretSnapshot`) derive `Debug` (redacted through
//! `SecretValue`) but never serde.
//!
//! This module imports nothing from `crate::backend`, `crate::cli`,
//! `crate::web`, or `crate::secret::manager`; adapters translate provider
//! wire formats into these types at one place each.

pub mod value;

// See the matching `#[allow(dead_code)]` note in `value.rs`: nothing outside
// tests calls `SecretValue` yet in this task's `xv` binary compile. Remove
// once Task 2 wires the domain types into real call sites.
#[allow(unused_imports)]
pub use value::SecretValue;
