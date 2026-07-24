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
/// In a configured workspace, a syntactically valid `alias:path` is always
/// authoritative. Literal-name probing must never redirect an explicitly
/// qualified read or mutation to another vault. In a degenerate workspace
/// with no configured aliases, the raw name remains literal for backward
/// compatibility.
pub async fn resolve_secret_target(
    raw: &str,
    ws: &Workspace,
    registry: &BackendRegistry,
    mode: TargetMode,
) -> Result<(TargetVault, String)> {
    // Degenerate workspace-of-one: no user-configured aliases exist, so a name
    // is ALWAYS a literal secret name — colon-address parsing must not apply.
    // This keeps bare `xv get`/`set` byte-identical to pre-workspace
    // resolution (a `:`-containing name is stored/read verbatim, no
    // alias-qualifier split; guarded by
    // `tests/e2e_workspaces.rs::no_workspace_byte_identical`). Resolve the raw
    // name against the sole default entry directly — no `parse_address`, and
    // no Read-mode search (there is only one vault to look in), so the resolved
    // (backend, vault, path) and the call pattern match the old no-workspace
    // path exactly. `mode` is irrelevant here for the same reason.
    if !ws.is_configured() {
        let entry = ws.default_entry()?.clone();
        let backend = materialize(registry, &entry)?;
        return Ok((TargetVault { backend, entry }, raw.to_string()));
    }

    let addr = parse_address(raw);

    match mode {
        TargetMode::Write => {
            let default_entry = ws.default_entry()?.clone();

            // `addr.alias == None` covers two distinct cases that both
            // land here as "literal name in the default vault", by
            // construction of `parse_address`: a genuinely bare name
            // (`raw` has no `:` at all), and a `:`-containing name whose
            // prefix isn't charset-valid as an alias (e.g. `"not
            // valid!:rest"` — `parse_address` never splits it, so the
            // whole string is already `addr.path` here). A charset-valid
            // prefix that simply isn't an ATTACHED alias (`addr.alias ==
            // Some(x)` but `ws.entry(x)` misses) still errors below,
            // deliberately: a destructive write silently falling back to
            // treating a typo'd alias as part of a literal name would be
            // its own silent-wrong-target bug, worse than the one this
            // exact-name-first probe exists to fix.
            let entry = match &addr.alias {
                Some(alias) => ws
                    .entry(alias)
                    .cloned()
                    .ok_or_else(|| unknown_alias_error(ws, alias))?,
                None => default_entry,
            };
            let backend = materialize(registry, &entry)?;
            let path = addr.alias.map(|_| addr.path.clone()).unwrap_or(addr.path);
            Ok((TargetVault { backend, entry }, path))
        }
        TargetMode::Read => {
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
    registry.materialize(&entry.backend).map_err(|e| {
        CrosstacheError::config(format!(
            "workspace vault '{}' (backend '{}') is unavailable: {e}",
            entry.alias, entry.backend
        ))
    })
}

/// Materialize the workspace's effective default entry without probing any
/// other attached backend.
///
/// The process registry is normally enough for the active backend. A
/// workspace may select a different named backend, so this falls back to a
/// lazy one-name registry built from the same config. This preserves the
/// workspace layer's "construct only what is touched" rule.
#[cfg(any(feature = "ui", test))]
pub(crate) fn materialize_default_entry(
    config: &crate::config::Config,
    workspace: &Workspace,
    registry: &BackendRegistry,
) -> Result<TargetVault> {
    let entry = workspace.default_entry()?.clone();
    let backend = match registry.materialize(&entry.backend) {
        Ok(backend) => backend,
        Err(_) => {
            let scoped = BackendRegistry::with_lazy(config, std::slice::from_ref(&entry.backend))
                .map_err(|error| CrosstacheError::config(error.to_string()))?;
            materialize(&scoped, &entry)?
        }
    };
    Ok(TargetVault { backend, entry })
}

fn unknown_alias_error(ws: &Workspace, alias: &str) -> CrosstacheError {
    let attached: Vec<&str> = ws.entries.iter().map(|e| e.alias.as_str()).collect();
    CrosstacheError::invalid_argument(format!(
        "unknown workspace alias '{alias}'; attached aliases: {}",
        attached.join(", ")
    ))
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

        let (target, path) =
            resolve_secret_target("ONLY_IN_STAGE", &ws, &registry, TargetMode::Read)
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

        let (target, path) =
            resolve_secret_target("stage:API_KEY", &ws, &registry, TargetMode::Read)
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

        let (target, path) =
            resolve_secret_target("SHARED_NAME", &ws, &registry, TargetMode::Write)
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
        assert_eq!(
            written.value.as_deref().map(|s| s.as_str()),
            Some("work-value")
        );

        // The stage copy must be untouched.
        let stage_copy = stage_backend
            .secrets()
            .get_secret("default", "SHARED_NAME", true)
            .await
            .expect("stage copy untouched");
        assert_eq!(
            stage_copy.value.as_deref().map(|s| s.as_str()),
            Some("stage-original")
        );
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
    async fn qualified_write_alias_wins_over_literal_name_in_default() {
        let tmp = tempfile::tempdir().unwrap();
        let (registry, mut ws) = two_vault_workspace(&tmp);
        // Flip the default to "stage" so "work" (the alias-shaped prefix
        // in the raw string below) is NOT the vault the probe should hit.
        for e in ws.entries.iter_mut() {
            e.default = e.alias == "stage";
        }
        ws.default_alias = "stage".to_string();

        let stage_backend = registry.materialize("local-b").unwrap();
        stage_backend
            .secrets()
            .set_secret("default", req("work:x", "literal-in-default"))
            .await
            .unwrap();

        let (target, path) = resolve_secret_target("work:x", &ws, &registry, TargetMode::Write)
            .await
            .expect("must resolve the explicit workspace alias");
        assert_eq!(
            target.entry.alias, "work",
            "an explicit alias must target its attached vault"
        );
        assert_eq!(path, "x");
    }

    /// When the alias-shaped prefix IS a real attached alias but no
    /// literal secret by that full name exists in the default vault, the
    /// probe misses and alias interpretation proceeds as before.
    #[tokio::test]
    async fn write_no_literal_match_falls_through_to_alias_interpretation() {
        let tmp = tempfile::tempdir().unwrap();
        let (registry, ws) = two_vault_workspace(&tmp);
        // Default is "work" here; nothing named "work:NEW_THING" exists
        // anywhere, so the exact-name-first probe (scoped to "work", the
        // default) must miss and fall through to targeting "work" via
        // alias interpretation with path "NEW_THING".
        let (target, path) =
            resolve_secret_target("work:NEW_THING", &ws, &registry, TargetMode::Write)
                .await
                .expect("must fall through to alias interpretation");
        assert_eq!(target.entry.alias, "work");
        assert_eq!(path, "NEW_THING");
    }

    /// A prefix that fails the alias charset check (`parse_address` never
    /// splits it) is already a literal name targeting the default vault
    /// with no probe needed — pinning this as the "current behavior" the
    /// Bugbot MEDIUM writeup asked to verify, not a new fallback for a
    /// charset-valid-but-unattached alias (that still errors, deliberately
    /// — see the comment at the `ok_or_else(unknown_alias_error)` call
    /// site in `resolve_secret_target`).
    #[tokio::test]
    async fn write_charset_invalid_prefix_is_literal_in_default() {
        let tmp = tempfile::tempdir().unwrap();
        let (registry, ws) = two_vault_workspace(&tmp);

        let (target, path) =
            resolve_secret_target("not valid!:x", &ws, &registry, TargetMode::Write)
                .await
                .expect(
                    "charset-invalid prefix must resolve as a literal name in the default vault",
                );
        assert_eq!(target.entry.alias, "work", "must target the default vault");
        assert_eq!(path, "not valid!:x");
    }

    #[tokio::test]
    async fn qualified_read_alias_wins_over_literal_name_probe() {
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
            .expect("must resolve via explicit alias");
        assert_eq!(target.entry.alias, "work");
        assert_eq!(path, "x");
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

    /// Regression pin for the Bugbot HIGH fix (rollback/purge silently
    /// skipping the workspace when the process's top-level active backend
    /// is Azure): `rollback`/`purge` decide legacy-vs-trait via
    /// `backend.kind() == BackendKind::Azure` on the value THIS function
    /// returns. This test builds a workspace whose DEFAULT entry is on the
    /// `azure` backend while a non-default entry is on a hermetic local
    /// store, with no separate "top-level active backend" standing for
    /// Azure at all (proving the kind check can't be reading some other,
    /// caller-side backend) — an unqualified write must resolve to a
    /// backend whose `.kind()` is `Azure`. No network call happens here:
    /// constructing `AzureBackend` succeeds without real credentials
    /// (auth is resolved lazily, only at first actual secret operation),
    /// so this stays fully hermetic while still exercising real
    /// `AzureBackend` construction through the same `materialize` path
    /// `execute_secret_rollback_direct`/`execute_secret_purge_direct` use.
    #[tokio::test]
    async fn resolved_backend_kind_reflects_workspace_entry_not_a_separate_active_backend() {
        use crate::backend::BackendKind;

        let tmp = tempfile::tempdir().unwrap();
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
        // No top-level `backend`/`local`/`azure` config at all — this
        // config's only backends are the workspace's own entries, so
        // there is no separate "active backend" for the azure branch's
        // decision to have accidentally read instead.
        let config = Config {
            named_backends,
            ..Default::default()
        };
        let registry =
            BackendRegistry::with_lazy(&config, &["azure".to_string(), "local-a".to_string()])
                .expect("must register");

        let ws = Workspace {
            entries: vec![
                WorkspaceEntry {
                    alias: "az".to_string(),
                    backend: "azure".to_string(),
                    vault: "az-vault".to_string(),
                    default: true,
                },
                WorkspaceEntry {
                    alias: "loc".to_string(),
                    backend: "local-a".to_string(),
                    vault: "default".to_string(),
                    default: false,
                },
            ],
            default_alias: "az".to_string(),
            source: WorkspaceSource::Context,
        };

        let (target, _path) =
            resolve_secret_target("SOME_SECRET", &ws, &registry, TargetMode::Write)
                .await
                .expect("azure backend must construct hermetically (no credentials needed until an actual secret operation)");
        assert_eq!(
            target.backend.kind(),
            BackendKind::Azure,
            "unqualified write must resolve to the default entry's backend kind (azure) — \
             this is the exact value rollback/purge's legacy-vs-trait check consumes"
        );
    }

    /// Bugbot round-3 MEDIUM fix: capability gates (`history`, `rollback`,
    /// `purge`, `restore`, `rotate --native`) must evaluate the RESOLVED
    /// target's capabilities, not a separate "active backend" assumption.
    /// `azure` and `local` genuinely differ on `has_rbac` (azure: true,
    /// local: false — real backends only differ on `has_rbac` and
    /// `has_secret_rotation`; the flags `history`/`rollback`/`purge`/
    /// `restore` actually check — `has_versioning`/`has_soft_delete` — are
    /// `true` on every backend, so `has_rbac` is the general, hermetic
    /// stand-in this test uses to pin the *general* "resolved capabilities,
    /// not a separate active backend's" seam every one of those call sites
    /// now shares (each does `resolve_workspace_or_default(...)` then
    /// `resolved.capabilities()`, identical in shape to this test). This is
    /// the unit-level proof the round-3 writeup asks for since no two real
    /// backends differ on the specific flags history/rollback/purge/restore
    /// check, so an e2e mismatch isn't constructible for those verbs
    /// (`rotate --native`'s `has_secret_rotation` mismatch IS e2e-drivable
    /// and is covered in `tests/e2e_workspaces.rs`).
    #[tokio::test]
    async fn resolved_backend_capabilities_reflect_workspace_entry_not_a_separate_active_backend() {
        let tmp = tempfile::tempdir().unwrap();
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
        // No top-level `backend`/`local`/`azure` config at all — same
        // reasoning as the sibling test above: no separate "active
        // backend" exists here for a buggy gate to have accidentally read.
        let config = Config {
            named_backends,
            ..Default::default()
        };
        let registry =
            BackendRegistry::with_lazy(&config, &["azure".to_string(), "local-a".to_string()])
                .expect("must register");

        // "loc"/local-a is the DEFAULT here (not azure): Write mode's
        // exact-name-first probe is scoped to the default vault only, and
        // probing azure would require a real network round-trip (unlike
        // constructing an AzureBackend, which is instant/credential-free)
        // — a qualified write ("az:...") below must never touch it.
        let ws = Workspace {
            entries: vec![
                WorkspaceEntry {
                    alias: "loc".to_string(),
                    backend: "local-a".to_string(),
                    vault: "default".to_string(),
                    default: true,
                },
                WorkspaceEntry {
                    alias: "az".to_string(),
                    backend: "azure".to_string(),
                    vault: "az-vault".to_string(),
                    default: false,
                },
            ],
            default_alias: "loc".to_string(),
            source: WorkspaceSource::Context,
        };

        // Unqualified (Write mode): resolves to the default ("loc"/local)
        // — capabilities().has_rbac must be false (local's), proving a
        // caller gating on `resolved.capabilities()` reads the entry that
        // was actually picked.
        let (default_target, _) =
            resolve_secret_target("SOME_SECRET", &ws, &registry, TargetMode::Write)
                .await
                .expect("local backend must resolve");
        assert!(
            !default_target.backend.capabilities().has_rbac,
            "resolved default (local) capabilities must be read"
        );

        // Qualified to "az" (azure): capabilities().has_rbac must now be
        // true (azure's), even though the WORKSPACE DEFAULT (local) has no
        // RBAC — proving the gate follows resolution per-call, not a
        // cached/assumed value from the default or any other entry. The
        // exact-name-first probe this triggers is scoped to the DEFAULT
        // ("loc"/local, hermetic), never to azure, so this stays
        // network-free even though the resolved target is azure.
        let (named_target, _) =
            resolve_secret_target("az:SOME_SECRET", &ws, &registry, TargetMode::Write)
                .await
                .expect("azure backend must construct hermetically");
        assert!(
            named_target.backend.capabilities().has_rbac,
            "resolved named entry (azure) capabilities must be read, not local's (the default)"
        );
    }

    /// `history`'s exact resolution mode (`TargetMode::Read`) exercises the
    /// same `resolve_secret_target` → `.capabilities()` seam pinned above
    /// for Write mode. A genuine capability VALUE mismatch isn't
    /// constructible here with an azure entry present — Read mode always
    /// searches every attached vault (including azure) for ANY name,
    /// qualified or not, which needs real network, unlike Write's
    /// default-only probe. This instead pins that Read mode resolves to
    /// the correct ENTRY — proving `history`'s post-resolution capability
    /// check (`backend.capabilities().has_versioning`, applied in
    /// `execute_secret_history_direct` right after this same call) reads
    /// whichever entry the search actually picked, not the workspace
    /// default or any other assumption — using two hermetic local stores.
    #[tokio::test]
    async fn history_gates_on_resolved_backend_capabilities() {
        let tmp = tempfile::tempdir().unwrap();
        let (registry, ws) = two_vault_workspace(&tmp);

        let stage_backend = registry.materialize("local-b").unwrap();
        stage_backend
            .secrets()
            .set_secret("default", req("HIST_ONLY_STAGE", "v"))
            .await
            .unwrap();

        // Unqualified, but only present in "stage" (not the default,
        // "work") — Read mode's search must resolve it there.
        let (target, _path) =
            resolve_secret_target("HIST_ONLY_STAGE", &ws, &registry, TargetMode::Read)
                .await
                .expect("must resolve via search");
        assert_eq!(
            target.entry.alias, "stage",
            "must resolve to the vault that actually has the secret, not the workspace default"
        );
        // The capability value `history` gates on, read from the entry
        // the search actually picked.
        assert!(target.backend.capabilities().has_versioning);
    }
}
