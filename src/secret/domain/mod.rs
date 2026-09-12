//! Backend-neutral secret domain model.
//!
//! Plaintext lives only in [`SecretValue`], which cannot be serialized,
//! displayed, or dereferenced; the single read is
//! [`SecretValue::expose_secret`], so `grep expose_secret` lists every
//! plaintext read in the crate — filtered to `SecretValue` receivers, since
//! age's `secrecy::ExposeSecret` shares the same method name for identity
//! material (see `src/secret/attachment*`, `src/backend/local/`,
//! `src/backend/attachment_key*`). Metadata types (`SecretMetadata`,
//! `SecretSummary`, `DeletedSecretSummary`) are value-free and serializable.
//! Value-bearing composites (`Secret`, `SecretRequest`,
//! `SecretUpdateRequest`, `SecretSnapshot`) derive `Debug` (redacted through
//! `SecretValue`) but never serde; `Secret::into_metadata` is the
//! one way a secret becomes serializable without its plaintext, and
//! [`Secret::disclose`] is the only way it becomes serializable *with* it —
//! so `grep -rn "\.disclose(" src` lists every boundary that serializes a
//! *whole secret value*. The one field-level exception is
//! `xv get --record --format json|yaml`, which serializes decoded envelope
//! fields via `expose_secret` rather than `disclose`; it is listed in
//! `docs/security.md`.
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
pub use disclosure::{parse_connection_components, DisclosedSecret};
pub use metadata::{DeletedSecretSummary, FieldUpdate, SecretAttributesUpdate, SecretSummary};
pub use request::{SecretRequest, SecretUpdateRequest};
pub use secret::{Secret, SecretMetadata, SecretSnapshot, SnapshotValue};
pub use value::SecretValue;
