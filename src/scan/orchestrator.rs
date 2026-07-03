//! High-level scanner orchestrator. Fetches secret values from one or
//! more vaults, builds the match engine, walks paths, and returns the
//! aggregated finding list.
//!
//! Live integration tests (Azure-dependent) live in
//! `tests/scan_tests.rs`; this module only carries pure-orchestration
//! tests that don't need a real backend.

use crate::error::{CrosstacheError, Result};
use crate::scan::engine::{MatchEngine, SecretRef};
use crate::scan::finding::Finding;
use std::path::PathBuf;

/// Maximum size (bytes) of a file the scanner will read into memory.
///
/// Files larger than this are skipped rather than read whole into RAM,
/// bounding the scanner's memory use regardless of input. 10 MiB comfortably
/// covers source, config, and env files while refusing to slurp multi-GB blobs
/// (build artifacts, archives, media) that a naive `read_to_string` would.
pub const MAX_SCAN_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Outcome of scanning a set of paths: the findings plus any files that were
/// skipped (too large or unreadable) so the caller can decide whether to
/// fail loud (CI/hook mode) or merely warn (interactive mode).
#[derive(Debug, Default)]
pub struct ScanOutcome {
    pub findings: Vec<Finding>,
    /// Files skipped because they exceeded [`MAX_SCAN_FILE_SIZE`], with size.
    pub skipped_too_large: Vec<(PathBuf, u64)>,
    /// Files skipped because they could not be read (non-UTF8, perms, gone).
    pub skipped_unreadable: Vec<PathBuf>,
}

impl ScanOutcome {
    /// Total number of files that were not scanned for any reason.
    pub fn skipped_count(&self) -> usize {
        self.skipped_too_large.len() + self.skipped_unreadable.len()
    }
}

/// Scan an already-walked list of paths against an already-built engine.
///
/// Memory is bounded: each file's size is checked before reading, and files
/// larger than [`MAX_SCAN_FILE_SIZE`] are skipped (recorded in the returned
/// [`ScanOutcome`]) rather than read whole into RAM. Unreadable files are
/// likewise recorded instead of silently dropped, so a caller in CI/hook mode
/// can fail loud rather than let an unscanned file hide a leak.
pub fn scan_paths(paths: &[PathBuf], engine: &MatchEngine) -> Result<ScanOutcome> {
    let mut outcome = ScanOutcome::default();
    for path in paths {
        // Stat first so an oversized file is never read into memory.
        match std::fs::metadata(path) {
            Ok(meta) if meta.len() > MAX_SCAN_FILE_SIZE => {
                tracing::debug!(
                    "skipping oversized file ({} bytes > {} cap): {}",
                    meta.len(),
                    MAX_SCAN_FILE_SIZE,
                    path.display()
                );
                outcome.skipped_too_large.push((path.clone(), meta.len()));
                continue;
            }
            Ok(_) => {}
            Err(e) => {
                tracing::debug!("skipping unstattable file {}: {e}", path.display());
                outcome.skipped_unreadable.push(path.clone());
                continue;
            }
        }

        let Ok(content) = std::fs::read_to_string(path) else {
            tracing::debug!("skipping unreadable file: {}", path.display());
            outcome.skipped_unreadable.push(path.clone());
            continue;
        };
        outcome.findings.extend(engine.scan_text(path, &content));
    }
    Ok(outcome)
}

/// Build a fail-loud error describing files that were skipped during a scan.
/// Intended for CI/hook mode, where an unscanned file could conceal a leak.
pub fn skipped_files_error(outcome: &ScanOutcome) -> CrosstacheError {
    let mut parts: Vec<String> = Vec::new();
    for (p, sz) in &outcome.skipped_too_large {
        parts.push(format!("{} (too large: {sz} bytes)", p.display()));
    }
    for p in &outcome.skipped_unreadable {
        parts.push(format!("{} (unreadable)", p.display()));
    }
    CrosstacheError::InvalidArgument(format!(
        "scan could not read {} file(s); refusing to pass in hook/CI mode: {}",
        outcome.skipped_count(),
        parts.join(", ")
    ))
}

