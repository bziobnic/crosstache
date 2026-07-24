//! AWS S3 file/blob backend.
//!
//! Files are stored under `<vault>/files/<name>` in a single configured
//! bucket, so vaults stay isolated (matching the local backend's per-vault
//! semantics, not Azure's single-container semantics).
//!
//! Security invariants (mirroring #223/#243 on the Azure side):
//! - File names are validated before key construction: no absolute paths,
//!   no `.`/`..` traversal segments, no backslashes or control characters.
//! - Downloads are size-guarded (5 GiB cap) via `HeadObject` before the GET
//!   and streamed to the writer — whole files are never buffered.
//! - Uploads above the part-size threshold use multipart upload, reading one
//!   chunk at a time (bounded memory), with abort-on-failure cleanup.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use aws_sdk_s3::Client as S3Client;
use chrono::Utc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::backend::error::BackendError;
use crate::backend::file::FileBackend;
use crate::blob::models::{BlobListItem, FileInfo, FileListRequest, FileUploadRequest};
use crate::config::settings::AwsConfig;
use crate::utils::format::format_size;
use crate::utils::progress::{NoopReporter, ProgressReporter};

use super::errors;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum object size accepted for download (mirrors the Azure blob cap).
pub const MAX_DOWNLOAD_SIZE_BYTES: u64 = 5 * 1024 * 1024 * 1024; // 5 GiB

/// S3 multipart constraint: every part except the last must be >= 5 MiB.
const MIN_PART_SIZE_BYTES: u64 = 5 * 1024 * 1024;

/// S3 multipart constraint: a single part may not exceed 5 GiB.
const MAX_PART_SIZE_BYTES: u64 = 5 * 1024 * 1024 * 1024;

/// S3 multipart constraint: at most 10,000 parts per upload.
const MAX_PARTS: u64 = 10_000;

/// S3 object key length limit (bytes).
const MAX_KEY_LEN: usize = 1024;

/// Key segment separating the vault prefix from file names.
const FILES_SEGMENT: &str = "files";

/// Object-metadata key carrying the comma-joined group list (same encoding
/// as the Azure blob backend).
const METADATA_KEY_GROUPS: &str = "groups";

/// S3 allows at most 10 tags per object.
const MAX_OBJECT_TAGS: usize = 10;

// ---------------------------------------------------------------------------
// Pure helpers (key construction, validation, chunk math)
// ---------------------------------------------------------------------------

/// Resolve the S3 bucket for file storage: `[aws].s3_bucket` first, then the
/// `XV_AWS_S3_BUCKET` env var. Errors with a setup hint when neither is set.
pub fn resolve_bucket(aws_cfg: &AwsConfig) -> Result<String, BackendError> {
    let env_bucket = std::env::var("XV_AWS_S3_BUCKET").ok();
    resolve_bucket_from(aws_cfg.s3_bucket.as_deref(), env_bucket.as_deref())
}

/// Pure bucket resolution (testable without env mutation).
fn resolve_bucket_from(cfg: Option<&str>, env: Option<&str>) -> Result<String, BackendError> {
    for candidate in [cfg, env] {
        if let Some(bucket) = candidate.map(str::trim) {
            if !bucket.is_empty() {
                return Ok(bucket.to_string());
            }
        }
    }
    Err(BackendError::InvalidArgument(
        "S3 bucket not configured for file storage: set [aws].s3_bucket in your config \
         (or the XV_AWS_S3_BUCKET env var) to an existing bucket. \
         xv does not create buckets."
            .into(),
    ))
}

/// Validate a vault name for use as a file key prefix.
///
/// The vault must form exactly one key segment — separators or traversal
/// tokens would break the `<vault>/files/<name>` isolation scheme.
pub fn validate_vault_for_files(vault: &str) -> Result<(), BackendError> {
    if vault.trim().is_empty() {
        return Err(BackendError::InvalidArgument(
            "vault name cannot be empty for file operations".into(),
        ));
    }
    if vault == "." || vault == ".." {
        return Err(BackendError::InvalidArgument(format!(
            "invalid vault name for file operations: '{vault}'"
        )));
    }
    if vault.contains('/') || vault.contains('\\') {
        return Err(BackendError::InvalidArgument(format!(
            "vault name '{vault}' cannot contain path separators for file operations"
        )));
    }
    if vault.chars().any(char::is_control) {
        return Err(BackendError::InvalidArgument(
            "vault name cannot contain control characters".into(),
        ));
    }
    Ok(())
}

