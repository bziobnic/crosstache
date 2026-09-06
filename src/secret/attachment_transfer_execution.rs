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
#[cfg(any(feature = "ui", test))]
#[derive(Debug, Serialize)]
pub struct TransferSummary {
    pub id: String,
    pub intent: TransferIntent,
    pub complete: bool,
}
#[derive(Debug)]
pub struct RecoveryStore {
    root: PathBuf,
}
impl RecoveryStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
    pub fn default_path() -> Result<PathBuf> {
        Ok(crate::config::settings::Config::get_config_path()?
            .parent()
            .ok_or_else(invalid)?
            .join("transfer-recovery"))
    }
    #[cfg(any(feature = "ui", test))]
    pub fn list(&self) -> Result<Vec<TransferSummary>> {
        if !self.root.try_exists().map_err(|_| invalid())? {
            return Ok(vec![]);
        }
        let session = storage::Session::open(&self.root, false)?;
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
const MAGIC: &[u8] = b"XV-TRANSFER-JOURNAL-2\0";
const MAX: usize = transfer::MAX_MANIFEST_BYTES;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
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
struct Journal {
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
impl Journal {
    fn validate(&self) -> Result<()> {
        valid_id(&self.id)?;
        self.plan.validate()?;
        if self.schema != 2
            || self.files.len() != self.plan.files.len()
            || self.source_revision.is_empty()
            || self.secret_commitment.len() != 64
            || hex::decode(&self.secret_commitment).is_err()
            || self.location.secrets.is_empty()
            || self.location.files.is_empty()
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
    let bytes = Zeroizing::new(serde_json::to_vec(journal).map_err(|_| invalid())?);
    if bytes.len() > MAX - 65536 {
        return Err(invalid());
    }
    let encrypted = crate::backend::local::crypto::encrypt_bytes(&bytes, &[identity.to_public()])?;
    let mut auth = mac(identity, MAGIC);
    auth.update(&encrypted);
    let mut result = MAGIC.to_vec();
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
    let body = bytes.strip_prefix(MAGIC).ok_or_else(invalid)?;
    if body.len() <= 32 {
        return Err(invalid());
    }
    let (tag, encrypted) = body.split_at(32);
    let mut auth = mac(identity, MAGIC);
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
    let journal: Journal = serde_json::from_slice(&plaintext).map_err(|_| invalid())?;
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
async fn supported(
    source: &dyn Backend,
    destination: &dyn Backend,
    intent: &TransferIntent,
) -> Result<TransferLocation> {
    intent.validate()?;
    if intent.operation != TransferOperation::Move
        || intent.destination_key_id.is_some()
        || intent.source_name == intent.destination_name
        || source.name() != "local"
        || destination.name() != "local"
    {
        return Err(BackendError::Unsupported(
            "Execution currently supports local same-vault attachment moves only".into(),
        )
        .into());
    }
    let a = source.transfer_location(&intent.source.vault).await?;
    let b = destination
        .transfer_location(&intent.destination.vault)
        .await?;
    if a != b {
        return Err(BackendError::Unsupported(
            "Attachment rename requires the same physical local vault".into(),
        )
        .into());
    }
    let ss = source.guarded_secrets();
    let ds = destination.guarded_secrets();
    let sf = source.files().ok_or_else(invalid)?;
    let df = destination.files().ok_or_else(invalid)?;
    if !ss.supports_conditional_delete()
        || !ds.supports_atomic_create()
        || !sf.supports_conditional_delete()
        || !df.supports_atomic_create()
    {
        return Err(BackendError::Unsupported(
            "Attachment transfer requires atomic create and conditional delete support".into(),
        )
        .into());
    }
    Ok(a)
}
pub async fn preview(
    source: &dyn Backend,
    destination: &dyn Backend,
    intent: TransferIntent,
) -> Result<OwnedExecutionPreview> {
    let support = supported(source, destination, &intent).await;
    let plan = transfer::plan(source, destination, intent).await?;
    let preview = plan.preview();
    Ok(OwnedExecutionPreview {
        intent: preview.intent.clone(), attachment_count: preview.attachment_count, ciphertext_bytes: preview.ciphertext_bytes,
        execution_supported: support.is_ok(), limitation: if support.is_ok() { "Offline only: stop all writers before applying or resuming" } else { "Execution requires a local same-physical-vault move with safe create and conditional delete support" }.into(),
        source_keys: preview.source_keys.into_iter().cloned().collect(), destination_key: preview.destination_key.clone(),
    })
}
fn offline_assertion(offline: bool) -> Result<()> {
    if !offline {
        return Err(CrosstacheError::invalid_argument(
            "Stop all writers and explicitly acknowledge --offline before applying or resuming",
        ));
    }
    Ok(())
}
fn recoverable(id: &str) -> CrosstacheError {
    CrosstacheError::conflict(format!("Attachment transfer {id} stopped; retain both names and recovery files. After resolving the conflict, resume with the same endpoints and --move --offline --resume {id}"))
}
pub async fn apply(
    source: &dyn Backend,
    destination: &dyn Backend,
    intent: TransferIntent,
    offline: bool,
    recovery: &RecoveryStore,
) -> Result<TransferReport> {
    offline_assertion(offline)?;
    let location = supported(source, destination, &intent).await?;
    let initial_secret = source
        .guarded_secrets()
        .get_secret_snapshot(&intent.source.vault, &intent.source_name, true)
        .await?;
    let plan = transfer::plan(source, destination, intent).await?;
    source
        .validate_transfer_recovery_path(&plan.intent.source.vault, &recovery.root)
        .await?;
    destination
        .validate_transfer_recovery_path(&plan.intent.destination.vault, &recovery.root)
        .await?;
    let session = storage::Session::open(&recovery.root, true)?;
    let secret = source
        .guarded_secrets()
        .get_secret_snapshot(&plan.intent.source.vault, &plan.intent.source_name, true)
        .await?;
    if secret.properties.version != plan.source_version
        || secret.revision != initial_secret.revision
    {
        return Err(conflict());
    }
    let mut journal = Journal {
        schema: 2,
        id: uuid::Uuid::new_v4().to_string(),
        source_revision: secret.revision,
        secret_commitment: commitment(
            &session.identity,
            &secret.properties,
            &plan.intent.destination_name,
        )?,
        files: plan.files.iter().map(|_| FileState::Prepared).collect(),
        plan,
        location,
        destination_revision: None,
        phase: Phase::Prepared,
        sequence: 0,
    };
    verify(source, destination, &session, &journal).await?;
    // A write can fail after rename/fsync; always return the operation ID once a
    // journal write has been attempted, including a lost success response.
    save(&session, &mut journal).map_err(|_| recoverable(&journal.id))?;
    execute(source, destination, &session, &mut journal)
        .await
        .map_err(|_| recoverable(&journal.id))
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
    let result = async {
        offline_assertion(offline)?;
        let location = supported(source, destination, &expected).await?;
        source
            .validate_transfer_recovery_path(&expected.source.vault, &recovery.root)
            .await?;
        destination
            .validate_transfer_recovery_path(&expected.destination.vault, &recovery.root)
            .await?;
        let session = storage::Session::open(&recovery.root, false)?;
        let mut journal = load(&session, id)?;
        if journal.plan.intent != expected || journal.location != location {
            return Err(invalid());
        }
        execute(source, destination, &session, &mut journal).await
    }
    .await;
    result.map_err(|_| recoverable(id))
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
async fn authenticate_saved(
    backend: &dyn Backend,
    vault: &str,
    file: &transfer::TransferFile,
    snapshot: &rewrap::Snapshot,
) -> Result<()> {
    use super::attachment_key as key;
    let reference = key::AttachmentKeyRef {
        key_id: key::AttachmentKeyId::parse(&file.source_key.key_id).ok_or_else(invalid)?,
        slot: match file.source_key.slot.as_str() {
            "legacy" => key::KeySlot::Legacy,
            "retained" => key::KeySlot::Retained,
            _ => return Err(invalid()),
        },
        provider_version: key::SecretVersion::new(file.source_key.provider_version.clone()),
    };
    let keys = backend.attachment_keys();
    let name = match reference.slot {
        key::KeySlot::Legacy => key::ACTIVE_POINTER_SECRET.to_owned(),
        key::KeySlot::Retained => key::retained_record_name(&reference.key_id),
    };
    let current = keys.get_secret(vault, &name, false).await?;
    if !current.enabled
        || (reference.slot == key::KeySlot::Retained
            && !key::is_marked_key_record(&current.content_type))
    {
        return Err(conflict());
    }
    let _plaintext = rewrap::authenticate(keys.as_ref(), vault, &reference, snapshot).await?;
    Ok(())
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
    if supported(source, destination, i).await? != journal.location {
        return Err(conflict());
    }
    let source_secret = match source
        .guarded_secrets()
        .get_secret_snapshot(&i.source.vault, &i.source_name, true)
        .await
    {
        Ok(s) => {
            if matches!(journal.phase, Phase::Complete)
                || s.revision != journal.source_revision
                || s.properties.name != i.source_name
                || commitment(&session.identity, &s.properties, &i.destination_name)?
                    != journal.secret_commitment
            {
                return Err(conflict());
            }
            Some(s.properties)
        }
        Err(BackendError::NotFound { .. })
            if matches!(journal.phase, Phase::SecretDeletePending | Phase::Complete) =>
        {
            None
        }
        Err(_) => return Err(conflict()),
    };
    match destination
        .guarded_secrets()
        .get_secret_snapshot(&i.destination.vault, &i.destination_name, true)
        .await
    {
        Ok(s) => {
            if journal.phase == Phase::Prepared
                || s.properties.name != i.destination_name
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
            if matches!(journal.phase, Phase::Prepared | Phase::SecretCreatePending) => {}
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
    for (file, state) in journal.plan.files.iter().zip(&journal.files) {
        if source_set.contains(&file.source_name) {
            let current = rewrap::snapshot(
                source.files().ok_or_else(invalid)?,
                &i.source.vault,
                &file.source_name,
            )
            .await?;
            if !matches_file(&current, file, None, true) {
                return Err(conflict());
            }
            authenticate_saved(source, &i.source.vault, file, &current).await?;
        } else if !matches!(state, FileState::DeletePending { .. }) {
            return Err(conflict());
        }
        if destination_set.contains(&file.destination_name) {
            if matches!(state, FileState::Prepared) {
                return Err(conflict());
            }
            let current = rewrap::snapshot(
                destination.files().ok_or_else(invalid)?,
                &i.destination.vault,
                &file.destination_name,
            )
            .await?;
            let generation = match state {
                FileState::Verified { generation } | FileState::DeletePending { generation } => {
                    Some(generation)
                }
                _ => None,
            };
            if !matches_file(&current, file, generation, false) {
                return Err(conflict());
            }
            authenticate_saved(destination, &i.destination.vault, file, &current).await?;
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
            .get_secret_snapshot(&i.destination.vault, &i.destination_name, true)
            .await
        {
            Err(BackendError::NotFound { .. }) => {
                boundary("before_secret_create")?;
                destination
                    .guarded_secrets()
                    .create_secret_if_absent(
                        &i.destination.vault,
                        rename_request_from_properties(&i.destination_name, &source_secret)?,
                    )
                    .await?;
                boundary("after_secret_create")?;
            }
            Ok(_) => {}
            Err(e) => return Err(e.into()),
        }
        verify(source, destination, session, journal).await?;
        let snapshot = destination
            .guarded_secrets()
            .get_secret_snapshot(&i.destination.vault, &i.destination_name, true)
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
            if matches!(journal.files[index], FileState::Prepared) {
                journal.files[index] = FileState::CreatePending;
                save(session, journal)?;
            }
            if matches!(journal.files[index], FileState::CreatePending) {
                verify(source, destination, session, journal).await?;
                let i = &journal.plan.intent;
                let file = &journal.plan.files[index];
                let dest_files = destination.files().ok_or_else(invalid)?;
                match dest_files
                    .get_file_restore_info(&i.destination.vault, &file.destination_name)
                    .await
                {
                    Err(BackendError::NotFound { .. }) => {
                        let current = rewrap::snapshot(
                            source.files().ok_or_else(invalid)?,
                            &i.source.vault,
                            &file.source_name,
                        )
                        .await?;
                        if !matches_file(&current, file, None, true) {
                            return Err(conflict());
                        }
                        boundary("before_file_create")?;
                        dest_files
                            .upload_file_if_absent(
                                &i.destination.vault,
                                crate::blob::models::FileUploadRequest {
                                    name: file.destination_name.clone(),
                                    content: current.data.content,
                                    content_type: Some(file.content_type.clone()),
                                    groups: file.groups.clone(),
                                    metadata: file.metadata.clone(),
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
                let current =
                    rewrap::snapshot(dest_files, &i.destination.vault, &file.destination_name)
                        .await?;
                if !matches_file(&current, file, None, false) || current.info.etag.is_empty() {
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
        journal.phase = Phase::Cleanup;
        save(session, journal)?;
    }
    if journal.phase == Phase::Cleanup {
        for index in 0..journal.files.len() {
            verify(source, destination, session, journal).await?;
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
