//! Offline, forward-only attachment transfer execution and durable recovery.
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absent_store_listing_does_not_create_anything() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("recovery");
        assert!(RecoveryStore::new(root.clone()).list().unwrap().is_empty());
        assert!(!root.exists());
    }
    #[test]
    fn recovery_parent_components_open_without_partial_creation() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("uncreated/../recovery");
        let session = storage::Session::open(&root, true).unwrap();
        assert!(!dir.path().join("uncreated").exists());
        drop(session);
        storage::Session::open(&dir.path().join("recovery"), false).unwrap();
    }
    #[test]
    fn configured_recovery_falls_back_only_when_legacy_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().canonicalize().unwrap().join("config");
        let data = dir.path().canonicalize().unwrap().join("data");
        std::fs::create_dir(&config).unwrap();
        assert_eq!(
            resolve_configured_path(&config.join("xv.conf"), None, Some(data.clone())).unwrap(),
            config.join("transfer-recovery")
        );
        std::fs::write(config.join(".git"), "gitdir: elsewhere").unwrap();
        assert_eq!(
            resolve_configured_path(&config.join("xv.conf"), None, Some(data.clone())).unwrap(),
            data.join("crosstache/transfer-recovery")
        );
        std::fs::create_dir(config.join("transfer-recovery")).unwrap();
        std::fs::write(
            config.join("transfer-recovery/identity"),
            "existing identity",
        )
        .unwrap();
        let error = resolve_configured_path(&config.join("xv.conf"), None, Some(data))
            .unwrap_err()
            .to_string();
        assert!(error.contains("XV_TRANSFER_RECOVERY_DIR"));
        assert!(error.contains("existing"));
    }
    #[test]
    fn configured_recovery_requires_override_when_all_defaults_are_git() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(home.join(".git")).unwrap();
        let config = home.join(".config/xv.conf");
        assert!(
            resolve_configured_path(&config, None, Some(home.join("data")))
                .unwrap_err()
                .to_string()
                .contains("XV_TRANSFER_RECOVERY_DIR")
        );
        let explicit = dir.path().canonicalize().unwrap().join("safe");
        assert_eq!(
            resolve_configured_path(&config, Some(explicit.clone()), None).unwrap(),
            explicit
        );
        assert!(!explicit.exists());
        assert!(resolve_configured_path(&config, Some(home.join("unsafe")), None).is_err());
    }
    #[test]
    fn operation_ids_are_canonical_and_path_safe() {
        assert!(valid_id("../../identity").is_err());
        assert!(valid_id("00000000000000000000000000000000").is_err());
        assert!(valid_id(&uuid::Uuid::new_v4().to_string()).is_ok());
    }
}
use super::attachment_transfer::{
    self as transfer, TransferIntent, TransferOperation, TransferPlan,
};
use crate::backend::{Backend, SecretBackend};
use crate::error::{CrosstacheError, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn invalid() -> CrosstacheError {
    CrosstacheError::invalid_argument("Invalid or unsafe attachment transfer recovery data")
}
fn conflict() -> CrosstacheError {
    CrosstacheError::conflict(
        "Attachment transfer evidence changed; source retained where possible",
    )
}
fn valid_id(id: &str) -> Result<()> {
    let parsed = uuid::Uuid::parse_str(id).map_err(|_| invalid())?;
    if parsed.to_string() != id {
        return Err(invalid());
    }
    Ok(())
}
#[derive(Debug, Serialize)]
pub struct OwnedExecutionPreview {
    pub intent: TransferIntent,
    pub attachment_count: usize,
    pub ciphertext_bytes: u64,
    pub execution_supported: bool,
    pub limitation: String,
    pub source_keys: Vec<transfer::TransferKeyBinding>,
    pub destination_key: Option<transfer::TransferKeyBinding>,
}
#[derive(Debug, Serialize)]
pub struct TransferReport {
    pub id: String,
    pub complete: bool,
}
// The binary shares this module but only its UI uses recovery listing. Keep the
// public library API available in ordinary file-ops builds.
#[cfg_attr(not(any(feature = "ui", test)), allow(dead_code))]
#[derive(Debug, Serialize)]
pub struct TransferSummary {
    pub id: String,
    pub intent: TransferIntent,
    pub complete: bool,
}
#[derive(Debug)]
pub struct RecoveryStore {
    root: PathBuf,
    config_path: Option<PathBuf>,
}
impl RecoveryStore {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            config_path: None,
        }
    }
    pub fn default_path() -> Result<PathBuf> {
        configured_path(&crate::config::settings::Config::get_config_path()?)
    }
    #[cfg(feature = "ui")]
    pub(crate) fn from_config_path(config_path: PathBuf) -> Self {
        Self {
            root: PathBuf::new(),
            config_path: Some(config_path),
        }
    }
    fn resolved_root(&self) -> Result<PathBuf> {
        match &self.config_path {
            Some(config) => configured_path(config),
            None => crate::utils::recovery_path::resolve(&self.root),
        }
    }
    // Used by UI and external library consumers, but not the non-UI binary.
    #[cfg_attr(not(any(feature = "ui", test)), allow(dead_code))]
    pub fn list(&self) -> Result<Vec<TransferSummary>> {
        let root = self.resolved_root()?;
        if !root.try_exists().map_err(|_| invalid())? {
            return Ok(vec![]);
        }
        let session = storage::Session::open(&root, false)?;
        let mut result = vec![];
        for name in session.names()? {
            let id = name.strip_suffix(".age").ok_or_else(invalid)?;
            valid_id(id)?;
            let journal = load(&session, id)?;
            result.push(TransferSummary {
                id: journal.id,
                intent: journal.plan.intent,
                complete: journal.phase == Phase::Complete,
            });
        }
        Ok(result)
    }
}
fn configured_path(config: &std::path::Path) -> Result<PathBuf> {
    resolve_configured_path(
        config,
        std::env::var_os("XV_TRANSFER_RECOVERY_DIR").map(PathBuf::from),
        dirs::data_local_dir(),
    )
}