/// Validate a user-facing file name before it becomes part of an object key.
///
/// Rejects empty names, absolute paths, `.`/`..` traversal segments,
/// backslashes, control characters, and trailing slashes. Forward slashes
/// are allowed for folder-style names (`docs/readme.md`), matching Azure.
pub fn validate_file_name(name: &str) -> Result<(), BackendError> {
    if name.trim().is_empty() {
        return Err(BackendError::InvalidArgument(
            "file name cannot be empty".into(),
        ));
    }
    if name.starts_with('/') {
        return Err(BackendError::InvalidArgument(format!(
            "file name '{name}' must be relative (no leading '/')"
        )));
    }
    if name.ends_with('/') {
        return Err(BackendError::InvalidArgument(format!(
            "file name '{name}' cannot end with '/'"
        )));
    }
    if name.contains('\\') {
        return Err(BackendError::InvalidArgument(format!(
            "file name '{name}' cannot contain backslashes (use '/' as separator)"
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(BackendError::InvalidArgument(
            "file name cannot contain control characters".into(),
        ));
    }
    if name
        .split('/')
        .any(|seg| seg.is_empty() || seg == "." || seg == "..")
    {
        return Err(BackendError::InvalidArgument(format!(
            "file name '{name}' contains path traversal or empty segments"
        )));
    }
    Ok(())
}

/// The key prefix under which a vault's files live.
pub fn files_prefix(vault: &str) -> String {
    format!("{vault}/{FILES_SEGMENT}/")
}

/// Build the object key for a file (no validation — see [`validated_key`]).
fn object_key(vault: &str, name: &str) -> String {
    format!("{vault}/{FILES_SEGMENT}/{name}")
}

/// Validate vault + name and return the full object key.
pub fn validated_key(vault: &str, name: &str) -> Result<String, BackendError> {
    validate_vault_for_files(vault)?;
    validate_file_name(name)?;
    let key = object_key(vault, name);
    if key.len() > MAX_KEY_LEN {
        return Err(BackendError::InvalidArgument(format!(
            "object key too long: {} bytes for '{name}' in vault '{vault}' (max {MAX_KEY_LEN})",
            key.len()
        )));
    }
    Ok(key)
}

/// Strip the vault's file prefix from an object key, returning the
/// user-facing file name. `None` if the key is outside the vault's files.
pub fn strip_file_key(vault: &str, key: &str) -> Option<String> {
    key.strip_prefix(&files_prefix(vault))
        .filter(|rest| !rest.is_empty())
        .map(str::to_string)
}

/// Compute the multipart part size for a file, honouring the configured
/// chunk size while satisfying S3 constraints: parts >= 5 MiB, <= 5 GiB,
/// and at most 10,000 parts per upload.
pub fn multipart_part_size(file_size: u64, configured_chunk_bytes: u64) -> u64 {
    let configured = configured_chunk_bytes.clamp(MIN_PART_SIZE_BYTES, MAX_PART_SIZE_BYTES);
    // Grow the part size if the configured one would exceed the part-count cap.
    let min_for_count = file_size.div_ceil(MAX_PARTS);
    configured.max(min_for_count).min(MAX_PART_SIZE_BYTES)
}

/// Number of parts a multipart upload will produce.
#[allow(dead_code)] // chunk-math contract; exercised by unit tests
pub fn part_count(file_size: u64, part_size: u64) -> u64 {
    file_size.div_ceil(part_size.max(1))
}

/// Reject downloads whose object size exceeds `max_bytes`.
fn validate_download_size(content_length: u64, max_bytes: u64) -> Result<(), BackendError> {
    if content_length > max_bytes {
        return Err(BackendError::InvalidArgument(format!(
            "Object size {} exceeds the maximum allowed download size of {}",
            format_size(content_length),
            format_size(max_bytes)
        )));
    }
    Ok(())
}

/// Convert the configured chunk size (MB) into bytes, clamped to >= 1 MB.
fn chunk_bytes(chunk_size_mb: usize) -> u64 {
    (chunk_size_mb.max(1) as u64) * 1024 * 1024
}

/// Encode object tags as an S3 `Tagging` query string. `None` when empty.
fn encode_tagging(tags: &HashMap<String, String>) -> Result<Option<String>, BackendError> {
    if tags.is_empty() {
        return Ok(None);
    }
    if tags.len() > MAX_OBJECT_TAGS {
        return Err(BackendError::InvalidArgument(format!(
            "Too many tags ({}) — S3 allows a maximum of {MAX_OBJECT_TAGS} tags per object. \
             Remove {} tag(s).",
            tags.len(),
            tags.len() - MAX_OBJECT_TAGS
        )));
    }
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    // Sort for deterministic output.
    let mut pairs: Vec<(&String, &String)> = tags.iter().collect();
    pairs.sort();
    for (k, v) in pairs {
        serializer.append_pair(k, v);
    }
    Ok(Some(serializer.finish()))
}

/// Convert an SDK timestamp to `chrono::DateTime<Utc>`.
fn to_chrono(dt: Option<&aws_sdk_s3::primitives::DateTime>) -> chrono::DateTime<Utc> {
    dt.and_then(|d| chrono::DateTime::from_timestamp(d.secs(), d.subsec_nanos()))
        .unwrap_or_else(Utc::now)
}

/// Read up to `chunk_size` bytes from `reader` (fewer at EOF, empty when
/// already at EOF). Same contract as the Azure blob manager's chunker.
async fn read_chunk<R: AsyncRead + Unpin>(
    reader: &mut R,
    chunk_size: usize,
) -> std::io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut chunk = vec![0u8; chunk_size];
    let mut bytes_read = 0;
    while bytes_read < chunk_size {
        let n = reader.read(&mut chunk[bytes_read..]).await?;
        if n == 0 {
            break;
        }
        bytes_read += n;
    }
    chunk.truncate(bytes_read);
    Ok(chunk)
}

/// Sort listing items: directories first, then files (both alphabetically).
fn sort_list_items(items: &mut [BlobListItem]) {
    items.sort_by(|a, b| match (a, b) {
        (BlobListItem::Directory { .. }, BlobListItem::File(_)) => std::cmp::Ordering::Less,
        (BlobListItem::File(_), BlobListItem::Directory { .. }) => std::cmp::Ordering::Greater,
        (BlobListItem::Directory { name: n1, .. }, BlobListItem::Directory { name: n2, .. }) => {
            n1.to_lowercase().cmp(&n2.to_lowercase())
        }
        (BlobListItem::File(f1), BlobListItem::File(f2)) => {
            f1.name.to_lowercase().cmp(&f2.name.to_lowercase())
        }
    });
}

// ---------------------------------------------------------------------------
// Upload spec
// ---------------------------------------------------------------------------

/// Metadata for a streaming upload (everything in [`FileUploadRequest`]
/// except the content, which arrives via the reader).
pub struct FileUploadSpec {
    pub name: String,
    pub content_type: Option<String>,
    pub groups: Vec<String>,
    pub metadata: HashMap<String, String>,
    pub tags: HashMap<String, String>,
}

impl From<&FileUploadRequest> for FileUploadSpec {
    fn from(request: &FileUploadRequest) -> Self {
        Self {
            name: request.name.clone(),
            content_type: request.content_type.clone(),
            groups: request.groups.clone(),
            metadata: request.metadata.clone(),
            tags: request.tags.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// AwsFileBackend
// ---------------------------------------------------------------------------

/// S3-backed file storage. One instance serves every vault; the target vault
/// is supplied per call (see [`FileBackend`]).
pub struct AwsFileBackend {
    client: S3Client,
    bucket: String,
    chunk_size_mb: usize,
    max_concurrent_uploads: usize,
}

impl AwsFileBackend {
    /// Create a backend for an existing bucket with default transfer settings.
    pub fn new(client: S3Client, bucket: String) -> Self {
        Self {
            client,
            bucket,
            chunk_size_mb: 8,
            max_concurrent_uploads: 3,
        }
    }

    /// Override chunk size (MB) and upload concurrency (builder style).
    /// Values are clamped to a minimum of 1; the effective part size is
    /// additionally clamped to S3's 5 MiB multipart minimum at upload time.
    pub fn with_transfer_config(mut self, chunk_size_mb: usize, max_concurrent: usize) -> Self {
        self.chunk_size_mb = chunk_size_mb.max(1);
        self.max_concurrent_uploads = max_concurrent.max(1);
        self
    }

    /// Upload a file from any async reader, streaming chunk-by-chunk.
    ///
    /// Files larger than the effective part size go through S3 multipart
    /// upload (concurrent parts, abort on failure); smaller files use a
    /// single `PutObject`. At most `max_concurrent_uploads + 1` chunks are
    /// in memory at any time.
    pub async fn upload_file_streaming<R: AsyncRead + Unpin>(
        &self,
        vault: &str,
        spec: FileUploadSpec,
        reader: &mut R,
        file_size: u64,
        reporter: &dyn ProgressReporter,
    ) -> Result<FileInfo, BackendError> {
        let key = validated_key(vault, &spec.name)?;

        let content_type = spec.content_type.clone().unwrap_or_else(|| {
            mime_guess::from_path(&spec.name)
                .first_or_octet_stream()
                .to_string()
        });

        // Object metadata, mirroring the Azure blob manager's conventions.
        let mut metadata = spec.metadata.clone();
        if !spec.groups.is_empty() {
            metadata.insert(METADATA_KEY_GROUPS.to_string(), spec.groups.join(","));
        }
        metadata.insert("uploaded_by".to_string(), "crosstache".to_string());
        metadata.insert("uploaded_at".to_string(), Utc::now().to_rfc3339());

        let tagging = encode_tagging(&spec.tags)?;

        reporter.set_total(file_size);

        let part_size = multipart_part_size(file_size, chunk_bytes(self.chunk_size_mb));
        // Cap reads at the declared size so a stream that keeps growing (e.g.
        // a file being appended to during upload) cannot make the S3 object
        // diverge from the size we validated and report in FileInfo.
        let mut reader = reader.take(file_size);
        let reader = &mut reader;
        let etag = if file_size > part_size {
            self.multipart_upload(
                &spec.name,
                &key,
                reader,
                part_size,
                &content_type,
                &metadata,
                &tagging,
                reporter,
            )
            .await?
        } else {
            // Small file: bounded read (<= part_size) and a single PutObject.
            let content = read_chunk(reader, file_size as usize)
                .await
                .map_err(|e| BackendError::Internal(format!("read file data: {e}")))?;
            let len = content.len() as u64;
            let out = self
                .client
                .put_object()
                .bucket(&self.bucket)
                .key(&key)
                .content_type(&content_type)
                .set_metadata(Some(metadata.clone()))
                .set_tagging(tagging.clone())
                .body(ByteStream::from(content))
                .send()
                .await
                .map_err(|e| errors::from_s3_put_object(&spec.name, e))?;
            reporter.advance(len);
            out.e_tag().unwrap_or_default().to_string()
        };
        reporter.finish_clear();

        Ok(FileInfo {
            name: spec.name,
            size: file_size,
            content_type,
            last_modified: Utc::now(),
            etag,
            groups: spec.groups,
            metadata,
            tags: spec.tags,
        })
    }

    /// Multipart upload: create, upload parts concurrently, complete.
    /// Aborts the multipart upload (best effort) on any failure so no
    /// orphaned parts accrue storage charges.
    #[allow(clippy::too_many_arguments)]
    async fn multipart_upload<R: AsyncRead + Unpin>(
        &self,
        name: &str,
        key: &str,
        reader: &mut R,
        part_size: u64,
        content_type: &str,
        metadata: &HashMap<String, String>,
        tagging: &Option<String>,
        reporter: &dyn ProgressReporter,
    ) -> Result<String, BackendError> {
        let create = self
            .client
            .create_multipart_upload()
            .bucket(&self.bucket)
            .key(key)
            .content_type(content_type)
            .set_metadata(Some(metadata.clone()))
            .set_tagging(tagging.clone())
            .send()
            .await
            .map_err(|e| errors::from_s3_create_multipart(name, e))?;
        let upload_id = create
            .upload_id()
            .ok_or_else(|| {
                BackendError::Internal("S3 did not return a multipart upload id".into())
            })?
            .to_string();

        let parts = match self
            .upload_parts(name, key, &upload_id, reader, part_size, reporter)
            .await
        {
            Ok(parts) => parts,
            Err(e) => {
                // Best-effort cleanup; the original error is what matters.
                let _ = self
                    .client
                    .abort_multipart_upload()
                    .bucket(&self.bucket)
                    .key(key)
                    .upload_id(&upload_id)
                    .send()
                    .await;
                return Err(e);
            }
        };

        let out = match self
            .client
            .complete_multipart_upload()
            .bucket(&self.bucket)
            .key(key)
            .upload_id(&upload_id)
            .multipart_upload(
                CompletedMultipartUpload::builder()
                    .set_parts(Some(parts))
                    .build(),
            )
            .send()
            .await
        {
            Ok(out) => out,
            Err(e) => {
                // Abort (best effort) so orphaned parts don't accrue storage
                // charges; the completion error is what matters.
                let _ = self
                    .client
                    .abort_multipart_upload()
                    .bucket(&self.bucket)
                    .key(key)
                    .upload_id(&upload_id)
                    .send()
                    .await;
                return Err(errors::from_s3_complete_multipart(name, e));
            }
        };

        Ok(out.e_tag().unwrap_or_default().to_string())
    }

    /// Read chunks and upload parts with bounded concurrency.
    async fn upload_parts<R: AsyncRead + Unpin>(
        &self,
        name: &str,
        key: &str,
        upload_id: &str,
        reader: &mut R,
        part_size: u64,
        reporter: &dyn ProgressReporter,
    ) -> Result<Vec<CompletedPart>, BackendError> {
        use tokio::sync::Semaphore;

        let semaphore = Arc::new(Semaphore::new(self.max_concurrent_uploads));
        type PartHandle = tokio::task::JoinHandle<Result<CompletedPart, BackendError>>;
        let mut handles: Vec<(PartHandle, u64)> = Vec::new();
        let mut part_number: i32 = 0;

        loop {
            let chunk = read_chunk(reader, part_size as usize)
                .await
                .map_err(|e| BackendError::Internal(format!("read file data: {e}")))?;
            if chunk.is_empty() {
                break;
            }
            part_number += 1;
            if part_number as u64 > MAX_PARTS {
                return Err(BackendError::Internal(format!(
                    "multipart upload would exceed {MAX_PARTS} parts — file larger than declared size?"
                )));
            }

            let permit = semaphore
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| BackendError::Internal(format!("semaphore error: {e}")))?;
            let client = self.client.clone();
            let bucket = self.bucket.clone();
            let key = key.to_string();
            let upload_id = upload_id.to_string();
            let name = name.to_string();
            let len = chunk.len() as u64;

            handles.push((
                tokio::spawn(async move {
                    let _permit = permit; // held for the duration of the upload
                    let out = client
                        .upload_part()
                        .bucket(bucket)
                        .key(key)
                        .upload_id(upload_id)
                        .part_number(part_number)
                        .body(ByteStream::from(chunk))
                        .send()
                        .await
                        .map_err(|e| errors::from_s3_upload_part(&name, e))?;
                    Ok(CompletedPart::builder()
                        .part_number(part_number)
                        .set_e_tag(out.e_tag().map(str::to_string))
                        .build())
                }),
                len,
            ));
        }

        let mut parts = Vec::with_capacity(handles.len());
        for (handle, len) in handles {
            let part = handle
                .await
                .map_err(|e| BackendError::Internal(format!("upload task panicked: {e}")))??;
            parts.push(part);
            reporter.advance(len);
        }
        Ok(parts)
    }

    /// Stream a file's contents to `writer`, enforcing the 5 GiB download
    /// cap. At most one body frame is buffered at a time. Returns the
    /// object's size in bytes.
    pub async fn download_file_to_writer<W: AsyncWrite + Unpin>(
        &self,
        vault: &str,
        name: &str,
        writer: &mut W,
        reporter: &dyn ProgressReporter,
    ) -> Result<u64, BackendError> {
        let key = validated_key(vault, name)?;

        let head = self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
            .map_err(|e| errors::from_s3_head_object(name, e))?;
        let content_length = head.content_length().unwrap_or(0).max(0) as u64;
        validate_download_size(content_length, MAX_DOWNLOAD_SIZE_BYTES)?;
        reporter.set_total(content_length);

        let out = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
            .map_err(|e| errors::from_s3_get_object(name, e))?;

        let mut body = out.body;
        let mut written: u64 = 0;
        while let Some(bytes) = body
            .try_next()
            .await
            .map_err(|e| BackendError::Network(format!("aws GetObject body stream: {e}")))?
        {
            writer
                .write_all(&bytes)
                .await
                .map_err(|e| BackendError::Internal(format!("write downloaded data: {e}")))?;
            written += bytes.len() as u64;
            reporter.advance(bytes.len() as u64);
        }
        writer
            .flush()
            .await
            .map_err(|e| BackendError::Internal(format!("flush downloaded data: {e}")))?;
        reporter.finish_clear();

        // A truncated body stream (connection drop mid-transfer) must not be
        // reported as a successful full-size download.
        if written != content_length {
            return Err(BackendError::Network(format!(
                "download of '{name}' truncated: expected {content_length} bytes, wrote {written}"
            )));
        }

        Ok(written)
    }

    /// Hierarchical listing at one prefix level using S3's delimiter support.
    /// Common prefixes become [`BlobListItem::Directory`] entries.
    pub async fn list_files_hierarchical(
        &self,
        vault: &str,
        request: FileListRequest,
    ) -> Result<Vec<BlobListItem>, BackendError> {
        validate_vault_for_files(vault)?;

        let base = files_prefix(vault);
        let user_prefix = normalize_user_prefix(request.prefix.as_deref());
        let full_prefix = format!("{base}{user_prefix}");
        let delimiter = request.delimiter.clone().unwrap_or_else(|| "/".to_string());
        // Hierarchical listing treats the user prefix as a FOLDER: a
        // non-empty prefix must end with the delimiter, otherwise `docs`
        // would also match sibling trees like `docs-extra/...` (Azure's
        // normalize_prefix appends '/' the same way). The flat
        // `list_files` path keeps exact-prefix semantics intentionally.
        let full_prefix = ensure_folder_prefix(full_prefix, &user_prefix, &delimiter);
        // Directory display names must be stripped with the SAME normalized
        // folder prefix used for the API call — stripping the raw user
        // prefix (`docs`) would leave a leading delimiter (`/api/` instead
        // of `api/`).
        let strip_base = ensure_folder_prefix(user_prefix.clone(), &user_prefix, &delimiter);

        let mut items: Vec<BlobListItem> = Vec::new();
        let mut continuation: Option<String> = None;
        loop {
            let page = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&full_prefix)
                .delimiter(&delimiter)
                .set_continuation_token(continuation.take())
                .send()
                .await
                .map_err(errors::from_s3_list_objects)?;

            for cp in page.common_prefixes() {
                let Some(full_key_prefix) = cp.prefix() else {
                    continue;
                };
                let Some(full_path) = strip_file_key(vault, full_key_prefix) else {
                    continue;
                };
                let dir_name = full_path
                    .strip_prefix(&strip_base)
                    .unwrap_or(&full_path)
                    .to_string();
                items.push(BlobListItem::Directory {
                    name: dir_name,
                    full_path,
                });
            }

            for obj in page.contents() {
                if let Some(info) = self.object_to_file_info(vault, obj) {
                    items.push(BlobListItem::File(info));
                }
            }

            if page.is_truncated() == Some(true) {
                continuation = page.next_continuation_token().map(str::to_string);
                if continuation.is_none() {
                    break;
                }
            } else {
                break;
            }
        }

        // Group filtering requires per-object metadata (S3 listings carry
        // none) — resolve via HeadObject only when a filter was requested.
        if let Some(ref filter_groups) = request.groups {
            let mut filtered = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    BlobListItem::File(info) => {
                        let full = self.get_file_info(vault, &info.name).await?;
                        if filter_groups.iter().any(|fg| full.groups.contains(fg)) {
                            filtered.push(BlobListItem::File(full));
                        }
                    }
                    dir => filtered.push(dir),
                }
            }
            items = filtered;
        }

        sort_list_items(&mut items);
        if let Some(limit) = request.limit {
            items.truncate(limit);
        }
        Ok(items)
    }

    /// Convert one S3 listing entry to a [`FileInfo`].
    ///
    /// S3 listings carry no user metadata or content type, so `content_type`
    /// is guessed from the name (display only) and `groups`/`metadata` stay
    /// empty unless resolved separately via `get_file_info`.
    fn object_to_file_info(
        &self,
        vault: &str,
        obj: &aws_sdk_s3::types::Object,
    ) -> Option<FileInfo> {
        let key = obj.key()?;
        let name = strip_file_key(vault, key)?;
        Some(FileInfo {
            content_type: mime_guess::from_path(&name)
                .first_or_octet_stream()
                .to_string(),
            size: obj.size().unwrap_or(0).max(0) as u64,
            last_modified: to_chrono(obj.last_modified()),
            etag: obj.e_tag().unwrap_or_default().to_string(),
            groups: Vec::new(),
            metadata: HashMap::new(),
            tags: HashMap::new(),
            name,
        })
    }
}

/// Normalize the user-supplied list prefix: trim leading slashes; no other
/// transformation (an exact-name prefix match is intended, like Azure).
fn normalize_user_prefix(prefix: Option<&str>) -> String {
    prefix
        .map(|p| p.trim_start_matches('/').to_string())
        .unwrap_or_default()
}

/// For hierarchical (delimiter-based) listing, a non-empty user prefix is a
/// folder and must end with the delimiter so `docs` cannot also match
/// sibling trees like `docs-extra/...` (mirrors Azure's normalize_prefix).
fn ensure_folder_prefix(full_prefix: String, user_prefix: &str, delimiter: &str) -> String {
    if !user_prefix.is_empty() && !full_prefix.ends_with(delimiter) {
        format!("{full_prefix}{delimiter}")
    } else {
        full_prefix
    }
}

#[async_trait]
impl FileBackend for AwsFileBackend {
    fn validate_file_name(&self, name: &str) -> Result<(), BackendError> {
        validate_file_name(name)
    }

    async fn upload_file(
        &self,
        vault: &str,
        request: FileUploadRequest,
        reporter: Option<&dyn ProgressReporter>,
    ) -> Result<FileInfo, BackendError> {
        let null = NoopReporter;
        let reporter = reporter.unwrap_or(&null);
        let spec = FileUploadSpec::from(&request);
        let file_size = request.content.len() as u64;
        let mut reader = std::io::Cursor::new(request.content);
        self.upload_file_streaming(vault, spec, &mut reader, file_size, reporter)
            .await
    }

    async fn download_file(
        &self,
        vault: &str,
        name: &str,
        reporter: Option<&dyn ProgressReporter>,
    ) -> Result<Vec<u8>, BackendError> {
        let null = NoopReporter;
        let reporter = reporter.unwrap_or(&null);
        let mut buf: Vec<u8> = Vec::new();
        self.download_file_to_writer(vault, name, &mut buf, reporter)
            .await?;
        Ok(buf)
    }

    async fn list_files(
        &self,
        vault: &str,
        request: FileListRequest,
    ) -> Result<Vec<FileInfo>, BackendError> {
        validate_vault_for_files(vault)?;

        let base = files_prefix(vault);
        let user_prefix = normalize_user_prefix(request.prefix.as_deref());
        let full_prefix = format!("{base}{user_prefix}");

        let mut results: Vec<FileInfo> = Vec::new();
        let mut continuation: Option<String> = None;
        loop {
            let page = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&full_prefix)
                .set_continuation_token(continuation.take())
                .send()
                .await
                .map_err(errors::from_s3_list_objects)?;

            for obj in page.contents() {
                if let Some(info) = self.object_to_file_info(vault, obj) {
                    results.push(info);
                }
            }

            if page.is_truncated() == Some(true) {
                continuation = page.next_continuation_token().map(str::to_string);
                if continuation.is_none() {
                    break;
                }
            } else {
                break;
            }
        }

        // Group filtering needs per-object metadata; see hierarchical note.
        if let Some(ref filter_groups) = request.groups {
            let mut filtered = Vec::with_capacity(results.len());
            for info in results {
                let full = self.get_file_info(vault, &info.name).await?;
                if filter_groups.iter().any(|fg| full.groups.contains(fg)) {
                    filtered.push(full);
                }
            }
            results = filtered;
        }

        results.sort_by(|a, b| a.name.cmp(&b.name));
        if let Some(limit) = request.limit {
            results.truncate(limit);
        }
        Ok(results)
    }

    async fn delete_file(&self, vault: &str, name: &str) -> Result<(), BackendError> {
        let key = validated_key(vault, name)?;

        // S3 DeleteObject succeeds silently for missing keys — head first so
        // deleting a nonexistent file reports NotFound like other backends.
        self.client
            .head_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
            .map_err(|e| errors::from_s3_head_object(name, e))?;

        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
            .map_err(|e| errors::from_s3_delete_object(name, e))?;
        Ok(())
    }

    async fn get_file_info(&self, vault: &str, name: &str) -> Result<FileInfo, BackendError> {
        let key = validated_key(vault, name)?;

        let head = self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
            .map_err(|e| errors::from_s3_head_object(name, e))?;

        let metadata: HashMap<String, String> = head.metadata().cloned().unwrap_or_default();
        let groups: Vec<String> = metadata
            .get(METADATA_KEY_GROUPS)
            .map(|g| g.split(',').map(|s| s.trim().to_string()).collect())
            .unwrap_or_default();

        // Tags need a separate call; degrade gracefully when the credentials
        // lack s3:GetObjectTagging (mirrors the Azure 403 fallback).
        let tags: HashMap<String, String> = match self
            .client
            .get_object_tagging()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
        {
            Ok(out) => out
                .tag_set()
                .iter()
                .map(|t| (t.key().to_string(), t.value().to_string()))
                .collect(),
            Err(e) => {
                match errors::from_s3_get_object_tagging(name, e) {
                    BackendError::PermissionDenied(_) => tracing::debug!(
                        "Tag read for '{name}' was denied; tags will be empty. \
                         Grant s3:GetObjectTagging to include them."
                    ),
                    other => tracing::warn!("Failed to fetch tags for '{name}': {other}"),
                }
                HashMap::new()
            }
        };

        Ok(FileInfo {
            name: name.to_string(),
            size: head.content_length().unwrap_or(0).max(0) as u64,
            content_type: head
                .content_type()
                .unwrap_or("application/octet-stream")
                .to_string(),
            last_modified: to_chrono(head.last_modified()),
            etag: head.e_tag().unwrap_or_default().to_string(),
            groups,
            metadata,
            tags,
        })
    }

    async fn list_files_hierarchical(
        &self,
        vault: &str,
        request: FileListRequest,
    ) -> Result<Vec<BlobListItem>, BackendError> {
        // Override the flat-derived default with S3's native delimited listing
        // (`CommonPrefixes`) — see the inherent method for the folder-prefix
        // and group-filter handling.
        AwsFileBackend::list_files_hierarchical(self, vault, request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── key construction & vault isolation ──────────────────────────────────

    #[test]
    fn object_key_is_vault_prefixed() {
        assert_eq!(
            validated_key("dev", "docs/readme.md").unwrap(),
            "dev/files/docs/readme.md"
        );
    }

    #[test]
    fn files_prefix_ends_with_slash() {
        assert_eq!(files_prefix("prod"), "prod/files/");
    }

    #[test]
    fn vaults_with_same_file_name_get_distinct_keys() {
        let dev = validated_key("dev", "config.txt").unwrap();
        let prod = validated_key("prod", "config.txt").unwrap();
        assert_ne!(dev, prod);
        assert!(dev.starts_with("dev/files/"));
        assert!(prod.starts_with("prod/files/"));
    }

    #[test]
    fn strip_file_key_only_matches_own_vault() {
        assert_eq!(
            strip_file_key("dev", "dev/files/config.txt"),
            Some("config.txt".to_string())
        );
        assert_eq!(strip_file_key("dev", "prod/files/config.txt"), None);
        assert_eq!(strip_file_key("dev", "dev/secrets/config.txt"), None);
        // The bare prefix itself is not a file.
        assert_eq!(strip_file_key("dev", "dev/files/"), None);
    }

    #[test]
    fn validated_key_rejects_overlong_keys() {
        // "vault/files/" (12 bytes) + name must stay <= 1024 bytes.
        let max_name = 1024 - files_prefix("vault").len();
        assert!(validated_key("vault", &"a".repeat(max_name)).is_ok());
        let err = validated_key("vault", &"a".repeat(max_name + 1)).unwrap_err();
        assert!(matches!(err, BackendError::InvalidArgument(_)));
    }

    // ── file name validation (containment) ──────────────────────────────────

    #[test]
    fn validate_file_name_accepts_normal_names() {
        for name in ["a.txt", "docs/readme.md", "configs/app.yaml", "naïve.txt"] {
            assert!(validate_file_name(name).is_ok(), "should accept: {name}");
        }
    }

    #[test]
    fn validate_file_name_rejects_traversal_and_absolute() {
        for name in [
            "",
            "  ",
            "/etc/passwd",
            "../escape.txt",
            "a/../escape.txt",
            "a/./b.txt",
            "..",
            "a//b.txt",
            "trailing/",
            "back\\slash.txt",
            "ctrl\u{7}.txt",
        ] {
            assert!(
                matches!(
                    validate_file_name(name),
                    Err(BackendError::InvalidArgument(_))
                ),
                "should reject: {name:?}"
            );
        }
    }

    #[test]
    fn validate_vault_rejects_separators_and_traversal() {
        for vault in ["", "..", ".", "a/b", "a\\b", "v\u{0}lt"] {
            assert!(
                matches!(
                    validate_vault_for_files(vault),
                    Err(BackendError::InvalidArgument(_))
                ),
                "should reject vault: {vault:?}"
            );
        }
        assert!(validate_vault_for_files("myproj-kv").is_ok());
    }

    // ── multipart chunk math ─────────────────────────────────────────────────

    const MIB: u64 = 1024 * 1024;

    #[test]
    fn part_size_clamps_small_chunks_to_s3_minimum() {
        // Default config chunk is 4 MB — below S3's 5 MiB multipart minimum.
        assert_eq!(multipart_part_size(100 * MIB, 4 * MIB), 5 * MIB);
        assert_eq!(multipart_part_size(100 * MIB, 0), 5 * MIB);
    }

    #[test]
    fn part_size_honours_configured_chunk_when_valid() {
        assert_eq!(multipart_part_size(100 * MIB, 16 * MIB), 16 * MIB);
    }

    #[test]
    fn part_size_grows_to_respect_max_part_count() {
        // 100,000 MiB at 5 MiB/part would need 20,000 parts (> 10,000 cap).
        let size = 100_000 * MIB;
        let part = multipart_part_size(size, 5 * MIB);
        assert!(part_count(size, part) <= MAX_PARTS, "part size {part}");
        assert!(part >= size.div_ceil(MAX_PARTS));
    }

    #[test]
    fn part_size_never_exceeds_s3_maximum() {
        assert_eq!(multipart_part_size(u64::MAX, u64::MAX), MAX_PART_SIZE_BYTES);
    }

    #[test]
    fn part_count_math() {
        assert_eq!(part_count(250, 100), 3);
        assert_eq!(part_count(200, 100), 2);
        assert_eq!(part_count(0, 100), 0);
        assert_eq!(part_count(1, 100), 1);
    }

    // ── download size guard ──────────────────────────────────────────────────

    #[test]
    fn download_size_guard_allows_under_and_exact() {
        assert!(validate_download_size(100, 1024).is_ok());
        assert!(validate_download_size(1024, 1024).is_ok());
        assert!(validate_download_size(0, 1024).is_ok());
        assert!(validate_download_size(MAX_DOWNLOAD_SIZE_BYTES, MAX_DOWNLOAD_SIZE_BYTES).is_ok());
    }

    #[test]
    fn download_size_guard_rejects_over_limit() {
        let err = validate_download_size(MAX_DOWNLOAD_SIZE_BYTES + 1, MAX_DOWNLOAD_SIZE_BYTES)
            .unwrap_err();
        assert!(
            err.to_string().contains("maximum allowed download size"),
            "got: {err}"
        );
    }

    // ── bucket resolution ────────────────────────────────────────────────────

    #[test]
    fn bucket_resolution_prefers_config_then_env() {
        assert_eq!(
            resolve_bucket_from(Some("cfg-bucket"), Some("env-bucket")).unwrap(),
            "cfg-bucket"
        );
        assert_eq!(
            resolve_bucket_from(None, Some("env-bucket")).unwrap(),
            "env-bucket"
        );
        assert_eq!(
            resolve_bucket_from(Some("  "), Some("env-bucket")).unwrap(),
            "env-bucket"
        );
    }

    #[test]
    fn bucket_resolution_errors_with_setup_hint_when_unset() {
        let err = resolve_bucket_from(None, None).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("s3_bucket"), "hint missing: {msg}");
        assert!(msg.contains("XV_AWS_S3_BUCKET"), "hint missing: {msg}");
        assert!(msg.contains("does not create"), "hint missing: {msg}");
    }

    // ── tagging ──────────────────────────────────────────────────────────────

    #[test]
    fn tagging_empty_is_none() {
        assert_eq!(encode_tagging(&HashMap::new()).unwrap(), None);
    }

    #[test]
    fn tagging_encodes_url_pairs() {
        let tags = HashMap::from([("env".to_string(), "prod east".to_string())]);
        assert_eq!(
            encode_tagging(&tags).unwrap().unwrap(),
            "env=prod+east".to_string()
        );
    }

    #[test]
    fn tagging_rejects_more_than_ten_tags() {
        let tags: HashMap<String, String> = (0..11)
            .map(|i| (format!("k{i}"), "v".to_string()))
            .collect();
        assert!(matches!(
            encode_tagging(&tags),
            Err(BackendError::InvalidArgument(_))
        ));
    }

    // ── misc helpers ─────────────────────────────────────────────────────────

    #[test]
    fn user_prefix_normalization_trims_leading_slashes() {
        assert_eq!(normalize_user_prefix(Some("/docs")), "docs");
        assert_eq!(normalize_user_prefix(Some("docs/")), "docs/");
        assert_eq!(normalize_user_prefix(None), "");
    }

    #[test]
    fn folder_prefix_gets_trailing_delimiter_for_hierarchical_list() {
        // `docs` must not match `docs-extra/...` in hierarchical listing
        assert_eq!(
            ensure_folder_prefix("v/files/docs".to_string(), "docs", "/"),
            "v/files/docs/"
        );
        // Already-terminated prefix unchanged
        assert_eq!(
            ensure_folder_prefix("v/files/docs/".to_string(), "docs/", "/"),
            "v/files/docs/"
        );
        // Empty user prefix (vault root): exact base prefix, no extra delimiter
        assert_eq!(
            ensure_folder_prefix("v/files/".to_string(), "", "/"),
            "v/files/"
        );
    }

    #[tokio::test]
    async fn read_chunk_splits_correctly() {
        let data = (0u8..=249).collect::<Vec<_>>();
        let mut cursor = std::io::Cursor::new(data.clone());
        let mut chunks: Vec<Vec<u8>> = Vec::new();
        loop {
            let chunk = read_chunk(&mut cursor, 100).await.unwrap();
            if chunk.is_empty() {
                break;
            }
            chunks.push(chunk);
        }
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0], &data[..100]);
        assert_eq!(chunks[1], &data[100..200]);
        assert_eq!(chunks[2], &data[200..]);
    }
}
