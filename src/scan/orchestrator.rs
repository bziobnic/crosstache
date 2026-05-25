//! High-level scanner orchestrator. Fetches secret values from one or
//! more vaults, builds the match engine, walks paths, and returns the
//! aggregated finding list.
//!
//! Live integration tests (Azure-dependent) live in
//! `tests/scan_tests.rs`; this module only carries pure-orchestration
//! tests that don't need a real backend.

use crate::error::Result;
use crate::scan::engine::{MatchEngine, SecretRef};
use crate::scan::finding::Finding;
use std::path::PathBuf;

/// Scan an already-walked list of paths against an already-built engine.
/// Pure I/O at the file level; no Azure calls.
pub fn scan_paths(paths: &[PathBuf], engine: &MatchEngine) -> Result<Vec<Finding>> {
    let mut findings: Vec<Finding> = Vec::new();
    for path in paths {
        let Ok(content) = std::fs::read_to_string(path) else {
            tracing::debug!("skipping unreadable file: {}", path.display());
            continue;
        };
        findings.extend(engine.scan_text(path, &content));
    }
    Ok(findings)
}

/// Fetch values for every secret in `vault_names` via a bounded
/// semaphore. Failures for individual vaults / secrets degrade
/// silently with a debug log.
pub async fn fetch_secret_values(
    secret_manager: &crate::secret::manager::SecretManager,
    vault_names: &[String],
    concurrency: usize,
) -> Result<Vec<SecretRef>> {
    use tokio::sync::Semaphore;
    let sem = std::sync::Arc::new(Semaphore::new(concurrency.max(1)));
    let mut handles = Vec::new();
    for vault in vault_names {
        // List secrets for this vault.
        let summaries = match secret_manager.secret_ops().list_secrets(vault, None).await {
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
            let ops = secret_manager.secret_ops().clone();
            let handle = tokio::spawn(async move {
                let _permit = sem.acquire_owned().await.ok()?;
                match ops.get_secret(&vault, &backend_name, true).await {
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
        let engine = MatchEngine::new(&secrets, &patterns);
        let paths = walk(&[temp.path()], &WalkConfig::default()).unwrap();
        let findings = scan_paths(&paths, &engine).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].secret_name.as_deref(), Some("DB_PW"));
    }
}
