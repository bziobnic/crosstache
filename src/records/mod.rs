//! Record types: structured secrets with typed fields.
//!
//! See `docs/superpowers/specs/2026-07-03-record-types-design.md` for the
//! design. This module owns type definitions/resolution (`types`) and the
//! JSON envelope codec (`envelope`) used to store secret-kind fields in the
//! backend secret value.

pub mod envelope;
pub mod types;

// Re-exports consumed by CLI wiring added later in Phase A (Tasks 4/6/7);
// unused from the `xv` binary target until then.
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
/// reserved bookkeeping tags a record write actually consumes.
///
/// This always includes `crate::backend::ALWAYS_WRITTEN_TAGS` (currently
/// `original_name` + `created_by`) — every backend `set_secret` write
/// stamps these unconditionally regardless of what the caller requests, so
/// a pre-check that omits them can pass here and still be rejected by the
/// backend as over budget (undercounting defeats fail-before-write right
/// at the boundary). `has_type` adds one more for the reserved `xv-type`
/// tag (always present on a record write); `has_groups`/`has_note`/
/// `has_folder` are conditional on whether this specific write sets them.
pub fn reserved_tag_count(
    has_type: bool,
    has_groups: bool,
    has_note: bool,
    has_folder: bool,
) -> usize {
    let mut count = crate::backend::ALWAYS_WRITTEN_TAGS.len();
    if has_type {
        count += 1;
    }
    if has_groups {
        count += 1;
    }
    if has_note {
        count += 1;
    }
    if has_folder {
        count += 1;
    }
    count
}

/// Extra reserved tag slots consumed by backend-specific bookkeeping that
/// [`reserved_tag_count`]'s universal (Azure-shaped) count doesn't cover.
///
/// AWS's `set_secret` (`src/backend/aws/secrets.rs`) always tags
/// `xv:content_type` whenever the request carries a content type — which
/// every record write does (`RECORD_CONTENT_TYPE`) — and additionally
/// tags `xv:expires_at` whenever an expiry is set. Neither Azure (content
/// type lives on the secret object, not a tag) nor the local backend
/// (unbounded tags) spend a slot on these, so this returns `0` for any
/// `backend_kind` other than `Aws`.
pub fn backend_extra_reserved_tags(
    backend_kind: crate::backend::BackendKind,
    has_expiry: bool,
) -> usize {
    match backend_kind {
        crate::backend::BackendKind::Aws => {
            // xv:content_type, always present on a record write, + xv:expires_at
            // when an expiry is set.
            1 + usize::from(has_expiry)
        }
        crate::backend::BackendKind::Azure | crate::backend::BackendKind::Local => 0,
    }
}

#[cfg(test)]
mod tag_budget_tests {
    use super::*;
    use crate::backend::NameCharset;

    fn caps(max_tags: Option<usize>, max_tag_value_len: Option<usize>) -> BackendCapabilities {
        BackendCapabilities {
            has_vaults: true,
            has_file_storage: false,
            has_rbac: false,
            has_audit: false,
            has_versioning: false,
            has_soft_delete: false,
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

    #[test]
    fn reserved_tag_count_includes_always_written_bookkeeping_tags() {
        // xv-type + original_name + created_by, no groups/note/folder.
        assert_eq!(reserved_tag_count(true, false, false, false), 3);
        // All optional bookkeeping tags present too.
        assert_eq!(reserved_tag_count(true, true, true, true), 6);
        // Even with has_type = false, the two always-written tags still count.
        assert_eq!(reserved_tag_count(false, false, false, false), 2);
    }

    /// Azure's real boundary (max_tags = 15): reserved_tag_count must
    /// include original_name + created_by so a record that would total
    /// exactly 15 tags on the wire passes, and one that would total 16
    /// fails *before* any backend call — reproducing the under-count bug
    /// where the old ad-hoc `reserved_count` computation (missing
    /// original_name/created_by) let a request through that Azure's real
    /// REST call would then reject.
    #[test]
    fn budget_boundary_at_azure_max_tags_matches_real_write() {
        let c = caps(Some(15), Some(256));

        // xv-type + original_name + created_by + groups = 4 reserved,
        // plus 11 f.* metadata field tags = 15 total on the wire. Must pass.
        let reserved = reserved_tag_count(true, true, false, false);
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

    #[test]
    fn backend_extra_reserved_tags_is_zero_for_azure_and_local() {
        assert_eq!(
            backend_extra_reserved_tags(crate::backend::BackendKind::Azure, true),
            0
        );
        assert_eq!(
            backend_extra_reserved_tags(crate::backend::BackendKind::Local, true),
            0
        );
    }

    #[test]
    fn backend_extra_reserved_tags_counts_aws_content_type_and_expiry() {
        // xv:content_type only (no expiry set).
        assert_eq!(
            backend_extra_reserved_tags(crate::backend::BackendKind::Aws, false),
            1
        );
        // xv:content_type + xv:expires_at.
        assert_eq!(
            backend_extra_reserved_tags(crate::backend::BackendKind::Aws, true),
            2
        );
    }

    /// AWS's real boundary (max_tags = 50): reserved_tag_count() alone
    /// (Azure-shaped) doesn't know about AWS's always-written
    /// xv:content_type tag or its conditional xv:expires_at tag, so a
    /// record write with an expiry set could pass the pre-check and still
    /// blow AWS's real 50-tag cap at the API without
    /// backend_extra_reserved_tags folded in. Reproduces that boundary:
    /// with the extra reserved tags counted, a 50-tag-total record passes
    /// and a 51-tag-total fails before write.
    #[test]
    fn budget_boundary_at_aws_max_tags_counts_content_type_and_expiry() {
        let c = caps(Some(50), Some(256));

        // Universal reserved: xv-type + original_name + created_by + groups = 4.
        // AWS extras: xv:content_type (always) + xv:expires_at (expiry set) = 2.
        let reserved = reserved_tag_count(true, true, false, false)
            + backend_extra_reserved_tags(crate::backend::BackendKind::Aws, true);
        assert_eq!(reserved, 6);

        // 6 reserved + 44 f.* fields = 50 total on the wire. Must pass.
        let mut fields_50 = BTreeMap::new();
        for i in 0..44 {
            fields_50.insert(format!("field{i}"), "v".to_string());
        }
        assert!(check_tag_budget(&c, reserved, &fields_50, &BTreeMap::new()).is_ok());

        // One more f.* field pushes the real wire total to 51. Must fail
        // before write.
        let mut fields_51 = fields_50.clone();
        fields_51.insert("field44".to_string(), "v".to_string());
        let err = check_tag_budget(&c, reserved, &fields_51, &BTreeMap::new()).unwrap_err();
        assert!(err.to_string().contains("51"), "{err}");
    }
}