/// Fetch values for every secret in `vault_names` via a bounded
/// semaphore. Failures for individual vaults / secrets degrade
/// silently with a debug log.
pub async fn fetch_secret_values(
    backend: std::sync::Arc<dyn crate::backend::Backend>,
    vault_names: &[String],
    concurrency: usize,
) -> Result<Vec<SecretRef>> {
    use tokio::sync::Semaphore;
    let sem = std::sync::Arc::new(Semaphore::new(concurrency.max(1)));
    let mut handles = Vec::new();
    for vault in vault_names {
        // List secrets for this vault via the active backend trait.
        let summaries = match backend.secrets().list_secrets(vault, None).await {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("list_secrets failed for vault {vault}: {e}");
                continue;
            }
        };
        for s in summaries {
            let sem = sem.clone();
            let vault = vault.clone();
            let secret_name = if s.original_name.is_empty() {
                s.name.clone()
            } else {
                s.original_name.clone()
            };
            let backend_name = s.name.clone();
            let backend = backend.clone();
            let handle = tokio::spawn(async move {
                let _permit = sem.acquire_owned().await.ok()?;
                match backend
                    .secrets()
                    .get_secret(&vault, &backend_name, true)
                    .await
                {
                    Ok(props) => props.value.map(|v| SecretRef {
                        name: secret_name,
                        vault,
                        value: v,
                    }),
                    Err(e) => {
                        tracing::debug!("get_secret failed for {vault}/{backend_name}: {e}");
                        None
                    }
                }
            });
            handles.push(handle);
        }
    }
    let mut refs = Vec::new();
    for h in handles {
        if let Ok(Some(r)) = h.await {
            refs.push(r);
        }
    }
    Ok(refs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::patterns::builtin_patterns;
    use crate::scan::walker::{walk, WalkConfig};
    use tempfile::tempdir;
    use zeroize::Zeroizing;

    #[test]
    fn scan_files_with_inline_engine() {
        // Pure unit test: skip the SecretManager fetch and feed the
        // orchestrator a pre-built engine.
        let temp = tempdir().unwrap();
        std::fs::write(temp.path().join("a.txt"), "key=hunter2-very-long-password").unwrap();

        let secrets = vec![SecretRef {
            name: "DB_PW".to_string(),
            vault: "v".to_string(),
            value: Zeroizing::new("hunter2-very-long-password".to_string()),
        }];
        let patterns = builtin_patterns();
        let engine = MatchEngine::new(
            &secrets,
            &patterns,
            crate::scan::engine::DEFAULT_MIN_VALUE_LENGTH,
        );
        let paths = walk(&[temp.path()], &WalkConfig::default()).unwrap();
        let outcome = scan_paths(&paths, &engine).unwrap();
        assert_eq!(outcome.findings.len(), 1);
        assert_eq!(outcome.findings[0].secret_name.as_deref(), Some("DB_PW"));
        assert_eq!(outcome.skipped_count(), 0);
    }

    #[test]
    fn oversized_file_is_skipped_not_read() {
        // A file larger than the cap must be recorded as skipped, never read
        // into memory, and must not produce findings.
        let temp = tempdir().unwrap();
        let big = temp.path().join("big.bin");
        // Write just over the cap. Content is the secret repeated so that a
        // naive read-whole scan WOULD match — proving we skipped, not matched.
        let secret = "hunter2-very-long-password";
        let mut f = std::fs::File::create(&big).unwrap();
        {
            use std::io::Write;
            let chunk = secret.repeat(1024); // ~26 KiB
            let iters = (MAX_SCAN_FILE_SIZE as usize / chunk.len()) + 2;
            for _ in 0..iters {
                f.write_all(chunk.as_bytes()).unwrap();
            }
        }
        assert!(std::fs::metadata(&big).unwrap().len() > MAX_SCAN_FILE_SIZE);

        let secrets = vec![SecretRef {
            name: "DB_PW".to_string(),
            vault: "v".to_string(),
            value: Zeroizing::new(secret.to_string()),
        }];
        let patterns = builtin_patterns();
        let engine = MatchEngine::new(
            &secrets,
            &patterns,
            crate::scan::engine::DEFAULT_MIN_VALUE_LENGTH,
        );
        let outcome = scan_paths(std::slice::from_ref(&big), &engine).unwrap();
        assert!(
            outcome.findings.is_empty(),
            "oversized file must not be scanned"
        );
        assert_eq!(outcome.skipped_too_large.len(), 1);
        assert_eq!(outcome.skipped_count(), 1);
        let err = skipped_files_error(&outcome).to_string();
        assert!(
            err.contains("too large"),
            "fail-loud error should explain why: {err}"
        );
    }
}