fn resolve_configured_path(
    config: &std::path::Path,
    override_path: Option<PathBuf>,
    data_dir: Option<PathBuf>,
) -> Result<PathBuf> {
    use crate::utils::recovery_path::{in_git, resolve};
    let outside_git = |path: PathBuf| -> Result<PathBuf> {
        let resolved = resolve(&path)?;
        if in_git(&resolved)? {
            return Err(CrosstacheError::invalid_argument(
                "Transfer recovery must be outside Git worktrees; set XV_TRANSFER_RECOVERY_DIR to a private directory outside Git and backend stores (or use CLI --recovery-dir)",
            ));
        }
        Ok(resolved)
    };
    if let Some(path) = override_path {
        return outside_git(path);
    }
    let legacy = resolve(
        &config
            .parent()
            .ok_or_else(invalid)?
            .join("transfer-recovery"),
    )?;
    if !in_git(&legacy)? {
        return Ok(legacy);
    }
    // Never silently abandon an identity or journal when selecting a safe default.
    match std::fs::read_dir(&legacy) {
        Ok(mut entries) => {
            if entries
                .next()
                .transpose()
                .map_err(|error| {
                    CrosstacheError::config(format!("Inspect existing recovery entry: {error}"))
                })?
                .is_some()
            {
                return Err(CrosstacheError::invalid_argument(format!(
                "The existing recovery directory '{}' is inside Git. Move the entire directory, including its identity and journals, to a private location outside Git and backend stores, then set XV_TRANSFER_RECOVERY_DIR to that location (or use CLI --recovery-dir)", legacy.display()
            )));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(CrosstacheError::config(format!(
                "Inspect existing recovery directory: {error}"
            )))
        }
    }
    let data = data_dir.ok_or_else(|| CrosstacheError::invalid_argument(
        "No safe recovery default is available; set XV_TRANSFER_RECOVERY_DIR to a private directory outside Git and backend stores",
    ))?;
    outside_git(data.join("crosstache/transfer-recovery"))
}

use super::{attachment_rewrap as rewrap, manager::SecretProperties};
use crate::backend::{
    error::BackendError, secret::rename_request_from_properties, TransferLocation,
};
use age::secrecy::ExposeSecret;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::io::Read;
use zeroize::Zeroizing;
#[path = "attachment_transfer_storage.rs"]
mod storage;
const MAGIC_V2: &[u8] = b"XV-TRANSFER-JOURNAL-2\0";
const MAGIC: &[u8] = b"XV-TRANSFER-JOURNAL-3\0";
const MAX: usize = transfer::MAX_MANIFEST_BYTES;
// New operations reserve the full six-byte JSON expansion of opaque tokens.
// Legacy v2 decoding/execution retains its original token acceptance.
const MAX_GENERATION_BYTES: usize = 1024;
const MAX_REVISION_BYTES: usize = 1024;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    DestinationNamespacePending,
    Prepared,
    SecretCreatePending,
    Copying,
    Cleanup,
    SecretDeletePending,
    Complete,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Generation {
    etag: String,
    modified: chrono::DateTime<chrono::Utc>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum FileState {
    Prepared,
    CreatePending,
    Verified { generation: Generation },
    DeletePending { generation: Generation },
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalV2 {
    schema: u32,
    id: String,
    plan: TransferPlan,
    location: TransferLocation,
    source_revision: String,
    secret_commitment: String,
    destination_revision: Option<String>,
    phase: Phase,
    files: Vec<FileState>,
    sequence: u64,
}
impl JournalV2 {
    fn validate(&self) -> Result<()> {
        valid_id(&self.id)?;
        self.plan.validate()?;
        if self.phase == Phase::DestinationNamespacePending
            || self.plan.intent.destination_folder.is_some()
            || self.schema != 2
            || self.files.len() != self.plan.files.len()
            || self.source_revision.is_empty()
            || self.secret_commitment.len() != 64
            || hex::decode(&self.secret_commitment).is_err()
            || self.location.secrets.is_empty()
            || self.location.files.is_empty()
            || self.location.files.starts_with("local-pending-files:")
            || self.location.keys.is_empty()
            || self.plan.intent.operation != TransferOperation::Move
            || self.plan.intent.destination_key_id.is_some()
            || self.plan.intent.source_name == self.plan.intent.destination_name
        {
            return Err(invalid());
        }
        let before_create = matches!(self.phase, Phase::Prepared | Phase::SecretCreatePending);
        if before_create != self.destination_revision.is_none()
            || self
                .destination_revision
                .as_ref()
                .is_some_and(String::is_empty)
        {
            return Err(invalid());
        }
        for state in &self.files {
            if before_create && !matches!(state, FileState::Prepared) {
                return Err(invalid());
            }
            if matches!(
                self.phase,
                Phase::Cleanup | Phase::SecretDeletePending | Phase::Complete
            ) && !matches!(
                state,
                FileState::Verified { .. } | FileState::DeletePending { .. }
            ) {
                return Err(invalid());
            }
            if matches!(self.phase, Phase::SecretDeletePending | Phase::Complete)
                && !matches!(state, FileState::DeletePending { .. })
            {
                return Err(invalid());
            }
            if matches!(state, FileState::DeletePending { .. })
                && !matches!(
                    self.phase,
                    Phase::Cleanup | Phase::SecretDeletePending | Phase::Complete
                )
            {
                return Err(invalid());
            }
            if let FileState::Verified { generation } | FileState::DeletePending { generation } =
                state
            {
                if generation.etag.is_empty() {
                    return Err(invalid());
                }
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DestinationFileEvidence {
    ciphertext_sha256: String,
    size: u64,
    metadata: std::collections::HashMap<String, String>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    // An in-memory route only: authenticated v2 envelopes are always written
    // back as their original v2 schema/magic, without migration size overhead.
    #[serde(skip)]
    legacy_v2: bool,
    schema: u32,
    id: String,
    plan: TransferPlan,
    source_location: TransferLocation,
    destination_location: TransferLocation,
    destination_ring: Option<rewrap::SavedRingBinding>,
    source_revision: String,
    source_commitment: String,
    secret_commitment: String,
    destination_revision: Option<String>,
    phase: Phase,
    files: Vec<FileState>,
    destination_files: Vec<Option<DestinationFileEvidence>>,
    sequence: u64,
}
impl Journal {
    fn legacy_record(&self) -> Result<JournalV2> {
        if !self.legacy_v2
            || self.schema != 3
            || self.source_location != self.destination_location
            || self.destination_ring.is_some()
            || self.source_commitment != self.secret_commitment
            || self.destination_files.len() != self.files.len()
            || self.destination_files.iter().any(Option::is_some)
        {
            return Err(invalid());
        }
        let old = JournalV2 {
            schema: 2,
            id: self.id.clone(),
            plan: self.plan.clone(),
            location: self.source_location.clone(),
            source_revision: self.source_revision.clone(),
            secret_commitment: self.secret_commitment.clone(),
            destination_revision: self.destination_revision.clone(),
            phase: self.phase.clone(),
            files: self.files.clone(),
            sequence: self.sequence,
        };
        old.validate()?;
        Ok(old)
    }
    fn validate(&self) -> Result<()> {
        if self.legacy_v2 {
            return self.legacy_record().map(|_| ());
        }
        valid_id(&self.id)?;
        self.plan.validate()?;
        let is_move = self.plan.intent.operation == TransferOperation::Move;
        let before_create = matches!(
            self.phase,
            Phase::DestinationNamespacePending | Phase::Prepared | Phase::SecretCreatePending
        );
        if self.schema != 3
            || self.files.len() != self.plan.files.len()
            || self.destination_files.len() != self.files.len()
            || self.source_revision.is_empty()
            || self.source_revision.len() > MAX_REVISION_BYTES
            || !valid_hash(&self.secret_commitment)
            || !valid_hash(&self.source_commitment)
            || before_create != self.destination_revision.is_none()
            || self
                .destination_revision
                .as_ref()
                .is_some_and(|revision| revision.is_empty() || revision.len() > MAX_REVISION_BYTES)
            || !is_move && matches!(self.phase, Phase::Cleanup | Phase::SecretDeletePending)
        {
            return Err(invalid());
        }
        for location in [&self.source_location, &self.destination_location] {
            if location.secrets.is_empty() || location.files.is_empty() || location.keys.is_empty()
            {
                return Err(invalid());
            }
        }
        if self
            .source_location
            .files
            .starts_with("local-pending-files:")
            || (self
                .destination_location
                .files
                .starts_with("local-pending-files:")
                != (self.phase == Phase::DestinationNamespacePending))
        {
            return Err(invalid());
        }
        physical_intent(
            &self.plan.intent,
            &self.source_location,
            &self.destination_location,
        )?;
        if let Some(ring) = &self.destination_ring {
            ring.validate()?;
            if let Some(key) = &self.plan.destination_key {
                if &ring.target != key {
                    return Err(invalid());
                }
            }
        } else if self.plan.destination_key.is_some() {
            return Err(invalid());
        }
        for ((file, state), evidence) in self
            .plan
            .files
            .iter()
            .zip(&self.files)
            .zip(&self.destination_files)
        {
            if before_create && !matches!(state, FileState::Prepared)
                || matches!(state, FileState::Prepared) != evidence.is_none()
                || !is_move && matches!(state, FileState::DeletePending { .. })
                || matches!(
                    self.phase,
                    Phase::Cleanup | Phase::SecretDeletePending | Phase::Complete
                ) && !matches!(
                    state,
                    FileState::Verified { .. } | FileState::DeletePending { .. }
                )
                || is_move
                    && matches!(self.phase, Phase::SecretDeletePending | Phase::Complete)
                    && !matches!(state, FileState::DeletePending { .. })
                || matches!(state, FileState::DeletePending { .. })
                    && !matches!(
                        self.phase,
                        Phase::Cleanup | Phase::SecretDeletePending | Phase::Complete
                    )
            {
                return Err(invalid());
            }
            if let FileState::Verified { generation } | FileState::DeletePending { generation } =
                state
            {
                if generation.etag.is_empty() || generation.etag.len() > MAX_GENERATION_BYTES {
                    return Err(invalid());
                }
            }
            if file.size > transfer::MAX_SOURCE_CIPHERTEXT_BYTES
                || file.etag.len() > MAX_GENERATION_BYTES
            {
                return Err(invalid());
            }
            if let Some(evidence) = evidence {
                if !valid_hash(&evidence.ciphertext_sha256)
                    || evidence.size > transfer::MAX_OUTPUT_CIPHERTEXT_BYTES
                {
                    return Err(invalid());
                }
                let mut expected_metadata = file.metadata.clone();
                if let Some(key) = &self.plan.destination_key {
                    super::attachment_key::apply_crypto_metadata(
                        &mut expected_metadata,
                        &key_reference(key)?,
                    );
                } else if evidence.size != file.size
                    || evidence.ciphertext_sha256 != file.ciphertext_sha256
                {
                    return Err(invalid());
                }
                if evidence.metadata != expected_metadata {
                    return Err(invalid());
                }
            }
        }
        Ok(())
    }
}
fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl JournalV2 {
    fn migrate(self) -> Result<Journal> {
        self.validate()?;
        let destination_files = self.files.iter().map(|_| None).collect();
        let result = Journal {
            legacy_v2: true,
            schema: 3,
            id: self.id,
            plan: self.plan,
            source_location: self.location.clone(),
            destination_location: self.location,
            destination_ring: None,
            source_revision: self.source_revision,
            source_commitment: self.secret_commitment.clone(),
            secret_commitment: self.secret_commitment,
            destination_revision: self.destination_revision,
            phase: self.phase,
            files: self.files,
            destination_files,
            sequence: self.sequence,
        };
        result.validate()?;
        Ok(result)
    }
}
fn mac(identity: &age::x25519::Identity, purpose: &[u8]) -> Hmac<Sha256> {
    let private = identity.to_string();
    let mut derivation = Sha256::new();
    derivation.update(b"crosstache/transfer-journal/private-key/v2\0");
    derivation.update(private.expose_secret().as_bytes());
    let key = Zeroizing::new(<[u8; 32]>::from(derivation.finalize()));
    let mut mac = Hmac::<Sha256>::new_from_slice(&*key).expect("HMAC accepts SHA256 key");
    mac.update(purpose);
    mac
}
fn commitment(
    identity: &age::x25519::Identity,
    secret: &SecretProperties,
    name: &str,
) -> Result<String> {
    let request = rename_request_from_properties(name, secret).map_err(|_| conflict())?;
    request_commitment(identity, &request)
}
fn request_commitment(
    identity: &age::x25519::Identity,
    request: &super::manager::SecretRequest,
) -> Result<String> {
    // A fixed tuple plus sorted user tags makes the commitment independent of
    // HashMap iteration and serde_json's optional preserve_order feature.
    let tags: std::collections::BTreeMap<_, _> =
        request.tags.as_ref().into_iter().flatten().collect();
    let canonical = (
        &request.name,
        request.value.as_str(),
        &request.content_type,
        request.enabled,
        request.expires_on,
        request.not_before,
        tags,
        &request.groups,
        &request.note,
        &request.folder,
    );
    let bytes = Zeroizing::new(serde_json::to_vec(&canonical).map_err(|_| invalid())?);
    let mut mac = mac(identity, b"secret-semantic-commitment\0");
    mac.update(&(bytes.len() as u64).to_be_bytes());
    mac.update(&bytes);
    Ok(hex::encode(mac.finalize().into_bytes()))
}
fn encode(journal: &Journal, identity: &age::x25519::Identity) -> Result<Vec<u8>> {
    journal.validate()?;
    let magic = if journal.legacy_v2 { MAGIC_V2 } else { MAGIC };
    let bytes = Zeroizing::new(if journal.legacy_v2 {
        serde_json::to_vec(&journal.legacy_record()?).map_err(|_| invalid())?
    } else {
        serde_json::to_vec(journal).map_err(|_| invalid())?
    });
    if bytes.len() > MAX - 65536 {
        return Err(invalid());
    }
    let encrypted = crate::backend::local::crypto::encrypt_bytes(&bytes, &[identity.to_public()])?;
    let mut auth = mac(identity, magic);
    auth.update(&encrypted);
    let mut result = magic.to_vec();
    result.extend_from_slice(&auth.finalize().into_bytes());
    result.extend(encrypted);
    if result.len() > MAX {
        return Err(invalid());
    }
    Ok(result)
}
fn decode(bytes: &[u8], identity: &age::x25519::Identity) -> Result<Journal> {
    if bytes.len() > MAX {
        return Err(invalid());
    }
    let magic = if bytes.starts_with(MAGIC) {
        MAGIC
    } else {
        MAGIC_V2
    };
    let body = bytes.strip_prefix(magic).ok_or_else(invalid)?;
    if body.len() <= 32 {
        return Err(invalid());
    }
    let (tag, encrypted) = body.split_at(32);
    let mut auth = mac(identity, magic);
    auth.update(encrypted);
    auth.verify_slice(tag).map_err(|_| invalid())?;
    let decryptor = match age::Decryptor::new_buffered(encrypted).map_err(|_| invalid())? {
        age::Decryptor::Recipients(d) => d,
        _ => return Err(invalid()),
    };
    let reader = decryptor
        .decrypt(std::iter::once(identity as &dyn age::Identity))
        .map_err(|_| invalid())?;
    let mut plaintext = Zeroizing::new(Vec::new());
    reader
        .take(MAX as u64 + 1)
        .read_to_end(&mut plaintext)
        .map_err(|_| invalid())?;
    if plaintext.len() > MAX {
        return Err(invalid());
    }
    let journal: Journal = if magic == MAGIC_V2 {
        serde_json::from_slice::<JournalV2>(&plaintext)
            .map_err(|_| invalid())?
            .migrate()?
    } else {
        serde_json::from_slice(&plaintext).map_err(|_| invalid())?
    };
    journal.validate()?;
    Ok(journal)
}
fn load(session: &storage::Session, id: &str) -> Result<Journal> {
    valid_id(id)?;
    let journal = decode(&session.read(&format!("{id}.age"), MAX)?, &session.identity)?;
    if journal.id != id {
        return Err(invalid());
    }
    Ok(journal)
}
fn save(session: &storage::Session, journal: &mut Journal) -> Result<()> {
    journal.sequence = journal.sequence.checked_add(1).ok_or_else(invalid)?;
    session.write(
        &format!("{}.age", journal.id),
        &encode(journal, &session.identity)?,
    )?;
    boundary("journal_saved")
}
#[cfg(not(test))]
fn boundary(_label: &str) -> Result<()> {
    Ok(())
}
#[cfg(test)]
thread_local! { static FAIL_AT: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) }; }
#[cfg(test)]
fn boundary(_label: &str) -> Result<()> {
    FAIL_AT.with(|slot| match slot.get() {
        Some(0) => {
            slot.set(None);
            Err(CrosstacheError::conflict("Injected interruption"))
        }
        Some(n) => {
            slot.set(Some(n - 1));
            Ok(())
        }
        None => Ok(()),
    })
}
fn physical_intent(
    intent: &TransferIntent,
    a: &TransferLocation,
    b: &TransferLocation,
) -> Result<()> {
    if (a.secrets == b.secrets || a.files == b.files)
        && intent.source_name == intent.destination_name
    {
        return Err(CrosstacheError::conflict(
            "Source and destination physically overlap",
        ));
    }
    if intent.source == intent.destination {
        if a != b {
            return Err(conflict());
        }
    } else if a.keys == b.keys {
        return Err(BackendError::Unsupported(
            "Aliased key namespaces: use the same resolved endpoint for both names".into(),
        )
        .into());
    }
    Ok(())
}
async fn supported(
    source: &dyn Backend,
    destination: &dyn Backend,
    intent: &TransferIntent,
) -> Result<(TransferLocation, TransferLocation)> {
    supported_route(source, destination, intent, false).await
}
async fn supported_route(
    source: &dyn Backend,
    destination: &dyn Backend,
    intent: &TransferIntent,
    legacy_v2: bool,
) -> Result<(TransferLocation, TransferLocation)> {
    intent.validate()?;
    let a = source.transfer_location(&intent.source.vault).await?;
    if a.files.starts_with("local-pending-files:") {
        return Err(BackendError::Unsupported("Source has no initialized attachment storage; use ordinary copy or move for secrets without attachments".into()).into());
    }
    let b = destination
        .transfer_location(&intent.destination.vault)
        .await
        .map_err(|error| CrosstacheError::invalid_argument(format!(
            "Destination namespace unavailable: {error}. Create the destination vault and explicitly initialize its attachment key with attachment-key initialize --apply --offline before transferring; use --to-key-id with its active key ID."
        )))?;
    if legacy_v2 {
        if source.name() != "local"
            || destination.name() != "local"
            || a != b
            || intent.operation != TransferOperation::Move
            || intent.destination_key_id.is_some()
            || intent.destination_folder.is_some()
            || intent.source_name == intent.destination_name
        {
            return Err(invalid());
        }
    } else {
        physical_intent(intent, &a, &b)?;
    }
    let ss = source.guarded_secrets();
    let ds = destination.guarded_secrets();
    let sf = source.files().ok_or_else(invalid)?;
    let df = destination.files().ok_or_else(invalid)?;
    if !ds.supports_atomic_create()
        || !df.supports_atomic_create()
        || intent.operation == TransferOperation::Move
            && (!ss.supports_conditional_delete() || !sf.supports_conditional_delete())
    {
        return Err(BackendError::Unsupported("Attachment Copy requires atomic destination secret/file create; Move additionally requires conditional source secret/file delete".into()).into());
    }
    Ok((a, b))
}
fn destination_request(
    source: &SecretProperties,
    intent: &TransferIntent,
) -> Result<super::manager::SecretRequest> {
    let mut request = rename_request_from_properties(&intent.destination_name, source)?;
    if let Some(folder) = &intent.destination_folder {
        request.folder = if folder == "/" {
            None
        } else {
            Some(folder.clone())
        };
    }
    Ok(request)
}
async fn checked_plan(
    source: &dyn Backend,
    destination: &dyn Backend,
    intent: TransferIntent,
) -> Result<TransferPlan> {
    let before = source
        .guarded_secrets()
        .get_transfer_snapshot(&intent.source.vault, &intent.source_name, true)
        .await?;
    let plan = transfer::plan(source, destination, intent).await?;
    let after = source
        .guarded_secrets()
        .get_transfer_snapshot(&plan.intent.source.vault, &plan.intent.source_name, true)
        .await?;
    if before.revision != after.revision
        || after.properties.version != plan.source_version
        || after.properties.name != plan.intent.source_name
    {
        return Err(conflict());
    }
    let request = destination_request(&after.properties, &plan.intent)?;
    crate::backend::secret::validate_transfer_request(destination, &request)?;
    destination
        .guarded_secrets()
        .validate_transfer_metadata(&plan.intent.destination.vault, &request)
        .await?;
    Ok(plan)
}
fn owned_preview(
    plan: &TransferPlan,
    support: Result<(TransferLocation, TransferLocation)>,
) -> OwnedExecutionPreview {
    let preview = plan.preview();
    let limitation = match &support {
        Ok(_) => "Offline only: stop all writers. Maximum source ciphertext 256 MiB; one-file encryption holds multiple buffers plus provider overhead, not streaming".into(),
        Err(error) => error.to_string(),
    };
    OwnedExecutionPreview {
        intent: preview.intent.clone(),
        attachment_count: preview.attachment_count,
        ciphertext_bytes: preview.ciphertext_bytes,
        execution_supported: support.is_ok(),
        limitation,
        source_keys: preview.source_keys.into_iter().cloned().collect(),
        destination_key: preview.destination_key.clone(),
    }
}
async fn load_destination_ring(
    destination: &dyn Backend,
    plan: &TransferPlan,
) -> Result<Option<rewrap::SavedRingBinding>> {
    if plan.files.is_empty() {
        return Ok(None);
    }
    let keys = destination.attachment_keys();
    let pointer = keys
        .get_secret(
            &plan.intent.destination.vault,
            super::attachment_key::ACTIVE_POINTER_SECRET,
            true,
        )
        .await?;
    let active = match pointer
        .value
        .as_deref()
        .and_then(|v| super::attachment_key::parse_pointer_value(v))
    {
        Some(super::attachment_key::PointerKind::V2 { active, .. }) => active,
        _ => return Err(conflict()),
    };
    let ring = rewrap::Ring::load(keys.as_ref(), &plan.intent.destination.vault, &active).await?;
    let saved = ring.saved();
    if plan
        .destination_key
        .as_ref()
        .is_some_and(|expected| expected != &saved.target)
    {
        return Err(conflict());
    }
    Ok(Some(saved))
}
fn validate_journal_budget(
    plan: &TransferPlan,
    locations: &(TransferLocation, TransferLocation),
    ring: &Option<rewrap::SavedRingBinding>,
    revision: &str,
) -> Result<()> {
    if revision.is_empty()
        || revision.len() > MAX_REVISION_BYTES
        || plan
            .files
            .iter()
            .any(|file| file.etag.is_empty() || file.etag.len() > MAX_GENERATION_BYTES)
    {
        return Err(BackendError::Unsupported(
            "Transfer provider revision/generation exceeds the 1024-byte recovery token limit"
                .into(),
        )
        .into());
    }
    let mut states = Vec::with_capacity(plan.files.len());
    let mut evidence = Vec::with_capacity(plan.files.len());
    for file in &plan.files {
        let mut metadata = file.metadata.clone();
        if let Some(binding) = &plan.destination_key {
            super::attachment_key::apply_crypto_metadata(&mut metadata, &key_reference(binding)?);
        }
        states.push(serde_json::json!({"state": "delete_pending", "generation": {
            "etag": "\0".repeat(MAX_GENERATION_BYTES), "modified": "+262142-12-31T23:59:59.999999999Z"
        }}));
        evidence.push(serde_json::json!({"ciphertext_sha256": "0".repeat(64), "size": u64::MAX, "metadata": metadata}));
    }
    // NUL reserves maximal JSON escaping. Numeric fields use maximum width;
    // the longest phase/state names and chrono date width cover future phases.
    // Local's pending namespace marker is longer than its eventual pinned ID.
    let worst = serde_json::json!({
        "schema": 3, "id": "00000000-0000-4000-8000-000000000000", "plan": plan,
        "source_location": locations.0, "destination_location": locations.1, "destination_ring": ring,
        "source_revision": revision, "source_commitment": "0".repeat(64), "secret_commitment": "0".repeat(64),
        "destination_revision": "\0".repeat(MAX_REVISION_BYTES), "phase": "destination_namespace_pending",
        "files": states, "destination_files": evidence, "sequence": u64::MAX,
    });
    if serde_json::to_vec(&worst).map_err(|_| invalid())?.len() > MAX - 2 * 65536 {
        return Err(BackendError::Unsupported("Transfer metadata exceeds the durable recovery journal budget; reduce the selected attachment metadata before retrying".into()).into());
    }
    Ok(())
}
/// Strict read-only whole-operation validation, suitable for batch preflight.
/// Does not open/create recovery storage or initialize keys/vaults/directories.
pub async fn preflight(
    source: &dyn Backend,
    destination: &dyn Backend,
    intent: TransferIntent,
) -> Result<OwnedExecutionPreview> {
    let locations = supported(source, destination, &intent).await?;
    // Readback needs value disclosure even while the destination is absent.
    // Authorize that exact route now instead of discovering denial after create.
    match destination
        .guarded_secrets()
        .get_transfer_snapshot(&intent.destination.vault, &intent.destination_name, true)
        .await
    {
        Err(BackendError::NotFound { .. }) => {}
        Ok(_) => return Err(conflict()),
        Err(error) => return Err(error.into()),
    }
    if intent.operation == TransferOperation::Move {
        source
            .guarded_secrets()
            .validate_transfer_delete(&intent.source.vault, &intent.source_name)
            .await?;
    }
    let plan = checked_plan(source, destination, intent).await?;
    let ring = load_destination_ring(destination, &plan).await?;
    let secret = source
        .guarded_secrets()
        .get_transfer_snapshot(&plan.intent.source.vault, &plan.intent.source_name, true)
        .await?;
    validate_journal_budget(&plan, &locations, &ring, &secret.revision)?;
    Ok(owned_preview(&plan, Ok(locations)))
}
pub async fn preview(
    source: &dyn Backend,
    destination: &dyn Backend,
    intent: TransferIntent,
) -> Result<OwnedExecutionPreview> {
    let support = supported(source, destination, &intent).await;
    let plan = checked_plan(source, destination, intent).await?;
    Ok(owned_preview(&plan, support))
}
fn offline_assertion(offline: bool) -> Result<()> {
    if !offline {
        return Err(CrosstacheError::invalid_argument(
            "Stop all writers and explicitly acknowledge --offline before applying or resuming",
        ));
    }
    Ok(())
}
fn recoverable(id: &str, intent: &TransferIntent, cause: &CrosstacheError) -> CrosstacheError {
    let operation = if intent.operation == TransferOperation::Move {
        " --move"
    } else {
        ""
    };
    let quote = |s: &str| format!("'{}'", s.replace('\'', "'\"'\"'"));
    let key = intent
        .destination_key_id
        .as_ref()
        .map(|s| format!(" --to-key-id {}", quote(s)))
        .unwrap_or_default();
    let folder = intent
        .destination_folder
        .as_ref()
        .map(|s| format!(" --to-folder {}", quote(s)))
        .unwrap_or_default();
    let saved = serde_json::to_string(intent).unwrap_or_else(|_| "unavailable".into());
    CrosstacheError::conflict(format!("Attachment transfer {id} stopped: {cause}. Retain both names and recovery files. Saved intent: {saved}. Resume with xv transfer {} --from <source-vault-or-alias> --to <destination-vault-or-alias> --new-name {}{operation}{key}{folder} --offline --resume {id}. Resolve each endpoint to its saved backend/vault and use the same recovery directory", quote(&intent.source_name), quote(&intent.destination_name)))
}
pub async fn apply(
    source: &dyn Backend,
    destination: &dyn Backend,
    intent: TransferIntent,
    offline: bool,
    recovery: &RecoveryStore,
) -> Result<TransferReport> {
    offline_assertion(offline)?;
    preflight(source, destination, intent.clone()).await?;
    let root = recovery.resolved_root()?;
    let (source_location, destination_location) = supported(source, destination, &intent).await?;
    let initial_secret = source
        .guarded_secrets()
        .get_transfer_snapshot(&intent.source.vault, &intent.source_name, true)
        .await?;
    let plan = checked_plan(source, destination, intent).await?;
    source
        .validate_transfer_recovery_path(&plan.intent.source.vault, &root)
        .await?;
    destination
        .validate_transfer_recovery_path(&plan.intent.destination.vault, &root)
        .await?;
    let destination_ring = load_destination_ring(destination, &plan).await?;
    let session = storage::Session::open(&root, true)?;
    let secret = source
        .guarded_secrets()
        .get_transfer_snapshot(&plan.intent.source.vault, &plan.intent.source_name, true)
        .await?;
    if secret.properties.version != plan.source_version
        || secret.revision != initial_secret.revision
    {
        return Err(conflict());
    }
    let mut journal = Journal {
        legacy_v2: false,
        schema: 3,
        id: uuid::Uuid::new_v4().to_string(),
        source_revision: secret.revision,
        source_commitment: commitment(
            &session.identity,
            &secret.properties,
            &plan.intent.destination_name,
        )?,
        secret_commitment: request_commitment(
            &session.identity,
            &destination_request(&secret.properties, &plan.intent)?,
        )?,
        files: plan.files.iter().map(|_| FileState::Prepared).collect(),
        destination_files: plan.files.iter().map(|_| None).collect(),
        plan,
        phase: if destination_location
            .files
            .starts_with("local-pending-files:")
        {
            Phase::DestinationNamespacePending
        } else {
            Phase::Prepared
        },
        source_location,
        destination_location,
        destination_ring,
        destination_revision: None,
        sequence: 0,
    };
    verify(source, destination, &session, &journal).await?;
    save(&session, &mut journal).map_err(|e| recoverable(&journal.id, &journal.plan.intent, &e))?;
    execute(source, destination, &session, &mut journal)
        .await
        .map_err(|e| recoverable(&journal.id, &journal.plan.intent, &e))
}
pub async fn resume(
    source: &dyn Backend,
    destination: &dyn Backend,
    expected: TransferIntent,
    id: &str,
    offline: bool,
    recovery: &RecoveryStore,
) -> Result<TransferReport> {
    valid_id(id)?;
    let mut recovery_intent = expected.clone();
    let result = async {
        offline_assertion(offline)?;
        let root = recovery.resolved_root()?;
        source
            .validate_transfer_recovery_path(&expected.source.vault, &root)
            .await?;
        destination
            .validate_transfer_recovery_path(&expected.destination.vault, &root)
            .await?;
        let session = storage::Session::open(&root, false)?;
        let mut journal = load(&session, id)?;
        recovery_intent = journal.plan.intent.clone();
        if journal.plan.intent != expected {
            return Err(invalid());
        }
        execute(source, destination, &session, &mut journal).await
    }
    .await;
    result.map_err(|e| recoverable(id, &recovery_intent, &e))
}
fn matches_file(
    snapshot: &rewrap::Snapshot,
    expected: &transfer::TransferFile,
    generation: Option<&Generation>,
    original: bool,
) -> bool {
    let info = &snapshot.info;
    info.name
        == if original {
            expected.source_name.as_str()
        } else {
            expected.destination_name.as_str()
        }
        && info.size == expected.size
        && info.content_type == expected.content_type
        && info.groups == expected.groups
        && info.tags == expected.tags
        && info.metadata == expected.metadata
        && hex::encode(Sha256::digest(&snapshot.data.content)) == expected.ciphertext_sha256
        && (!original
            || (info.etag == expected.etag && info.last_modified == expected.last_modified))
        && generation.is_none_or(|g| info.etag == g.etag && info.last_modified == g.modified)
}
fn key_reference(
    binding: &transfer::TransferKeyBinding,
) -> Result<super::attachment_key::AttachmentKeyRef> {
    use super::attachment_key as key;
    binding.validate()?;
    Ok(key::AttachmentKeyRef {
        key_id: key::AttachmentKeyId::parse(&binding.key_id).ok_or_else(invalid)?,
        slot: key::KeySlot::parse(&binding.slot).ok_or_else(invalid)?,
        provider_version: key::SecretVersion::new(binding.provider_version.clone()),
    })
}
async fn authenticate_saved(
    backend: &dyn Backend,
    vault: &str,
    binding: &transfer::TransferKeyBinding,
    snapshot: &rewrap::Snapshot,
) -> Result<Zeroizing<Vec<u8>>> {
    use super::attachment_key as key;
    let reference = key_reference(binding)?;
    let keys = backend.attachment_keys();
    let name = match reference.slot {
        key::KeySlot::Legacy => key::ACTIVE_POINTER_SECRET.to_owned(),
        key::KeySlot::Retained => key::retained_record_name(&reference.key_id),
    };
    let current = keys.get_secret(vault, &name, false).await?;
    if !current.enabled
        || (reference.slot == key::KeySlot::Retained
            && (!key::is_marked_key_record(&current.content_type)
                || current.version != binding.provider_version))
    {
        return Err(conflict());
    }
    rewrap::authenticate(keys.as_ref(), vault, &reference, snapshot).await
}
fn matches_destination(
    snapshot: &rewrap::Snapshot,
    file: &transfer::TransferFile,
    evidence: Option<&DestinationFileEvidence>,
    generation: Option<&Generation>,
) -> bool {
    let info = &snapshot.info;
    info.name == file.destination_name
        && info.size == evidence.map_or(file.size, |e| e.size)
        && info.metadata == *evidence.map_or(&file.metadata, |e| &e.metadata)
        && hex::encode(Sha256::digest(&snapshot.data.content))
            == *evidence.map_or(&file.ciphertext_sha256, |e| &e.ciphertext_sha256)
        && info.content_type == file.content_type
        && info.groups == file.groups
        && info.tags == file.tags
        && generation.is_none_or(|g| info.etag == g.etag && info.last_modified == g.modified)
}
async fn saved_snapshot(
    journal: &Journal,
    files: &dyn crate::backend::FileBackend,
    vault: &str,
    name: &str,
    original: bool,
) -> Result<rewrap::Snapshot> {
    if journal.legacy_v2 {
        rewrap::snapshot(files, vault, name).await
    } else if original {
        transfer::snapshot(files, vault, name).await
    } else {
        transfer::destination_snapshot(files, vault, name).await
    }
}
async fn verify(
    source: &dyn Backend,
    destination: &dyn Backend,
    session: &storage::Session,
    journal: &Journal,
) -> Result<Option<SecretProperties>> {
    journal.validate()?;
    session.check_location()?;
    let i = &journal.plan.intent;
    let (a, b) = supported_route(source, destination, i, journal.legacy_v2).await?;
    if a != journal.source_location
        || (journal.phase != Phase::DestinationNamespacePending
            && b != journal.destination_location)
        || b.secrets != journal.destination_location.secrets
        || b.keys != journal.destination_location.keys
    {
        return Err(conflict());
    }
    if let Some(ring) = &journal.destination_ring {
        ring.recheck(destination.attachment_keys().as_ref(), &i.destination.vault)
            .await?;
    }
    let source_secret = match source
        .guarded_secrets()
        .get_transfer_snapshot(&i.source.vault, &i.source_name, true)
        .await
    {
        Ok(s) => {
            if !journal.legacy_v2
                && (s.revision.is_empty() || s.revision.len() > MAX_REVISION_BYTES)
            {
                return Err(invalid());
            }
            if journal.phase == Phase::Complete && i.operation == TransferOperation::Move
                || s.revision != journal.source_revision
                || s.properties.name != i.source_name
                || commitment(&session.identity, &s.properties, &i.destination_name)?
                    != journal.source_commitment
                || request_commitment(&session.identity, &destination_request(&s.properties, i)?)?
                    != journal.secret_commitment
            {
                return Err(conflict());
            }
            Some(s.properties)
        }
        Err(BackendError::NotFound { .. })
            if i.operation == TransferOperation::Move
                && matches!(journal.phase, Phase::SecretDeletePending | Phase::Complete) =>
        {
            None
        }
        Err(_) => return Err(conflict()),
    };
    match destination
        .guarded_secrets()
        .get_transfer_snapshot(&i.destination.vault, &i.destination_name, true)
        .await
    {
        Ok(s) => {
            if !journal.legacy_v2
                && (s.revision.is_empty() || s.revision.len() > MAX_REVISION_BYTES)
            {
                return Err(invalid());
            }
            if matches!(
                journal.phase,
                Phase::DestinationNamespacePending | Phase::Prepared
            ) || s.properties.name != i.destination_name
                || commitment(&session.identity, &s.properties, &i.destination_name)?
                    != journal.secret_commitment
                || journal
                    .destination_revision
                    .as_ref()
                    .is_some_and(|r| *r != s.revision)
            {
                return Err(conflict());
            }
        }
        Err(BackendError::NotFound { .. })
            if matches!(
                journal.phase,
                Phase::DestinationNamespacePending | Phase::Prepared | Phase::SecretCreatePending
            ) => {}
        Err(_) => return Err(conflict()),
    }
    use std::collections::BTreeSet;
    let source_names = source
        .attachment_names(&i.source.vault, &i.source_name)
        .await?;
    let destination_names = destination
        .attachment_names(&i.destination.vault, &i.destination_name)
        .await?;
    let source_set: BTreeSet<_> = source_names.iter().collect();
    let destination_set: BTreeSet<_> = destination_names.iter().collect();
    if source_set.len() != source_names.len()
        || destination_set.len() != destination_names.len()
        || source_set
            .iter()
            .any(|n| !journal.plan.files.iter().any(|f| &f.source_name == *n))
        || destination_set
            .iter()
            .any(|n| !journal.plan.files.iter().any(|f| &f.destination_name == *n))
    {
        return Err(conflict());
    }
    if source_secret.is_none() && !source_set.is_empty() {
        return Err(conflict());
    }
    for ((file, state), evidence) in journal
        .plan
        .files
        .iter()
        .zip(&journal.files)
        .zip(&journal.destination_files)
    {
        if source_set.contains(&file.source_name) {
            let current = saved_snapshot(
                journal,
                source.files().ok_or_else(invalid)?,
                &i.source.vault,
                &file.source_name,
                true,
            )
            .await?;
            if !matches_file(&current, file, None, true) {
                return Err(conflict());
            }
            authenticate_saved(source, &i.source.vault, &file.source_key, &current).await?;
        } else if !matches!(state, FileState::DeletePending { .. }) {
            return Err(conflict());
        }
        if destination_set.contains(&file.destination_name) {
            if matches!(state, FileState::Prepared) {
                return Err(conflict());
            }
            let current = saved_snapshot(
                journal,
                destination.files().ok_or_else(invalid)?,
                &i.destination.vault,
                &file.destination_name,
                false,
            )
            .await?;
            if !journal.legacy_v2
                && (current.info.etag.is_empty() || current.info.etag.len() > MAX_GENERATION_BYTES)
            {
                return Err(invalid());
            }
            let generation = match state {
                FileState::Verified { generation } | FileState::DeletePending { generation } => {
                    Some(generation)
                }
                _ => None,
            };
            if !matches_destination(&current, file, evidence.as_ref(), generation) {
                return Err(conflict());
            }
            let plaintext = authenticate_saved(
                destination,
                &i.destination.vault,
                journal
                    .plan
                    .destination_key
                    .as_ref()
                    .unwrap_or(&file.source_key),
                &current,
            )
            .await?;
            drop(current);
            if matches!(state, FileState::CreatePending) {
                let original = saved_snapshot(
                    journal,
                    source.files().ok_or_else(invalid)?,
                    &i.source.vault,
                    &file.source_name,
                    true,
                )
                .await?;
                if !matches_file(&original, file, None, true) {
                    return Err(conflict());
                }
                let source_plaintext =
                    authenticate_saved(source, &i.source.vault, &file.source_key, &original)
                        .await?;
                if *plaintext != *source_plaintext {
                    return Err(conflict());
                }
            }
        } else if !matches!(state, FileState::Prepared | FileState::CreatePending) {
            return Err(conflict());
        }
    }
    Ok(source_secret)
}
async fn execute(
    source: &dyn Backend,
    destination: &dyn Backend,
    session: &storage::Session,
    journal: &mut Journal,
) -> Result<TransferReport> {
    verify(source, destination, session, journal).await?;
    if journal.phase == Phase::Complete {
        return Ok(TransferReport {
            id: journal.id.clone(),
            complete: true,
        });
    }
    if journal.phase == Phase::DestinationNamespacePending {
        verify(source, destination, session, journal).await?;
        boundary("before_namespace_prepare")?;
        let location = destination
            .prepare_transfer_destination(
                &journal.plan.intent.destination.vault,
                &journal.destination_location,
            )
            .await?;
        boundary("after_namespace_prepare")?;
        if location.files.starts_with("local-pending-files:") {
            return Err(conflict());
        }
        journal.destination_location = location;
        journal.phase = Phase::Prepared;
        save(session, journal)?;
        verify(source, destination, session, journal).await?;
    }
    if journal.phase == Phase::Prepared {
        journal.phase = Phase::SecretCreatePending;
        save(session, journal)?;
    }
    if journal.phase == Phase::SecretCreatePending {
        let source_secret = verify(source, destination, session, journal)
            .await?
            .ok_or_else(conflict)?;
        let i = &journal.plan.intent;
        match destination
            .guarded_secrets()
            .get_transfer_snapshot(&i.destination.vault, &i.destination_name, true)
            .await
        {
            Err(BackendError::NotFound { .. }) => {
                let request = destination_request(&source_secret, i)?;
                crate::backend::secret::validate_transfer_request(destination, &request)?;
                destination
                    .guarded_secrets()
                    .validate_transfer_metadata(&i.destination.vault, &request)
                    .await?;
                boundary("before_secret_create")?;
                destination
                    .guarded_secrets()
                    .create_secret_if_absent(&i.destination.vault, request)
                    .await?;
                boundary("after_secret_create")?;
            }
            Ok(_) => {}
            Err(e) => return Err(e.into()),
        }
        verify(source, destination, session, journal).await?;
        let snapshot = destination
            .guarded_secrets()
            .get_transfer_snapshot(&i.destination.vault, &i.destination_name, true)
            .await?;
        if commitment(&session.identity, &snapshot.properties, &i.destination_name)?
            != journal.secret_commitment
        {
            return Err(conflict());
        }
        journal.destination_revision = Some(snapshot.revision);
        journal.phase = Phase::Copying;
        save(session, journal)?;
    }
    if journal.phase == Phase::Copying {
        for index in 0..journal.files.len() {
            verify(source, destination, session, journal).await?;
            if matches!(
                journal.files[index],
                FileState::Prepared | FileState::CreatePending
            ) {
                let i = journal.plan.intent.clone();
                let file = journal.plan.files[index].clone();
                let dest_files = destination.files().ok_or_else(invalid)?;
                // Inspect first. Existing randomized output is judged against the
                // OLD durable digest; never replace evidence before reconciliation.
                match dest_files
                    .get_file_restore_info(&i.destination.vault, &file.destination_name)
                    .await
                {
                    Err(BackendError::NotFound { .. }) => {
                        let current = saved_snapshot(
                            journal,
                            source.files().ok_or_else(invalid)?,
                            &i.source.vault,
                            &file.source_name,
                            true,
                        )
                        .await?;
                        if !matches_file(&current, &file, None, true) {
                            return Err(conflict());
                        }
                        let plaintext =
                            authenticate_saved(source, &i.source.vault, &file.source_key, &current)
                                .await?;
                        let mut metadata = file.metadata.clone();
                        let content = if let Some(binding) = &journal.plan.destination_key {
                            let reference = key_reference(binding)?;
                            let identity = rewrap::exact_identity(
                                destination.attachment_keys().as_ref(),
                                &i.destination.vault,
                                &reference,
                            )
                            .await?;
                            super::attachment_key::apply_crypto_metadata(&mut metadata, &reference);
                            let ciphertext = crate::backend::local::crypto::encrypt_bytes(
                                &plaintext,
                                &[identity.to_public()],
                            )?;
                            drop(current);
                            ciphertext
                        } else {
                            current.data.content
                        };
                        drop(plaintext);
                        if !journal.legacy_v2
                            && content.len() as u64 > transfer::MAX_OUTPUT_CIPHERTEXT_BYTES
                        {
                            return Err(invalid());
                        }
                        if !journal.legacy_v2 {
                            journal.destination_files[index] = Some(DestinationFileEvidence {
                                ciphertext_sha256: hex::encode(Sha256::digest(&content)),
                                size: content.len() as u64,
                                metadata: metadata.clone(),
                            });
                        }
                        journal.files[index] = FileState::CreatePending;
                        save(session, journal)?;
                        verify(source, destination, session, journal).await?;
                        boundary("before_file_create")?;
                        dest_files
                            .upload_file_if_absent(
                                &i.destination.vault,
                                crate::blob::models::FileUploadRequest {
                                    name: file.destination_name.clone(),
                                    content,
                                    content_type: Some(file.content_type.clone()),
                                    groups: file.groups.clone(),
                                    metadata,
                                    tags: file.tags.clone(),
                                },
                                None,
                            )
                            .await?;
                        boundary("after_file_create")?;
                    }
                    Ok(_) => {}
                    Err(e) => return Err(e.into()),
                }
                verify(source, destination, session, journal).await?;
                let current = saved_snapshot(
                    journal,
                    dest_files,
                    &i.destination.vault,
                    &file.destination_name,
                    false,
                )
                .await?;
                if !matches_destination(
                    &current,
                    &file,
                    journal.destination_files[index].as_ref(),
                    None,
                ) || current.info.etag.is_empty()
                {
                    return Err(conflict());
                }
                journal.files[index] = FileState::Verified {
                    generation: Generation {
                        etag: current.info.etag,
                        modified: current.info.last_modified,
                    },
                };
                save(session, journal)?;
            }
        }
        verify(source, destination, session, journal).await?;
        journal.phase = if journal.plan.intent.operation == TransferOperation::Move {
            Phase::Cleanup
        } else {
            Phase::Complete
        };
        save(session, journal)?;
    }
    if journal.phase == Phase::Cleanup {
        for index in 0..journal.files.len() {
            verify(source, destination, session, journal).await?;
            source
                .guarded_secrets()
                .validate_transfer_delete(
                    &journal.plan.intent.source.vault,
                    &journal.plan.intent.source_name,
                )
                .await?;
            if let FileState::Verified { generation } = &journal.files[index] {
                journal.files[index] = FileState::DeletePending {
                    generation: generation.clone(),
                };
                save(session, journal)?;
            }
            verify(source, destination, session, journal).await?;
            let i = &journal.plan.intent;
            let file = &journal.plan.files[index];
            let sf = source.files().ok_or_else(invalid)?;
            match sf
                .get_file_restore_info(&i.source.vault, &file.source_name)
                .await
            {
                Ok(info) => {
                    if info.etag != file.etag {
                        return Err(conflict());
                    }
                    boundary("before_file_delete")?;
                    sf.delete_file_if_etag(&i.source.vault, &file.source_name, &file.etag)
                        .await?;
                    boundary("after_file_delete")?;
                }
                Err(BackendError::NotFound { .. }) => {}
                Err(e) => return Err(e.into()),
            }
            verify(source, destination, session, journal).await?;
            if source
                .attachment_names(&i.source.vault, &i.source_name)
                .await?
                .contains(&file.source_name)
            {
                return Err(conflict());
            }
            save(session, journal)?;
        }
        verify(source, destination, session, journal).await?;
        let i = &journal.plan.intent;
        if !source
            .attachment_names(&i.source.vault, &i.source_name)
            .await?
            .is_empty()
        {
            return Err(conflict());
        }
        journal.phase = Phase::SecretDeletePending;
        save(session, journal)?;
    }
    if journal.phase == Phase::SecretDeletePending {
        let original = verify(source, destination, session, journal).await?;
        if original.is_some() {
            let i = &journal.plan.intent;
            if !source
                .attachment_names(&i.source.vault, &i.source_name)
                .await?
                .is_empty()
            {
                return Err(conflict());
            }
            source
                .guarded_secrets()
                .validate_transfer_delete(&i.source.vault, &i.source_name)
                .await?;
            boundary("before_secret_delete")?;
            source
                .guarded_secrets()
                .delete_secret_if_revision(
                    &i.source.vault,
                    &i.source_name,
                    &journal.source_revision,
                )
                .await?;
            boundary("after_secret_delete")?;
        }
        if verify(source, destination, session, journal)
            .await?
            .is_some()
        {
            return Err(conflict());
        }
        journal.phase = Phase::Complete;
        save(session, journal)?;
    }
    Ok(TransferReport {
        id: journal.id.clone(),
        complete: journal.phase == Phase::Complete,
    })
}
#[cfg(test)]
#[path = "attachment_transfer_execution_tests.rs"]
mod execution_tests;
