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

pub mod disclosure;
pub mod metadata;
pub mod request;
pub mod secret;
pub mod value;

// `ConnectionComponent` and `connection_string_key_description` are reached
// through `disclosure::` by the only caller today, so the `xv` binary's own
// module tree sees these re-exports as unused. Remove once Task 3 wires the
// domain types into the remaining call sites.
#[allow(unused_imports)]
pub use disclosure::{
    connection_string_key_description, parse_connection_components, ConnectionComponent,
};
pub use metadata::{DeletedSecretSummary, FieldUpdate, SecretAttributesUpdate, SecretSummary};
pub use request::{SecretRequest, SecretUpdateRequest};
pub use secret::{SecretProperties, SecretSnapshot};
// See the matching `#[allow(dead_code)]` note in `value.rs`: nothing outside
// tests calls `SecretValue` yet in the `xv` binary compile. Remove once Task 3
// wires the value type into real call sites.
#[allow(unused_imports)]
pub use value::SecretValue;
