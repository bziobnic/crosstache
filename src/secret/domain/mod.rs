//! Backend-neutral secret domain model.
//!
//! Plaintext lives only in [`SecretValue`], which cannot be serialized,
//! displayed, or dereferenced; the single read is
//! [`SecretValue::expose_secret`], so `grep expose_secret` lists every
//! plaintext read in the crate. Metadata types (`SecretMetadata`,
//! `SecretSummary`, `DeletedSecretSummary`) are value-free and serializable.
//! Value-bearing composites (`SecretProperties`, `SecretRequest`,
//! `SecretUpdateRequest`, `SecretSnapshot`) derive `Debug` (redacted through
//! `SecretValue`) but never serde; `SecretProperties::into_metadata` is the
//! one way a secret becomes serializable, and it drops the plaintext.
//!
//! This module imports nothing from `crate::backend`, `crate::cli`,
//! `crate::web`, or `crate::secret::manager`; adapters translate provider
//! wire formats into these types at one place each.

pub mod disclosure;
pub mod metadata;
pub mod request;
pub mod secret;
pub mod value;

// `ConnectionComponent` and `connection_string_key_description` stay reachable
// through `disclosure::`; only the parser has a caller worth a short path.
pub use disclosure::parse_connection_components;
pub use metadata::{DeletedSecretSummary, FieldUpdate, SecretAttributesUpdate, SecretSummary};
pub use request::{SecretRequest, SecretUpdateRequest};
pub use secret::{SecretMetadata, SecretProperties, SecretSnapshot};
pub use value::SecretValue;
