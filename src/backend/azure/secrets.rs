//! Azure secret backend adapter.
//!
//! Wraps the existing [`AzureSecretOperations`] (which implements
//! [`SecretOperations`]) behind the new [`SecretBackend`] trait.

#[allow(unused_imports)]
use std::sync::Arc;

use async_trait::async_trait;

use std::collections::HashMap;

use crate::backend::error::BackendError;
use crate::backend::secret::SecretBackend;
use crate::secret::domain::SecretValue;
use crate::secret::domain::{
    DeletedSecretSummary, FieldUpdate, SecretAttributesUpdate, SecretProperties, SecretRequest,
    SecretSummary, SecretUpdateRequest,
};
use crate::secret::manager::SecretOperations;

use super::map_error;

/// Adapter that implements [`SecretBackend`] by delegating to an existing
/// [`SecretOperations`] implementation (i.e. `AzureSecretOperations`).
#[allow(dead_code)]
pub struct AzureSecretBackend {
    inner: Arc<dyn SecretOperations>,
}

impl AzureSecretBackend {
    /// Wrap an existing `SecretOperations` implementor.
    #[allow(dead_code)]
    pub fn new(inner: Arc<dyn SecretOperations>) -> Self {
        Self { inner }
    }

    /// Fetch the current tags of a secret, tolerating disabled secrets.
    ///
    /// `GET {vault}/secrets/{name}` returns HTTP 403 `SecretDisabled` for a
    /// disabled secret, but the versions list (`GET .../versions`) still
    /// exposes attributes and tags, so use it as the second source.
    async fn current_tags(
        &self,
        vault: &str,
        name: &str,
    ) -> Result<HashMap<String, String>, BackendError> {
        let get_err = match self.inner.get_secret(vault, name, false).await {
            Ok(current) => return Ok(current.tags),
            Err(e) => e,
        };
        let mut versions = self
            .inner
            .get_secret_versions(vault, name)
            .await
            .map_err(|_| map_error(get_err))?;
        versions.sort_by_key(|v| v.created_timestamp);
        versions
            .pop()
            .map(|v| v.tags)
            .ok_or_else(|| BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            })
    }

    /// Resolve a user-friendly version identifier ("v6" / "6") to the Azure
    /// Key Vault version GUID that `rollback_secret` needs. A value that isn't
    /// a friendly number is assumed to already be a GUID and returned
    /// unchanged — the accept-both contract local/aws honor (their rollback
    /// takes the version identifier as-is). Ported verbatim from the CLI's
    /// former `resolve_version_to_guid`, keeping the trait rollback path total
    /// for Azure.
    async fn resolve_version_guid(
        &self,
        vault: &str,
        name: &str,
        version: &str,
    ) -> Result<String, BackendError> {
        let Ok(version_num) = version.trim_start_matches('v').parse::<u32>() else {
            // Not a friendly number — assume it is already a GUID.
            return Ok(version.to_string());
        };
        if version_num == 0 {
            return Err(BackendError::InvalidArgument(
                "Version number must be 1 or greater (v1 is the oldest version)".to_string(),
            ));
        }
        let versions = self
            .inner
            .get_secret_versions(vault, name)
            .await
            .map_err(map_error)?;
        let max_version = versions
            .iter()
            .filter_map(|v| v.version_number)
            .max()
            .unwrap_or(0);
        match versions
            .into_iter()
            .find(|v| v.version_number == Some(version_num))
        {
            Some(v) => Ok(v.version),
            None => Err(BackendError::InvalidArgument(format!(
                "Version v{version_num} not found for secret '{name}'. \
                 Available versions: v1–v{max_version} (use 'xv history {name}' to list them)"
            ))),
        }
    }
}

