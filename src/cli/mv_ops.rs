//! Move (rename/relocate) command parsing and validation.
//!
//! Grammar (spec 2026-07-02-fs-verbs): a trailing `/` marks a folder;
//! `/` alone is the vault root; otherwise the last segment is the secret
//! name and everything before it the folder. A bare destination therefore
//! means "vault root + rename".

use crate::backend::BackendRegistry;
use crate::cli::helpers::{resolve_vault_for_trait, use_trait_path};
use crate::cli::ls_view::{display_name, qualified_display_name};
use crate::cli::secret_ops::invalidate_trait_secret_cache;
use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::secret::manager::{FieldUpdate, SecretSummary, SecretUpdateRequest};
use crate::utils::output;
use crate::utils::suggestions::closest_match;

/// Plan for moving secrets or folders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MvPlan {
    /// Move a single secret.
    Secret {
        src_folder: Option<String>,
        src_name: String,
        dest_folder: Option<String>,
        dest_name: String,
    },
    /// Move (rename) a folder.
    Folder {
        src_prefix: String,
        dest_prefix: Option<String>,
    },
}

/// Parse the `xv mv` operands into a move plan. See the module doc for the
/// full grammar.
pub(crate) fn parse_mv(source: &str, dest: &str) -> Result<MvPlan> {
    let source = source.trim();
    let dest = dest.trim();
    if source.is_empty() || dest.is_empty() {
        return Err(CrosstacheError::invalid_argument(
            "mv requires a SOURCE and a DEST",
        ));
    }

    // `/` alone also ends in '/', so this single check covers both cases.
    let src_is_folder = source.ends_with('/');
    if src_is_folder {
        let src_prefix = source.trim_matches('/');
        if src_prefix.is_empty() {
            return Err(CrosstacheError::invalid_argument(
                "moving the vault root is not supported; name a folder (e.g. 'app/')",
            ));
        }
        if src_prefix.split('/').any(str::is_empty) {
            return Err(CrosstacheError::invalid_argument(format!(
                "invalid source folder '{source}'"
            )));
        }
        let dest_prefix = if dest == "/" {
            None
        } else if let Some(stripped) = dest.strip_suffix('/') {
            let p = stripped.trim_start_matches('/');
            if p.is_empty() || p.split('/').any(str::is_empty) {
                return Err(CrosstacheError::invalid_argument(format!(
                    "invalid destination folder '{dest}'"
                )));
            }
            Some(p.to_string())
        } else {
            return Err(CrosstacheError::invalid_argument(format!(
                "folder moves require a folder destination ending in / (got '{dest}'); \
                 did you mean '{dest}/'?"
            )));
        };
        return Ok(MvPlan::Folder {
            src_prefix: src_prefix.to_string(),
            dest_prefix,
        });
    }

    let (src_folder, src_name) = split_secret_path(source)?;
    let (dest_folder, dest_name) = if dest == "/" {
        (None, src_name.clone())
    } else if let Some(stripped) = dest.strip_suffix('/') {
        let p = stripped.trim_start_matches('/');
        if p.is_empty() || p.split('/').any(str::is_empty) {
            return Err(CrosstacheError::invalid_argument(format!(
                "invalid destination folder '{dest}'"
            )));
        }
        (Some(p.to_string()), src_name.clone())
    } else {
        split_secret_path(dest)?
    };

    Ok(MvPlan::Secret {
        src_folder,
        src_name,
        dest_folder,
        dest_name,
    })
}

/// Split `folder/name` (no trailing slash): last segment = name, the rest =
/// folder (`None` at the root). Leading `/` is tolerated (`/x` == `x`).
fn split_secret_path(path: &str) -> Result<(Option<String>, String)> {
    let path = path.trim_start_matches('/');
    let (folder, name) = match path.rsplit_once('/') {
        Some((f, n)) => (Some(f), n),
        None => (None, path),
    };
    if name.is_empty() || folder.is_some_and(|f| f.is_empty() || f.split('/').any(str::is_empty)) {
        return Err(CrosstacheError::invalid_argument(format!(
            "invalid secret path '{path}'"
        )));
    }
    Ok((folder.map(String::from), name.to_string()))
}

/// Normalize a folder value for comparison: `None` and `Some("")` are both
/// "the vault root".
fn norm_folder(folder: Option<&str>) -> Option<&str> {
    folder.filter(|f| !f.is_empty())
}

