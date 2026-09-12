//! Read-only attachment key and file-reference observations.

use serde::Serialize;

use crate::backend::attachment_keys::AttachmentKeyStore;
use crate::backend::error::BackendError;
use crate::backend::file::FileBackend;
use crate::blob::models::FileListRequest;
use crate::error::{AttachmentError, Result};
use zeroize::Zeroizing;

use crate::secret::attachment_key::{
    self, AttachmentKeyMaterial, DownloadPlan, KeySlot, PointerKind, SecretVersion,
    ACTIVE_POINTER_SECRET,
};

/// Safe, serializable observation of the attachment-key pointer and its active record.
#[derive(Debug, Serialize)]
pub struct KeyStatus {
    pub schema_version: u32,
    pub mode: String,
    pub active_key_id: Option<String>,
    pub active_version: Option<String>,
    pub legacy_key_id: Option<String>,
    pub problem_code: Option<String>,
}

/// Safe, serializable metadata-only inventory of all file references.
#[derive(Debug, Serialize)]
pub struct FileInventory {
    pub schema_version: u32,
    pub observation: String,
    pub files: Vec<FileInventoryEntry>,
}

/// One file's managed-reference classification. Only validated schema-1
/// references populate the key fields.
#[derive(Debug, Serialize)]
pub struct FileInventoryEntry {
    pub name: String,
    pub classification: String,
    pub key_id: Option<String>,
    pub slot: Option<String>,
    pub provider_version: Option<String>,
}

fn invalid_status(problem: AttachmentError) -> KeyStatus {
    KeyStatus {
        schema_version: 1,
        mode: "invalid".into(),
        active_key_id: None,
        active_version: None,
        legacy_key_id: None,
        problem_code: Some(problem.code().into()),
    }
}

/// Observe the canonical pointer and, for V2, verify the active retained
/// record's identity-derived ID. Domain integrity problems are reported with
/// stable safe codes; provider failures are returned to the caller.
pub async fn key_status(keys: &dyn AttachmentKeyStore, vault: &str) -> Result<KeyStatus> {
    let pointer = match keys.get_secret(vault, ACTIVE_POINTER_SECRET, true).await {
        Ok(pointer) => pointer,
        Err(BackendError::NotFound { .. }) => {
            return Ok(KeyStatus {
                schema_version: 1,
                mode: "absent".into(),
                active_key_id: None,
                active_version: None,
                legacy_key_id: None,
                problem_code: None,
            });
        }
        Err(error) => return Err(error.into()),
    };

    let Some(value) = pointer.value else {
        return Ok(invalid_status(AttachmentError::PointerInvalid));
    };
    match attachment_key::parse_pointer_value(value.expose_secret()) {
        Some(PointerKind::V1RawIdentity) => {
            if pointer.version.is_empty() {
                return Ok(invalid_status(AttachmentError::KeyVersionInvalid));
            }
            let Some(material) = AttachmentKeyMaterial::from_identity(
                KeySlot::Legacy,
                SecretVersion::new(pointer.version.clone()),
                Zeroizing::new(value.expose_secret().to_owned()),
            ) else {
                return Ok(invalid_status(AttachmentError::KeyInvalid));
            };
            Ok(KeyStatus {
                schema_version: 1,
                mode: "v1".into(),
                active_key_id: Some(material.reference().key_id.as_str().into()),
                active_version: Some(pointer.version),
                legacy_key_id: None,
                problem_code: None,
            })
        }
        Some(PointerKind::V2 { active, legacy }) => {
            let name = attachment_key::retained_record_name(&active);
            let record = match keys.get_secret(vault, &name, true).await {
                Ok(record) => record,
                Err(BackendError::NotFound { .. }) => {
                    return Ok(invalid_status(AttachmentError::KeyMissing));
                }
                Err(error) => return Err(error.into()),
            };
            let Some(identity) = record.value else {
                return Ok(invalid_status(AttachmentError::KeyInvalid));
            };
            if record.version.is_empty() {
                return Ok(invalid_status(AttachmentError::KeyVersionInvalid));
            }
            let Some(material) = AttachmentKeyMaterial::from_identity(
                KeySlot::Retained,
                SecretVersion::new(record.version.clone()),
                Zeroizing::new(identity.expose_secret().to_owned()),
            ) else {
                return Ok(invalid_status(AttachmentError::KeyInvalid));
            };
            if !material.verify_id(&active) {
                return Ok(invalid_status(AttachmentError::KeyMismatch));
            }
            Ok(KeyStatus {
                schema_version: 1,
                mode: "v2".into(),
                active_key_id: Some(active.as_str().into()),
                active_version: Some(record.version),
                legacy_key_id: legacy.map(|id| id.as_str().into()),
                problem_code: None,
            })
        }
        None => Ok(invalid_status(AttachmentError::PointerInvalid)),
    }
}