/// Build the full replacement tag map for an attributes-only `PATCH`:
/// resolve merge/replace semantics while preserving immutable identity metadata
/// and byte-for-byte values of fields for which no update was requested.
fn build_patched_tags(
    request: &SecretUpdateRequest,
    current_tags: &HashMap<String, String>,
) -> HashMap<String, String> {
    // Resolve tags: honor replace_tags semantics.
    let mut tags = match &request.tags {
        Some(new_tags) if !request.replace_tags => {
            let mut merged = current_tags.clone();
            merged.extend(new_tags.clone());
            merged
        }
        Some(new_tags) => new_tags.clone(),
        // No tag change requested: preserve every existing tag (including
        // custom user tags and `groups`) rather than starting from an empty
        // map. Identity metadata is preserved below, and explicit
        // note/folder/groups overrides still apply after that.
        None => current_tags.clone(),
    };

    // Resolve groups: honor replace_groups semantics.
    // When request.groups is None, preserve existing groups (no change requested).
    let groups = match &request.groups {
        Some(new_groups) if !request.replace_groups => {
            // Merge: append new_groups to existing
            let mut existing: Vec<String> = current_tags
                .get("groups")
                .map(|g| g.split(',').map(|s| s.trim().to_string()).collect())
                .unwrap_or_default();
            for g in new_groups {
                if !existing.contains(g) {
                    existing.push(g.clone());
                }
            }
            Some(existing)
        }
        Some(new_groups) => {
            // Replace: use new_groups as-is (may be empty)
            Some(new_groups.clone())
        }
        None => None,
    };

    // A metadata patch does not rename a record or change its creator. Preserve
    // these fields, including absence, and do not accept tag-based spoofing.
    for name in [
        crate::backend::TAG_ORIGINAL_NAME,
        crate::backend::TAG_CREATED_BY,
    ] {
        tags.remove(name);
        if let Some(value) = current_tags.get(name) {
            tags.insert(name.into(), value.clone());
        }
    }

    tags.remove("groups");
    if let Some(groups) = groups {
        if !groups.is_empty() {
            tags.insert("groups".to_string(), groups.join(","));
        }
    } else if let Some(groups) = current_tags.get("groups") {
        // No groups update: preserve spaces and an explicitly empty value.
        tags.insert("groups".into(), groups.clone());
    }

    // Handle note: remove first, then conditionally insert.
    tags.remove("note");
    if let Some(note) = request
        .note
        .clone()
        .apply(current_tags.get("note").cloned())
    {
        tags.insert("note".to_string(), note);
    }

    // Handle folder: remove first, then conditionally insert.
    tags.remove("folder");
    if let Some(folder) = request
        .folder
        .clone()
        .apply(current_tags.get("folder").cloned())
    {
        tags.insert("folder".to_string(), folder);
    }

    tags
}

#[async_trait]
impl SecretBackend for AzureSecretBackend {
    async fn set_secret(
        &self,
        vault: &str,
        request: SecretRequest,
    ) -> Result<SecretProperties, BackendError> {
        self.inner
            .set_secret(vault, &request)
            .await
            .map_err(map_error)
    }

    async fn get_secret(
        &self,
        vault: &str,
        name: &str,
        include_value: bool,
    ) -> Result<SecretProperties, BackendError> {
        self.inner
            .get_secret(vault, name, include_value)
            .await
            .map_err(map_error)
    }

    async fn get_transfer_snapshot(
        &self,
        vault: &str,
        name: &str,
        include_value: bool,
    ) -> Result<crate::secret::domain::SecretSnapshot, BackendError> {
        let properties = self.get_secret(vault, name, include_value).await?;
        if properties.version.is_empty() {
            return Err(BackendError::Unsupported(
                "Azure transfer requires a provider version".into(),
            ));
        }
        let revision = crate::backend::secret::transfer_metadata_revision(&properties)?;
        let after = self.get_secret(vault, name, false).await?;
        if crate::backend::secret::transfer_metadata_revision(&after)? != revision {
            return Err(BackendError::Conflict(
                "Azure secret metadata/version changed during transfer read".into(),
            ));
        }
        Ok(crate::secret::domain::SecretSnapshot {
            properties,
            revision,
        })
    }

    async fn get_secret_version(
        &self,
        vault: &str,
        name: &str,
        version: &str,
        include_value: bool,
    ) -> Result<SecretProperties, BackendError> {
        self.inner
            .get_secret_version(vault, name, version, include_value)
            .await
            .map_err(map_error)
    }

    async fn list_secrets(
        &self,
        vault: &str,
        group_filter: Option<&str>,
    ) -> Result<Vec<SecretSummary>, BackendError> {
        self.inner
            .list_secrets(vault, group_filter)
            .await
            .map_err(map_error)
    }

    async fn delete_secret(&self, vault: &str, name: &str) -> Result<(), BackendError> {
        self.inner
            .delete_secret(vault, name)
            .await
            .map_err(map_error)
    }