/// True if any secret in `secrets` already occupies `dest_name` — either as
/// its display name or as its raw backend (sanitized) name. A secret whose
/// backend key equals the destination but whose display label differs would
/// otherwise slip past the pre-check, letting the folder update apply before
/// `rename_secret`'s own exists-guard fails and leaving a half-applied move.
fn dest_collides(secrets: &[SecretSummary], dest_name: &str) -> bool {
    secrets
        .iter()
        .any(|s| display_name(s) == dest_name || s.name == dest_name)
}

pub(crate) async fn execute_mv(
    source: String,
    dest: String,
    dry_run: bool,
    yes: bool,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    if !use_trait_path(registry) {
        return Err(CrosstacheError::config(
            "No backend registry available. Run 'xv config show' to check your configuration.",
        ));
    }
    let plan = parse_mv(&source, &dest)?;
    let reg = registry.expect("use_trait_path guarantees Some");
    let vault_name = resolve_vault_for_trait(&config, registry).await?;
    let secrets = reg
        .active()
        .secrets()
        .list_secrets(&vault_name, None)
        .await?;

    match plan {
        MvPlan::Secret {
            src_folder,
            src_name,
            dest_folder,
            dest_name,
        } => {
            execute_secret_mv(
                reg,
                &config,
                &vault_name,
                secrets,
                &source,
                &dest,
                src_folder,
                src_name,
                dest_folder,
                dest_name,
                dry_run,
            )
            .await
        }
        MvPlan::Folder {
            src_prefix,
            dest_prefix,
        } => {
            execute_folder_mv(
                reg,
                &config,
                &vault_name,
                secrets,
                src_prefix,
                dest_prefix,
                dry_run,
                yes,
            )
            .await
        }
    }
}

