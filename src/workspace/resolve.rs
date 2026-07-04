//! Workspace-aware secret target resolution, shared by every CLI verb that
//! accepts a secret name (`get`, `set`, and — per the Phase B/C plan — the
//! remaining read/write verbs).
//!
//! This module is only consulted when a [`Workspace`] exists; the
//! no-workspace path in `src/cli/secret_ops.rs` is untouched and stays
//! byte-identical to pre-workspace behavior.

use std::sync::Arc;

use crate::backend::{Backend, BackendRegistry};
use crate::error::{CrosstacheError, Result};

use super::{parse_address, Workspace, WorkspaceEntry};

/// Whether a resolution is for a read (searches attached vaults on an
/// unqualified name) or a write (targets the default vault only — writes
/// never search).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetMode {
    Read,
    Write,
}

/// A resolved workspace target: the live backend handle to operate on,
/// plus the entry it came from.
#[derive(Clone)]
pub struct TargetVault {
    pub backend: Arc<dyn Backend>,
    pub entry: WorkspaceEntry,
}

impl std::fmt::Debug for TargetVault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TargetVault")
            .field("entry", &self.entry)
            .finish()
    }
}

/// Resolve a raw CLI secret argument (e.g. `"work:app/db/pass"` or
/// `"DB_PASSWORD"`) against an active workspace.
///
/// - **Read**: an unqualified name is searched across every attached vault
///   (bounded-concurrent existence probes). Exactly one match resolves to
///   that vault; zero is a not-found error; two or more is
///   [`CrosstacheError::AmbiguousSecret`] (exit 13), listing every match's
///   qualified `alias:name` form. A qualified `alias:path` goes straight to
///   that vault — no search.
/// - **Write**: an unqualified name ALWAYS targets the default entry —
///   never searched. A qualified `alias:path` goes to that vault.
///
/// **Exact-name-first** (Read only): before alias interpretation, the FULL
/// raw string is probed as a literal secret name across attached vaults.
/// A hit short-circuits alias parsing entirely — mirrors `inject`'s
/// dot-split rule, protecting literal names that happen to contain `:`
/// (realistic only on the local backend's unrestricted charset).
pub async fn resolve_secret_target(
    raw: &str,
    ws: &Workspace,
    registry: &BackendRegistry,
    mode: TargetMode,
) -> Result<(TargetVault, String)> {
    let addr = parse_address(raw);

    match mode {
        TargetMode::Write => {
            let entry = match &addr.alias {
                Some(alias) => ws.entry(alias).cloned().ok_or_else(|| unknown_alias_error(ws, alias))?,
                None => ws.default_entry().clone(),
            };
            let backend = materialize(registry, &entry)?;
            let path = addr.alias.map(|_| addr.path.clone()).unwrap_or(addr.path);
            Ok((TargetVault { backend, entry }, path))
        }
        TargetMode::Read => {
            // Exact-name-first: only meaningful when the raw string parsed
            // as `alias:path` at all (a bare name IS the search target
            // already, so there's nothing to "win over").
            if addr.alias.is_some() {
                if let Some(entry) = exact_name_match(raw, ws, registry).await? {
                    let backend = materialize(registry, &entry)?;
                    return Ok((TargetVault { backend, entry }, raw.to_string()));
                }
            }

            if let Some(alias) = &addr.alias {
                let entry = ws
                    .entry(alias)
                    .cloned()
                    .ok_or_else(|| unknown_alias_error(ws, alias))?;
                let backend = materialize(registry, &entry)?;
                return Ok((TargetVault { backend, entry }, addr.path));
            }

            // Unqualified read: search every attached vault.
            let matches = search_all(&addr.path, ws, registry).await?;
            match matches.len() {
                0 => Err(CrosstacheError::secret_not_found(addr.path.clone())),
                1 => {
                    let entry = matches.into_iter().next().unwrap();
                    let backend = materialize(registry, &entry)?;
                    Ok((TargetVault { backend, entry }, addr.path))
                }
                _ => {
                    let candidates: Vec<String> = matches.iter().map(|e| e.alias.clone()).collect();
                    Err(CrosstacheError::ambiguous_secret(addr.path, candidates))
                }
            }
        }
    }
}

fn materialize(registry: &BackendRegistry, entry: &WorkspaceEntry) -> Result<Arc<dyn Backend>> {
    registry
        .materialize(&entry.backend)
        .map_err(|e| CrosstacheError::config(format!(
            "workspace vault '{}' (backend '{}') is unavailable: {e}",
            entry.alias, entry.backend
        )))
}

fn unknown_alias_error(ws: &Workspace, alias: &str) -> CrosstacheError {
    let attached: Vec<&str> = ws.entries.iter().map(|e| e.alias.as_str()).collect();
    CrosstacheError::invalid_argument(format!(
        "unknown workspace alias '{alias}'; attached aliases: {}",
        attached.join(", ")
    ))
}