    async fn update_secret(
        &self,
        vault: &str,
        name: &str,
        request: SecretUpdateRequest,
    ) -> Result<SecretProperties, BackendError> {
        // Attributes/tags-only updates (no value change) go
        // through `PATCH {vault}/secrets/{name}` instead of the full-write
        // path below: the full write must read the current value first and
        // confirm afterwards, both of which return HTTP 403 `SecretDisabled`
        // on a disabled secret — making `xv update --enabled true` (re-enable)
        // impossible. PATCH touches only attributes/tags, works on disabled
        // secrets, and creates no new version.
        //
        // Clearing exp/nbf still needs the full write: omitted PATCH
        // attribute fields are left unchanged, so a clear cannot be expressed.
        let attributes_only = request.value.is_none();
        let clears_dates = matches!(request.expires_on, FieldUpdate::Clear)
            || matches!(request.not_before, FieldUpdate::Clear);
        if attributes_only && !clears_dates {
            // PATCH replaces the whole tag map when `tags` is present, so a
            // tag-affecting change needs the current tags to build the full
            // desired map; otherwise omit `tags` and Azure leaves them as-is.
            let tag_affecting = request.tags.is_some()
                || request.groups.is_some()
                || !request.note.is_unchanged()
                || !request.folder.is_unchanged();
            let tags = if tag_affecting {
                let current_tags = self.current_tags(vault, name).await?;
                Some(build_patched_tags(&request, &current_tags))
            } else {
                None
            };
            let update = SecretAttributesUpdate {
                enabled: request.enabled,
                content_type: request.content_type.clone(),
                expires_on: match request.expires_on {
                    FieldUpdate::Set(v) => Some(v),
                    _ => None,
                },
                not_before: match request.not_before {
                    FieldUpdate::Set(v) => Some(v),
                    _ => None,
                },
                tags,
            };
            return self
                .inner
                .update_secret_attributes(vault, name, &update)
                .await
                .map_err(map_error);
        }

        // The old SecretOperations::update_secret takes &SecretRequest, but
        // the new trait takes SecretUpdateRequest. Translate by building a
        // SecretRequest from the update request fields.

        // Determine whether we need to read the current secret.
        // We need it when:
        //  - value is None (to avoid overwriting with empty string)
        //  - replace_tags is false and new tags are provided (to merge)
        //  - replace_groups is false and new groups are provided (to merge)
        //  - any tri-state metadata field is Unchanged (the underlying Azure
        //    update is a PUT, so unchanged fields must be carried forward)
        let needs_current = request.value.is_none()
            || (!request.replace_tags && request.tags.is_some())
            || (!request.replace_groups && request.groups.is_some())
            || request.expires_on.is_unchanged()
            || request.not_before.is_unchanged()
            || request.note.is_unchanged()
            || request.folder.is_unchanged();

        let current = if needs_current {
            Some(
                self.inner
                    .get_secret(vault, name, true)
                    .await
                    .map_err(map_error)?,
            )
        } else {
            None
        };

        // Resolve the value: use the provided value, or fall back to the current one.
        let value = match request.value {
            Some(v) => v,
            None => current
                .as_ref()
                .and_then(|c| c.value.clone())
                .unwrap_or_else(|| SecretValue::new(String::new())),
        };

        // Resolve tags: honor replace_tags semantics.
        let tags = match request.tags {
            Some(new_tags) if !request.replace_tags => {
                // Merge: start with existing tags, then overlay new ones.
                let mut merged = current.as_ref().map(|c| c.tags.clone()).unwrap_or_default();
                merged.extend(new_tags);
                Some(merged)
            }
            other => other,
        };

        // Resolve groups: honor replace_groups semantics.
        let groups = match request.groups {
            Some(new_groups) if !request.replace_groups => {
                // Merge: start with existing groups (stored in tags as comma-separated),
                // then add any new groups that aren't already present.
                let mut existing: Vec<String> = current
                    .as_ref()
                    .and_then(|c| c.tags.get("groups"))
                    .map(|g| g.split(',').map(|s| s.trim().to_string()).collect())
                    .unwrap_or_default();
                for g in new_groups {
                    if !existing.contains(&g) {
                        existing.push(g);
                    }
                }
                Some(existing)
            }
            other => other,
        };

        let compat_request = SecretRequest {
            name: request.name.clone(),
            value,
            content_type: request.content_type,
            enabled: request.enabled,
            expires_on: request
                .expires_on
                .apply(current.as_ref().and_then(|c| c.expires_on)),
            not_before: request
                .not_before
                .apply(current.as_ref().and_then(|c| c.not_before)),
            tags,
            groups,
            note: request
                .note
                .apply(current.as_ref().and_then(|c| c.tags.get("note").cloned())),
            folder: request
                .folder
                .apply(current.as_ref().and_then(|c| c.tags.get("folder").cloned())),
        };
        self.inner
            .update_secret(vault, name, &compat_request)
            .await
            .map_err(map_error)
    }

    // ------------------------------------------------------------------
    // Optional operations — Azure supports all of these
    // ------------------------------------------------------------------

    async fn list_versions(
        &self,
        vault: &str,
        name: &str,
    ) -> Result<Vec<SecretProperties>, BackendError> {
        self.inner
            .get_secret_versions(vault, name)
            .await
            .map_err(map_error)
    }

    async fn rollback(
        &self,
        vault: &str,
        name: &str,
        version: &str,
    ) -> Result<SecretProperties, BackendError> {
        // Azure's rollback API needs the underlying version GUID; resolve a
        // friendly "v6"/"6" first (raw GUIDs pass through unchanged).
        let resolved = self.resolve_version_guid(vault, name, version).await?;
        self.inner
            .rollback_secret(vault, name, &resolved)
            .await
            .map_err(map_error)
    }

    async fn restore_secret(
        &self,
        vault: &str,
        name: &str,
    ) -> Result<SecretProperties, BackendError> {
        self.inner
            .restore_secret(vault, name)
            .await
            .map_err(map_error)
    }

