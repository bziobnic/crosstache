//! Record types: structured secrets with typed fields.
//!
//! See `docs/superpowers/specs/2026-07-03-record-types-design.md` for the
//! design. This module owns type definitions/resolution (`types`) and the
//! JSON envelope codec (`envelope`) used to store secret-kind fields in the
//! backend secret value.

pub mod envelope;
pub mod types;

pub use envelope::{
    encode_envelope, is_record, parse_envelope, FIELD_TAG_PREFIX, RECORD_CONTENT_TYPE, TYPE_TAG,
};
pub use types::{builtin_types, FieldDef, FieldKind, RecordType, TypeSource};