/// Move/rename a single secret. Ordering is binding (spec 2026-07-02-fs-verbs):
/// find source → no-op check → collision pre-check → folder update →
/// rename. The collision pre-check happens before the folder update so a
/// doomed rename never leaves a half-applied folder change behind.
#[allow(clippy::too_many_arguments)]
async fn execute_secret_mv(
    reg: &BackendRegistry,
    config: &Config,
    vault_name: &str,
    secrets: Vec<SecretSummary>,
    source: &str,
    dest: &str,
    src_folder: Option<String>,
    src_name: String,
    dest_folder: Option<String>,
    dest_name: String,
    dry_run: bool,
) -> Result<()> {
    let src_folder_norm = norm_folder(src_folder.as_deref());

    let found = secrets.iter().find(|s| {
        display_name(s) == src_name && norm_folder(s.folder.as_deref()) == src_folder_norm
    });

    let Some(found) = found else {
        let candidates: Vec<String> = secrets
            .iter()
            .map(|s| qualified_display_name(s, ""))
            .collect();
        return Err(CrosstacheError::secret_not_found(format!(
            "{source} (in vault '{vault_name}')"
        ))
        .with_suggestion(closest_match(source, &candidates).map(String::from)));
    };

    let current_folder_norm = norm_folder(found.folder.as_deref());
    let dest_folder_norm = norm_folder(dest_folder.as_deref());

    // No-op: same folder, same name.
    if dest_folder_norm == current_folder_norm && dest_name == src_name {
        output::info(&format!(
            "'{source}' is already at '{dest}' — nothing to do"
        ));
        return Ok(());
    }

    // Collision pre-check — before any mutation — only relevant when the
    // name is actually changing.
    if dest_name != src_name && dest_collides(&secrets, &dest_name) {
        return Err(CrosstacheError::conflict(format!(
            "secret '{dest_name}' already exists in vault '{vault_name}' — delete it first or pick another name"
        )));
    }

    if dry_run {
        let dest_qualified = match &dest_folder {
            Some(f) if !f.is_empty() => format!("{f}/{dest_name}"),
            _ => dest_name.clone(),
        };
        println!("{source} -> {dest_qualified}");
        output::info("1 secret would move (dry run)");
        return Ok(());
    }

    let mut folder_updated = false;
    if dest_folder_norm != current_folder_norm {
        let request = SecretUpdateRequest {
            name: src_name.clone(),
            value: None,
            content_type: None,
            enabled: None,
            expires_on: FieldUpdate::Unchanged,
            not_before: FieldUpdate::Unchanged,
            tags: None,
            groups: None,
            note: FieldUpdate::Unchanged,
            folder: match &dest_folder {
                Some(f) => FieldUpdate::Set(f.clone()),
                None => FieldUpdate::Clear,
            },
            replace_tags: false,
            replace_groups: false,
        };
        reg.active()
            .secrets()
            .update_secret(vault_name, &src_name, request)
            .await?;
        invalidate_trait_secret_cache(config, vault_name);
        folder_updated = true;
    }

    if dest_name != src_name {
        let rename_result = reg
            .active()
            .secrets()
            .rename_secret(vault_name, &src_name, &dest_name)
            .await;
        // Rename may mutate state even when it errors (e.g. RenameIncomplete),
        // so invalidate unconditionally before inspecting the result.
        invalidate_trait_secret_cache(config, vault_name);
        if let Err(e) = rename_result {
            if folder_updated {
                let msg = if matches!(
                    e,
                    crate::backend::error::BackendError::RenameIncomplete { .. }
                ) {
                    "the folder update was applied; the rename did not complete cleanly — both names currently exist (see the error below for recovery)"
                } else {
                    "the folder update was applied; the rename did not complete — the secret keeps its original name"
                };
                output::warn(msg);
            }
            return Err(e.into());
        }
    }

    let dest_qualified = match &dest_folder {
        Some(f) if !f.is_empty() => format!("{f}/{dest_name}"),
        _ => dest_name,
    };
    output::success(&format!("Moved '{source}' to '{dest_qualified}'"));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn execute_folder_mv(
    reg: &BackendRegistry,
    config: &Config,
    vault_name: &str,
    secrets: Vec<SecretSummary>,
    src_prefix: String,
    dest_prefix: Option<String>,
    dry_run: bool,
    yes: bool,
) -> Result<()> {
    use crate::cli::helpers::confirm_proceed;
    use crate::cli::ls_view::{display_name, relative_to_scope};

    if dest_prefix.as_deref() == Some(src_prefix.as_str()) {
        return Err(CrosstacheError::invalid_argument(format!(
            "source and destination folders are identical ('{src_prefix}/')"
        )));
    }

    // (old qualified path, new qualified path, name to call the API with, new folder tag or None=clear)
    let mut moves: Vec<(String, String, String, Option<String>)> = Vec::new();
    for s in &secrets {
        let folder = s.folder.as_deref().unwrap_or("");
        let Some(remainder) = relative_to_scope(folder, &src_prefix) else {
            continue; // out of scope (segment boundary enforced by relative_to_scope)
        };
        let new_folder = match (&dest_prefix, remainder) {
            (Some(d), "") => Some(d.clone()),
            (Some(d), rest) => Some(format!("{d}/{rest}")),
            (None, "") => None,
            (None, rest) => Some(rest.to_string()),
        };
        if norm_folder(new_folder.as_deref()) == norm_folder(Some(folder)) {
            continue; // already at destination
        }
        let name = display_name(s).to_string();
        let old_path = format!("{folder}/{name}");
        let new_path = match &new_folder {
            Some(f) => format!("{f}/{name}"),
            None => name.clone(),
        };
        moves.push((old_path, new_path, name, new_folder));
    }

    if moves.is_empty() {
        return Err(CrosstacheError::invalid_argument(format!(
            "no secrets under '{src_prefix}/'"
        )));
    }

    if dry_run {
        for (old, new, _, _) in &moves {
            println!("{old} -> {new}");
        }
        output::info(&format!("{} secrets would move (dry run)", moves.len()));
        return Ok(());
    }

    let dest_label = dest_prefix
        .as_deref()
        .map_or("/".to_string(), |d| format!("{d}/"));
    eprintln!(
        "Moving {} secrets from '{src_prefix}/' to '{dest_label}':",
        moves.len()
    );
    for (old, new, _, _) in moves.iter().take(10) {
        eprintln!("  {old} -> {new}");
    }
    if moves.len() > 10 {
        eprintln!("  ... ({} more; --dry-run to list all)", moves.len() - 10);
    }
    if !confirm_proceed(yes, &format!("Move {} secrets?", moves.len()), "--yes")? {
        output::info("Aborted; nothing moved.");
        return Ok(());
    }

    let mut failures = 0usize;
    for (old, new, name, new_folder) in &moves {
        let request = SecretUpdateRequest {
            name: name.clone(),
            value: None,
            content_type: None,
            enabled: None,
            expires_on: FieldUpdate::Unchanged,
            not_before: FieldUpdate::Unchanged,
            tags: None,
            groups: None,
            note: FieldUpdate::Unchanged,
            folder: match new_folder {
                Some(f) => FieldUpdate::Set(f.clone()),
                None => FieldUpdate::Clear,
            },
            replace_tags: false,
            replace_groups: false,
        };
        if let Err(e) = reg
            .active()
            .secrets()
            .update_secret(vault_name, name, request)
            .await
        {
            failures += 1;
            output::warn(&format!("failed to move '{old}' to '{new}': {e}"));
        }
    }
    invalidate_trait_secret_cache(config, vault_name);
    let moved = moves.len() - failures;
    if failures > 0 {
        return Err(CrosstacheError::unknown(format!(
            "moved {moved} of {} secrets; {failures} failed (see warnings above)",
            moves.len()
        )));
    }
    output::success(&format!(
        "Moved {moved} secrets from '{src_prefix}/' to '{dest_label}'"
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(sf: Option<&str>, sn: &str, df: Option<&str>, dn: &str) -> MvPlan {
        MvPlan::Secret {
            src_folder: sf.map(String::from),
            src_name: sn.into(),
            dest_folder: df.map(String::from),
            dest_name: dn.into(),
        }
    }

    #[test]
    fn grammar_table() {
        // (source, dest, expected) — every row of the spec grammar table.
        let cases = [
            (
                "db/pass",
                "app/",
                secret(Some("db"), "pass", Some("app"), "pass"),
            ),
            (
                "db/pass",
                "app/pw",
                secret(Some("db"), "pass", Some("app"), "pw"),
            ),
            (
                "db/pass",
                "newname",
                secret(Some("db"), "pass", None, "newname"),
            ),
            ("app/pass", "/", secret(Some("app"), "pass", None, "pass")),
            ("pass", "app/", secret(None, "pass", Some("app"), "pass")),
            (
                "a/b/pass",
                "x/y/",
                secret(Some("a/b"), "pass", Some("x/y"), "pass"),
            ),
            (
                "app/",
                "svc/",
                MvPlan::Folder {
                    src_prefix: "app".into(),
                    dest_prefix: Some("svc".into()),
                },
            ),
            (
                "app/",
                "/",
                MvPlan::Folder {
                    src_prefix: "app".into(),
                    dest_prefix: None,
                },
            ),
            (
                "app/db/",
                "svc/",
                MvPlan::Folder {
                    src_prefix: "app/db".into(),
                    dest_prefix: Some("svc".into()),
                },
            ),
        ];
        for (src, dst, want) in cases {
            let got = parse_mv(src, dst).unwrap_or_else(|e| panic!("mv {src} {dst}: {e}"));
            assert_eq!(got, want, "mv {src} {dst}");
        }
    }

    #[test]
    fn grammar_errors() {
        // Folder source requires a folder destination.
        let e = parse_mv("app/", "svc").unwrap_err().to_string();
        assert!(e.contains("ending in /"), "{e}");
        // Vault root is not a movable source.
        assert!(parse_mv("/", "svc/").is_err());
        // Empty operands.
        assert!(parse_mv("", "x").is_err());
        assert!(parse_mv("x", "").is_err());
        assert!(parse_mv("x", "   ").is_err());
        // Destination that is only a slashless empty name after a folder.
        assert!(parse_mv("db/pass", "app//").is_err());
        // Folder source with an internal empty segment.
        assert!(parse_mv("app//db/", "svc/").is_err());
    }

    fn summary(name: &str, original_name: &str) -> SecretSummary {
        SecretSummary {
            name: name.to_string(),
            original_name: original_name.to_string(),
            note: None,
            folder: None,
            groups: None,
            updated_on: String::new(),
            enabled: true,
            content_type: String::new(),
            tags: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn dest_collides_matches_display_name() {
        let secrets = vec![summary("sanitized-name", "pretty-name")];
        assert!(dest_collides(&secrets, "pretty-name"));
    }

    #[test]
    fn dest_collides_matches_backend_name_only() {
        // Backend (sanitized) name equals the destination, but the display
        // label (original_name) differs — this is exactly the case that
        // slipped past the old display-name-only pre-check.
        let secrets = vec![summary("dest-name", "some-other-display-name")];
        assert!(dest_collides(&secrets, "dest-name"));
    }

    #[test]
    fn dest_collides_no_match() {
        let secrets = vec![summary("sanitized-name", "pretty-name")];
        assert!(!dest_collides(&secrets, "unrelated-name"));
    }

    // -----------------------------------------------------------------
    // execute_folder_mv — partial bulk failure
    // -----------------------------------------------------------------

    use crate::backend::error::BackendError;
    use crate::backend::{Backend, BackendCapabilities, BackendKind, NameCharset, SecretBackend};
    use crate::secret::manager::SecretProperties;
    use std::sync::{Arc, Mutex};

    fn fake_secret_properties(name: &str) -> SecretProperties {
        SecretProperties {
            name: name.to_string(),
            original_name: name.to_string(),
            value: None,
            version: "v1".to_string(),
            version_number: Some(1),
            created_timestamp: 0,
            created_on: "2026-07-02".to_string(),
            updated_on: "2026-07-02".to_string(),
            enabled: true,
            expires_on: None,
            not_before: None,
            tags: std::collections::HashMap::new(),
            content_type: String::new(),
            recovery_level: None,
        }
    }

    fn folder_summary(name: &str, folder: &str) -> SecretSummary {
        SecretSummary {
            name: name.to_string(),
            original_name: name.to_string(),
            note: None,
            folder: Some(folder.to_string()),
            groups: None,
            updated_on: String::new(),
            enabled: true,
            content_type: String::new(),
            tags: std::collections::HashMap::new(),
        }
    }

    /// Backend whose `update_secret` fails for one specific secret name and
    /// succeeds for every other, recording every name it was called with —
    /// exercises `execute_folder_mv`'s partial-failure path (sequential
    /// apply, per-failure warning, single cache invalidation, and the
    /// "moved X of Y" error).
    struct PartialFailBackend {
        fail_name: String,
        calls: Mutex<Vec<String>>,
    }

    impl PartialFailBackend {
        fn new(fail_name: &str) -> Self {
            Self {
                fail_name: fail_name.to_string(),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn called_names(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl SecretBackend for PartialFailBackend {
        async fn set_secret(
            &self,
            _vault: &str,
            _request: crate::secret::manager::SecretRequest,
        ) -> std::result::Result<SecretProperties, BackendError> {
            Err(BackendError::Unsupported("test backend".into()))
        }

        async fn get_secret(
            &self,
            _vault: &str,
            _name: &str,
            _include_value: bool,
        ) -> std::result::Result<SecretProperties, BackendError> {
            Err(BackendError::Unsupported("test backend".into()))
        }

        async fn get_secret_version(
            &self,
            _vault: &str,
            _name: &str,
            _version: &str,
            _include_value: bool,
        ) -> std::result::Result<SecretProperties, BackendError> {
            Err(BackendError::Unsupported("test backend".into()))
        }

        async fn list_secrets(
            &self,
            _vault: &str,
            _group_filter: Option<&str>,
        ) -> std::result::Result<Vec<SecretSummary>, BackendError> {
            Ok(Vec::new())
        }

        async fn delete_secret(
            &self,
            _vault: &str,
            _name: &str,
        ) -> std::result::Result<(), BackendError> {
            Err(BackendError::Unsupported("test backend".into()))
        }

        async fn update_secret(
            &self,
            _vault: &str,
            name: &str,
            _request: SecretUpdateRequest,
        ) -> std::result::Result<SecretProperties, BackendError> {
            self.calls.lock().unwrap().push(name.to_string());
            if name == self.fail_name {
                Err(BackendError::Internal(format!(
                    "simulated failure updating '{name}'"
                )))
            } else {
                Ok(fake_secret_properties(name))
            }
        }
    }

    #[async_trait::async_trait]
    impl Backend for PartialFailBackend {
        fn name(&self) -> &'static str {
            "local"
        }

        fn kind(&self) -> BackendKind {
            BackendKind::Local
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities {
                has_vaults: true,
                has_file_storage: false,
                has_rbac: false,
                has_audit: false,
                has_versioning: true,
                has_soft_delete: true,
                has_secret_rotation: false,
                has_groups: true,
                has_folders: true,
                has_notes: true,
                has_expiry: true,
                max_secret_size: None,
                max_name_length: None,
                name_charset: NameCharset::Unrestricted,
                max_tags: None,
                max_tag_value_len: None,
            }
        }

        fn secrets(&self) -> &dyn SecretBackend {
            self
        }

        async fn health_check(&self) -> std::result::Result<(), BackendError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn execute_folder_mv_partial_failure_reports_count_and_attempts_all() {
        let backend = Arc::new(PartialFailBackend::new("b"));
        let registry = BackendRegistry::new(backend.clone());
        let config = Config::default();

        let secrets = vec![
            folder_summary("a", "app"),
            folder_summary("b", "app"),
            folder_summary("c", "app"),
        ];

        let result = execute_folder_mv(
            &registry,
            &config,
            "test-vault",
            secrets,
            "app".to_string(),
            Some("svc".to_string()),
            false,
            true,
        )
        .await;

        let err = match result {
            Ok(()) => panic!("one secret's update fails; execute_folder_mv must report it"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("moved 2 of 3"),
            "expected moved-count message, got: {msg}"
        );
        assert!(
            msg.contains('1') && msg.to_lowercase().contains("failed"),
            "expected failure count in message, got: {msg}"
        );

        // All three secrets were attempted, not just up to the failure.
        let mut called = backend.called_names();
        called.sort();
        assert_eq!(
            called,
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }
}