/// Enumerate every listed file and refresh its object metadata without reading
/// file bytes. Any provider failure aborts the report.
pub async fn file_inventory(files: &dyn FileBackend, vault: &str) -> Result<FileInventory> {
    let listed = files
        .list_files(
            vault,
            FileListRequest {
                prefix: None,
                groups: None,
                limit: None,
                delimiter: None,
            },
        )
        .await?;
    let mut entries = Vec::with_capacity(listed.len());
    for listed_file in listed {
        let info = files.get_file_info(vault, &listed_file.name).await?;
        let (classification, key_id, slot, provider_version) =
            match attachment_key::classify_download(&info.name, &info.metadata, true) {
                DownloadPlan::Schema1 { key_ref } => (
                    "schema1",
                    Some(key_ref.key_id.as_str().to_string()),
                    Some(key_ref.slot.as_str().to_string()),
                    Some(key_ref.provider_version.as_str().to_string()),
                ),
                DownloadPlan::LegacyNoSchema => ("legacy_unversioned", None, None, None),
                DownloadPlan::ReferenceInvalid | DownloadPlan::FailClosedNonCiphertext => {
                    ("invalid_reference", None, None, None)
                }
                DownloadPlan::Passthrough => ("unmanaged", None, None, None),
            };
        entries.push(FileInventoryEntry {
            name: info.name,
            classification: classification.into(),
            key_id,
            slot,
            provider_version,
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(FileInventory {
        schema_version: 1,
        observation: "metadata_only".into(),
        files: entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use age::secrecy::ExposeSecret;
    use async_trait::async_trait;
    use chrono::Utc;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::backend::file::FileDownloadSnapshot;
    use crate::blob::models::{FileInfo, FileUploadRequest};
    use crate::secret::domain::{SecretProperties, SecretRequest, SecretValue};
    use crate::utils::progress::ProgressReporter;

    fn secret(name: &str, value: Option<String>, version: &str) -> SecretProperties {
        SecretProperties {
            name: name.into(),
            original_name: name.into(),
            value: value.map(SecretValue::new),
            version: version.into(),
            version_number: None,
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

    struct FakeKeys {
        records: HashMap<String, SecretProperties>,
        deny_reads: bool,
        writes: AtomicUsize,
    }

    #[async_trait]
    impl AttachmentKeyStore for FakeKeys {
        async fn get_secret(
            &self,
            _vault: &str,
            name: &str,
            _include_value: bool,
        ) -> std::result::Result<SecretProperties, BackendError> {
            if self.deny_reads {
                return Err(BackendError::PermissionDenied(
                    "opaque provider detail".into(),
                ));
            }
            self.records
                .get(name)
                .cloned()
                .ok_or(BackendError::NotFound {
                    name: name.into(),
                    suggestion: None,
                })
        }

        async fn get_secret_version(
            &self,
            _vault: &str,
            _name: &str,
            _version: &str,
            _include_value: bool,
        ) -> std::result::Result<SecretProperties, BackendError> {
            panic!("status must not request historical versions")
        }

        async fn set_secret(
            &self,
            _vault: &str,
            _request: SecretRequest,
        ) -> std::result::Result<SecretProperties, BackendError> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            panic!("status must not mutate custody")
        }
    }

    fn fake_keys(records: impl IntoIterator<Item = SecretProperties>) -> FakeKeys {
        FakeKeys {
            records: records.into_iter().map(|p| (p.name.clone(), p)).collect(),
            deny_reads: false,
            writes: AtomicUsize::new(0),
        }
    }

    #[tokio::test]
    async fn empty_local_status_does_not_create_a_pointer() {
        use crate::backend::{local::LocalBackend, Backend};
        use crate::config::settings::LocalConfig;

        let temp = tempfile::tempdir().unwrap();
        let backend = LocalBackend::new(Some(&LocalConfig {
            store_path: Some(temp.path().join("store").display().to_string()),
            key_file: Some(temp.path().join("identity").display().to_string()),
            default_vault: Some("default".into()),
            ..Default::default()
        }))
        .unwrap();
        let report = key_status(backend.attachment_keys().as_ref(), "default")
            .await
            .unwrap();
        assert_eq!(report.mode, "absent");
        assert!(matches!(
            backend
                .secrets()
                .get_secret("default", ACTIVE_POINTER_SECRET, true)
                .await,
            Err(BackendError::NotFound { .. })
        ));
    }

    #[tokio::test]
    async fn valid_v1_and_v2_status_use_identity_derived_ids_and_exact_versions() {
        let v1 = age::x25519::Identity::generate();
        let v1_value = v1.to_string().expose_secret().to_string();
        let expected_v1 = attachment_key::AttachmentKeyId::derive(&v1.to_public().to_string());
        let keys = fake_keys([secret(ACTIVE_POINTER_SECRET, Some(v1_value), "legacy-v7")]);
        let report = key_status(&keys, "vault").await.unwrap();
        assert_eq!(report.mode, "v1");
        assert_eq!(report.active_key_id.as_deref(), Some(expected_v1.as_str()));
        assert_eq!(report.active_version.as_deref(), Some("legacy-v7"));

        let active_identity = age::x25519::Identity::generate();
        let active_id =
            attachment_key::AttachmentKeyId::derive(&active_identity.to_public().to_string());
        let legacy_id = attachment_key::AttachmentKeyId::derive("age1legacyfixture");
        let retained_name = attachment_key::retained_record_name(&active_id);
        let keys = fake_keys([
            secret(
                ACTIVE_POINTER_SECRET,
                Some(attachment_key::format_v2_pointer(
                    &active_id,
                    Some(&legacy_id),
                )),
                "pointer-v3",
            ),
            secret(
                &retained_name,
                Some(active_identity.to_string().expose_secret().to_string()),
                "provider-active-9",
            ),
        ]);
        let report = key_status(&keys, "vault").await.unwrap();
        assert_eq!(report.mode, "v2");
        assert_eq!(report.active_key_id.as_deref(), Some(active_id.as_str()));
        assert_eq!(report.active_version.as_deref(), Some("provider-active-9"));
        assert_eq!(report.legacy_key_id.as_deref(), Some(legacy_id.as_str()));
        assert_eq!(keys.writes.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn malformed_missing_and_mismatched_records_return_only_stable_problem_codes() {
        let malformed = fake_keys([secret(
            ACTIVE_POINTER_SECRET,
            Some("PRIVATE-MALFORMED-VALUE".into()),
            "v1",
        )]);
        let report = key_status(&malformed, "vault").await.unwrap();
        assert_eq!(report.mode, "invalid");
        assert_eq!(
            report.problem_code.as_deref(),
            Some("xv-attachment-pointer-invalid")
        );
        assert!(!format!("{report:?}").contains("PRIVATE-MALFORMED-VALUE"));

        let malformed_identity = fake_keys([secret(
            ACTIVE_POINTER_SECRET,
            Some("AGE-SECRET-KEY-1-NOT-A-VALID-IDENTITY".into()),
            "v1",
        )]);
        assert_eq!(
            key_status(&malformed_identity, "vault")
                .await
                .unwrap()
                .problem_code
                .as_deref(),
            Some("xv-attachment-key-invalid")
        );

        let valueless = fake_keys([secret(ACTIVE_POINTER_SECRET, None, "v1")]);
        assert_eq!(
            key_status(&valueless, "vault")
                .await
                .unwrap()
                .problem_code
                .as_deref(),
            Some("xv-attachment-pointer-invalid")
        );

        let expected = attachment_key::AttachmentKeyId::derive("age1expectedfixture");
        let missing = fake_keys([secret(
            ACTIVE_POINTER_SECRET,
            Some(attachment_key::format_v2_pointer(&expected, None)),
            "v2",
        )]);
        assert_eq!(
            key_status(&missing, "vault")
                .await
                .unwrap()
                .problem_code
                .as_deref(),
            Some("xv-attachment-key-missing")
        );

        let other = age::x25519::Identity::generate();
        let retained = attachment_key::retained_record_name(&expected);
        let mismatch = fake_keys([
            secret(
                ACTIVE_POINTER_SECRET,
                Some(attachment_key::format_v2_pointer(&expected, None)),
                "v2",
            ),
            secret(
                &retained,
                Some(other.to_string().expose_secret().to_string()),
                "record-v1",
            ),
        ]);
        assert_eq!(
            key_status(&mismatch, "vault")
                .await
                .unwrap()
                .problem_code
                .as_deref(),
            Some("xv-attachment-key-mismatch")
        );
    }

    #[tokio::test]
    async fn status_propagates_provider_permission_failures() {
        let keys = FakeKeys {
            records: HashMap::new(),
            deny_reads: true,
            writes: AtomicUsize::new(0),
        };
        assert!(matches!(
            key_status(&keys, "vault").await,
            Err(crate::error::CrosstacheError::PermissionDenied(_))
        ));
        assert_eq!(keys.writes.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn empty_active_provider_version_is_an_invalid_domain_status() {
        let identity = age::x25519::Identity::generate();
        let raw = identity.to_string().expose_secret().to_string();
        let v1 = fake_keys([secret(ACTIVE_POINTER_SECRET, Some(raw.clone()), "")]);
        assert_eq!(
            key_status(&v1, "vault")
                .await
                .unwrap()
                .problem_code
                .as_deref(),
            Some("xv-attachment-key-version-invalid")
        );

        let id = attachment_key::AttachmentKeyId::derive(&identity.to_public().to_string());
        let retained = attachment_key::retained_record_name(&id);
        let v2 = fake_keys([
            secret(
                ACTIVE_POINTER_SECRET,
                Some(attachment_key::format_v2_pointer(&id, None)),
                "pointer-v1",
            ),
            secret(&retained, Some(raw), ""),
        ]);
        assert_eq!(
            key_status(&v2, "vault")
                .await
                .unwrap()
                .problem_code
                .as_deref(),
            Some("xv-attachment-key-version-invalid")
        );
    }

    fn file(name: &str, metadata: HashMap<String, String>) -> FileInfo {
        FileInfo {
            name: name.into(),
            size: 0,
            content_type: "application/octet-stream".into(),
            last_modified: Utc::now(),
            etag: String::new(),
            groups: vec![],
            metadata,
            tags: HashMap::new(),
        }
    }

    struct FakeFiles {
        listed: Vec<FileInfo>,
        fresh: HashMap<String, FileInfo>,
        fail_list: bool,
        fail_info: Option<String>,
        info_reads: AtomicUsize,
    }

    #[async_trait]
    impl FileBackend for FakeFiles {
        async fn upload_file(
            &self,
            _vault: &str,
            _request: FileUploadRequest,
            _reporter: Option<&dyn ProgressReporter>,
        ) -> std::result::Result<FileInfo, BackendError> {
            panic!("inventory must not upload")
        }
        async fn download_file(
            &self,
            _vault: &str,
            _name: &str,
            _reporter: Option<&dyn ProgressReporter>,
        ) -> std::result::Result<Vec<u8>, BackendError> {
            panic!("inventory must not download bytes")
        }
        async fn download_file_snapshot(
            &self,
            _vault: &str,
            _name: &str,
            _reporter: Option<&dyn ProgressReporter>,
        ) -> std::result::Result<FileDownloadSnapshot, BackendError> {
            panic!("inventory must not download snapshots")
        }
        async fn list_files(
            &self,
            _vault: &str,
            request: FileListRequest,
        ) -> std::result::Result<Vec<FileInfo>, BackendError> {
            assert!(request.limit.is_none());
            assert!(request.prefix.is_none());
            assert!(request.groups.is_none());
            assert!(request.delimiter.is_none());
            if self.fail_list {
                return Err(BackendError::PermissionDenied(
                    "opaque listing failure".into(),
                ));
            }
            Ok(self.listed.clone())
        }
        async fn delete_file(
            &self,
            _vault: &str,
            _name: &str,
        ) -> std::result::Result<(), BackendError> {
            panic!("inventory must not delete")
        }
        async fn get_file_info(
            &self,
            _vault: &str,
            name: &str,
        ) -> std::result::Result<FileInfo, BackendError> {
            self.info_reads.fetch_add(1, Ordering::SeqCst);
            if self.fail_info.as_deref() == Some(name) {
                return Err(BackendError::Network("opaque transport failure".into()));
            }
            Ok(self.fresh[name].clone())
        }
    }

    #[tokio::test]
    async fn inventory_refreshes_metadata_classifies_all_files_and_reads_no_bytes() {
        let identity = age::x25519::Identity::generate();
        let key_id = attachment_key::AttachmentKeyId::derive(&identity.to_public().to_string());
        let mut schema1 = HashMap::from([
            (
                attachment_key::META_ENCRYPTED.into(),
                attachment_key::ENC_VALUE_AGE.into(),
            ),
            (
                attachment_key::META_CRYPTO_SCHEMA.into(),
                attachment_key::CRYPTO_SCHEMA_V1.into(),
            ),
            (attachment_key::META_KEY_ID.into(), key_id.as_str().into()),
            (attachment_key::META_KEY_SLOT.into(), "retained".into()),
            (attachment_key::META_KEY_VERSION.into(), "opaque-v12".into()),
        ]);
        let invalid = HashMap::from([
            (
                attachment_key::META_ENCRYPTED.into(),
                attachment_key::ENC_VALUE_AGE.into(),
            ),
            (attachment_key::META_CRYPTO_SCHEMA.into(), "99".into()),
        ]);
        let legacy = HashMap::from([(
            attachment_key::META_ENCRYPTED.into(),
            attachment_key::ENC_VALUE_AGE.into(),
        )]);
        let malformed = HashMap::from([
            (
                attachment_key::META_ENCRYPTED.into(),
                attachment_key::ENC_VALUE_AGE.into(),
            ),
            (
                attachment_key::META_CRYPTO_SCHEMA.into(),
                attachment_key::CRYPTO_SCHEMA_V1.into(),
            ),
        ]);
        let fresh: HashMap<String, FileInfo> = HashMap::from([
            (
                "z-schema".to_string(),
                file("z-schema", std::mem::take(&mut schema1)),
            ),
            ("a-ordinary".to_string(), file("a-ordinary", HashMap::new())),
            (
                "attachments/legacy".to_string(),
                file("attachments/legacy", legacy),
            ),
            ("k-malformed".to_string(), file("k-malformed", malformed)),
            ("m-invalid".to_string(), file("m-invalid", invalid)),
        ]);
        let backend = FakeFiles {
            listed: fresh
                .keys()
                .map(|name| file(name, HashMap::new()))
                .collect(),
            fresh,
            fail_list: false,
            fail_info: None,
            info_reads: AtomicUsize::new(0),
        };
        let report = file_inventory(&backend, "vault").await.unwrap();
        assert_eq!(report.observation, "metadata_only");
        assert_eq!(backend.info_reads.load(Ordering::SeqCst), 5);
        let values = serde_json::to_value(&report).unwrap();
        let rows = values["files"].as_array().unwrap();
        assert_eq!(rows[0]["name"], "a-ordinary");
        assert_eq!(rows[0]["classification"], "unmanaged");
        assert_eq!(rows[1]["classification"], "legacy_unversioned");
        assert_eq!(rows[2]["classification"], "invalid_reference");
        assert_eq!(rows[3]["classification"], "invalid_reference");
        assert_eq!(rows[4]["classification"], "schema1");
        assert_eq!(rows[4]["key_id"], key_id.as_str());
        assert_eq!(rows[4]["slot"], "retained");
        assert_eq!(rows[4]["provider_version"], "opaque-v12");
        assert!(rows[..4].iter().all(|row| row["key_id"].is_null()));
    }

    #[tokio::test]
    async fn inventory_aborts_on_metadata_provider_error() {
        let fresh = HashMap::from([
            ("a".into(), file("a", HashMap::new())),
            ("b".into(), file("b", HashMap::new())),
        ]);
        let backend = FakeFiles {
            listed: vec![file("a", HashMap::new()), file("b", HashMap::new())],
            fresh,
            fail_list: false,
            fail_info: Some("b".into()),
            info_reads: AtomicUsize::new(0),
        };
        assert!(matches!(
            file_inventory(&backend, "vault").await,
            Err(crate::error::CrosstacheError::NetworkError(_))
        ));
    }

    #[tokio::test]
    async fn inventory_aborts_on_list_provider_error_without_metadata_reads() {
        let backend = FakeFiles {
            listed: vec![],
            fresh: HashMap::new(),
            fail_list: true,
            fail_info: None,
            info_reads: AtomicUsize::new(0),
        };
        assert!(matches!(
            file_inventory(&backend, "vault").await,
            Err(crate::error::CrosstacheError::PermissionDenied(_))
        ));
        assert_eq!(backend.info_reads.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn reports_serialize_stable_public_contract() {
        let status = KeyStatus {
            schema_version: 1,
            mode: "absent".into(),
            active_key_id: None,
            active_version: None,
            legacy_key_id: None,
            problem_code: None,
        };
        assert_eq!(
            serde_json::to_value(status).unwrap(),
            serde_json::json!({
                "schema_version": 1,
                "mode": "absent",
                "active_key_id": null,
                "active_version": null,
                "legacy_key_id": null,
                "problem_code": null
            })
        );

        let inventory = FileInventory {
            schema_version: 1,
            observation: "metadata_only".into(),
            files: vec![],
        };
        assert_eq!(
            serde_json::to_value(inventory).unwrap(),
            serde_json::json!({
                "schema_version": 1,
                "observation": "metadata_only",
                "files": []
            })
        );
    }
}