    async fn purge_secret(&self, vault: &str, name: &str) -> Result<(), BackendError> {
        self.inner
            .purge_secret(vault, name)
            .await
            .map_err(map_error)
    }

    async fn secret_exists(&self, vault: &str, name: &str) -> Result<bool, BackendError> {
        self.inner
            .secret_exists(vault, name)
            .await
            .map_err(map_error)
    }

    async fn list_deleted_secrets(
        &self,
        vault: &str,
    ) -> Result<Vec<DeletedSecretSummary>, BackendError> {
        self.inner
            .list_deleted_secrets(vault)
            .await
            .map_err(map_error)
    }

    async fn backup_secret(&self, vault: &str, name: &str) -> Result<Vec<u8>, BackendError> {
        self.inner
            .backup_secret(vault, name)
            .await
            .map_err(map_error)
    }

    async fn restore_from_backup(
        &self,
        vault: &str,
        backup: &[u8],
    ) -> Result<SecretProperties, BackendError> {
        self.inner
            .restore_secret_from_backup(vault, backup)
            .await
            .map_err(map_error)
    }
}

#[cfg(test)]
mod build_patched_tags_tests {
    use super::*;
    use crate::secret::domain::FieldUpdate;

    fn base_request(name: &str) -> SecretUpdateRequest {
        SecretUpdateRequest {
            name: name.to_string(),
            expected_revision: None,
            value: None,
            content_type: None,
            enabled: None,
            expires_on: FieldUpdate::Unchanged,
            not_before: FieldUpdate::Unchanged,
            tags: None,
            groups: None,
            note: FieldUpdate::Unchanged,
            folder: FieldUpdate::Unchanged,
            replace_tags: false,
            replace_groups: false,
        }
    }

    fn current_tags() -> HashMap<String, String> {
        let mut tags = HashMap::new();
        tags.insert("groups".to_string(), "team-a".to_string());
        tags.insert("custom".to_string(), "keep".to_string());
        tags.insert("note".to_string(), "old".to_string());
        tags
    }

    #[test]
    fn retirement_tag_delta_preserves_custom_and_absent_identity_metadata_byte_for_byte() {
        use crate::secret::attachment_key::{self as key, AttachmentKeyId};
        let name = key::retained_record_name(&AttachmentKeyId::derive("test"));
        for custom in [false, true] {
            let mut current = HashMap::from([
                ("groups".into(), "one, two".into()),
                ("note".into(), "preserved note".into()),
                ("custom".into(), "preserved".into()),
            ]);
            if custom {
                current.insert(
                    crate::backend::TAG_CREATED_BY.into(),
                    "external-import".into(),
                );
                current.insert(
                    crate::backend::TAG_ORIGINAL_NAME.into(),
                    "imported spelling".into(),
                );
            }
            let mut request = base_request(&name);
            request.tags = Some(HashMap::from([(
                key::KEY_RETIRED_TAG.into(),
                "true".into(),
            )]));
            let mut expected = current.clone();
            expected.insert(key::KEY_RETIRED_TAG.into(), "true".into());
            assert_eq!(build_patched_tags(&request, &current), expected);
        }
    }

    #[test]
    fn none_tags_preserves_existing_custom_tags_and_groups() {
        let current = current_tags();
        let mut request = base_request("my-secret");
        request.note = FieldUpdate::Set("new".to_string());

        let result = build_patched_tags(&request, &current);

        assert_eq!(result.get("custom").map(String::as_str), Some("keep"));
        assert_eq!(result.get("groups").map(String::as_str), Some("team-a"));
        assert_eq!(result.get("note").map(String::as_str), Some("new"));
        assert!(!result.contains_key("original_name"));
        assert!(!result.contains_key("created_by"));
    }

    #[test]
    fn some_tags_without_replace_merges_over_current() {
        let current = current_tags();
        let mut request = base_request("my-secret");
        let mut new_tags = HashMap::new();
        new_tags.insert("x".to_string(), "1".to_string());
        request.tags = Some(new_tags);
        request.replace_tags = false;

        let result = build_patched_tags(&request, &current);

        assert_eq!(result.get("x").map(String::as_str), Some("1"));
        // Existing custom tag and groups survive the merge.
        assert_eq!(result.get("custom").map(String::as_str), Some("keep"));
        assert_eq!(result.get("groups").map(String::as_str), Some("team-a"));
    }

    #[test]
    fn some_tags_with_replace_drops_current_custom_tags() {
        let current = current_tags();
        let mut request = base_request("my-secret");
        let mut new_tags = HashMap::new();
        new_tags.insert("x".to_string(), "1".to_string());
        request.tags = Some(new_tags);
        request.replace_tags = true;

        let result = build_patched_tags(&request, &current);

        assert_eq!(result.get("x").map(String::as_str), Some("1"));
        // Full replacement: old custom tag is gone...
        assert!(!result.contains_key("custom"));
        // Metadata updates do not invent previously absent identity tags.
        assert!(!result.contains_key("original_name"));
        assert!(!result.contains_key("created_by"));
    }

