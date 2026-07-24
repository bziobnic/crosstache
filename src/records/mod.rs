//! Record types: structured secrets with typed fields.
//!
//! See `docs/superpowers/specs/2026-07-03-record-types-design.md` for the
//! design. This module owns type definitions/resolution (`types`) and the
//! JSON envelope codec (`envelope`) used to store secret-kind fields in the
//! backend secret value.

pub mod conversion;
pub mod envelope;
pub mod types;

// Re-exports consumed by CLI wiring added later in Phase A (Tasks 4/6/7);
// unused from the `xv` binary target until then.
#[allow(unused_imports)]
pub use conversion::{
    apply_atomic_conversion, apply_conversion, preview_conversion,
    validate_conditional_conversion_backend, validate_conversion_backend, ConversionPreview,
    ConversionRequest, ConversionTarget,
};
#[allow(unused_imports)]
pub use envelope::{
    encode_envelope, is_record, parse_envelope, FIELD_TAG_PREFIX, RECORD_CONTENT_TYPE, TYPE_TAG,
};
#[allow(unused_imports)]
pub use types::{
    builtin_types, find_type, resolve_types, FieldDef, FieldDefConfig, FieldKind, RecordType,
    RecordTypeConfig, TypeSource,
};

use crate::backend::BackendCapabilities;
use crate::error::{CrosstacheError, Result};
use std::collections::BTreeMap;

/// Checks that a record write stays within the active backend's tag
/// budget, failing *before* any backend call.
///
/// `reserved_count` is the number of reserved bookkeeping tags actually
/// present on this write (`xv-type`, `groups`, `note`, `folder`,
/// `original_name`, `created_by`); `field_tags` are the record's `f.*`
/// metadata-field tags; `user_tags` are additional user-supplied
/// (`--tag`) tags. A backend with `max_tags = None` (unbounded) never
/// errors on count. Both `field_tags` and `user_tags` values are checked
/// against `max_tag_value_len` (when set) independently of the count
/// check — a user tag is exactly as capable of exceeding the backend's
/// per-value limit as a metadata field is, so skipping it here would let
/// an oversized `--tag` pass this pre-check and fail at the backend,
/// contradicting fail-before-write.
#[allow(dead_code)] // consumed by `xv set --type` / `xv update --field` (Tasks 6/8)
pub fn check_tag_budget(
    caps: &BackendCapabilities,
    reserved_count: usize,
    field_tags: &BTreeMap<String, String>,
    user_tags: &BTreeMap<String, String>,
) -> Result<()> {
    if let Some(max_len) = caps.max_tag_value_len {
        for (field, value) in field_tags {
            if value.len() > max_len {
                return Err(CrosstacheError::config(format!(
                    "field '{field}' value is {} characters, exceeding the backend's {max_len}-character \
                     tag value limit. Consider declaring it kind = \"secret\" instead of metadata.",
                    value.len()
                )));
            }
        }
        for (tag, value) in user_tags {
            if value.len() > max_len {
                return Err(CrosstacheError::config(format!(
                    "--tag '{tag}' value is {} characters, exceeding the backend's {max_len}-character \
                     tag value limit.",
                    value.len()
                )));
            }
        }
    }

    if let Some(max_tags) = caps.max_tags {
        let field_count = field_tags.len();
        let user_tag_count = user_tags.len();
        let total = reserved_count + field_count + user_tag_count;
        if total > max_tags {
            return Err(CrosstacheError::config(format!(
                "record would use {total} tags, exceeding the backend's {max_tags}-tag limit \
                 (reserved: {reserved_count}, fields: {field_count}, user: {user_tag_count}). \
                 Reduce the number of metadata fields or user tags, or move a field to kind = \"secret\"."
            )));
        }
    }

    Ok(())
}

