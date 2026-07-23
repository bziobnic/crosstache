//! Move (rename/relocate) command parsing and validation.
//!
//! Grammar (spec 2026-07-02-fs-verbs): a trailing `/` marks a folder;
//! `/` alone is the vault root; otherwise the last segment is the secret
//! name and everything before it the folder. A bare destination therefore
//! means "vault root + rename".

use crate::backend::BackendRegistry;
use crate::cli::helpers::{resolve_vault_for_trait, use_trait_path};
use crate::cli::ls_view::{display_name, qualified_display_name};
use crate::cli::secret_ops::{confirm_reserved_key_write, invalidate_trait_secret_cache};
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
    /// Bulk-move every secret whose name matches a glob into a destination
    /// folder (2026-07-03 mv-filter design). Matched secrets keep their
    /// names; only the `folder` metadata is rewritten — a rename is
    /// impossible for a multi-secret move.
    Filter {
        pattern: String,
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
        let dest_prefix = parse_folder_dest(dest)?;
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

/// Parse a folder-only destination (`folder/` or `/`) — shared by folder
/// moves and `--filter` moves, since a rename is impossible for a
/// multi-secret move. Same error shape as the original inline folder-move
/// check: "folder moves require a folder destination ending in / (got
/// '...'); did you mean '.../'?".
fn parse_folder_dest(dest: &str) -> Result<Option<String>> {
    if dest == "/" {
        return Ok(None);
    }
    if let Some(stripped) = dest.strip_suffix('/') {
        let p = stripped.trim_start_matches('/');
        if p.is_empty() || p.split('/').any(str::is_empty) {
            return Err(CrosstacheError::invalid_argument(format!(
                "invalid destination folder '{dest}'"
            )));
        }
        return Ok(Some(p.to_string()));
    }
    Err(CrosstacheError::invalid_argument(format!(
        "folder moves require a folder destination ending in / (got '{dest}'); \
         did you mean '{dest}/'?"
    )))
}

/// Parse `xv mv --filter <GLOB> DEST`. Validates the glob compiles (fails
/// loud with `invalid_argument`, matching `ls`/`find --filter`, before any
/// backend call) and that `DEST` is a folder destination.
pub(crate) fn parse_mv_filter(pattern: &str, dest: &str) -> Result<MvPlan> {
    let pattern = pattern.trim();
    let dest = dest.trim();
    if pattern.is_empty() {
        return Err(CrosstacheError::invalid_argument(
            "--filter requires a non-empty glob pattern",
        ));
    }
    if dest.is_empty() {
        return Err(CrosstacheError::invalid_argument("mv requires a DEST"));
    }
    // Validate the glob compiles before any backend call.
    crate::utils::helpers::compile_name_glob(pattern)?;
    let dest_prefix = parse_folder_dest(dest)?;
    Ok(MvPlan::Filter {
        pattern: pattern.to_string(),
        dest_prefix,
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
    source: Option<String>,
    dest: Option<String>,
    filter: Option<String>,
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

    // DEST is `Option<String>` in the arg parser only so clap accepts a
    // non-required SOURCE positional (clap forbids optional-then-required);
    // it is always required in practice.
    let dest = match dest {
        Some(d) => d,
        None if source.is_none() && filter.is_none() => {
            // Neither SOURCE, --filter, nor DEST given — match parse_mv's own
            // "both operands missing" wording rather than naming DEST alone.
            return Err(CrosstacheError::invalid_argument(
                "mv requires a SOURCE and a DEST",
            ));
        }
        None => return Err(CrosstacheError::invalid_argument("mv requires a DEST")),
    };

    // Exactly one of SOURCE / --filter — checked, and (for --filter) the
    // glob compiled and validated, before any backend call below.
    let filter_plan = match (&source, &filter) {
        (Some(_), Some(_)) => {
            return Err(CrosstacheError::invalid_argument(
                "mv accepts either SOURCE or --filter <GLOB>, not both",
            ));
        }
        (None, None) => {
            return Err(CrosstacheError::invalid_argument(
                "mv requires either a SOURCE argument or --filter <GLOB>",
            ));
        }
        (None, Some(pattern)) => Some(parse_mv_filter(pattern, &dest)?),
        (Some(_), None) => None,
    };

    let reg = registry.expect("use_trait_path guarantees Some");

    // Workspace-aware vault resolution (Bugbot BLOCKER/MAJOR fix, Phase C
    // follow-up): a single-secret `mv` (no `--filter`) resolves BOTH
    // operands through the exact same `resolve_secret_target` rule
    // `get`/`set`/`update`/etc. already use — exact-name-first (scoped to
    // the workspace default vault), `alias:path` -> that entry, unqualified
    // -> the default entry. This fixes three bugs the previous
    // both-sides-must-be-aliased gate had:
    //   - dest-only alias (`mv SECRET work:new`) used to fall through to
    //     `parse_mv`, which only splits on `/`, silently renaming to a
    //     LITERAL name containing a colon in the wrong vault.
    //   - source-only alias (`mv work:SECRET archive/`) used to treat
    //     `work:SECRET` as a literal name and fail not-found.
    //   - unqualified `mv a b` under a workspace resolved
    //     `resolve_vault_for_trait`'s config-level vault instead of the
    //     workspace's default entry, diverging from `get`/`set`.
    // Exactly one alias / no alias / both aliases are all handled
    // uniformly: whichever (backend, vault) pair each side resolves to,
    // compare them — identical pairs degenerate to the ordinary same-vault
    // rename/re-folder path (just resolved against the workspace's chosen
    // vault instead of the config-level one); different pairs route to the
    // cross-vault copy+delete machinery. `--filter` and no-workspace-at-all
    // are unaffected: they fall straight through to the pre-workspace path
    // below, byte-identical.
    if filter.is_none() {
        // Only a REAL (configured) workspace takes the alias-aware `mv` path;
        // `resolve_configured_workspace` returns `None` with no configured
        // workspace, so a degenerate single-vault `mv` falls through to the
        // byte-identical pre-workspace path below.
        if let Some(ws) = crate::workspace::resolve_configured_workspace(&config).await? {
            let src_raw = source
                .as_deref()
                .expect("checked above: exactly one of SOURCE/--filter is Some");
            let backend_names: Vec<String> = ws.entries.iter().map(|e| e.backend.clone()).collect();
            let ws_registry = BackendRegistry::with_lazy(&config, &backend_names)
                .map_err(|e| CrosstacheError::config(e.to_string()))?;

            let (src_target, src_path) = crate::workspace::resolve_secret_target(
                src_raw,
                &ws,
                &ws_registry,
                crate::workspace::TargetMode::Write,
            )
            .await?;
            let (dst_target, dst_path) = crate::workspace::resolve_secret_target(
                &dest,
                &ws,
                &ws_registry,
                crate::workspace::TargetMode::Write,
            )
            .await?;

            if src_target.entry.backend == dst_target.entry.backend
                && src_target.entry.vault == dst_target.entry.vault
            {
                // Degenerate case: both operands resolved to the SAME
                // (backend, vault) — e.g. an unqualified `mv` under a
                // workspace (both sides -> the default entry), or an
                // explicit alias on one/both sides naming that same entry.
                // Run the ordinary same-vault rename/re-folder machinery
                // against the RESOLVED vault, using the ORIGINAL raw
                // strings (not the alias-stripped paths) for user-facing
                // messages so they echo exactly what was typed.
                let vault_name = src_target.entry.vault.clone();
                let backend_name = src_target.entry.backend.clone();
                let backend = src_target.backend.clone();
                let secrets = backend.secrets().list_secrets(&vault_name, None).await?;
                let plan = parse_mv(&src_path, &dst_path)?;
                return match plan {
                    MvPlan::Secret {
                        src_folder,
                        src_name,
                        dest_folder,
                        dest_name,
                    } => {
                        execute_secret_mv(
                            &backend,
                            &backend_name,
                            &config,
                            &vault_name,
                            secrets,
                            src_raw,
                            &dest,
                            src_folder,
                            src_name,
                            dest_folder,
                            dest_name,
                            dry_run,
                            yes,
                        )
                        .await
                    }
                    MvPlan::Folder {
                        src_prefix,
                        dest_prefix,
                    } => {
                        execute_folder_mv(
                            &backend,
                            &backend_name,
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
                    MvPlan::Filter { .. } => unreachable!("parse_mv never returns MvPlan::Filter"),
                };
            }

            // Genuine cross-vault move: only a single secret is supported
            // today (folder/`--filter` moves across vaults are out of
            // scope, same restriction `execute_cross_vault_alias_mv`
            // already enforces internally).
            return execute_cross_vault_alias_mv(
                &ws_registry,
                &config,
                &src_target.entry,
                &dst_target.entry,
                &src_path,
                &dst_path,
                dry_run,
                yes,
            )
            .await;
        }
    }

    // At this point either `--filter` was given, or no workspace is
    // attached at all (the single-secret workspace path above already
    // returned). `--filter` bulk moves target the workspace DEFAULT entry
    // when one is attached (Bugbot MEDIUM fix, round 4) — consistent with
    // the single-secret path above, which already resolves an unqualified
    // operand to the default; without this, `mv --filter` diverged onto
    // `resolve_vault_for_trait`'s config-level vault while sibling
    // single-secret `mv` targeted the workspace default. No workspace:
    // unchanged, byte-identical. Cross-vault `--filter` moves stay out of
    // scope — only the DEFAULT entry is ever chosen here.
    let workspace_for_filter = if filter.is_some() {
        // No configured workspace ⇒ `None`: bulk `mv --filter` stays
        // byte-identical to the pre-workspace path.
        crate::workspace::resolve_configured_workspace(&config).await?
    } else {
        None
    };
    let (vault_name, backend_name, backend) = if let Some(ws) = workspace_for_filter {
        let backend_names: Vec<String> = ws.entries.iter().map(|e| e.backend.clone()).collect();
        let ws_registry = BackendRegistry::with_lazy(&config, &backend_names)
            .map_err(|e| CrosstacheError::config(e.to_string()))?;
        let default_entry = ws.default_entry()?.clone();
        let backend = ws_registry
            .materialize(&default_entry.backend)
            .map_err(|e| {
                CrosstacheError::config(format!(
                    "workspace vault '{}' (backend '{}') is unavailable: {e}",
                    default_entry.alias, default_entry.backend
                ))
            })?;
        (default_entry.vault, default_entry.backend, backend)
    } else {
        let vault_name = resolve_vault_for_trait(&config, registry).await?;
        (
            vault_name,
            config.effective_backend_name().to_string(),
            reg.active_arc(),
        )
    };
    let secrets = backend.secrets().list_secrets(&vault_name, None).await?;

    if let Some(MvPlan::Filter {
        pattern,
        dest_prefix,
    }) = filter_plan
    {
        return execute_filter_mv(
            &backend,
            &backend_name,
            &config,
            &vault_name,
            secrets,
            pattern,
            dest_prefix,
            dry_run,
            yes,
        )
        .await;
    }

    let source = source.expect("checked above: exactly one of SOURCE/--filter is Some");
    let plan = parse_mv(&source, &dest)?;

    match plan {
        MvPlan::Secret {
            src_folder,
            src_name,
            dest_folder,
            dest_name,
        } => {
            execute_secret_mv(
                &backend,
                &backend_name,
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
                yes,
            )
            .await
        }
        MvPlan::Folder {
            src_prefix,
            dest_prefix,
        } => {
            execute_folder_mv(
                &backend,
                &backend_name,
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
        MvPlan::Filter { .. } => unreachable!("parse_mv never returns MvPlan::Filter"),
    }
}

/// Cross-vault `mv` via workspace aliases (Phase C Task 12): moves a single
/// secret between two ATTACHED workspace entries — potentially on different
/// backends — using copy(+tag-budget check)+delete, exactly mirroring the
/// existing `xv copy`/`xv move --from/--to` cross-vault machinery
/// (`rename_request_from_properties`, the #315 metadata-preservation path,
/// and `check_dest_tag_budget`'s destination-caps pre-check) rather than the
/// same-vault rename/re-folder logic above. Only a single secret is
/// supported today — folder/`--filter` moves across vaults are out of scope
/// for this task.
#[allow(clippy::too_many_arguments)]
async fn execute_cross_vault_alias_mv(
    ws_registry: &BackendRegistry,
    config: &Config,
    src_entry: &crate::workspace::WorkspaceEntry,
    dst_entry: &crate::workspace::WorkspaceEntry,
    src_path: &str,
    dst_path: &str,
    dry_run: bool,
    yes: bool,
) -> Result<()> {
    let plan = parse_mv(src_path, dst_path)?;
    let (src_folder, src_name, dest_folder, dest_name) = match plan {
        MvPlan::Secret {
            src_folder,
            src_name,
            dest_folder,
            dest_name,
        } => (src_folder, src_name, dest_folder, dest_name),
        MvPlan::Folder { .. } | MvPlan::Filter { .. } => {
            return Err(CrosstacheError::invalid_argument(
                "cross-vault mv via workspace aliases only supports a single secret today; \
                 folder and --filter moves across vaults are not yet supported",
            ));
        }
    };

    let src_backend = ws_registry.materialize(&src_entry.backend).map_err(|e| {
        CrosstacheError::config(format!(
            "workspace vault '{}' (backend '{}') is unavailable: {e}",
            src_entry.alias, src_entry.backend
        ))
    })?;
    let dst_backend = ws_registry.materialize(&dst_entry.backend).map_err(|e| {
        CrosstacheError::config(format!(
            "workspace vault '{}' (backend '{}') is unavailable: {e}",
            dst_entry.alias, dst_entry.backend
        ))
    })?;

    // Find the source secret by its qualified (folder, display-name) path —
    // same lookup `execute_secret_mv` uses for the same-vault case.
    let src_secrets = src_backend
        .secrets()
        .list_secrets(&src_entry.vault, None)
        .await?;
    let src_folder_norm = norm_folder(src_folder.as_deref());
    let found = src_secrets.iter().find(|s| {
        display_name(s) == src_name && norm_folder(s.folder.as_deref()) == src_folder_norm
    });
    let Some(found) = found else {
        let qualified_src = match &src_folder {
            Some(f) => format!("{f}/{src_name}"),
            None => src_name.clone(),
        };
        return Err(CrosstacheError::secret_not_found(format!(
            "{qualified_src} (in workspace vault '{}')",
            src_entry.alias
        )));
    };

    // Destination collision pre-check — before any mutation, mirroring
    // `execute_secret_mv`'s same-vault ordering. `xv mv` has no `--force`
    // anywhere (same-vault renames refuse collisions too), so cross-vault
    // alias `mv` never overwrites either — that's consistent, not a gap.
    // The message points at the one command that DOES overwrite:
    // `xv move --from/--to --force` (Bugbot MINOR fix).
    let dst_secrets = dst_backend
        .secrets()
        .list_secrets(&dst_entry.vault, None)
        .await?;
    if dest_collides(&dst_secrets, &dest_name) {
        return Err(CrosstacheError::conflict(format!(
            "secret '{dest_name}' already exists in workspace vault '{}' — delete it first, pick another name, \
             or use 'xv move --from {} --to {} --force' to overwrite",
            dst_entry.alias, src_entry.alias, dst_entry.alias
        )));
    }

    let qualified_src = match &src_folder {
        Some(f) => format!("{}:{f}/{src_name}", src_entry.alias),
        None => format!("{}:{src_name}", src_entry.alias),
    };
    let qualified_dst = match &dest_folder {
        Some(f) if !f.is_empty() => format!("{}:{f}/{dest_name}", dst_entry.alias),
        _ => format!("{}:{dest_name}", dst_entry.alias),
    };

    if dry_run {
        println!("{qualified_src} -> {qualified_dst}");
        output::info("1 secret would move across vaults (dry run)");
        return Ok(());
    }

    // Moving the reserved attachment-encryption-key secret out of its source
    // vault displaces it there — see the same-vault `execute_secret_mv`
    // guard above. Cross-vault `mv` has no `--force` (mirrors same-vault),
    // so always prompt.
    if !confirm_reserved_key_write(&found.name, yes, "Moving", "--yes")? {
        output::info("Aborted; secret not moved.");
        return Ok(());
    }

    let source_secret = src_backend
        .secrets()
        .get_secret(&src_entry.vault, &found.name, true)
        .await?;

    // #315 metadata preservation: reuse the same envelope-preserving request
    // builder the single-vault `xv copy`/`xv move --from/--to` path uses —
    // groups/note/tags/record envelopes ride along as-is; the folder is
    // overridden to the mv grammar's resolved destination folder.
    let mut secret_request =
        crate::backend::secret::rename_request_from_properties(&dest_name, &source_secret)?;
    secret_request.folder = dest_folder.clone();

    // Destination tag-budget check BEFORE any write.
    crate::cli::secret_ops::check_dest_tag_budget(dst_backend.as_ref(), &secret_request)?;

    dst_backend
        .secrets()
        .set_secret(&dst_entry.vault, secret_request)
        .await?;
    invalidate_trait_secret_cache(config, &dst_entry.backend, &dst_entry.vault);

    src_backend
        .secrets()
        .delete_secret(&src_entry.vault, &found.name)
        .await?;
    invalidate_trait_secret_cache(config, &src_entry.backend, &src_entry.vault);

    output::success(&format!("Moved '{qualified_src}' to '{qualified_dst}'"));
    Ok(())
}

/// Move/rename a single secret. Ordering is binding (spec 2026-07-02-fs-verbs):
/// find source → no-op check → collision pre-check → folder update →
/// rename. The collision pre-check happens before the folder update so a
/// doomed rename never leaves a half-applied folder change behind.
///
/// Takes an explicit `backend`/`backend_name` (rather than a
/// `BackendRegistry`) so callers can pass either `reg.active_arc()` (no
/// workspace) or a workspace entry's own resolved backend (Bugbot
/// BLOCKER/MAJOR fix, Phase C follow-up): every `mv` form must resolve the
/// SAME way `get`/`set`/etc. already do, never a separate config-level
/// vault when a workspace is attached.
#[allow(clippy::too_many_arguments)]
async fn execute_secret_mv(
    backend: &std::sync::Arc<dyn crate::backend::Backend>,
    backend_name: &str,
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
    yes: bool,
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

    // Renaming the reserved attachment-encryption-key secret away from its
    // well-known name displaces it — `xv attach`/`xv attachments` look it up
    // by that literal name, so every attachment in the vault becomes
    // unreadable. `mv` has no --force/--yes for a single-secret rename, so
    // always prompt.
    if dest_name != src_name && !confirm_reserved_key_write(&found.name, yes, "Renaming", "--yes")?
    {
        output::info("Aborted; secret not moved.");
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
        backend
            .secrets()
            .update_secret(vault_name, &src_name, request)
            .await?;
        invalidate_trait_secret_cache(config, backend_name, vault_name);
        folder_updated = true;
    }

    if dest_name != src_name {
        let rename_result = backend
            .secrets()
            .rename_secret(vault_name, &src_name, &dest_name)
            .await;
        // Rename may mutate state even when it errors (e.g. RenameIncomplete),
        // so invalidate unconditionally before inspecting the result.
        invalidate_trait_secret_cache(config, backend_name, vault_name);
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
    backend: &std::sync::Arc<dyn crate::backend::Backend>,
    backend_name: &str,
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
        if let Err(e) = backend
            .secrets()
            .update_secret(vault_name, name, request)
            .await
        {
            failures += 1;
            output::warn(&format!("failed to move '{old}' to '{new}': {e}"));
        }
    }
    invalidate_trait_secret_cache(config, backend_name, vault_name);
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

/// True if `moving` (secrets about to be re-foldered into `dest_prefix`)
/// would collide with a secret that already resides in `dest_prefix` and is
/// NOT itself one of the moving secrets. A filter move only rewrites the
/// `folder` tag (names never change), so the only place a collision can
/// arise is inside the destination folder itself — but a moving secret's
/// *display* name can still equal an unrelated occupant's *backend*
/// (sanitized) name, or vice versa, so the comparison mirrors
/// `dest_collides` (#302) exactly: for each moving secret, its display name
/// is checked against both the occupant's display name AND its backend
/// name (`display_name(occupant) == moving_display || occupant.name ==
/// moving_display`), not display-vs-display alone. Missing the
/// backend-name leg would let a mismatched-name pair (e.g. moving secret
/// displays as "x", occupant's raw backend name is "x") slip past this
/// pre-check and have downstream name resolution against the destination
/// folder resolve the wrong secret.
///
/// Note: `execute_folder_mv` (whole-folder moves) has no equivalent guard
/// today — this asymmetry is accepted for now as a candidate for future
/// alignment, not addressed here.
///
/// Returns the colliding display name for the error message.
fn dest_folder_collision<'a>(
    secrets: &'a [SecretSummary],
    moving: &[&'a SecretSummary],
    dest_prefix: Option<&str>,
) -> Option<&'a str> {
    let dest_norm = norm_folder(dest_prefix);
    let moving_backend_names: std::collections::HashSet<&str> =
        moving.iter().map(|s| s.name.as_str()).collect();

    for occupant in secrets {
        if moving_backend_names.contains(occupant.name.as_str()) {
            continue; // this secret is itself moving, not a foreign occupant
        }
        if norm_folder(occupant.folder.as_deref()) != dest_norm {
            continue;
        }
        let occupant_display = display_name(occupant);
        for m in moving {
            let moving_display = display_name(m);
            // Same predicate as `dest_collides`: match on either the
            // occupant's display name or its backend (sanitized) name.
            if occupant_display == moving_display || occupant.name == moving_display {
                return Some(occupant_display);
            }
        }
    }
    None
}

/// Execute an `xv mv --filter <GLOB> DEST` bulk folder move (2026-07-03
/// mv-filter design). Mirrors `execute_folder_mv`'s bulk machinery — count +
/// sample plan confirmation, `--yes` bypass, non-TTY refusal, `--dry-run`
/// preview, collision pre-check before any move, attempt-all /
/// report-failure-count partial-failure behavior — but the candidate set is
/// every secret in the vault matching `pattern` (either display or backend
/// name, same predicate as `ls`/`find --filter`) instead of a folder subtree.
#[allow(clippy::too_many_arguments)]
async fn execute_filter_mv(
    backend: &std::sync::Arc<dyn crate::backend::Backend>,
    backend_name: &str,
    config: &Config,
    vault_name: &str,
    secrets: Vec<SecretSummary>,
    pattern: String,
    dest_prefix: Option<String>,
    dry_run: bool,
    yes: bool,
) -> Result<()> {
    use crate::cli::helpers::confirm_proceed;
    use crate::utils::helpers::{compile_name_glob, glob_matches_either_name};

    let matcher = compile_name_glob(&pattern)?;

    let matched: Vec<&SecretSummary> = secrets
        .iter()
        .filter(|s| glob_matches_either_name(&matcher, &s.name, &s.original_name))
        .collect();

    if matched.is_empty() {
        return Err(CrosstacheError::invalid_argument(format!(
            "no secrets matched --filter '{pattern}'"
        )));
    }

    let dest_norm = norm_folder(dest_prefix.as_deref());
    let mut moving: Vec<&SecretSummary> = Vec::new();
    let mut skipped = 0usize;
    for s in &matched {
        let folder = s.folder.as_deref().unwrap_or("");
        if norm_folder(Some(folder)) == dest_norm {
            skipped += 1;
        } else {
            moving.push(s);
        }
    }

    let dest_label = dest_prefix
        .as_deref()
        .map_or("/".to_string(), |d| format!("{d}/"));

    // Collision pre-check — before any mutation, and before --dry-run even
    // returns, mirroring the ordering in `execute_secret_mv`.
    if let Some(occupant) = dest_folder_collision(&secrets, &moving, dest_prefix.as_deref()) {
        return Err(CrosstacheError::conflict(format!(
            "secret '{occupant}' already exists in '{dest_label}' — delete it first or pick another destination"
        )));
    }

    if moving.is_empty() {
        output::info(&format!(
            "{skipped} secret(s) already in '{dest_label}'; nothing to move"
        ));
        return Ok(());
    }

    // (old qualified path, new qualified path, name to call the API with)
    let moves: Vec<(String, String, String)> = moving
        .iter()
        .map(|s| {
            let folder = s.folder.as_deref().unwrap_or("");
            let name = display_name(s).to_string();
            let old_path = if folder.is_empty() {
                name.clone()
            } else {
                format!("{folder}/{name}")
            };
            let new_path = match &dest_prefix {
                Some(f) => format!("{f}/{name}"),
                None => name.clone(),
            };
            (old_path, new_path, name)
        })
        .collect();

    if dry_run {
        for (old, new, _) in &moves {
            println!("{old} -> {new}");
        }
        if skipped > 0 {
            output::info(&format!(
                "{skipped} secret(s) already in '{dest_label}' (skipped)"
            ));
        }
        output::info(&format!("{} secrets would move (dry run)", moves.len()));
        return Ok(());
    }

    eprintln!(
        "Moving {} secrets matching --filter '{pattern}' to '{dest_label}':",
        moves.len()
    );
    for (old, new, _) in moves.iter().take(10) {
        eprintln!("  {old} -> {new}");
    }
    if moves.len() > 10 {
        eprintln!("  ... ({} more; --dry-run to list all)", moves.len() - 10);
    }
    if skipped > 0 {
        eprintln!("  ({skipped} secret(s) already in '{dest_label}', skipped)");
    }
    if !confirm_proceed(yes, &format!("Move {} secrets?", moves.len()), "--yes")? {
        output::info("Aborted; nothing moved.");
        return Ok(());
    }

    let mut failures = 0usize;
    for (old, new, name) in &moves {
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
            folder: match &dest_prefix {
                Some(f) => FieldUpdate::Set(f.clone()),
                None => FieldUpdate::Clear,
            },
            replace_tags: false,
            replace_groups: false,
        };
        if let Err(e) = backend
            .secrets()
            .update_secret(vault_name, name, request)
            .await
        {
            failures += 1;
            output::warn(&format!("failed to move '{old}' to '{new}': {e}"));
        }
    }
    invalidate_trait_secret_cache(config, backend_name, vault_name);
    let moved = moves.len() - failures;
    if failures > 0 {
        return Err(CrosstacheError::unknown(format!(
            "moved {moved} of {} secrets; {failures} failed (see warnings above)",
            moves.len()
        )));
    }
    output::success(&format!(
        "Moved {moved} secrets matching --filter '{pattern}' to '{dest_label}'"
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
    // dest_folder_collision — `--filter` move collision pre-check.
    //
    // A filter move only rewrites the `folder` tag: matched secrets keep
    // their names, so a collision can only arise when a *different* secret
    // already resident in the destination folder shares a moving secret's
    // display or backend name. The local backend never diverges name from
    // original_name (see the comment above `mv_sanitized_name_rename_with_space`
    // in `tests/e2e_local_backend.rs`), so — same escape hatch as #326's
    // either-name matching — this is exercised at the unit level with
    // synthetic summaries whose name and original_name differ.
    // -----------------------------------------------------------------

    fn summary_full(name: &str, original_name: &str, folder: Option<&str>) -> SecretSummary {
        SecretSummary {
            name: name.to_string(),
            original_name: original_name.to_string(),
            note: None,
            folder: folder.map(String::from),
            groups: None,
            updated_on: String::new(),
            enabled: true,
            content_type: String::new(),
            tags: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn dest_folder_collision_matches_display_name_of_dest_occupant() {
        // Already in 'archive/': backend name "raw-o", displays as "test-x".
        let occupant = summary_full("raw-o", "test-x", Some("archive"));
        // Moving in from elsewhere: backend name "raw-m", also displays as
        // "test-x" — a different secret that would collide in 'archive/'.
        let moving_secret = summary_full("raw-m", "test-x", Some("other"));
        let secrets = vec![occupant, moving_secret.clone()];
        let moving = vec![&secrets[1]];

        let collision = dest_folder_collision(&secrets, &moving, Some("archive"));
        assert_eq!(collision, Some("test-x"));
    }

    #[test]
    fn dest_folder_collision_matches_moving_display_against_occupant_backend_name() {
        // Already in 'archive/': backend name "test-x" (the raw, sanitized
        // name), displays as an unrelated "pretty-occupant".
        let occupant = summary_full("test-x", "pretty-occupant", Some("archive"));
        // Moving in from elsewhere: displays as "test-x" — matches the
        // occupant's *backend* name, not its display name. This is the
        // Bugbot-flagged gap: a display-vs-display-only check would miss
        // this and let the bulk pre-check pass.
        let moving_secret = summary_full("raw-m", "test-x", Some("other"));
        let secrets = vec![occupant, moving_secret.clone()];
        let moving = vec![&secrets[1]];

        let collision = dest_folder_collision(&secrets, &moving, Some("archive"));
        assert_eq!(collision, Some("pretty-occupant"));
    }

    #[test]
    fn dest_folder_collision_no_overlap_passes() {
        // Same destination folder, but no name overlap on either side.
        let occupant = summary_full("raw-o", "test-x", Some("archive"));
        let moving_secret = summary_full("raw-m", "test-y", Some("other"));
        let secrets = vec![occupant, moving_secret.clone()];
        let moving = vec![&secrets[1]];

        assert!(dest_folder_collision(&secrets, &moving, Some("archive")).is_none());
    }

    #[test]
    fn dest_folder_collision_ignores_secrets_outside_dest_folder() {
        let elsewhere = summary_full("raw-o", "test-x", Some("not-dest"));
        let moving_secret = summary_full("raw-m", "test-x", Some("other"));
        let secrets = vec![elsewhere, moving_secret.clone()];
        let moving = vec![&secrets[1]];

        assert!(dest_folder_collision(&secrets, &moving, Some("archive")).is_none());
    }

    #[test]
    fn dest_folder_collision_ignores_the_moving_secret_itself() {
        // The secret already sitting in the destination IS one of the
        // moving secrets (already-in-dest case) — not a foreign occupant.
        let moving_secret = summary_full("raw-m", "test-x", Some("archive"));
        let secrets = vec![moving_secret.clone()];
        let moving = vec![&secrets[0]];

        assert!(dest_folder_collision(&secrets, &moving, Some("archive")).is_none());
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
                has_restore: true,
                has_purge: true,
                has_scheduled_purge: false,
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
        let backend_dyn: Arc<dyn Backend> = backend.clone();
        let config = Config::default();

        let secrets = vec![
            folder_summary("a", "app"),
            folder_summary("b", "app"),
            folder_summary("c", "app"),
        ];

        let result = execute_folder_mv(
            &backend_dyn,
            "local",
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

    // -----------------------------------------------------------------
    // Phase C Task 12: cross-vault mv/copy destination tag-budget check
    // -----------------------------------------------------------------

    /// A fake backend exposing Azure's real tag cap (`max_tags: Some(15)`)
    /// without needing real Azure credentials/network — mirrors
    /// `PartialFailBackend` above but only `capabilities()` matters here.
    struct AzureCapsBackend;

    #[async_trait::async_trait]
    impl SecretBackend for AzureCapsBackend {
        async fn set_secret(
            &self,
            _vault: &str,
            _request: crate::secret::manager::SecretRequest,
        ) -> std::result::Result<SecretProperties, BackendError> {
            panic!("set_secret must never be called: the tag-budget check must reject BEFORE any write");
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
            _name: &str,
            _request: SecretUpdateRequest,
        ) -> std::result::Result<SecretProperties, BackendError> {
            Err(BackendError::Unsupported("test backend".into()))
        }
    }

    #[async_trait::async_trait]
    impl Backend for AzureCapsBackend {
        fn name(&self) -> &'static str {
            "azure"
        }
        fn kind(&self) -> BackendKind {
            BackendKind::Azure
        }
        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities {
                has_vaults: true,
                has_file_storage: true,
                has_rbac: true,
                has_audit: true,
                has_versioning: true,
                has_soft_delete: true,
                has_restore: true,
                has_purge: true,
                has_scheduled_purge: true,
                has_secret_rotation: false,
                has_groups: true,
                has_folders: true,
                has_notes: true,
                has_expiry: true,
                max_secret_size: None,
                max_name_length: None,
                name_charset: NameCharset::Unrestricted,
                max_tags: Some(15),
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

    fn secret_request_with_tags(n: usize) -> crate::secret::manager::SecretRequest {
        let tags: std::collections::HashMap<String, String> = (0..n)
            .map(|i| (format!("tag{i}"), format!("v{i}")))
            .collect();
        crate::secret::manager::SecretRequest {
            name: "CREDS".to_string(),
            value: zeroize::Zeroizing::new("hunter2".to_string()),
            content_type: None,
            enabled: None,
            expires_on: None,
            not_before: None,
            tags: Some(tags),
            groups: None,
            note: None,
            folder: None,
        }
    }

    #[test]
    fn mv_alias_dest_tag_budget_checked_at_destination() {
        let dest = AzureCapsBackend;
        // 14 user tags + crosstache's 2 always-written metadata tags
        // (original_name/created_by) = 16 > Azure's 15-tag cap.
        let request = secret_request_with_tags(14);
        let err = crate::cli::secret_ops::check_dest_tag_budget(&dest, &request)
            .expect_err("must reject before any write when over the destination's tag budget");
        let msg = err.to_string();
        assert!(msg.contains("15"), "{msg}");
    }

    #[test]
    fn mv_alias_dest_tag_budget_allows_request_within_cap() {
        let dest = AzureCapsBackend;
        // 5 user tags + 2 reserved = 7, well within Azure's 15-tag cap.
        let request = secret_request_with_tags(5);
        crate::cli::secret_ops::check_dest_tag_budget(&dest, &request)
            .expect("must allow a request within the destination's tag budget");
    }

    /// A request with folder+note+groups+user tags landing EXACTLY at the
    /// destination's `max_tags` (Bugbot MINOR fix): pins the reserved-count
    /// math (`ALWAYS_WRITTEN_TAGS.len()` + groups/note/folder presence)
    /// against double-counting with `rename_request_from_properties`'s
    /// structured fields — folder/note/groups are counted ONCE each here,
    /// not once as a structured field AND again as a raw tag.
    fn secret_request_with_full_metadata(
        user_tag_count: usize,
    ) -> crate::secret::manager::SecretRequest {
        let tags: std::collections::HashMap<String, String> = (0..user_tag_count)
            .map(|i| (format!("tag{i}"), format!("v{i}")))
            .collect();
        crate::secret::manager::SecretRequest {
            name: "CREDS".to_string(),
            value: zeroize::Zeroizing::new("hunter2".to_string()),
            content_type: None,
            enabled: None,
            expires_on: None,
            not_before: None,
            tags: Some(tags),
            groups: Some(vec!["team-a".to_string()]),
            note: Some("important".to_string()),
            folder: Some("app".to_string()),
        }
    }

    #[test]
    fn mv_alias_dest_tag_budget_boundary_exact_cap_passes_one_over_fails() {
        let dest = AzureCapsBackend;
        // reserved = 2 always-written + groups(1) + note(1) + folder(1) = 5;
        // 10 user tags -> total 15 == Azure's cap: must be ALLOWED.
        let at_cap = secret_request_with_full_metadata(10);
        crate::cli::secret_ops::check_dest_tag_budget(&dest, &at_cap)
            .expect("exactly at the destination's tag cap must be allowed");

        // 11 user tags -> total 16 > cap: must be REJECTED before any write.
        let over_cap = secret_request_with_full_metadata(11);
        let err = crate::cli::secret_ops::check_dest_tag_budget(&dest, &over_cap)
            .expect_err("one over the destination's tag cap must be rejected before any write");
        assert!(err.to_string().contains("15"), "{err}");
    }
}