    #[test]
    fn metadata_patch_preserves_empty_groups_and_rejects_identity_tag_spoofing() {
        for existing in [false, true] {
            let mut current = HashMap::from([("groups".into(), String::new())]);
            if existing {
                current.insert(crate::backend::TAG_ORIGINAL_NAME.into(), "original".into());
                current.insert(crate::backend::TAG_CREATED_BY.into(), "creator".into());
            }
            let mut request = base_request("name");
            request.tags = Some(HashMap::from([
                (crate::backend::TAG_ORIGINAL_NAME.into(), "spoofed".into()),
                (crate::backend::TAG_CREATED_BY.into(), "spoofed".into()),
            ]));
            assert_eq!(build_patched_tags(&request, &current), current);
        }
    }

    #[test]
    fn groups_merge_appends_to_existing_groups() {
        let current = current_tags();
        let mut request = base_request("my-secret");
        request.groups = Some(vec!["b".to_string()]);
        request.replace_groups = false;

        let result = build_patched_tags(&request, &current);

        assert_eq!(result.get("groups").map(String::as_str), Some("team-a,b"));
    }

    #[test]
    fn clear_note_removes_existing_note() {
        let current = current_tags();
        let mut request = base_request("my-secret");
        request.note = FieldUpdate::Clear;

        let result = build_patched_tags(&request, &current);

        // Note key should be completely removed
        assert!(!result.contains_key("note"));
        // Other tags should be preserved
        assert_eq!(result.get("custom").map(String::as_str), Some("keep"));
        assert_eq!(result.get("groups").map(String::as_str), Some("team-a"));
    }

    #[test]
    fn clear_folder_removes_existing_folder() {
        let mut current = current_tags();
        current.insert("folder".to_string(), "app/db".to_string());
        let mut request = base_request("my-secret");
        request.folder = FieldUpdate::Clear;

        let result = build_patched_tags(&request, &current);

        // Folder key should be completely removed
        assert!(!result.contains_key("folder"));
        // Other tags should be preserved
        assert_eq!(result.get("custom").map(String::as_str), Some("keep"));
        assert_eq!(result.get("groups").map(String::as_str), Some("team-a"));
        assert_eq!(result.get("note").map(String::as_str), Some("old"));
    }

    #[test]
    fn clear_groups_via_replace_empty_list() {
        let current = current_tags();
        let mut request = base_request("my-secret");
        request.groups = Some(vec![]); // Empty list with replace_groups=true
        request.replace_groups = true;

        let result = build_patched_tags(&request, &current);

        // Groups key should be completely removed
        assert!(!result.contains_key("groups"));
        // Other tags should be preserved
        assert_eq!(result.get("custom").map(String::as_str), Some("keep"));
        assert_eq!(result.get("note").map(String::as_str), Some("old"));
    }

    #[test]
    fn unchanged_note_preserves_existing_value() {
        let current = current_tags();
        let mut request = base_request("my-secret");
        request.note = FieldUpdate::Unchanged; // Explicitly unchanged

        let result = build_patched_tags(&request, &current);

        // Note should be preserved from current_tags
        assert_eq!(result.get("note").map(String::as_str), Some("old"));
        // Other tags should be preserved
        assert_eq!(result.get("custom").map(String::as_str), Some("keep"));
        assert_eq!(result.get("groups").map(String::as_str), Some("team-a"));
    }

    #[test]
    fn unchanged_folder_preserves_existing_value() {
        let mut current = current_tags();
        current.insert("folder".to_string(), "app/db".to_string());
        let mut request = base_request("my-secret");
        request.folder = FieldUpdate::Unchanged; // Explicitly unchanged

        let result = build_patched_tags(&request, &current);

        // Folder should be preserved from current_tags
        assert_eq!(result.get("folder").map(String::as_str), Some("app/db"));
        // Other tags should be preserved
        assert_eq!(result.get("custom").map(String::as_str), Some("keep"));
        assert_eq!(result.get("groups").map(String::as_str), Some("team-a"));
    }
}

#[cfg(test)]
mod atomic_conversion_update_tests {
    use super::*;
    use crate::error::Result;
    use chrono::{TimeZone, Utc};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    struct AtomicUpdateMock {
        reads: AtomicUsize,
        updates: Mutex<Vec<SecretRequest>>,
        retirement: Option<Mutex<SecretProperties>>,
        patches: AtomicUsize,
        transfer_drift: bool,
    }

    impl AtomicUpdateMock {
        fn new() -> Self {
            Self {
                reads: AtomicUsize::new(0),
                updates: Mutex::new(Vec::new()),
                retirement: None,
                patches: AtomicUsize::new(0),
                transfer_drift: false,
            }
        }
    }