/// Computes `reserved_count` for [`check_tag_budget`]: the number of
/// reserved bookkeeping tags a record write actually consumes on the wire
/// for a given backend.
///
/// This single, backend-aware predictor replaces two earlier functions
/// (a universal `reserved_tag_count` plus an AWS-only
/// `backend_extra_reserved_tags` add-on) that together mis-modeled two
/// backends at once: the universal count assumed every backend always
/// writes `created_by` and a `note` tag, which is true for Azure but not
/// for AWS (see the per-backend derivation below) — so near AWS's 50-tag
/// cap the old model could *falsely reject* a write that would actually
/// succeed. Each arm here is derived directly from what that backend's
/// `set_secret` implementation actually puts on the wire, so it can't
/// drift the same way again.
///
/// - `has_type`: the reserved `xv-type` tag is always present on a record
///   write, on every backend (it rides in `SecretRequest.tags`, not a
///   backend-specific field, so it costs the same slot everywhere).
/// - `has_groups` / `has_note` / `has_folder` / `has_expiry`: whether this
///   specific write sets `SecretRequest.groups` / `.note` / `.folder` /
///   `.expires_on`.
///
/// ## Per-backend derivation
///
/// **Azure** — `SecretManager::prepare_secret_request` (`src/secret/manager.rs`,
/// the path `xv set`'s full write always takes; `xv update`'s
/// attributes-only PATCH via `azure::secrets::build_patched_tags` mirrors
/// the same stamps and isn't reachable from `xv set --type`):
/// unconditionally inserts `original_name` and `created_by` as tags, then
/// `groups`/`note`/`folder` as tags only when present. `content_type` and
/// expiry are secret *attributes* on the Key Vault object, not tags — 0
/// slots for those.
///
/// **AWS** — `AwsSecretBackend::set_secret` (`src/backend/aws/secrets.rs`):
/// always pushes `xv:original_name`; pushes `xv:groups`/`xv:folder` only
/// when present; pushes `xv:content_type` whenever the request has a
/// content type (every record write does — `RECORD_CONTENT_TYPE`), and
/// `xv:expires_at` whenever an expiry is set. `note` becomes the secret's
/// `Description` (`create_builder.description(note)`), never a tag — 0
/// slots. `created_by`: `metadata::TAG_CREATED_BY` exists as a constant
/// but `set_secret` never writes it (`#[allow(dead_code)]`, "exercised by
/// the round-trip tests only") — 0 slots.
///
/// **Local** — `LocalBackend::set_secret` (`src/backend/local/secrets.rs`):
/// `SecretMeta.tags` is exactly `request.tags.clone().unwrap_or_default()`
/// with nothing added; `original_name`/`created_by`/`groups`/`note`/
/// `folder`/`content_type`/expiry are all separate `SecretMeta` struct
/// fields, never tags. Only `has_type` (which rides in `request.tags`
/// itself, same as every backend) contributes here. This arm is moot in
/// practice — local's `BackendCapabilities.max_tags` is `None`, so
/// `check_tag_budget` never applies the count check — but is kept
/// accurate rather than a shortcut, per the same "can't drift" reasoning
/// as the other two arms.
pub fn predicted_reserved_tag_count(
    backend: crate::backend::BackendKind,
    has_type: bool,
    has_groups: bool,
    has_note: bool,
    has_folder: bool,
    has_expiry: bool,
) -> usize {
    predicted_reserved_tag_count_for_shape(
        backend, has_type, has_groups, has_note, has_folder, has_expiry, has_type,
    )
}

pub fn predicted_reserved_tag_count_for_shape(
    backend: crate::backend::BackendKind,
    has_type: bool,
    has_groups: bool,
    has_note: bool,
    has_folder: bool,
    has_expiry: bool,
    has_content_type: bool,
) -> usize {
    let type_tag = usize::from(has_type);
    match backend {
        crate::backend::BackendKind::Azure => {
            type_tag
                + crate::backend::ALWAYS_WRITTEN_TAGS.len() // original_name + created_by
                + usize::from(has_groups)
                + usize::from(has_note)
                + usize::from(has_folder)
            // content_type / expiry: secret attributes, not tags — 0.
        }
        crate::backend::BackendKind::Aws => {
            type_tag
                + 1 // xv:original_name, always
                + usize::from(has_groups) // xv:groups
                + usize::from(has_folder) // xv:folder
                + usize::from(has_content_type) // xv:content_type only for non-empty content type
                + usize::from(has_expiry) // xv:expires_at
                                          // note -> Description, not a tag; created_by is never written — 0 each.
        }
        crate::backend::BackendKind::Local => {
            type_tag
            // original_name/created_by/groups/note/folder/content_type/expiry
            // are all separate SecretMeta struct fields, never tags — 0 each.
            // (Moot: local's max_tags is None, so check_tag_budget never
            // applies the count check regardless.)
        }
    }
}

#[cfg(test)]
mod tag_budget_tests {
    use super::*;
    use crate::backend::NameCharset;