/// Probe whether the FULL raw string (including any `:`) is a literal
/// secret name anywhere in the workspace. Fail-loud on any probe error —
/// a partial union could hide a real match. Returns the single matching
/// entry, or `None` if it exists in zero vaults (fall through to alias
/// interpretation), or an ambiguity error if it exists in ≥2.
async fn exact_name_match(
    raw: &str,
    ws: &Workspace,
    registry: &BackendRegistry,
) -> Result<Option<WorkspaceEntry>> {
    let matches = search_all(raw, ws, registry).await?;
    match matches.len() {
        0 => Ok(None),
        1 => Ok(Some(matches.into_iter().next().unwrap())),
        _ => {
            let candidates: Vec<String> = matches.iter().map(|e| e.alias.clone()).collect();
            Err(CrosstacheError::ambiguous_secret(raw.to_string(), candidates))
        }
    }
}

/// Search every attached vault for a secret literally named `name`.
/// Fail-loud: any per-vault error aborts the whole search rather than
/// silently treating it as "not found there" (spec §Read semantics:
/// partial failure fails loud).
async fn search_all(
    name: &str,
    ws: &Workspace,
    registry: &BackendRegistry,
) -> Result<Vec<WorkspaceEntry>> {
    let mut found = Vec::new();
    for entry in &ws.entries {
        let backend = materialize(registry, entry)?;
        let exists = backend
            .secrets()
            .secret_exists(&entry.vault, name)
            .await
            .map_err(|e| {
                CrosstacheError::config(format!(
                    "workspace vault '{}' (backend '{}') failed while searching for '{name}': {e}",
                    entry.alias, entry.backend
                ))
            })?;
        if exists {
            found.push(entry.clone());
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::settings::{Config, LocalConfig, NamedBackendEntry};
    use crate::secret::manager::SecretRequest;
    use crate::workspace::WorkspaceSource;
    use std::collections::HashMap;
    use zeroize::Zeroizing;

    /// Build a `BackendRegistry` with two hermetic local stores
    /// ("local-a"/"local-b") registered for lazy construction, plus a
    /// two-entry `Workspace` (`work` -> local-a, `stage` -> local-b,
    /// `work` default).
    fn two_vault_workspace(tmp: &tempfile::TempDir) -> (BackendRegistry, Workspace) {
        let mut named_backends = HashMap::new();
        named_backends.insert(
            "local-a".to_string(),
            NamedBackendEntry::Local(LocalConfig {
                store_path: Some(tmp.path().join("store-a").to_string_lossy().to_string()),
                key_file: Some(tmp.path().join("key-a.txt").to_string_lossy().to_string()),
                default_vault: Some("default".into()),
                encrypt_metadata: None,
                opaque_filenames: None,
            }),
        );
        named_backends.insert(
            "local-b".to_string(),
            NamedBackendEntry::Local(LocalConfig {
                store_path: Some(tmp.path().join("store-b").to_string_lossy().to_string()),
                key_file: Some(tmp.path().join("key-b.txt").to_string_lossy().to_string()),
                default_vault: Some("default".into()),
                encrypt_metadata: None,
                opaque_filenames: None,
            }),
        );
        let config = Config {
            named_backends,
            ..Default::default()
        };
        let registry =
            BackendRegistry::with_lazy(&config, &["local-a".to_string(), "local-b".to_string()])
                .expect("must register");

        let ws = Workspace {
            entries: vec![
                WorkspaceEntry {
                    alias: "work".to_string(),
                    backend: "local-a".to_string(),
                    vault: "default".to_string(),
                    default: true,
                },
                WorkspaceEntry {
                    alias: "stage".to_string(),
                    backend: "local-b".to_string(),
                    vault: "default".to_string(),
                    default: false,
                },
            ],
            default_alias: "work".to_string(),
            source: WorkspaceSource::Context,
        };
        (registry, ws)
    }

    fn req(name: &str, value: &str) -> SecretRequest {
        SecretRequest {
            name: name.to_string(),
            value: Zeroizing::new(value.to_string()),
            content_type: None,
            enabled: None,
            expires_on: None,
            not_before: None,
            tags: None,
            groups: None,
            note: None,
            folder: None,
        }
    }

    #[tokio::test]
    async fn get_unqualified_unique_match_resolves_to_that_vault() {
        let tmp = tempfile::tempdir().unwrap();
        let (registry, ws) = two_vault_workspace(&tmp);

        let stage_backend = registry.materialize("local-b").unwrap();
        stage_backend
            .secrets()
            .set_secret("default", req("ONLY_IN_STAGE", "v1"))
            .await
            .unwrap();

        let (target, path) = resolve_secret_target("ONLY_IN_STAGE", &ws, &registry, TargetMode::Read)
            .await
            .expect("must resolve");
        assert_eq!(target.entry.alias, "stage");
        assert_eq!(path, "ONLY_IN_STAGE");
    }

    #[tokio::test]
    async fn get_ambiguous_errors_with_qualified_forms() {
        let tmp = tempfile::tempdir().unwrap();
        let (registry, ws) = two_vault_workspace(&tmp);

        for backend_name in ["local-a", "local-b"] {
            let backend = registry.materialize(backend_name).unwrap();
            backend
                .secrets()
                .set_secret("default", req("DB_PASSWORD", "v1"))
                .await
                .unwrap();
        }

        let err = resolve_secret_target("DB_PASSWORD", &ws, &registry, TargetMode::Read)
            .await
            .expect_err("must be ambiguous");
        assert_eq!(err.code(), "xv-ambiguous-secret");
        assert_eq!(err.exit_code(), 13);
        let msg = err.to_string();
        assert!(msg.contains("work:DB_PASSWORD"), "{msg}");
        assert!(msg.contains("stage:DB_PASSWORD"), "{msg}");
    }

    #[tokio::test]
    async fn get_qualified_reads_named_vault_without_search() {
        let tmp = tempfile::tempdir().unwrap();
        let (registry, ws) = two_vault_workspace(&tmp);

        let stage_backend = registry.materialize("local-b").unwrap();
        stage_backend
            .secrets()
            .set_secret("default", req("API_KEY", "stage-value"))
            .await
            .unwrap();

        let (target, path) = resolve_secret_target("stage:API_KEY", &ws, &registry, TargetMode::Read)
            .await
            .expect("qualified read must resolve directly, no search needed");
        assert_eq!(target.entry.alias, "stage");
        assert_eq!(path, "API_KEY");
    }

    #[tokio::test]
    async fn get_unknown_alias_errors_listing_attached() {
        let tmp = tempfile::tempdir().unwrap();
        let (registry, ws) = two_vault_workspace(&tmp);

        let err = resolve_secret_target("nope:API_KEY", &ws, &registry, TargetMode::Read)
            .await
            .expect_err("unknown alias must error");
        let msg = err.to_string();
        assert!(msg.contains("nope"), "{msg}");
        assert!(msg.contains("work"), "{msg}");
        assert!(msg.contains("stage"), "{msg}");
    }

    #[tokio::test]
    async fn set_unqualified_writes_default_only_never_searches() {
        let tmp = tempfile::tempdir().unwrap();
        let (registry, ws) = two_vault_workspace(&tmp);

        // Secret exists ONLY in the non-default vault ("stage"). An
        // unqualified write must go to the default ("work") regardless —
        // writes never search.
        let stage_backend = registry.materialize("local-b").unwrap();
        stage_backend
            .secrets()
            .set_secret("default", req("SHARED_NAME", "stage-original"))
            .await
            .unwrap();

        let (target, path) = resolve_secret_target("SHARED_NAME", &ws, &registry, TargetMode::Write)
            .await
            .expect("write must resolve to default");
        assert_eq!(target.entry.alias, "work");
        assert_eq!(path, "SHARED_NAME");

        target
            .backend
            .secrets()
            .set_secret(&target.entry.vault, req("SHARED_NAME", "work-value"))
            .await
            .unwrap();

        let work_backend = registry.materialize("local-a").unwrap();
        let written = work_backend
            .secrets()
            .get_secret("default", "SHARED_NAME", true)
            .await
            .expect("must be written to work");
        assert_eq!(written.value.as_deref().map(|s| s.as_str()), Some("work-value"));

        // The stage copy must be untouched.
        let stage_copy = stage_backend
            .secrets()
            .get_secret("default", "SHARED_NAME", true)
            .await
            .expect("stage copy untouched");
        assert_eq!(stage_copy.value.as_deref().map(|s| s.as_str()), Some("stage-original"));
    }

    #[tokio::test]
    async fn set_qualified_writes_named_vault() {
        let tmp = tempfile::tempdir().unwrap();
        let (registry, ws) = two_vault_workspace(&tmp);

        let (target, path) =
            resolve_secret_target("stage:NEW_SECRET", &ws, &registry, TargetMode::Write)
                .await
                .expect("qualified write must resolve");
        assert_eq!(target.entry.alias, "stage");
        assert_eq!(path, "NEW_SECRET");
    }

    #[tokio::test]
    async fn exact_name_with_colon_wins_over_alias_interpretation() {
        let tmp = tempfile::tempdir().unwrap();
        let (registry, ws) = two_vault_workspace(&tmp);

        // A secret literally named "work:x" in the "work" vault (the local
        // backend's unrestricted charset allows this) must win over
        // treating "work" as an alias qualifier.
        let work_backend = registry.materialize("local-a").unwrap();
        work_backend
            .secrets()
            .set_secret("default", req("work:x", "literal-value"))
            .await
            .unwrap();

        let (target, path) = resolve_secret_target("work:x", &ws, &registry, TargetMode::Read)
            .await
            .expect("must resolve via exact-name-first");
        assert_eq!(target.entry.alias, "work");
        assert_eq!(path, "work:x");
    }

    #[tokio::test]
    async fn get_zero_matches_is_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let (registry, ws) = two_vault_workspace(&tmp);

        let err = resolve_secret_target("NOPE", &ws, &registry, TargetMode::Read)
            .await
            .expect_err("must be not-found");
        assert_eq!(err.code(), "xv-secret-not-found");
    }
}