    fn properties() -> SecretProperties {
        SecretProperties {
            name: "secret".into(),
            original_name: "secret".into(),
            value: None,
            version: "1".into(),
            version_number: Some(1),
            created_timestamp: 0,
            created_on: String::new(),
            updated_on: String::new(),
            enabled: true,
            expires_on: None,
            not_before: None,
            tags: HashMap::new(),
            content_type: String::new(),
            recovery_level: None,
        }
    }

    #[async_trait]
    impl SecretOperations for AtomicUpdateMock {
        async fn get_secret(
            &self,
            _vault: &str,
            _name: &str,
            _include_value: bool,
        ) -> Result<SecretProperties> {
            let read = self.reads.fetch_add(1, Ordering::SeqCst);
            if let Some(state) = &self.retirement {
                let mut p = state.lock().unwrap().clone();
                if !_include_value {
                    p.value = None;
                }
                return Ok(p);
            }
            let mut p = properties();
            if self.transfer_drift && read > 0 {
                p.tags.insert("folder".into(), "changed".into());
            }
            Ok(p)
        }

        async fn update_secret_attributes(
            &self,
            _v: &str,
            _n: &str,
            update: &SecretAttributesUpdate,
        ) -> Result<SecretProperties> {
            assert!(update.enabled.is_none());
            assert!(update.content_type.is_none());
            assert!(update.expires_on.is_none());
            assert!(update.not_before.is_none());
            let mut p = self.retirement.as_ref().unwrap().lock().unwrap();
            let mut expected = p.tags.clone();
            expected.insert(
                crate::secret::attachment_key::KEY_RETIRED_TAG.into(),
                "true".into(),
            );
            assert_eq!(update.tags.as_ref(), Some(&expected));
            p.tags = expected;
            self.patches.fetch_add(1, Ordering::SeqCst);
            Ok(p.clone())
        }

        async fn update_secret(
            &self,
            _vault: &str,
            _name: &str,
            request: &SecretRequest,
        ) -> Result<SecretProperties> {
            self.updates.lock().unwrap().push(request.clone());
            Ok(properties())
        }