    fn caps(max_tags: Option<usize>, max_tag_value_len: Option<usize>) -> BackendCapabilities {
        BackendCapabilities {
            has_atomic_record_conversion: false,
            has_conditional_record_conversion: false,
            has_atomic_rename: false,
            has_atomic_file_create: false,
            has_enable_disable: false,
            has_vaults: true,
            has_file_storage: false,
            has_rbac: false,
            has_audit: false,
            has_versioning: false,
            has_soft_delete: false,
            has_restore: false,
            has_purge: false,
            has_scheduled_purge: false,
            has_secret_rotation: false,
            has_groups: true,
            has_folders: true,
            has_notes: true,
            has_expiry: true,
            max_secret_size: None,
            max_name_length: None,
            name_charset: NameCharset::Unrestricted,
            max_tags,
            max_tag_value_len,
        }
    }

    /// `n` dummy user tags with short values, for tests that only care
    /// about the *count* toward the tag budget.
    fn user_tags(n: usize) -> BTreeMap<String, String> {
        (0..n)
            .map(|i| (format!("user{i}"), "v".to_string()))
            .collect()
    }

    #[test]
    fn budget_ok_under_cap() {
        let c = caps(Some(15), Some(256));
        let mut fields = BTreeMap::new();
        fields.insert("username".to_string(), "bob".to_string());
        assert!(check_tag_budget(&c, 2, &fields, &user_tags(1)).is_ok());
    }

    #[test]
    fn budget_errors_over_cap_with_breakdown() {
        let c = caps(Some(3), Some(256));
        let mut fields = BTreeMap::new();
        fields.insert("a".to_string(), "1".to_string());
        fields.insert("b".to_string(), "2".to_string());
        let err = check_tag_budget(&c, 2, &fields, &user_tags(1)).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("reserved"), "{msg}");
        assert!(msg.contains("fields"), "{msg}");
        assert!(msg.contains("user"), "{msg}");
    }

    #[test]
    fn budget_errors_on_long_tag_value() {
        let c = caps(Some(15), Some(4));
        let mut fields = BTreeMap::new();
        fields.insert("username".to_string(), "toolong".to_string());
        let err = check_tag_budget(&c, 1, &fields, &BTreeMap::new()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("username"), "{msg}");
        assert!(msg.contains("kind = \"secret\""), "{msg}");
    }

    #[test]
    fn budget_errors_on_long_user_tag_value() {
        // Azure's real per-value cap (256 chars): a 257-char user --tag
        // value must fail before write, naming the offending tag key; a
        // 256-char value must pass.
        let c = caps(Some(15), Some(256));

        let mut over = BTreeMap::new();
        over.insert("owner".to_string(), "x".repeat(257));
        let err = check_tag_budget(&c, 1, &BTreeMap::new(), &over).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("owner"), "{msg}");
        assert!(msg.contains("257"), "{msg}");

        let mut at_limit = BTreeMap::new();
        at_limit.insert("owner".to_string(), "x".repeat(256));
        assert!(check_tag_budget(&c, 1, &BTreeMap::new(), &at_limit).is_ok());
    }

    #[test]
    fn no_caps_never_errors() {
        let c = caps(None, None);
        let mut fields = BTreeMap::new();
        for i in 0..100 {
            fields.insert(format!("field{i}"), "x".repeat(10_000));
        }
        assert!(check_tag_budget(&c, 100, &fields, &user_tags(100)).is_ok());
    }

    use crate::backend::BackendKind;

    #[test]
    fn predicted_reserved_tag_count_azure_matches_prepare_secret_request() {
        // xv-type + original_name + created_by, no groups/note/folder.
        assert_eq!(
            predicted_reserved_tag_count(BackendKind::Azure, true, false, false, false, false),
            3
        );
        // All optional bookkeeping tags present too; expiry is a secret
        // attribute on Azure, not a tag, so it must NOT add a slot.
        assert_eq!(
            predicted_reserved_tag_count(BackendKind::Azure, true, true, true, true, true),
            6
        );
        // Even with has_type = false, the two always-written tags still count.
        assert_eq!(
            predicted_reserved_tag_count(BackendKind::Azure, false, false, false, false, false),
            2
        );
    }

    #[test]
    fn predicted_reserved_tag_count_aws_matches_set_secret() {
        // xv-type + xv:original_name + xv:content_type only: no groups,
        // no folder, no expiry — and note/created_by must NOT add a slot
        // (note -> Description, created_by is never written on AWS).
        assert_eq!(
            predicted_reserved_tag_count(BackendKind::Aws, true, false, true, false, false),
            3
        );
        // groups + folder + expiry all present.
        assert_eq!(
            predicted_reserved_tag_count(BackendKind::Aws, true, true, false, true, true),
            6
        );
    }

    #[test]
    fn aws_plain_untype_does_not_budget_an_empty_content_type_tag() {
        assert_eq!(
            predicted_reserved_tag_count_for_shape(
                BackendKind::Aws,
                false,
                false,
                false,
                false,
                false,
                false,
            ),
            1
        );
        assert_eq!(
            predicted_reserved_tag_count_for_shape(
                BackendKind::Aws,
                true,
                false,
                false,
                false,
                false,
                true,
            ),
            3
        );
    }

    #[test]
    fn predicted_reserved_tag_count_local_only_counts_the_type_tag() {
        // original_name/created_by/groups/note/folder/content_type/expiry
        // are all separate SecretMeta struct fields on local — never tags.
        assert_eq!(
            predicted_reserved_tag_count(BackendKind::Local, true, true, true, true, true),
            1
        );
        assert_eq!(
            predicted_reserved_tag_count(BackendKind::Local, false, true, true, true, true),
            0
        );
    }

    /// Azure's real boundary (max_tags = 15): the predictor must include
    /// original_name + created_by so a record that would total exactly 15
    /// tags on the wire passes, and one that would total 16 fails *before*
    /// any backend call.
    #[test]
    fn budget_boundary_at_azure_max_tags_matches_real_write() {
        let c = caps(Some(15), Some(256));

        // xv-type + original_name + created_by + groups = 4 reserved,
        // plus 11 f.* metadata field tags = 15 total on the wire. Must pass.
        let reserved =
            predicted_reserved_tag_count(BackendKind::Azure, true, true, false, false, false);
        assert_eq!(reserved, 4);
        let mut fields_15 = BTreeMap::new();
        for i in 0..11 {
            fields_15.insert(format!("field{i}"), "v".to_string());
        }
        assert!(check_tag_budget(&c, reserved, &fields_15, &BTreeMap::new()).is_ok());

        // One more f.* field pushes the real wire total to 16. Must fail
        // before write.
        let mut fields_16 = fields_15.clone();
        fields_16.insert("field11".to_string(), "v".to_string());
        let err = check_tag_budget(&c, reserved, &fields_16, &BTreeMap::new()).unwrap_err();
        assert!(err.to_string().contains("16"), "{err}");
    }

    /// AWS's real boundary (max_tags = 50), WITH an expiry set (so both
    /// xv:content_type and xv:expires_at are on the wire) but WITHOUT note
    /// (which costs 0 slots on AWS — the round-2 fix's bug: the old
    /// universal predictor assumed every backend spends a slot on `note`,
    /// which is only true for Azure). Reproduces the fix: a record with
    /// note set that would have been falsely counted as over budget on
    /// the old model now correctly passes at the real 50-tag boundary,
    /// and one more field still fails before write.
    #[test]
    fn budget_boundary_at_aws_max_tags_counts_content_type_and_expiry_not_note() {
        let c = caps(Some(50), Some(256));

        // xv-type + xv:original_name + xv:groups + xv:content_type +
        // xv:expires_at = 5 reserved (note is set on this write too, but
        // costs 0 AWS tag slots — it becomes the Description).
        let reserved =
            predicted_reserved_tag_count(BackendKind::Aws, true, true, true, false, true);
        assert_eq!(reserved, 5);

        // 5 reserved + 45 f.* fields = 50 total on the wire. Must pass —
        // the old over-counting model (which added a phantom `note` slot
        // and a phantom `created_by` slot) would have rejected this at
        // only 43 f.* fields.
        let mut fields_50 = BTreeMap::new();
        for i in 0..45 {
            fields_50.insert(format!("field{i}"), "v".to_string());
        }
        assert!(check_tag_budget(&c, reserved, &fields_50, &BTreeMap::new()).is_ok());

        // One more f.* field pushes the real wire total to 51. Must fail
        // before write.
        let mut fields_51 = fields_50.clone();
        fields_51.insert("field45".to_string(), "v".to_string());
        let err = check_tag_budget(&c, reserved, &fields_51, &BTreeMap::new()).unwrap_err();
        assert!(err.to_string().contains("51"), "{err}");
    }
}