        async fn set_secret(&self, _v: &str, _r: &SecretRequest) -> Result<SecretProperties> {
            unimplemented!()
        }
        async fn get_secret_version(
            &self,
            _v: &str,
            _n: &str,
            version: &str,
            include: bool,
        ) -> Result<SecretProperties> {
            let mut p = self.retirement.as_ref().unwrap().lock().unwrap().clone();
            assert_eq!(p.version, version);
            if !include {
                p.value = None;
            }
            Ok(p)
        }
        async fn list_secrets(&self, _v: &str, _g: Option<&str>) -> Result<Vec<SecretSummary>> {
            unimplemented!()
        }
        async fn delete_secret(&self, _v: &str, _n: &str) -> Result<()> {
            unimplemented!()
        }
        async fn restore_secret(&self, _v: &str, _n: &str) -> Result<SecretProperties> {
            unimplemented!()
        }
        async fn purge_secret(&self, _v: &str, _n: &str) -> Result<()> {
            unimplemented!()
        }
        async fn list_deleted_secrets(&self, _v: &str) -> Result<Vec<DeletedSecretSummary>> {
            unimplemented!()
        }
        async fn secret_exists(&self, _v: &str, _n: &str) -> Result<bool> {
            unimplemented!()
        }
        async fn get_secret_versions(&self, _v: &str, _n: &str) -> Result<Vec<SecretProperties>> {
            unimplemented!()
        }
        async fn rollback_secret(
            &self,
            _v: &str,
            _n: &str,
            _version: &str,
        ) -> Result<SecretProperties> {
            unimplemented!()
        }
        async fn backup_secret(&self, _v: &str, _n: &str) -> Result<Vec<u8>> {
            unimplemented!()
        }
        async fn restore_secret_from_backup(
            &self,
            _v: &str,
            _b: &[u8],
        ) -> Result<SecretProperties> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn azure_transfer_read_rechecks_metadata_without_enabling_cas() {
        let stable = Arc::new(AtomicUpdateMock::new());
        let adapter = AzureSecretBackend::new(stable.clone());
        let before = adapter
            .get_transfer_snapshot("vault", "secret", false)
            .await
            .unwrap();
        assert_eq!(stable.reads.load(Ordering::SeqCst), 2);
        assert!(!adapter.supports_atomic_create());
        assert!(!adapter.supports_conditional_delete());
        assert!(!before.revision.is_empty());
        let mut drift = AtomicUpdateMock::new();
        drift.transfer_drift = true;
        assert!(AzureSecretBackend::new(Arc::new(drift))
            .get_transfer_snapshot("vault", "secret", false)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn retirement_azure_adapter_uses_only_patch_and_preserves_exact_identity_metadata() {
        use crate::backend::attachment_keys::{AttachmentKeyStore, RawAttachmentKeyStore};
        use crate::secret::attachment_key::{
            self as key, AttachmentKeyId, AttachmentKeyRef, KeySlot, SecretVersion,
        };
        use age::secrecy::ExposeSecret;
        for custom in [false, true] {
            let identity = age::x25519::Identity::generate();
            let id = AttachmentKeyId::derive(&identity.to_public().to_string());
            let mut p = properties();
            p.name = key::retained_record_name(&id);
            p.original_name = if custom {
                "imported spelling".into()
            } else {
                p.name.clone()
            };
            p.value = Some(SecretValue::new(identity.to_string().expose_secret()));
            p.content_type = key::KEY_RECORD_CONTENT_TYPE.into();
            p.tags = HashMap::from([
                ("groups".into(), "one, two".into()),
                ("custom".into(), "preserved".into()),
            ]);
            if custom {
                p.tags.insert(
                    crate::backend::TAG_ORIGINAL_NAME.into(),
                    p.original_name.clone(),
                );
                p.tags
                    .insert(crate::backend::TAG_CREATED_BY.into(), "external".into());
            }
            let reference = AttachmentKeyRef {
                key_id: id,
                slot: KeySlot::Retained,
                provider_version: SecretVersion::new(p.version.clone()),
            };
            let mut mock = AtomicUpdateMock::new();
            mock.retirement = Some(Mutex::new(p.clone()));
            let inner = Arc::new(mock);
            let backend = AzureSecretBackend::new(inner.clone());
            let keys = RawAttachmentKeyStore::versioned(&backend);
            let after = keys.mark_retired("vault", &reference).await.unwrap();
            assert_eq!(after.value, p.value);
            assert_eq!(after.version, p.version);
            assert_eq!(after.enabled, p.enabled);
            assert_eq!(after.original_name, p.original_name);
            assert_eq!(inner.patches.load(Ordering::SeqCst), 1);
            assert!(inner.updates.lock().unwrap().is_empty());
            keys.mark_retired("vault", &reference).await.unwrap();
            assert_eq!(inner.patches.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn complete_conversion_shape_uses_one_put_without_a_read_or_patch() {
        let inner = Arc::new(AtomicUpdateMock::new());
        let backend = AzureSecretBackend::new(inner.clone());
        let expires = Utc.with_ymd_and_hms(2030, 1, 2, 0, 0, 0).unwrap();
        let request = SecretUpdateRequest {
            name: "secret".into(),
            expected_revision: None,
            value: Some(SecretValue::new("converted-envelope")),
            content_type: Some("application/vnd.xv.record+json".into()),
            enabled: Some(false),
            expires_on: FieldUpdate::Set(expires),
            not_before: FieldUpdate::Clear,
            tags: Some(HashMap::from([("xv-type".into(), "api-key".into())])),
            groups: Some(vec!["ops".into()]),
            note: FieldUpdate::Set("preserved".into()),
            folder: FieldUpdate::Clear,
            replace_tags: true,
            replace_groups: true,
        };

        backend
            .update_secret("vault", "secret", request)
            .await
            .unwrap();

        assert_eq!(inner.reads.load(Ordering::SeqCst), 0);
        let updates = inner.updates.lock().unwrap();
        assert_eq!(updates.len(), 1);
        assert_eq!(
            updates[0].content_type.as_deref(),
            Some("application/vnd.xv.record+json")
        );
        assert_eq!(updates[0].enabled, Some(false));
        assert_eq!(updates[0].expires_on, Some(expires));
        assert_eq!(updates[0].not_before, None);
        assert_eq!(updates[0].groups.as_deref(), Some(&["ops".to_string()][..]));
        assert_eq!(updates[0].note.as_deref(), Some("preserved"));
        assert_eq!(updates[0].folder, None);
    }
}

#[cfg(test)]
mod rollback_version_resolver_tests {
    //! Behavior lock for the friendly-version→GUID resolver that moved into
    //! `AzureSecretBackend::rollback` (`resolve_version_guid`) during Phase 2.
    //! Exercised through the public `SecretBackend::rollback` against a hand
    //! -rolled `SecretOperations` mock — no live Azure. The mock's
    //! `rollback_secret` echoes the resolved version back as the returned
    //! `SecretProperties.version`, so each test can assert exactly which
    //! version string the resolver handed to Azure's rollback API.

    use super::*;
    use crate::error::Result;

    fn sp(version_number: Option<u32>, version: &str) -> SecretProperties {
        SecretProperties {
            name: "s".into(),
            original_name: "s".into(),
            value: None,
            version: version.into(),
            version_number,
            created_timestamp: 0,
            created_on: String::new(),
            updated_on: String::new(),
            enabled: true,
            expires_on: None,
            not_before: None,
            tags: HashMap::new(),
            content_type: String::new(),
            recovery_level: None,
        }
    }

    struct VersionResolverMock {
        versions: Vec<SecretProperties>,
    }

    #[async_trait]
    impl SecretOperations for VersionResolverMock {
        async fn get_secret_versions(
            &self,
            _vault: &str,
            _name: &str,
        ) -> Result<Vec<SecretProperties>> {
            Ok(self.versions.clone())
        }

        async fn rollback_secret(
            &self,
            _vault: &str,
            _name: &str,
            version: &str,
        ) -> Result<SecretProperties> {
            // Echo the resolved version GUID back so the caller can assert
            // exactly what `resolve_version_guid` passed to the rollback API.
            Ok(sp(None, version))
        }

        // ── Unused in these tests ──────────────────────────────────────
        async fn set_secret(&self, _v: &str, _r: &SecretRequest) -> Result<SecretProperties> {
            unimplemented!()
        }
        async fn get_secret(&self, _v: &str, _n: &str, _i: bool) -> Result<SecretProperties> {
            unimplemented!()
        }
        async fn get_secret_version(
            &self,
            _v: &str,
            _n: &str,
            _ver: &str,
            _i: bool,
        ) -> Result<SecretProperties> {
            unimplemented!()
        }
        async fn list_secrets(&self, _v: &str, _g: Option<&str>) -> Result<Vec<SecretSummary>> {
            unimplemented!()
        }
        async fn delete_secret(&self, _v: &str, _n: &str) -> Result<()> {
            unimplemented!()
        }
        async fn update_secret(
            &self,
            _v: &str,
            _n: &str,
            _r: &SecretRequest,
        ) -> Result<SecretProperties> {
            unimplemented!()
        }
        async fn restore_secret(&self, _v: &str, _n: &str) -> Result<SecretProperties> {
            unimplemented!()
        }
        async fn purge_secret(&self, _v: &str, _n: &str) -> Result<()> {
            unimplemented!()
        }
        async fn list_deleted_secrets(&self, _v: &str) -> Result<Vec<DeletedSecretSummary>> {
            unimplemented!()
        }
        async fn secret_exists(&self, _v: &str, _n: &str) -> Result<bool> {
            unimplemented!()
        }
        async fn backup_secret(&self, _v: &str, _n: &str) -> Result<Vec<u8>> {
            unimplemented!()
        }
        async fn restore_secret_from_backup(
            &self,
            _v: &str,
            _b: &[u8],
        ) -> Result<SecretProperties> {
            unimplemented!()
        }
    }

    fn backend(versions: Vec<SecretProperties>) -> AzureSecretBackend {
        AzureSecretBackend::new(Arc::new(VersionResolverMock { versions }))
    }

    /// v0 is rejected before any version lookup — v1 is the oldest version.
    #[tokio::test]
    async fn v0_is_rejected() {
        let err = backend(vec![])
            .rollback("vault", "s", "v0")
            .await
            .expect_err("v0 must be rejected");
        assert!(
            err.to_string()
                .contains("Version number must be 1 or greater"),
            "unexpected error: {err}"
        );
    }

    /// A friendly version above the available range errors with a message that
    /// names the missing version and the available v1–vN range.
    #[tokio::test]
    async fn unknown_friendly_version_errors_with_available_range() {
        let versions = vec![sp(Some(1), "guid-1"), sp(Some(2), "guid-2")];
        let err = backend(versions)
            .rollback("vault", "s", "v5")
            .await
            .expect_err("v5 must be not-found");
        let msg = err.to_string();
        assert!(
            msg.contains("Version v5 not found"),
            "unexpected error: {msg}"
        );
        assert!(
            msg.contains("v1–v2"),
            "should name the available range: {msg}"
        );
    }

    /// A raw GUID (not a `vN`/`N` number) passes straight through to the
    /// rollback API unchanged — no version lookup.
    #[tokio::test]
    async fn raw_guid_passes_through_unchanged() {
        let props = backend(vec![])
            .rollback("vault", "s", "abcd-1234-raw-guid")
            .await
            .expect("raw GUID must pass through");
        assert_eq!(props.version, "abcd-1234-raw-guid");
    }

    /// A friendly `vN` (and bare `N`) resolves to that version's underlying
    /// GUID, which is what gets handed to the rollback API.
    #[tokio::test]
    async fn friendly_version_resolves_to_guid() {
        let versions = vec![sp(Some(1), "guid-1"), sp(Some(2), "guid-2")];
        let props = backend(versions.clone())
            .rollback("vault", "s", "v2")
            .await
            .expect("v2 must resolve");
        assert_eq!(props.version, "guid-2");

        // Bare "1" (no leading 'v') resolves the same way.
        let props = backend(versions)
            .rollback("vault", "s", "1")
            .await
            .expect("bare 1 must resolve");
        assert_eq!(props.version, "guid-1");
    }
}
