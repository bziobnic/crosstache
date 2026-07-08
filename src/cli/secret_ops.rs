//! Secret command execution handlers.

use crate::backend::BackendCapabilities;
use crate::backend::{BackendKind, BackendRef, BackendRegistry};
use crate::cli::commands::{CharsetType, SecretWriteArgs, ShareCommands};
use crate::cli::helpers::{
    confirm_destructive, copy_to_clipboard, generate_random_value, mask_secrets,
    resolve_vault_for_trait, schedule_clipboard_clear, share_unsupported_error, use_trait_path,
};
use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::records::{
    encode_envelope, find_type, FieldDef, FieldKind, RecordType, FIELD_TAG_PREFIX,
    RECORD_CONTENT_TYPE, TYPE_TAG,
};
use crate::utils::format::OutputFormat;
use crate::utils::output;
use crate::utils::pagination::Pagination;
use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::sync::Arc;
use zeroize::Zeroizing;

/// Read a secret value from `reader`, preserving the input bytes exactly.
/// With `trim`, leading/trailing whitespace is stripped (the pre-v0.11.1
/// default, now opt-in via `--trim`).
fn read_secret_value<R: std::io::Read>(reader: &mut R, trim: bool) -> Result<String> {
    let mut buffer = String::new();
    reader.read_to_string(&mut buffer)?;
    if trim {
        Ok(buffer.trim().to_string())
    } else {
        Ok(buffer)
    }
}

fn read_secret_value_from_stdin(trim: bool) -> Result<String> {
    read_secret_value(&mut std::io::stdin(), trim)
}

/// Apply env-profile `group`/`folder` write-time defaults to `meta` in
/// place, when the caller didn't pass an explicit `--group`/`--folder`.
/// Shared by `xv set` (`execute_secret_set_direct`) and `xv gen --save`
/// (`save_generated_secret` in `system_ops.rs`) so both construct identical
/// requests from the same metadata flags via `SecretWriteArgs::to_secret_request`
/// — the "set and gen --save produce byte-identical requests" invariant.
/// CLI values always win; an explicit `--folder` (including one that
/// resolves to an empty value) short-circuits inside `resolve_folder`.
pub(crate) async fn apply_profile_write_defaults(
    meta: &mut SecretWriteArgs,
    config: &Config,
) -> Result<()> {
    if meta.group.is_empty() {
        if let Some(group) = config.resolve_group(None).await? {
            meta.group = vec![group];
        }
    }
    if meta.folder.is_none() {
        meta.folder = config.resolve_folder(None).await?;
    }
    Ok(())
}

/// Routes user-supplied `--field`/`--field-secret` pairs into metadata-tag
/// and envelope (secret) maps, per record-types plan Task 6:
/// - `--field name=value`: uses the type's declared kind when `name` is a
///   declared field; ad-hoc names default to metadata.
/// - `--field-secret name=value`: always routes to the envelope
///   (secret), overriding the type's declared kind if any.
/// - Either flag targeting the type's primary field is an error — the
///   primary value only ever arrives via `--value`/`--stdin`/prompt.
fn route_fields(
    record_type: &RecordType,
    fields: &[(String, String)],
    secret_fields: &[(String, String)],
) -> Result<(BTreeMap<String, String>, BTreeMap<String, String>)> {
    // Reject the same field name appearing more than once across
    // --field/--field-secret (or repeated within one flag) before doing
    // anything else. Two values for one field would otherwise silently
    // pick a winner depending on kind/insertion order — e.g. `--field
    // a=1 --field-secret a=2` would store `a` as both an f.* tag AND an
    // envelope entry, and `get --field a` would only ever see the
    // envelope one, silently ignoring the tag.
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (name, _) in fields.iter().chain(secret_fields.iter()) {
        if !seen.insert(name.as_str()) {
            return Err(CrosstacheError::config(format!(
                "field '{name}' was supplied more than once across --field/--field-secret; \
                 each field may only be set once"
            )));
        }
    }

    let mut metadata: BTreeMap<String, String> = BTreeMap::new();
    let mut secret: BTreeMap<String, String> = BTreeMap::new();

    for (name, val) in fields {
        if let Some(def) = record_type.field(name) {
            if def.primary {
                return Err(CrosstacheError::invalid_argument(format!(
                    "field '{name}' is the primary field of type '{}'; set it via 'xv update <name> \
                     <value>', 'xv update <name> --stdin', or 'xv rotate <name>', not --field",
                    record_type.name
                )));
            }
            match def.kind {
                FieldKind::Metadata => {
                    metadata.insert(name.clone(), val.clone());
                }
                FieldKind::Secret => {
                    secret.insert(name.clone(), val.clone());
                }
            }
        } else {
            metadata.insert(name.clone(), val.clone());
        }
    }

    for (name, val) in secret_fields {
        if let Some(def) = record_type.field(name) {
            if def.primary {
                return Err(CrosstacheError::invalid_argument(format!(
                    "field '{name}' is the primary field of type '{}'; set it via 'xv update <name> \
                     <value>', 'xv update <name> --stdin', or 'xv rotate <name>', not --field-secret",
                    record_type.name
                )));
            }
        }
        secret.insert(name.clone(), val.clone());
    }

    Ok((metadata, secret))
}

/// True when `name` is present in `metadata` or `secret` with a
/// non-blank value. Only meaningful for required fields: a non-required
/// field is free to keep an explicit empty value (an explicit empty is a
/// legitimate user choice there — mirrors how env-profile defaults treat
/// blank-as-absent per #320, applied here only to the required-ness
/// check, not to whether the field gets stored), but a required field
/// supplied as `--field name=` or `--field name=" "` isn't meaningfully
/// "set" and must be treated as missing — matching the interactive
/// prompt path, which already rejects a blank answer for a required
/// field.
fn has_non_blank_value(
    name: &str,
    metadata: &BTreeMap<String, String>,
    secret: &BTreeMap<String, String>,
) -> bool {
    metadata
        .get(name)
        .or_else(|| secret.get(name))
        .is_some_and(|v| !v.trim().is_empty())
}

/// Every required field (except the primary, which is always required and
/// supplied separately) that is missing — absent, or present with an
/// empty/whitespace-only value — from both maps, in declared order.
fn missing_required_fields<'a>(
    record_type: &'a RecordType,
    metadata: &BTreeMap<String, String>,
    secret: &BTreeMap<String, String>,
) -> Vec<&'a FieldDef> {
    record_type
        .fields
        .iter()
        .filter(|f| f.required && !f.primary)
        .filter(|f| !has_non_blank_value(&f.name, metadata, secret))
        .collect()
}

/// Ordered list of fields still needing a value, primary last. Extracted as
/// a pure function so the field-ordering rule (primary prompted last) is
/// unit-testable without a TTY.
fn prompt_plan<'a>(
    record_type: &'a RecordType,
    provided: &BTreeMap<String, String>,
) -> Vec<&'a FieldDef> {
    let mut non_primary: Vec<&FieldDef> = record_type
        .fields
        .iter()
        .filter(|f| !f.primary && !provided.contains_key(&f.name))
        .collect();
    if let Some(primary) = record_type.fields.iter().find(|f| f.primary) {
        non_primary.push(primary);
    }
    non_primary
}

/// Interactively prompts for every field in `prompt_plan`, splitting the
/// results into (metadata, secret, primary_value). Metadata fields accept
/// an empty answer unless required; secret fields are masked via
/// `rpassword`. Called only when the caller supplied no `--field`s and no
/// `--value`/`--stdin` on an interactive TTY.
/// (metadata fields, secret fields, primary field value)
type RecordFieldPlan = (BTreeMap<String, String>, BTreeMap<String, String>, String);

fn interactive_prompt_record_fields(
    record_type: &RecordType,
    already_provided: &BTreeMap<String, String>,
) -> Result<RecordFieldPlan> {
    use crate::utils::interactive::InteractivePrompt;

    let prompt = InteractivePrompt::new();
    let mut metadata = BTreeMap::new();
    let mut secret = BTreeMap::new();
    let mut primary_value = String::new();

    for field in prompt_plan(record_type, already_provided) {
        match field.kind {
            FieldKind::Metadata => {
                let label = if field.required {
                    format!("{} (required)", field.name)
                } else {
                    field.name.clone()
                };
                let answer = prompt.input_text(&label, None)?;
                if field.required && answer.trim().is_empty() {
                    return Err(CrosstacheError::config(format!(
                        "field '{}' is required for type '{}'",
                        field.name, record_type.name
                    )));
                }
                if !answer.is_empty() {
                    metadata.insert(field.name.clone(), answer);
                }
            }
            FieldKind::Secret => {
                let answer = rpassword::prompt_password(format!("{}: ", field.name))?;
                if field.required && answer.is_empty() {
                    return Err(CrosstacheError::config(format!(
                        "field '{}' is required for type '{}'",
                        field.name, record_type.name
                    )));
                }
                if field.primary {
                    primary_value = answer;
                } else if !answer.is_empty() {
                    secret.insert(field.name.clone(), answer);
                }
            }
        }
    }

    Ok((metadata, secret, primary_value))
}

/// Builds the `SecretRequest` for `xv set <name> --type <type>`: resolves
/// the type, routes `--field`/`--field-secret` (or runs the interactive
/// prompt when none were given and no `--value`/`--stdin`), enforces
/// required fields, checks the tag budget, and encodes the envelope. Fails
/// before any backend call on every validation error.
#[allow(clippy::too_many_arguments)]
async fn build_record_set_request(
    name: &str,
    value: Option<String>,
    stdin: bool,
    trim: bool,
    type_name: &str,
    fields: &[(String, String)],
    secret_fields: &[(String, String)],
    meta: &SecretWriteArgs,
    config: &Config,
    caps: BackendCapabilities,
    backend_kind: crate::backend::BackendKind,
) -> Result<crate::secret::manager::SecretRequest> {
    let types = config.resolve_record_types().await?;
    let Some(record_type) = find_type(&types, type_name) else {
        let mut known: Vec<&str> = types.iter().map(|t| t.name.as_str()).collect();
        known.sort_unstable();
        return Err(CrosstacheError::config(format!(
            "unknown type '{type_name}'. Known types: {}",
            known.join(", ")
        )));
    };

    // Reject any user `--tag` that collides with a reserved record tag
    // name, before any prompting or backend call. Applying `--tag` after
    // xv-type/f.* would silently overwrite the record's own bookkeeping
    // (e.g. `--tag xv-type=other` desyncs the type marker from the
    // envelope, breaking type resolution and plain `get`); rejecting is at
    // least as strict as the untyped `set` path, which already loses a
    // user-supplied `original_name`/`created_by` tag to the backend's own
    // unconditional overwrite (see `crate::backend::ALWAYS_WRITTEN_TAGS`).
    //
    // Also reject `groups`/`note`/`folder` — on the untyped `set` path
    // these keys don't error either, but they're not silently ignored:
    // `AzureSecretOperations::prepare_secret_request` (Azure's real write path)
    // copies `request.tags` (which would include a user `--tag note=y`)
    // into the tag map FIRST, then unconditionally re-inserts
    // `groups`/`note`/`folder` from the dedicated `--group`/`--note`/
    // `--folder` flags when those flags were passed — so `--note x --tag
    // note=y` deterministically keeps `x` (the dedicated flag always wins
    // over the same-named `--tag`), never `y`, and never errors. That
    // silent "last write wins" merge is exactly the desync class round 1
    // already rejected for xv-type/f.*/original_name/created_by, so the
    // record path is intentionally stricter here than the untyped path:
    // fail loud instead of silently picking a winner.
    for key in meta.tag.iter().map(|(k, _)| k.as_str()) {
        let collides = key == TYPE_TAG
            || key.starts_with(FIELD_TAG_PREFIX)
            || key == crate::backend::TAG_ORIGINAL_NAME
            || key == crate::backend::TAG_CREATED_BY
            || key == "groups"
            || key == "note"
            || key == "folder";
        if collides {
            return Err(CrosstacheError::config(format!(
                "--tag '{key}' collides with a reserved record tag name ({}, {FIELD_TAG_PREFIX}*, \
                 {}, {}, groups, note, folder); rename it or use --group/--note/--folder instead",
                TYPE_TAG,
                crate::backend::TAG_ORIGINAL_NAME,
                crate::backend::TAG_CREATED_BY
            )));
        }
    }

    let (metadata, mut secret_map, primary_value) =
        if fields.is_empty() && secret_fields.is_empty() && value.is_none() && !stdin {
            if std::io::stdin().is_terminal() {
                interactive_prompt_record_fields(record_type, &BTreeMap::new())?
            } else {
                return Err(CrosstacheError::config(format!(
                    "type '{type_name}' requires field values; pass --value/--stdin for the \
                     primary field and --field/--field-secret for the rest (no TTY available \
                     for interactive prompts)"
                )));
            }
        } else {
            let (metadata, secret_map) = route_fields(record_type, fields, secret_fields)?;
            // Fail before prompting for the primary value: a user
            // shouldn't be asked for a (possibly masked, hard-to-retype)
            // secret and only afterward be told a required metadata field
            // was missing.
            let missing = missing_required_fields(record_type, &metadata, &secret_map);
            if !missing.is_empty() {
                let names: Vec<&str> = missing.iter().map(|f| f.name.as_str()).collect();
                return Err(CrosstacheError::config(format!(
                    "type '{type_name}' is missing required field(s): {}",
                    names.join(", ")
                )));
            }
            let primary_value = if let Some(v) = value {
                v
            } else if stdin {
                read_secret_value_from_stdin(trim)?
            } else if std::io::stdin().is_terminal() {
                rpassword::prompt_password(format!(
                    "Enter value for '{}' (primary field of '{type_name}'): ",
                    record_type.primary().name
                ))?
            } else {
                // No TTY to prompt on: don't hang scripts/CI waiting on
                // stdin that will never produce input. Fail before write
                // with a clear, actionable message instead of an opaque
                // "empty primary" error further down.
                return Err(CrosstacheError::config(format!(
                    "type '{type_name}' requires a value for its primary field \
                     ('{}'), and no TTY is available to prompt for one; pass \
                     --value or --stdin",
                    record_type.primary().name
                )));
            };
            (metadata, secret_map, primary_value)
        };

    if primary_value.is_empty() {
        return Err(CrosstacheError::config(
            "primary field value cannot be empty",
        ));
    }

    let missing = missing_required_fields(record_type, &metadata, &secret_map);
    if !missing.is_empty() {
        let names: Vec<&str> = missing.iter().map(|f| f.name.as_str()).collect();
        return Err(CrosstacheError::config(format!(
            "type '{type_name}' is missing required field(s): {}",
            names.join(", ")
        )));
    }

    // Tag budget: reserved (backend-specific bookkeeping tags this write
    // will actually cause the active backend's `set_secret` to attach) +
    // f.* metadata fields + user --tag count. `predicted_reserved_tag_count`
    // is derived per-backend from what each backend's `set_secret` really
    // puts on the wire (see its doc comment) — a single universal count
    // would either under-count (missing a backend-specific tag, letting a
    // write pass this pre-check and still blow the real cap) or
    // over-count (assuming a tag every backend doesn't actually write,
    // falsely rejecting a write that would have succeeded).
    let reserved_count = crate::records::predicted_reserved_tag_count(
        backend_kind,
        true, // xv-type: always present on a record write
        !meta.group.is_empty(),
        meta.note.is_some(),
        meta.folder.is_some(),
        meta.expires.is_some(),
    );
    let field_tags: BTreeMap<String, String> = metadata
        .iter()
        .map(|(k, v)| (format!("{FIELD_TAG_PREFIX}{k}"), v.clone()))
        .collect();
    let user_tags: BTreeMap<String, String> = meta.tag.iter().cloned().collect();
    crate::records::check_tag_budget(&caps, reserved_count, &field_tags, &user_tags)?;

    // Build the envelope: secret fields + primary.
    secret_map.insert(record_type.primary().name.clone(), primary_value);
    let envelope_value = encode_envelope(&secret_map)?;

    // Tags: xv-type + f.* metadata fields + user --tag.
    let mut tags: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    tags.insert(TYPE_TAG.to_string(), record_type.name.clone());
    for (k, v) in &field_tags {
        tags.insert(k.clone(), v.clone());
    }
    for (k, v) in &user_tags {
        tags.insert(k.clone(), v.clone());
    }
    let _ = metadata; // folded into field_tags above; kept for clarity at the call site

    use crate::utils::datetime::parse_datetime_or_duration;
    let expires_on = match meta.expires.as_deref() {
        Some(s) => Some(parse_datetime_or_duration(s)?),
        None => None,
    };
    let not_before_on = match meta.not_before.as_deref() {
        Some(s) => Some(parse_datetime_or_duration(s)?),
        None => None,
    };

    Ok(crate::secret::manager::SecretRequest {
        name: name.to_string(),
        value: Zeroizing::new(envelope_value),
        content_type: Some(RECORD_CONTENT_TYPE.to_string()),
        enabled: Some(true),
        expires_on,
        not_before: not_before_on,
        tags: Some(tags),
        groups: meta.groups_opt(),
        note: meta.note.clone(),
        folder: meta.folder.clone(),
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_secret_set_direct(
    args: Vec<String>,
    stdin: bool,
    trim: bool,
    value: Option<String>,
    type_name: Option<String>,
    fields: Vec<(String, String)>,
    secret_fields: Vec<(String, String)>,
    meta: SecretWriteArgs,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    // Only `--expires` / `--not-before` are inspected directly here (to reject
    // them for bulk set); all other write-time metadata is applied uniformly via
    // `meta.to_secret_request`.
    let SecretWriteArgs {
        expires,
        not_before,
        ..
    } = meta.clone();

    // `--value` is only meaningful for a single secret; reject it alongside
    // bulk KEY=value arguments so the inline value can't be silently dropped.
    let is_bulk = args.len() > 1 || args.iter().any(|a| a.contains('='));
    if value.is_some() && is_bulk {
        return Err(CrosstacheError::invalid_argument(
            "--value can only be used when setting a single secret (not with KEY=value bulk args)",
        ));
    }
    if type_name.is_some() && is_bulk {
        return Err(CrosstacheError::invalid_argument(
            "--type can only be used when setting a single secret (not with KEY=value bulk args)",
        ));
    }

    // ── Trait-based path (non-Azure backends) ──────────────────────────
    if use_trait_path(registry) {
        // Apply env-profile `group`/`folder` write-time defaults when the
        // caller didn't pass an explicit `--group`/`--folder`. Shared with
        // `xv gen --save` via `apply_profile_write_defaults`.
        let mut meta = meta;
        apply_profile_write_defaults(&mut meta, &config).await?;

        if args.len() == 1 && !args[0].contains('=') && type_name.is_some() {
            // Typed record single-secret set.
            //
            // Workspace-aware resolution: no workspace attached ⇒ this
            // returns exactly (reg.active_arc(), vault_name, args[0]) —
            // byte-identical to the pre-workspace behavior. Writes never
            // search: an unqualified name always targets the default entry.
            let (backend, backend_name, vault_name, name) =
                crate::cli::helpers::resolve_workspace_or_default(
                    &args[0],
                    &config,
                    crate::workspace::TargetMode::Write,
                )
                .await?;
            let name = &name;
            let request = build_record_set_request(
                name,
                value.clone(),
                stdin,
                trim,
                type_name.as_deref().expect("checked Some above"),
                &fields,
                &secret_fields,
                &meta,
                &config,
                backend.capabilities(),
                backend.kind(),
            )
            .await?;
            let props = backend.secrets().set_secret(&vault_name, request).await?;
            output::success(&format!(
                "Successfully set record '{}' (type: {})",
                props.original_name,
                type_name.as_deref().unwrap_or("")
            ));
            println!("   Vault: {vault_name}");
            println!("   Version: {}", props.version);
            output::hint(&format!("Verify with 'xv get {}'", props.original_name));
            invalidate_trait_secret_cache(&config, &backend_name, &vault_name);
            return Ok(());
        } else if args.len() == 1 && !args[0].contains('=') {
            // Single secret set. Same workspace-aware resolution as above.
            let (backend, backend_name, vault_name, name) =
                crate::cli::helpers::resolve_workspace_or_default(
                    &args[0],
                    &config,
                    crate::workspace::TargetMode::Write,
                )
                .await?;
            let name = &name;
            let secret_value = if let Some(v) = value.clone() {
                v
            } else if stdin {
                read_secret_value_from_stdin(trim)?
            } else {
                rpassword::prompt_password(format!("Enter value for secret '{name}': "))?
            };
            if secret_value.is_empty() {
                return Err(CrosstacheError::config("Secret value cannot be empty"));
            }
            // Build the request via the shared helper so `set` and `gen --save`
            // construct identical requests from the same metadata flags.
            let request = meta.to_secret_request(name, Zeroizing::new(secret_value))?;
            let props = backend.secrets().set_secret(&vault_name, request).await?;
            output::success(&format!(
                "Successfully set secret '{}'",
                props.original_name
            ));
            println!("   Vault: {vault_name}");
            println!("   Version: {}", props.version);
            output::hint(&format!("Verify with 'xv get {}'", props.original_name));
            invalidate_trait_secret_cache(&config, &backend_name, &vault_name);
            return Ok(());
        } else {
            // Bulk set
            if stdin {
                return Err(CrosstacheError::invalid_argument(
                    "--stdin cannot be used with bulk set operation",
                ));
            }
            if expires.is_some() || not_before.is_some() {
                return Err(CrosstacheError::invalid_argument(
                    "--expires and --not-before cannot be used with bulk set operation",
                ));
            }
            let pairs = parse_bulk_set_args(args)?;
            output::step(&format!("Setting {} secret(s)...", pairs.len()));
            let mut success_count = 0usize;
            let mut error_count = 0usize;
            // (backend, vault) pairs actually written to, for cache
            // invalidation below — a bulk set can span more than one
            // workspace entry when individual keys are alias-qualified
            // (`work:KEY=value`), so a single `vault_name` from the top of
            // this branch is no longer sufficient once a workspace is
            // attached. The backend name travels alongside the vault name
            // (not just `config.effective_backend_name()`) since two
            // workspace entries can share a vault NAME on different
            // backends — invalidating by vault name alone would target the
            // wrong `(backend, vault)` cache directory.
            let mut touched_vaults: std::collections::HashSet<(String, String)> =
                std::collections::HashSet::new();
            for (key, value) in pairs {
                // Workspace-aware resolution per key (BLOCKER fix): each
                // bulk pair is resolved independently so `alias:KEY=value`
                // qualification works, and an unqualified key always lands
                // in the workspace's default vault — never searched, same
                // contract as the single-secret path above. No workspace
                // attached ⇒ every key resolves to (reg.active_arc(),
                // vault_name, key) exactly as before.
                let (backend, backend_name, key_vault_name, resolved_key) =
                    match crate::cli::helpers::resolve_workspace_or_default(
                        &key,
                        &config,
                        crate::workspace::TargetMode::Write,
                    )
                    .await
                    {
                        Ok(resolved) => resolved,
                        Err(e) => {
                            output::warn(&format!("  ✗ {key}: {e}"));
                            error_count += 1;
                            continue;
                        }
                    };
                // Build each request via the shared helper so bulk set applies
                // the same write-time metadata (--group/--note/--folder/--tag)
                // as the single-secret path. (--expires/--not-before are rejected
                // for bulk above, so they're always None here.)
                let request = meta.to_secret_request(&resolved_key, Zeroizing::new(value))?;
                match backend.secrets().set_secret(&key_vault_name, request).await {
                    Ok(props) => {
                        output::success(&format!("  ✓ {}", props.original_name));
                        success_count += 1;
                        touched_vaults.insert((backend_name, key_vault_name));
                    }
                    Err(e) => {
                        output::warn(&format!("  ✗ {key}: {e}"));
                        error_count += 1;
                    }
                }
            }
            for (backend_name, v) in &touched_vaults {
                invalidate_trait_secret_cache(&config, backend_name, v);
            }
            if error_count > 0 {
                output::warn(&format!(
                    "Bulk set complete: {success_count} succeeded, {error_count} failed"
                ));
                // Any failed write must surface as a non-zero exit so scripts
                // and CI don't treat a partial failure as success.
                return Err(CrosstacheError::unknown(format!(
                    "{error_count} of {} secret(s) failed to set",
                    success_count + error_count
                )));
            }
            output::success(&format!(
                "Bulk set complete: {success_count} succeeded, {error_count} failed"
            ));
            return Ok(());
        }
    }

    Err(CrosstacheError::config(
        "No backend registry available. Run 'xv config show' to check your configuration.",
    ))
}

/// Parses a record's envelope, failing loud (never returning raw JSON as if
/// it were a value) when the content type says "record" but the value
/// isn't a valid envelope.
fn parse_record_envelope_or_fail(
    name: &str,
    content_type: &str,
    value: &str,
) -> Result<BTreeMap<String, String>> {
    crate::records::parse_envelope(value).map_err(|e| {
        CrosstacheError::config(format!(
            "secret '{name}' is marked as a record (content-type: {content_type}) but its \
             value is not a valid record envelope: {e}"
        ))
    })
}

/// All field names a record exposes: envelope keys (secret fields) union
/// `f.*` tag names (metadata fields), for "unknown field" error messages.
fn record_field_names(
    envelope: &BTreeMap<String, String>,
    tags: &std::collections::HashMap<String, String>,
) -> Vec<String> {
    let mut names: Vec<String> = envelope.keys().cloned().collect();
    for key in tags.keys() {
        if let Some(field) = key.strip_prefix(FIELD_TAG_PREFIX) {
            names.push(field.to_string());
        }
    }
    names.sort();
    names.dedup();
    names
}

/// Looks up one field's value: envelope (secret fields) first, then the
/// `f.<name>` tag (metadata fields).
fn lookup_record_field<'a>(
    field: &str,
    envelope: &'a BTreeMap<String, String>,
    tags: &'a std::collections::HashMap<String, String>,
) -> Option<&'a str> {
    if let Some(v) = envelope.get(field) {
        return Some(v.as_str());
    }
    tags.get(&format!("{FIELD_TAG_PREFIX}{field}"))
        .map(|s| s.as_str())
}

/// Decides the clipboard success message and whether to schedule an
/// auto-clear for `xv get --field`, mirroring plain `get`'s clipboard
/// handling for secret-kind fields exactly (same message shape, same
/// `schedule_clipboard_clear` call when `timeout > 0`). Metadata-kind
/// fields intentionally never schedule a clear: they're listable without
/// fetching the secret at all (e.g. via `--record`, or a future `ls` field
/// lift), so treating a clipboard copy of one as equally sensitive as a
/// secret buys nothing. Extracted as a pure function so this branching is
/// unit-testable without a real clipboard.
fn field_clipboard_outcome(
    name: &str,
    field_name: &str,
    is_secret_field: bool,
    clipboard_timeout: u64,
) -> (String, bool) {
    if is_secret_field && clipboard_timeout > 0 {
        (
            format!(
                "Field '{field_name}' of '{name}' copied to clipboard (auto-clears in {clipboard_timeout}s)"
            ),
            true,
        )
    } else {
        (
            format!("Field '{field_name}' of '{name}' copied to clipboard"),
            false,
        )
    }
}

/// Resolves the value `xv` should hand back for a secret reference: an
/// explicit field's value when `field` is `Some`, or the record's primary
/// field (mirroring plain `get`'s compatibility contract) when `field` is
/// `None` and the secret is a typed record. An untyped secret with
/// `field: None` returns its value unchanged; an untyped secret with an
/// explicit field is an error.
///
/// Shared by `get`'s plain/`--field` read paths and `xv inject`'s
/// `{{ secret:name.field }}` / `xv://vault/name#field` grammar (record-types
/// plan Task 12) so field-read semantics can't drift between the two
/// commands.
fn record_field_value(
    name: &str,
    secret: &crate::secret::manager::SecretProperties,
    field: Option<&str>,
    types: &[RecordType],
) -> Result<Zeroizing<String>> {
    let is_rec = crate::records::is_record(&secret.content_type);
    let raw_value = secret.value.as_deref().map(|s| s.as_str()).unwrap_or("");

    if let Some(field_name) = field {
        if !is_rec {
            return Err(CrosstacheError::config(format!(
                "secret '{name}' is not a typed record (value is not marked {}); field access \
                 ('.{field_name}') only applies to typed records.",
                crate::records::RECORD_CONTENT_TYPE
            )));
        }
        let envelope = parse_record_envelope_or_fail(name, &secret.content_type, raw_value)?;
        let Some(v) = lookup_record_field(field_name, &envelope, &secret.tags) else {
            let known = record_field_names(&envelope, &secret.tags);
            return Err(CrosstacheError::config(format!(
                "secret '{name}' has no field '{field_name}'. Known fields: {}",
                known.join(", ")
            )));
        };
        return Ok(Zeroizing::new(v.to_string()));
    }

    if !is_rec {
        return match &secret.value {
            Some(v) => Ok(v.clone()),
            None => Err(CrosstacheError::config(format!(
                "secret '{name}' resolved but has no value"
            ))),
        };
    }

    let envelope = parse_record_envelope_or_fail(name, &secret.content_type, raw_value)?;
    let type_name = secret.tags.get(TYPE_TAG).cloned().unwrap_or_default();
    let Some(record_type) = find_type(types, &type_name) else {
        return Err(CrosstacheError::config(format!(
            "secret '{name}' has type '{type_name}', which has no resolvable type definition \
             (check your [types.*] config). Its primary field can't be determined; reference a \
             specific field, e.g. via 'xv get {name} --field <name>' or 'xv get {name} --record', \
             to access this record's raw fields."
        )));
    };
    let primary_name = &record_type.primary().name;
    let Some(primary_value) = envelope.get(primary_name) else {
        return Err(CrosstacheError::config(format!(
            "secret '{name}' is missing its primary field '{primary_name}' in the record envelope"
        )));
    };
    Ok(Zeroizing::new(primary_value.clone()))
}

/// Resolves record types on first need, caching the outcome (success or
/// failure) so it happens at most once per command. `xv run`/`xv inject`
/// iterate a whole selection of secrets that may be entirely untyped, so
/// type resolution must be lazy: an all-untyped selection must succeed
/// exactly as before the record-types feature, never failing because of an
/// unrelated broken `[types.*]` config block that no referenced secret
/// actually uses. Callers should only invoke this when a fetched secret is
/// actually a record (`crate::records::is_record`); an untyped secret
/// should never trigger it at all. `CrosstacheError` isn't `Clone`, so the
/// error path is cached as a `String` and re-wrapped on each cached lookup.
async fn resolve_types_lazily(
    cache: &mut Option<std::result::Result<Vec<RecordType>, String>>,
    config: &Config,
) -> Result<Vec<RecordType>> {
    if cache.is_none() {
        *cache = Some(
            config
                .resolve_record_types()
                .await
                .map_err(|e| e.to_string()),
        );
    }
    match cache.as_ref().expect("just populated above") {
        Ok(types) => Ok(types.clone()),
        Err(msg) => Err(CrosstacheError::config(msg.clone())),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_secret_get_direct(
    name: &str,
    raw: bool,
    version: Option<String>,
    field: Option<String>,
    record: bool,
    format: OutputFormat,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    // ── Trait-based path (non-Azure backends) ──────────────────────────
    if use_trait_path(registry) {
        // Workspace-aware resolution: no workspace attached ⇒ this returns
        // exactly (reg.active_arc(), resolve_vault_for_trait(...), name) —
        // byte-identical to the pre-workspace behavior.
        let (backend, _backend_name, vault_name, name) =
            crate::cli::helpers::resolve_workspace_or_default(
                name,
                &config,
                crate::workspace::TargetMode::Read,
            )
            .await?;
        let name = name.as_str();

        let secret = if let Some(ref ver) = version {
            backend
                .secrets()
                .get_secret_version(&vault_name, name, ver, true)
                .await?
        } else {
            backend
                .secrets()
                .get_secret(&vault_name, name, true)
                .await?
        };

        let is_rec = crate::records::is_record(&secret.content_type);
        // Only resolved when the secret is actually a record: an untyped
        // secret's `get` must never fail because of an unrelated broken
        // `[types.*]` config block (byte-identical-everywhere guarantee).
        let types = if is_rec {
            config.resolve_record_types().await?
        } else {
            Vec::new()
        };

        // ── `--record`: full record view, all fields, requested format ──
        if record {
            if !is_rec {
                return Err(CrosstacheError::config(format!(
                    "secret '{name}' is not a typed record (value is not marked {}); \
                     --record only applies to typed records. Use 'xv update {name} --type <type>' \
                     to convert it.",
                    crate::records::RECORD_CONTENT_TYPE
                )));
            }
            let value = secret.value.as_deref().map(|s| s.as_str()).unwrap_or("");
            let envelope = parse_record_envelope_or_fail(name, &secret.content_type, value)?;
            let mut all_fields: std::collections::BTreeMap<String, String> = envelope.clone();
            for (k, v) in &secret.tags {
                if let Some(f) = k.strip_prefix(FIELD_TAG_PREFIX) {
                    all_fields.insert(f.to_string(), v.clone());
                }
            }
            let type_name = secret.tags.get(TYPE_TAG).cloned().unwrap_or_default();
            let resolved = format.resolve_for_stdout();
            let body = serde_json::json!({
                "name": name,
                "type": type_name,
                "fields": all_fields,
            });
            match resolved {
                OutputFormat::Json => println!(
                    "{}",
                    serde_json::to_string_pretty(&body).map_err(|e| {
                        CrosstacheError::serialization(format!("JSON serialization failed: {e}"))
                    })?
                ),
                OutputFormat::Yaml => println!(
                    "{}",
                    serde_yaml::to_string(&body).map_err(|e| {
                        CrosstacheError::serialization(format!("YAML serialization failed: {e}"))
                    })?
                ),
                _ => {
                    println!("{name}  (type: {type_name})");
                    for (k, v) in &all_fields {
                        println!("  {k}: {v}");
                    }
                }
            }
            return Ok(());
        }

        // ── `--field NAME`: one field, either kind ──
        if let Some(field_name) = field {
            if !is_rec {
                return Err(CrosstacheError::config(format!(
                    "secret '{name}' is not a typed record (value is not marked {}); \
                     --field only applies to typed records. Use 'xv update {name} --type <type>' \
                     to convert it.",
                    crate::records::RECORD_CONTENT_TYPE
                )));
            }
            let field_value = record_field_value(name, &secret, Some(&field_name), &types)?;
            // A field found in the envelope is secret-kind; one found only
            // via the f.<name> tag is metadata-kind (listable without
            // fetching the secret in the first place). `record_field_value`
            // already validated the field exists, so re-parsing the
            // envelope here is purely for this classification, not for the
            // lookup/error-message logic (which lives in one place now).
            let value = secret.value.as_deref().map(|s| s.as_str()).unwrap_or("");
            let envelope = parse_record_envelope_or_fail(name, &secret.content_type, value)?;
            let is_secret_field = envelope.contains_key(&field_name);

            if raw {
                print!("{}", field_value.as_str());
            } else {
                match copy_to_clipboard(&field_value) {
                    Ok(()) => {
                        let (message, schedule_clear) = field_clipboard_outcome(
                            name,
                            &field_name,
                            is_secret_field,
                            config.clipboard_timeout,
                        );
                        output::success(&message);
                        if schedule_clear {
                            schedule_clipboard_clear(config.clipboard_timeout);
                        }
                    }
                    Err(e) => {
                        output::warn(&format!("Failed to copy to clipboard: {e}"));
                        eprintln!(
                            "Use 'xv get {name} --field {field_name} --raw' to print the value to stdout instead."
                        );
                    }
                }
            }
            return Ok(());
        }

        // ── Plain `get`: primary field for records, untouched for untyped ──
        // Routed through `record_field_value` for the record case so the
        // primary/field extraction logic lives in exactly one place, shared
        // with `--field` above and `xv inject`'s record grammar.
        let effective_value: Option<Zeroizing<String>> = if is_rec {
            Some(record_field_value(name, &secret, None, &types)?)
        } else {
            secret.value
        };

        if raw {
            if let Some(value) = effective_value {
                print!("{}", value.as_str());
            }
        } else if let Some(ref value) = effective_value {
            match copy_to_clipboard(value) {
                Ok(()) => {
                    let timeout = config.clipboard_timeout;
                    if timeout > 0 {
                        output::success(&format!(
                            "Secret '{name}' copied to clipboard (auto-clears in {timeout}s)"
                        ));
                        schedule_clipboard_clear(timeout);
                    } else {
                        output::success(&format!("Secret '{name}' copied to clipboard"));
                    }
                }
                Err(e) => {
                    output::warn(&format!("Failed to copy to clipboard: {e}"));
                    eprintln!("Use 'xv get {name} --raw' to print the value to stdout instead.");
                }
            }
        } else {
            output::warn(&format!("Secret '{name}' has no value"));
        }
        return Ok(());
    }

    Err(CrosstacheError::config(
        "No backend registry available. Run 'xv config show' to check your configuration.",
    ))
}

fn secret_summary_matches_group(
    secret: &crate::secret::manager::SecretSummary,
    group: &str,
) -> bool {
    secret
        .groups
        .as_ref()
        .map(|groups| groups.split(',').any(|grp| grp.trim() == group))
        .unwrap_or(false)
}

fn trait_secret_cache_key(backend_name: &str, vault_name: &str) -> crate::cache::CacheKey {
    crate::cache::CacheKey::SecretsList {
        backend: backend_name.to_string(),
        vault_name: vault_name.to_string(),
    }
}

/// Invalidate the secrets-list cache entry for `(backend_name, vault_name)`.
///
/// `backend_name` must be the REGISTRY name of the backend actually written
/// to — the resolved `Backend::name()` (or, with an attached workspace, the
/// entry's `backend` field) — not necessarily `config.effective_backend_name()`,
/// which can differ once a workspace write targets a non-default entry. A
/// mismatch here would invalidate the wrong `(backend, vault)` cache
/// directory, leaving a stale cached list behind after a write.
pub(crate) fn invalidate_trait_secret_cache(config: &Config, backend_name: &str, vault_name: &str) {
    let cache_manager = crate::cache::CacheManager::from_config(config);
    cache_manager.invalidate(&trait_secret_cache_key(backend_name, vault_name));
}

fn filter_secret_summaries_for_display(
    mut secrets: Vec<crate::secret::manager::SecretSummary>,
    group: Option<&str>,
    all: bool,
) -> Vec<crate::secret::manager::SecretSummary> {
    if !all {
        secrets.retain(|s| s.enabled);
    }
    if let Some(g) = group {
        secrets.retain(|s| secret_summary_matches_group(s, g));
    }
    secrets
}

/// Fold summaries into (group → member count), tokenizing the comma-separated
/// `groups` tag exactly like `secret_summary_matches_group`. A group repeated
/// within one secret counts that secret once.
fn derive_group_rows(secrets: &[crate::secret::manager::SecretSummary]) -> Vec<GroupListRow> {
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for s in secrets {
        let Some(groups) = s.groups.as_deref() else {
            continue;
        };
        let mut seen = std::collections::HashSet::new();
        for g in groups.split(',') {
            let g = g.trim();
            if !g.is_empty() && seen.insert(g) {
                *counts.entry(g.to_string()).or_insert(0) += 1;
            }
        }
    }
    counts
        .into_iter()
        .map(|(group, secrets)| GroupListRow { group, secrets })
        .collect()
}

pub(crate) async fn execute_group_command(
    command: crate::cli::commands::GroupCommands,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    match command {
        crate::cli::commands::GroupCommands::List { no_cache } => {
            execute_group_list(no_cache, config, registry).await
        }
    }
}

async fn execute_group_list(
    no_cache: bool,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    use crate::cache::CacheManager;
    use crate::utils::format::TableFormatter;

    if !use_trait_path(registry) {
        return Err(CrosstacheError::config(
            "No backend registry available. Run 'xv config show' to check your configuration.",
        ));
    }
    let reg = registry.expect("use_trait_path guarantees Some");
    let vault_name = resolve_vault_for_trait(&config, registry).await?;

    // Same fetch-or-cache flow as `xv ls` (shared CacheKey::SecretsList
    // dataset). Cache key uses `config.effective_backend_name()` (the
    // registry/config name), NOT `reg.active().name()` (the backend kind) —
    // see `resolve_workspace_or_default`'s doc comment for why the two
    // diverge whenever a named backend is active.
    let cache_manager = CacheManager::from_config(&config);
    let cache_key = trait_secret_cache_key(config.effective_backend_name(), &vault_name);
    let use_cache = cache_manager.is_enabled() && !no_cache;
    let cached = if use_cache {
        cache_manager.get::<Vec<crate::secret::manager::SecretSummary>>(&cache_key)
    } else {
        None
    };
    let secrets = match cached {
        Some(secrets) => secrets,
        None => {
            let fetched = reg
                .active()
                .secrets()
                .list_secrets(&vault_name, None)
                .await?;
            if use_cache {
                cache_manager.set(&cache_key, &fetched);
            }
            fetched
        }
    };

    let filtered = filter_secret_summaries_for_display(secrets, None, false);
    let rows = derive_group_rows(&filtered);
    let fmt = config.runtime_output_format;
    let human = matches!(
        fmt,
        OutputFormat::Table | OutputFormat::Plain | OutputFormat::Raw
    );

    if rows.is_empty() && human {
        crate::utils::output::info(&crate::utils::list_output::empty_state_message(
            "groups",
            Some(&format!("vault '{vault_name}'")),
        ));
        return Ok(());
    }

    let formatter = TableFormatter::new(
        fmt,
        config.no_color,
        config.template.clone(),
        config.runtime_columns.clone(),
    );
    println!("{}", formatter.format_table(&rows)?);
    if human {
        println!(
            "{}",
            crate::utils::list_output::count_label(
                rows.len(),
                rows.len(),
                "group",
                "groups",
                Some(&format!("vault '{vault_name}'")),
                false,
            )
        );
    }
    Ok(())
}

const SECRET_LIST_NOTE_WRAP_WIDTH: usize = 40;

/// Synthetic, in-memory-only tag key used to carry a workspace entry's alias
/// through the existing `ls_view::scope_secrets`/sort/render pipeline for
/// union `ls` (multi-vault workspaces plan, Phase B Task 7). Never written
/// to a backend and never surfaced under this name in output — every
/// display path below reads it via [`workspace_alias_of`] and either renders
/// a VAULT column (table/long views, JSON) or strips it before prefixing
/// (grid view, `--names-only`). Reusing `SecretSummary.tags` (rather than a
/// parallel `Vec<(String, SecretSummary)>`) lets union `ls` share every
/// existing folder-scoping/sort/render helper unchanged.
const WORKSPACE_ALIAS_TAG: &str = "__xv_workspace_alias";

/// Companion to [`WORKSPACE_ALIAS_TAG`] carrying the entry's real vault name,
/// so the long (`-l`) view can show the actual vault behind an alias.
const WORKSPACE_VAULT_TAG: &str = "__xv_workspace_vault";

/// Read back a [`WORKSPACE_ALIAS_TAG`] value stashed by the union `ls` path.
/// `None` for ordinary (non-workspace or single-vault) listings.
fn workspace_alias_of(s: &crate::secret::manager::SecretSummary) -> Option<&str> {
    s.tags.get(WORKSPACE_ALIAS_TAG).map(String::as_str)
}

/// Read back the [`WORKSPACE_VAULT_TAG`] value (the entry's real vault name).
/// `None` for ordinary (non-workspace or single-vault) listings.
fn workspace_vault_of(s: &crate::secret::manager::SecretSummary) -> Option<&str> {
    s.tags.get(WORKSPACE_VAULT_TAG).map(String::as_str)
}

#[derive(Debug, Clone, serde::Serialize, tabled::Tabled)]
struct SecretListDisplayRow {
    #[tabled(rename = "Name")]
    name: String,
    #[tabled(rename = "Note")]
    note: String,
    #[tabled(rename = "Folder")]
    folder: String,
    #[tabled(rename = "Groups")]
    groups: String,
    #[tabled(rename = "Updated")]
    updated_on: String,
}

/// Row shape for `xv group list`.
#[derive(Debug, Clone, serde::Serialize, tabled::Tabled)]
struct GroupListRow {
    #[tabled(rename = "Group")]
    group: String,
    #[tabled(rename = "Secrets")]
    secrets: usize,
}

/// Table row shape used only when at least one listed secret is a typed
/// record (record-types plan Task 10) — adds a `Type` column. Kept as a
/// separate struct (rather than always adding the column to
/// `SecretListDisplayRow`) so an untyped-only listing's table output stays
/// byte-identical to pre-Task-10 behavior.
#[derive(Debug, Clone, serde::Serialize, tabled::Tabled)]
struct SecretListDisplayRowTyped {
    #[tabled(rename = "Name")]
    name: String,
    #[tabled(rename = "Type")]
    record_type: String,
    #[tabled(rename = "Note")]
    note: String,
    #[tabled(rename = "Folder")]
    folder: String,
    #[tabled(rename = "Groups")]
    groups: String,
    #[tabled(rename = "Updated")]
    updated_on: String,
}

fn format_secret_list_rows_for_human(
    secrets: &[crate::secret::manager::SecretSummary],
) -> Vec<SecretListDisplayRow> {
    secrets
        .iter()
        .map(|secret| SecretListDisplayRow {
            name: secret.name.clone(),
            note: secret
                .note
                .as_deref()
                .map(|note| wrap_text_to_width(note, SECRET_LIST_NOTE_WRAP_WIDTH))
                .unwrap_or_default(),
            folder: secret.folder.clone().unwrap_or_default(),
            groups: secret.groups.clone().unwrap_or_default(),
            updated_on: crate::cli::ls_view::date_portion_for_display(&secret.updated_on),
        })
        .collect()
}

fn format_secret_list_rows_for_human_typed(
    secrets: &[crate::secret::manager::SecretSummary],
) -> Vec<SecretListDisplayRowTyped> {
    secrets
        .iter()
        .map(|secret| SecretListDisplayRowTyped {
            name: secret.name.clone(),
            record_type: secret.tags.get(TYPE_TAG).cloned().unwrap_or_default(),
            note: secret
                .note
                .as_deref()
                .map(|note| wrap_text_to_width(note, SECRET_LIST_NOTE_WRAP_WIDTH))
                .unwrap_or_default(),
            folder: secret.folder.clone().unwrap_or_default(),
            groups: secret.groups.clone().unwrap_or_default(),
            updated_on: crate::cli::ls_view::date_portion_for_display(&secret.updated_on),
        })
        .collect()
}

/// Table row shape for union `ls` (workspace with ≥2 attached vaults) —
/// adds a `Vault` column naming the alias each row came from. Kept as its
/// own struct (rather than always adding the column to
/// `SecretListDisplayRow`) so single-vault/no-workspace table output stays
/// byte-identical (spec §Read semantics: "VAULT column ONLY when the
/// workspace has ≥2 entries").
#[derive(Debug, Clone, serde::Serialize, tabled::Tabled)]
struct SecretListDisplayRowVault {
    #[tabled(rename = "Name")]
    name: String,
    #[tabled(rename = "Vault")]
    vault: String,
    #[tabled(rename = "Note")]
    note: String,
    #[tabled(rename = "Folder")]
    folder: String,
    #[tabled(rename = "Groups")]
    groups: String,
    #[tabled(rename = "Updated")]
    updated_on: String,
}

/// Union `ls` variant of `SecretListDisplayRowTyped`: both the `Vault` and
/// `Type` columns together, used when a multi-entry workspace's merged
/// listing contains at least one typed record.
#[derive(Debug, Clone, serde::Serialize, tabled::Tabled)]
struct SecretListDisplayRowVaultTyped {
    #[tabled(rename = "Name")]
    name: String,
    #[tabled(rename = "Vault")]
    vault: String,
    #[tabled(rename = "Type")]
    record_type: String,
    #[tabled(rename = "Note")]
    note: String,
    #[tabled(rename = "Folder")]
    folder: String,
    #[tabled(rename = "Groups")]
    groups: String,
    #[tabled(rename = "Updated")]
    updated_on: String,
}

fn format_secret_list_rows_for_human_vault(
    secrets: &[crate::secret::manager::SecretSummary],
) -> Vec<SecretListDisplayRowVault> {
    secrets
        .iter()
        .map(|secret| SecretListDisplayRowVault {
            name: secret.name.clone(),
            vault: workspace_alias_of(secret).unwrap_or_default().to_string(),
            note: secret
                .note
                .as_deref()
                .map(|note| wrap_text_to_width(note, SECRET_LIST_NOTE_WRAP_WIDTH))
                .unwrap_or_default(),
            folder: secret.folder.clone().unwrap_or_default(),
            groups: secret.groups.clone().unwrap_or_default(),
            updated_on: crate::cli::ls_view::date_portion_for_display(&secret.updated_on),
        })
        .collect()
}

fn format_secret_list_rows_for_human_vault_typed(
    secrets: &[crate::secret::manager::SecretSummary],
) -> Vec<SecretListDisplayRowVaultTyped> {
    secrets
        .iter()
        .map(|secret| SecretListDisplayRowVaultTyped {
            name: secret.name.clone(),
            vault: workspace_alias_of(secret).unwrap_or_default().to_string(),
            record_type: secret.tags.get(TYPE_TAG).cloned().unwrap_or_default(),
            note: secret
                .note
                .as_deref()
                .map(|note| wrap_text_to_width(note, SECRET_LIST_NOTE_WRAP_WIDTH))
                .unwrap_or_default(),
            folder: secret.folder.clone().unwrap_or_default(),
            groups: secret.groups.clone().unwrap_or_default(),
            updated_on: crate::cli::ls_view::date_portion_for_display(&secret.updated_on),
        })
        .collect()
}

/// True when any secret in `secrets` carries the reserved `xv-type` tag —
/// decides whether the table view gains a `Type` column.
fn any_secret_typed(secrets: &[crate::secret::manager::SecretSummary]) -> bool {
    secrets.iter().any(|s| s.tags.contains_key(TYPE_TAG))
}

/// Filters `secrets` down to those whose `xv-type` tag matches `type_name`
/// (record-types plan Task 10's `ls --type` filter).
fn filter_secrets_by_type(
    mut secrets: Vec<crate::secret::manager::SecretSummary>,
    type_name: Option<&str>,
) -> Vec<crate::secret::manager::SecretSummary> {
    if let Some(t) = type_name {
        secrets.retain(|s| s.tags.get(TYPE_TAG).map(String::as_str) == Some(t));
    }
    secrets
}

/// Filters `secrets` down to those whose name (either the user-facing
/// `original_name` or the backend `name`) matches `filter`'s glob pattern.
/// Used by `xv ls --filter` and, on the pre-scoring candidate set, by
/// `xv find --filter`. `filter` must already have been validated by
/// [`crate::utils::helpers::compile_name_glob`] before any backend call;
/// this recompiles the (now-known-valid) pattern to apply it.
fn filter_secrets_by_glob(
    mut secrets: Vec<crate::secret::manager::SecretSummary>,
    filter: Option<&str>,
) -> Result<Vec<crate::secret::manager::SecretSummary>> {
    if let Some(pattern) = filter {
        let matcher = crate::utils::helpers::compile_name_glob(pattern)?;
        secrets.retain(|s| {
            crate::utils::helpers::glob_matches_either_name(&matcher, &s.name, &s.original_name)
        });
    }
    Ok(secrets)
}

/// Same as [`filter_secrets_by_glob`] but for `xv ls --deleted` summaries.
fn filter_deleted_secrets_by_glob(
    mut items: Vec<crate::secret::manager::DeletedSecretSummary>,
    filter: Option<&str>,
) -> Result<Vec<crate::secret::manager::DeletedSecretSummary>> {
    if let Some(pattern) = filter {
        let matcher = crate::utils::helpers::compile_name_glob(pattern)?;
        items.retain(|s| {
            crate::utils::helpers::glob_matches_either_name(&matcher, &s.name, &s.original_name)
        });
    }
    Ok(items)
}

/// Lifts a `SecretSummary`'s `f.*` tags into a `fields` map and its
/// `xv-type` tag into `record_type`, for `ls --format json` (record-types
/// plan Task 10). Other keys match `SecretSummary`'s existing JSON shape
/// exactly (same field names, no `tags` key) so untyped-secret JSON output
/// is unaffected beyond the two new keys.
fn secret_summary_to_json_with_fields(
    s: &crate::secret::manager::SecretSummary,
) -> serde_json::Value {
    let mut fields = serde_json::Map::new();
    for (k, v) in &s.tags {
        if let Some(f) = k.strip_prefix(FIELD_TAG_PREFIX) {
            fields.insert(f.to_string(), serde_json::Value::String(v.clone()));
        }
    }
    serde_json::json!({
        "name": s.name,
        "original_name": s.original_name,
        "note": s.note,
        "folder": s.folder,
        "groups": s.groups,
        "updated_on": s.updated_on,
        "enabled": s.enabled,
        "content_type": s.content_type,
        "record_type": s.tags.get(TYPE_TAG),
        "fields": fields,
    })
}

fn wrap_text_to_width(input: &str, width: usize) -> String {
    if width == 0 || input.is_empty() {
        return input.to_string();
    }

    input
        .split('\n')
        .map(|paragraph| wrap_paragraph_to_width(paragraph, width))
        .collect::<Vec<_>>()
        .join("\n")
}

fn wrap_paragraph_to_width(paragraph: &str, width: usize) -> String {
    let mut lines = Vec::new();
    let mut current = String::new();

    for word in paragraph.split_whitespace() {
        push_wrapped_word(word, width, &mut current, &mut lines);
    }

    if !current.is_empty() {
        lines.push(current);
    }

    lines.join("\n")
}

fn push_wrapped_word(word: &str, width: usize, current: &mut String, lines: &mut Vec<String>) {
    use unicode_width::UnicodeWidthChar;
    let display_width = crate::cli::ls_view::display_width;
    let word_len = display_width(word);

    if word_len > width {
        if !current.is_empty() {
            lines.push(std::mem::take(current));
        }
        let mut chunk = String::new();
        let mut chunk_w = 0usize;
        for ch in word.chars() {
            let w = UnicodeWidthChar::width(ch).unwrap_or(0);
            if chunk_w + w > width && !chunk.is_empty() {
                lines.push(std::mem::take(&mut chunk));
                chunk_w = 0;
            }
            chunk.push(ch);
            chunk_w += w;
        }
        if !chunk.is_empty() {
            *current = chunk;
        }
        return;
    }

    if current.is_empty() {
        current.push_str(word);
        return;
    }

    let projected_len = display_width(current) + 1 + word_len;
    if projected_len <= width {
        current.push(' ');
        current.push_str(word);
    } else {
        lines.push(std::mem::take(current));
        current.push_str(word);
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn display_cached_secret_list(
    secrets: Vec<crate::secret::manager::SecretSummary>,
    group: Option<String>,
    all: bool,
    path: &str,
    long: bool,
    recursive: bool,
    sort: crate::cli::commands::LsSort,
    pagination: Pagination,
    pager: bool,
    vault_name: &str,
    config: &Config,
    names_only: bool,
    type_filter: Option<&str>,
    filter: Option<&str>,
    show_vault: bool,
) -> Result<()> {
    use crate::cli::ls_view::{self, LsEntry};
    use crate::utils::format::TableFormatter;
    use crate::utils::pagination::{paginate_slice, pagination_footer_text};
    use std::fmt::Write as _;

    let filtered = filter_secret_summaries_for_display(secrets, group.as_deref(), all);
    let filtered = filter_secrets_by_type(filtered, type_filter);
    let filtered = filter_secrets_by_glob(filtered, filter)?;
    let mut scoped = ls_view::scope_secrets(filtered, path);
    if sort == crate::cli::commands::LsSort::Updated {
        // Deliberate: `--sort updated` is an explicit user request for time
        // order and intentionally drops the alias-primary ordering below —
        // the spec's "stable sort: alias, then name" is union `ls`'s
        // *default* merge order, not a rule that overrides an explicit
        // `--sort`.
        ls_view::sort_secrets_by_updated_desc(&mut scoped.secrets);
        ls_view::sort_secrets_by_updated_desc(&mut scoped.subtree);
    } else if show_vault {
        // Union `ls` (spec §Read semantics): "results merge (stable sort:
        // alias, then name)" — `scope_secrets` already sorted `secrets`/
        // `subtree` by name alone; re-sort with alias as the primary key
        // (Rust's slice sort is stable, so ties within the same alias keep
        // their existing name order).
        let by_alias_then_name =
            |a: &crate::secret::manager::SecretSummary,
             b: &crate::secret::manager::SecretSummary| {
                workspace_alias_of(a)
                    .unwrap_or("")
                    .cmp(workspace_alias_of(b).unwrap_or(""))
                    .then_with(|| ls_view::display_name(a).cmp(ls_view::display_name(b)))
            };
        scoped.secrets.sort_by(by_alias_then_name);
        scoped.subtree.sort_by(by_alias_then_name);
    }

    // Pipe-friendly modes: flat recursive subtree, unchanged schema.
    // Qualification is opt-in via -r (the bare --names-only shape is shipped).
    // Union listings prefix `alias/` so piped output stays disambiguated
    // across vaults, mirroring `find`'s vault-prefix style — but ONLY when
    // `show_vault` is true (workspace has >=2 entries), exactly the same
    // gate the VAULT column/grid-prefix paths honor below. A single-entry
    // workspace (or no workspace) must be byte-identical to the
    // no-workspace path in EVERY output form, including --names-only
    // (Bugbot review MEDIUM: this branch used to prefix whenever the
    // synthetic alias tag was present, ignoring `show_vault` entirely).
    if names_only {
        for s in &scoped.subtree {
            let label = if recursive {
                ls_view::qualified_display_name(s, path)
            } else {
                ls_view::display_name(s).to_string()
            };
            match workspace_alias_of(s).filter(|_| show_vault) {
                Some(alias) => println!("{alias}/{label}"),
                None => println!("{label}"),
            }
        }
        return Ok(());
    }

    let fmt = config.runtime_output_format;
    let human_table_like = matches!(
        fmt,
        OutputFormat::Table | OutputFormat::Plain | OutputFormat::Raw
    );

    if !human_table_like {
        if long {
            crate::utils::output::warn("--long is ignored for machine-readable formats");
        }
        let page = paginate_slice(&scoped.subtree, pagination);
        if fmt == OutputFormat::Json {
            // JSON output lifts `f.*` tags into a `fields` map and
            // `xv-type` into `record_type` (record-types plan Task 10).
            // Union listings also carry a `vault` key with the alias.
            let body: Vec<serde_json::Value> = page
                .items
                .iter()
                .map(|s| {
                    let mut v = secret_summary_to_json_with_fields(s);
                    if show_vault {
                        if let serde_json::Value::Object(ref mut map) = v {
                            map.insert(
                                "vault".to_string(),
                                serde_json::Value::String(
                                    workspace_alias_of(s).unwrap_or_default().to_string(),
                                ),
                            );
                        }
                    }
                    v
                })
                .collect();
            let output = serde_json::to_string_pretty(&body).map_err(|e| {
                CrosstacheError::serialization(format!("JSON serialization failed: {e}"))
            })?;
            crate::utils::pager::print_output(&output, pager)?;
            return Ok(());
        }
        let formatter = TableFormatter::new(
            fmt,
            config.no_color,
            config.template.clone(),
            config.runtime_columns.clone(),
        );
        let output = formatter.format_table(&page.items)?;
        crate::utils::pager::print_output(&output, pager)?;
        return Ok(());
    }

    // Grid/long views ignore --columns for rendering, but an unknown column
    // name must still error like every other view. Validate up front — before
    // the empty-scope early return below — so empty scopes reject typos too;
    // the "ignored for the grid/long view" warning (below) only fires once we
    // know the selection is valid.
    let is_grid_or_long_view = !config.format_explicit || long;
    if config.runtime_columns.is_some() && is_grid_or_long_view {
        let formatter = TableFormatter::new(
            fmt,
            config.no_color,
            config.template.clone(),
            config.runtime_columns.clone(),
        );
        if show_vault {
            formatter.validate_columns::<SecretListDisplayRowVault>()?;
        } else {
            formatter.validate_columns::<SecretListDisplayRow>()?;
        }
    }

    if scoped.subtree.is_empty() {
        // Explicit table/plain/raw view: validate an unknown --columns
        // selection here too (grid/long already validated above).
        if config.format_explicit && !long {
            let formatter = TableFormatter::new(
                fmt,
                config.no_color,
                config.template.clone(),
                config.runtime_columns.clone(),
            );
            if show_vault {
                formatter.validate_columns::<SecretListDisplayRowVault>()?;
            } else {
                formatter.validate_columns::<SecretListDisplayRow>()?;
            }
        }
        let scope_desc = if !path.is_empty() {
            format!("folder '{path}'")
        } else {
            format!("vault '{vault_name}'")
        };
        let msg = if all {
            crate::utils::list_output::empty_state_message("secrets", Some(&scope_desc))
        } else {
            format!(
                "{} Use --all to show disabled secrets.",
                crate::utils::list_output::empty_state_message(
                    "enabled secrets",
                    Some(&scope_desc)
                )
            )
        };
        crate::utils::output::info(&msg);
        return Ok(());
    }

    let mut output = String::new();
    output.push('\n');
    // Color only for styled table/grid; plain/raw must not emit ANSI escapes
    let color = !config.no_color && fmt == OutputFormat::Table;
    if color {
        let _ = writeln!(output, "\x1b[36mVault: {}\x1b[0m", vault_name);
    } else {
        let _ = writeln!(output, "Vault: {}", vault_name);
    }
    output.push('\n');

    // Legacy rounded table only on explicit --format table|plain|raw.
    if config.format_explicit && !long {
        let table_secrets = &scoped.subtree;
        let page = paginate_slice(table_secrets, pagination);
        let formatter = TableFormatter::new(
            fmt,
            config.no_color,
            config.template.clone(),
            config.runtime_columns.clone(),
        );
        // TYPE column only when at least one listed secret is typed, so an
        // untyped-only listing's table output stays byte-identical to
        // pre-Task-10 behavior (record-types plan Task 10). VAULT column
        // (this function's own `show_vault`) composes independently.
        match (show_vault, any_secret_typed(&page.items)) {
            (true, true) => {
                let display_rows = format_secret_list_rows_for_human_vault_typed(&page.items);
                output.push_str(&formatter.format_table(&display_rows)?);
            }
            (true, false) => {
                let display_rows = format_secret_list_rows_for_human_vault(&page.items);
                output.push_str(&formatter.format_table(&display_rows)?);
            }
            (false, true) => {
                let display_rows = format_secret_list_rows_for_human_typed(&page.items);
                output.push_str(&formatter.format_table(&display_rows)?);
            }
            (false, false) => {
                let display_rows = format_secret_list_rows_for_human(&page.items);
                output.push_str(&formatter.format_table(&display_rows)?);
            }
        }
        output.push('\n');
        let _ = writeln!(
            output,
            "{} in vault '{}'",
            crate::utils::list_output::count_label(
                page.items.len(),
                page.total_items,
                "secret",
                "secrets",
                None,
                page.page_size.is_some(),
            ),
            vault_name
        );
        if let Some(footer) = pagination_footer_text(&page, "secret", "secrets", fmt) {
            output.push('\n');
            output.push_str(&footer);
        }
        crate::utils::pager::print_output(&output, pager)?;
        return Ok(());
    }

    // ls-style grid / long listing.
    if config.runtime_columns.is_some() {
        crate::utils::output::warn(
            "--columns is ignored for the grid/long view; use --format table",
        );
    }
    let entries: Vec<LsEntry> = if recursive {
        ls_view::qualified_subtree(
            &scoped.subtree,
            path,
            sort == crate::cli::commands::LsSort::Name,
        )
        .into_iter()
        .map(LsEntry::Secret)
        .collect()
    } else {
        ls_view::entries_for_display(&scoped)
    };
    // Grid/long ls-style views have no tabular column to add a VAULT slot
    // to — union listings instead prefix `alias/` onto the displayed name,
    // mirroring `find`'s existing vault-prefix convention (folder entries
    // are virtual groupings that can span vaults, so they stay unprefixed).
    // The long (`-l`) view additionally appends the real vault name when it
    // differs from the alias, so `-l` identifies the backing vault (aliases
    // can be renamed) without the grid/table views changing.
    let entries: Vec<LsEntry> = if show_vault {
        entries
            .into_iter()
            .map(|e| match e {
                LsEntry::Secret(mut s) => {
                    if let Some(alias) = workspace_alias_of(&s) {
                        let label = ls_view::display_name(&s).to_string();
                        let vault_suffix = match workspace_vault_of(&s).filter(|_| long) {
                            Some(vault) if vault != alias => format!(" ({vault})"),
                            _ => String::new(),
                        };
                        s.original_name = format!("{alias}/{label}{vault_suffix}");
                    }
                    LsEntry::Secret(s)
                }
                other => other,
            })
            .collect()
    } else {
        entries
    };
    let folder_count = if recursive { 0 } else { scoped.folders.len() };
    let secret_count = entries.len() - folder_count;

    let page = paginate_slice(&entries, pagination);
    let rendered = if long {
        ls_view::render_long(&page.items, color)
    } else {
        let width = crossterm::terminal::size()
            .map(|(w, _)| w as usize)
            .unwrap_or(80);
        ls_view::render_grid(&page.items, width, color)
    };
    output.push_str(&rendered);
    output.push('\n');
    let mut count_line = crate::utils::list_output::count_label(
        page.items
            .iter()
            .filter(|e| matches!(e, LsEntry::Secret(_)))
            .count(),
        secret_count,
        "secret",
        "secrets",
        None,
        page.page_size.is_some(),
    );
    if folder_count > 0 {
        let _ = write!(
            count_line,
            ", {}",
            crate::utils::list_output::pluralize(folder_count, "folder", "folders")
        );
    }
    let _ = writeln!(output, "{} in vault '{}'", count_line, vault_name);
    if let Some(footer) = pagination_footer_text(&page, "entry", "entries", fmt) {
        output.push('\n');
        output.push_str(&footer);
    }
    crate::utils::pager::print_output(&output, pager)?;
    Ok(())
}

/// Row shape for `xv ls --deleted` — machine formats get this array; empty
/// strings mark dates the backend cannot supply (AWS/local purge schedule).
#[derive(Debug, Clone, serde::Serialize, tabled::Tabled)]
struct DeletedSecretListRow {
    #[tabled(rename = "Name")]
    name: String,
    #[tabled(rename = "Deleted")]
    deleted: String,
    #[tabled(rename = "Purge Scheduled")]
    purge_scheduled: String,
}

fn deleted_display_name(s: &crate::secret::manager::DeletedSecretSummary) -> &str {
    if s.original_name.is_empty() {
        &s.name
    } else {
        &s.original_name
    }
}

fn deleted_list_rows(
    items: &[crate::secret::manager::DeletedSecretSummary],
    human: bool,
) -> Vec<DeletedSecretListRow> {
    items
        .iter()
        .map(|s| {
            let fmt_date = |d: &Option<String>| {
                let v = d.clone().unwrap_or_default();
                if human {
                    crate::cli::ls_view::date_portion_for_display(&v)
                } else {
                    v
                }
            };
            DeletedSecretListRow {
                name: deleted_display_name(s).to_string(),
                deleted: fmt_date(&s.deleted_on),
                purge_scheduled: fmt_date(&s.scheduled_purge_on),
            }
        })
        .collect()
}

/// `xv ls --deleted`: list soft-deleted secrets awaiting restore/purge.
/// Always bypasses the secrets-list cache (which only holds live secrets)
/// and requires the backend to advertise `has_soft_delete`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_deleted_secret_list(
    pagination: Pagination,
    pager: bool,
    names_only: bool,
    long: bool,
    sort: crate::cli::commands::LsSort,
    filter: Option<String>,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    use crate::cli::commands::LsSort;

    // Validate the glob before any backend call.
    if let Some(pattern) = filter.as_deref() {
        crate::utils::helpers::compile_name_glob(pattern)?;
    }

    // Workspace union path (multi-vault workspaces plan, Phase B Task 9
    // remaining scope): consulted ONLY when a REAL (configured) workspace is
    // attached. `resolve_configured_workspace` returns `None` with no
    // configured workspace, so the degenerate single-vault case falls straight
    // through to the trait-path code below, byte-identical to pre-workspace
    // `ls --deleted`.
    if let Some(ws) = crate::workspace::resolve_configured_workspace(&config).await? {
        return execute_deleted_secret_list_workspace(
            ws, pagination, pager, names_only, long, sort, filter, config,
        )
        .await;
    }

    if !use_trait_path(registry) {
        return Err(CrosstacheError::config(
            "No backend registry available. Run 'xv config show' to check your configuration.",
        ));
    }

    // Resolve the backend + vault through the unified workspace path instead
    // of `reg.active()`: `ls --deleted` lists the default vault's trash, so it
    // resolves as a write target would — no secret name, no search
    // (`TargetMode::Write` never searches). With no configured workspace the
    // degenerate workspace-of-one resolves over `config.effective_backend_name()`
    // and the default vault, so this stays byte-identical while the capability
    // gate below targets the RESOLVED backend, matching purge/restore/rotate.
    // The empty raw name only feeds the (discarded) resolved path.
    let (backend, _backend_name, vault_name, _resolved_path) =
        crate::cli::helpers::resolve_workspace_or_default(
            "",
            &config,
            crate::workspace::TargetMode::Write,
        )
        .await?;

    // Capability gate — same shape as `xv restore`'s.
    let unsupported = || {
        CrosstacheError::InvalidArgument(format!(
            "The {} backend does not support listing deleted secrets (soft-delete not available).",
            backend.name()
        ))
    };
    if !backend.capabilities().has_soft_delete {
        return Err(unsupported());
    }

    // Always live — the SecretsList cache holds live secrets only, and trash
    // freshness matters right after a delete.
    let items = match backend.secrets().list_deleted_secrets(&vault_name).await {
        Ok(items) => items,
        Err(crate::backend::BackendError::Unsupported(_)) => return Err(unsupported()),
        Err(e) => return Err(e.into()),
    };

    let mut items = filter_deleted_secrets_by_glob(items, filter.as_deref())?;

    match sort {
        LsSort::Name => items.sort_by(|a, b| deleted_display_name(a).cmp(deleted_display_name(b))),
        // In deleted mode "updated" means the deleted date (newest first).
        LsSort::Updated => items.sort_by(|a, b| {
            b.deleted_on
                .cmp(&a.deleted_on)
                .then_with(|| deleted_display_name(a).cmp(deleted_display_name(b)))
        }),
    }

    display_deleted_secret_list(
        items,
        pagination,
        pager,
        names_only,
        long,
        &vault_name,
        &config,
    )
}

/// Rendering tail of `xv ls --deleted`, shared by the single-vault path
/// above and the workspace-union path below (multi-vault workspaces plan,
/// Phase B Task 9) — everything after the candidate `items` are fetched,
/// glob-filtered, and sorted is identical regardless of how many vaults
/// they came from.
#[allow(clippy::too_many_arguments)]
fn display_deleted_secret_list(
    items: Vec<crate::secret::manager::DeletedSecretSummary>,
    pagination: Pagination,
    pager: bool,
    names_only: bool,
    long: bool,
    vault_name: &str,
    config: &Config,
) -> Result<()> {
    use crate::cli::ls_view;
    use crate::utils::format::TableFormatter;
    use crate::utils::pagination::{paginate_slice, pagination_footer_text};
    use std::fmt::Write as _;

    if names_only {
        for s in &items {
            println!("{}", deleted_display_name(s));
        }
        return Ok(());
    }

    let fmt = config.runtime_output_format;
    let human_table_like = matches!(
        fmt,
        OutputFormat::Table | OutputFormat::Plain | OutputFormat::Raw
    );
    let page = paginate_slice(&items, pagination);

    if !human_table_like {
        if long {
            crate::utils::output::warn("--long is ignored for machine-readable formats");
        }
        let formatter = TableFormatter::new(
            fmt,
            config.no_color,
            config.template.clone(),
            config.runtime_columns.clone(),
        );
        let rows = deleted_list_rows(&page.items, false);
        let output = formatter.format_table(&rows)?;
        crate::utils::pager::print_output(&output, pager)?;
        return Ok(());
    }

    // Validate --columns up front so an unknown selection errors even when
    // the result set is empty (branch-wide rule shared with `display_cached_secret_list`).
    if config.runtime_columns.is_some() {
        let formatter = TableFormatter::new(
            fmt,
            config.no_color,
            config.template.clone(),
            config.runtime_columns.clone(),
        );
        formatter.validate_columns::<DeletedSecretListRow>()?;
    }

    if items.is_empty() {
        crate::utils::output::info(&crate::utils::list_output::empty_state_message(
            "deleted secrets",
            Some(&format!("vault '{vault_name}'")),
        ));
        return Ok(());
    }

    let mut output = String::new();
    output.push('\n');
    let color = !config.no_color && fmt == OutputFormat::Table;
    if color {
        let _ = writeln!(
            output,
            "\x1b[36mVault: {} (deleted secrets)\x1b[0m",
            vault_name
        );
    } else {
        let _ = writeln!(output, "Vault: {} (deleted secrets)", vault_name);
    }
    output.push('\n');

    if config.format_explicit && !long {
        let formatter = TableFormatter::new(
            fmt,
            config.no_color,
            config.template.clone(),
            config.runtime_columns.clone(),
        );
        let rows = deleted_list_rows(&page.items, true);
        output.push_str(&formatter.format_table(&rows)?);
        output.push('\n');
    } else {
        if config.runtime_columns.is_some() {
            crate::utils::output::warn(
                "--columns is ignored for the grid/long view; use --format table",
            );
        }
        if long {
            output.push_str(&ls_view::render_deleted_long(&page.items));
        } else {
            let width = crossterm::terminal::size()
                .map(|(w, _)| w as usize)
                .unwrap_or(80);
            let labels: Vec<String> = page
                .items
                .iter()
                .map(|s| deleted_display_name(s).to_string())
                .collect();
            output.push_str(&ls_view::render_name_grid(&labels, width));
        }
        output.push('\n');
    }

    let _ = writeln!(
        output,
        "{} in vault '{}'",
        crate::utils::list_output::count_label(
            page.items.len(),
            page.total_items,
            "deleted secret",
            "deleted secrets",
            None,
            page.page_size.is_some(),
        ),
        vault_name
    );
    if let Some(footer) = pagination_footer_text(&page, "deleted secret", "deleted secrets", fmt) {
        output.push('\n');
        output.push_str(&footer);
    }
    crate::utils::pager::print_output(&output, pager)?;
    Ok(())
}

/// `xv ls --deleted` over every vault attached to a workspace (multi-vault
/// workspaces plan, Phase B Task 9 remaining scope): per-vault capability
/// gating, never silent, never a hard failure of the whole view. A vault
/// whose backend lacks `has_soft_delete` (checked up front via
/// `capabilities()`, and defensively again via the `Unsupported` error a
/// backend might still return) is skipped with a stderr note naming
/// vault+backend; every capable vault's results merge, `alias/`-prefixed
/// (mirroring `find`'s convention) when the workspace has ≥2 entries.
/// Stderr note printed when a union `ls --deleted` skips an attached vault
/// whose backend lacks soft-delete support (spec §Capability differences:
/// "never silent, never fatal"). Pulled out as its own pure function so the
/// exact wording is unit-testable without constructing a real backend
/// lacking `has_soft_delete` — no shipped backend (Azure/local/AWS) lacks
/// it today, so the skip branch itself isn't e2e-drivable (same class of
/// limitation documented on the capability tests in
/// `src/workspace/resolve.rs`).
fn deleted_list_capability_skip_note(alias: &str, backend_name: &str) -> String {
    format!("note: '{alias}' ({backend_name}) has no soft-delete; --deleted skipped for it")
}

/// **Shared ordering convention** with `execute_secret_list_workspace`
/// (union `ls`), so the two union paths can't drift apart again (Bugbot
/// review MEDIUM): name-based filtering (`--filter` glob, and any future
/// name-based filter) ALWAYS runs against bare per-vault names BEFORE the
/// `alias/` display prefix is applied. The live union `ls` path gets this
/// for free — the alias travels as a synthetic tag (`WORKSPACE_ALIAS_TAG`)
/// separate from `name`/`original_name` until the final render step, well
/// after `filter_secrets_by_glob` runs on the merged (still bare-named)
/// set. `DeletedSecretSummary` has no such synthetic-tag slot — the alias
/// can only be baked into `original_name` — so this function must apply
/// `filter_deleted_secrets_by_glob` per vault BEFORE that bake-in,
/// mirroring the live path's effective ordering explicitly instead of
/// getting it for free. Filtering after prefixing (the pre-fix behavior)
/// silently broke `--filter 'PROD_*'`-style anchored globs against
/// `"alias/PROD_X"`, which no longer starts with `PROD_`.
#[allow(clippy::too_many_arguments)]
async fn execute_deleted_secret_list_workspace(
    ws: crate::workspace::Workspace,
    pagination: Pagination,
    pager: bool,
    names_only: bool,
    long: bool,
    sort: crate::cli::commands::LsSort,
    filter: Option<String>,
    config: Config,
) -> Result<()> {
    use crate::cli::commands::LsSort;

    let backend_names: Vec<String> = ws.entries.iter().map(|e| e.backend.clone()).collect();
    let ws_registry = BackendRegistry::with_lazy(&config, &backend_names)
        .map_err(|e| CrosstacheError::config(e.to_string()))?;

    let show_vault = ws.entries.len() >= 2;
    let mut items: Vec<crate::secret::manager::DeletedSecretSummary> = Vec::new();
    for entry in &ws.entries {
        let backend = ws_registry.materialize(&entry.backend).map_err(|e| {
            CrosstacheError::config(format!(
                "workspace vault '{}' (backend '{}') is unavailable: {e}",
                entry.alias, entry.backend
            ))
        })?;

        if !backend.capabilities().has_soft_delete {
            eprintln!(
                "{}",
                deleted_list_capability_skip_note(&entry.alias, &entry.backend)
            );
            continue;
        }

        let fetched = match backend.secrets().list_deleted_secrets(&entry.vault).await {
            Ok(items) => items,
            Err(crate::backend::BackendError::Unsupported(_)) => {
                eprintln!(
                    "{}",
                    deleted_list_capability_skip_note(&entry.alias, &entry.backend)
                );
                continue;
            }
            Err(e) => {
                return Err(CrosstacheError::config(format!(
                    "workspace vault '{}' (backend '{}') failed to list deleted secrets: {e}",
                    entry.alias, entry.backend
                )));
            }
        };

        // Filter on BARE per-vault names first (see this function's doc
        // comment: shared ordering convention with the live union `ls`
        // path) — only AFTER that does the `alias/` display prefix apply.
        let mut fetched = filter_deleted_secrets_by_glob(fetched, filter.as_deref())?;

        if show_vault {
            for s in &mut fetched {
                let label = deleted_display_name(s).to_string();
                s.original_name = format!("{}/{}", entry.alias, label);
            }
        }
        items.extend(fetched);
    }

    // Filtering already happened per vault (on bare names) above — no
    // second glob pass here, which would otherwise re-filter against
    // already-`alias/`-prefixed names for a >=2-entry workspace.

    match sort {
        LsSort::Name => items.sort_by(|a, b| deleted_display_name(a).cmp(deleted_display_name(b))),
        LsSort::Updated => items.sort_by(|a, b| {
            b.deleted_on
                .cmp(&a.deleted_on)
                .then_with(|| deleted_display_name(a).cmp(deleted_display_name(b)))
        }),
    }

    let vault_label = if show_vault {
        format!("workspace ({} vaults attached)", ws.entries.len())
    } else {
        ws.entries
            .first()
            .map(|e| e.vault.clone())
            .unwrap_or_default()
    };

    display_deleted_secret_list(
        items,
        pagination,
        pager,
        names_only,
        long,
        &vault_label,
        &config,
    )
}

/// Union `ls` over every vault attached to a workspace (multi-vault
/// workspaces plan, Phase B Task 7). Only reached when
/// [`crate::workspace::resolve_workspace`] returns `Some` — the no-workspace
/// path in `execute_secret_list_direct` is untouched.
///
/// Per vault: materialize its backend (fail loud naming vault+backend on
/// any error — spec §Read semantics, "no partial unions"), fetch via the
/// same per-`(backend, vault)` cache key `xv ls` uses in the single-vault
/// case, apply the same expiry-filter detail-fetch logic, then tag each
/// `SecretSummary` with its originating alias (`WORKSPACE_ALIAS_TAG`) before
/// merging into one list. `display_cached_secret_list` handles the rest
/// (folder scoping, filters, sort, pagination, VAULT column) identically to
/// the single-vault path, since it doesn't care whether its input came from
/// one vault or several.
#[allow(clippy::too_many_arguments)]
async fn execute_secret_list_workspace(
    ws: crate::workspace::Workspace,
    path: String,
    group: Option<String>,
    all: bool,
    expiring: Option<String>,
    expired: bool,
    no_cache: bool,
    pagination: Pagination,
    pager: bool,
    names_only: bool,
    long: bool,
    recursive: bool,
    sort: crate::cli::commands::LsSort,
    type_filter: Option<String>,
    filter: Option<String>,
    config: Config,
) -> Result<()> {
    use crate::cache::CacheManager;

    let backend_names: Vec<String> = ws.entries.iter().map(|e| e.backend.clone()).collect();
    let ws_registry = BackendRegistry::with_lazy(&config, &backend_names)
        .map_err(|e| CrosstacheError::config(e.to_string()))?;

    let cache_manager = CacheManager::from_config(&config);
    let use_cache = cache_manager.is_enabled() && !no_cache && expiring.is_none() && !expired;

    let mut merged: Vec<crate::secret::manager::SecretSummary> = Vec::new();
    for entry in &ws.entries {
        let backend = ws_registry.materialize(&entry.backend).map_err(|e| {
            CrosstacheError::config(format!(
                "workspace vault '{}' (backend '{}') is unavailable: {e}",
                entry.alias, entry.backend
            ))
        })?;

        let cache_key = crate::cache::CacheKey::SecretsList {
            backend: entry.backend.clone(),
            vault_name: entry.vault.clone(),
        };

        let cached = if use_cache {
            cache_manager.get::<Vec<crate::secret::manager::SecretSummary>>(&cache_key)
        } else {
            None
        };

        let mut secrets = match cached {
            Some(secrets) => secrets,
            None => {
                let fetched = backend
                    .secrets()
                    .list_secrets(&entry.vault, None)
                    .await
                    .map_err(|e| {
                        CrosstacheError::config(format!(
                            "workspace vault '{}' (backend '{}') failed to list secrets: {e}",
                            entry.alias, entry.backend
                        ))
                    })?;
                if cache_manager.is_enabled() && !no_cache {
                    cache_manager.set(&cache_key, &fetched);
                }
                fetched
            }
        };

        // Expiry filtering: same per-secret detail-fetch logic as the
        // single-vault path (`execute_secret_list_direct`), scoped to this
        // entry's own backend/vault. A per-secret fetch failure here is a
        // warning (matching the single-vault behavior), not a whole-union
        // failure — only the initial `list_secrets`/`materialize` calls
        // above are fail-loud per spec §Read semantics.
        if expired || expiring.is_some() {
            use crate::utils::datetime::{is_expired, is_expiring_within};

            let display_candidates =
                filter_secret_summaries_for_display(secrets, group.as_deref(), all);
            let mut filtered_secrets = Vec::new();
            for secret_summary in display_candidates {
                match backend
                    .secrets()
                    .get_secret(&entry.vault, &secret_summary.name, false)
                    .await
                {
                    Ok(secret_props) => {
                        let should_include = if expired {
                            is_expired(secret_props.expires_on)
                        } else if let Some(ref duration) = expiring {
                            match is_expiring_within(secret_props.expires_on, duration) {
                                Ok(is_exp) => is_exp,
                                Err(e) => {
                                    eprintln!("Warning: Invalid duration '{}': {}", duration, e);
                                    false
                                }
                            }
                        } else {
                            true
                        };
                        if should_include {
                            filtered_secrets.push(secret_summary);
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "Warning: Failed to get details for secret '{}': {}",
                            secret_summary.name, e
                        );
                    }
                }
            }
            secrets = filtered_secrets;
        }

        for s in &mut secrets {
            s.tags
                .insert(WORKSPACE_ALIAS_TAG.to_string(), entry.alias.clone());
            s.tags
                .insert(WORKSPACE_VAULT_TAG.to_string(), entry.vault.clone());
        }
        merged.extend(secrets);
    }

    let show_vault = ws.entries.len() >= 2;
    let vault_label = if show_vault {
        format!("workspace ({} vaults attached)", ws.entries.len())
    } else {
        // Single-entry workspace: no VAULT column, and the header/footer
        // "vault '<name>'" line matches what a no-workspace `ls` against
        // that same vault would show.
        ws.entries[0].vault.clone()
    };

    display_cached_secret_list(
        merged,
        if expired || expiring.is_some() {
            None
        } else {
            group
        },
        if expired || expiring.is_some() {
            true
        } else {
            all
        },
        &path,
        long,
        recursive,
        sort,
        pagination,
        pager,
        &vault_label,
        &config,
        names_only,
        type_filter.as_deref(),
        filter.as_deref(),
        show_vault,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_secret_list_direct(
    path: String,
    group: Option<String>,
    all: bool,
    expiring: Option<String>,
    expired: bool,
    no_cache: bool,
    pagination: Pagination,
    pager: bool,
    names_only: bool,
    long: bool,
    recursive: bool,
    sort: crate::cli::commands::LsSort,
    type_filter: Option<String>,
    filter: Option<String>,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    // Validate the glob before any backend call.
    if let Some(pattern) = filter.as_deref() {
        crate::utils::helpers::compile_name_glob(pattern)?;
    }

    // Workspace union path (multi-vault workspaces plan, Phase B Task 7):
    // consulted ONLY when a REAL (configured) workspace is attached.
    // `resolve_configured_workspace` returns `None` with no configured
    // workspace, so the degenerate single-vault case falls straight through to
    // the untouched trait-path code below, byte-identical to pre-workspace
    // `ls` output.
    if let Some(ws) = crate::workspace::resolve_configured_workspace(&config).await? {
        return execute_secret_list_workspace(
            ws,
            path,
            group,
            all,
            expiring,
            expired,
            no_cache,
            pagination,
            pager,
            names_only,
            long,
            recursive,
            sort,
            type_filter,
            filter,
            config,
        )
        .await;
    }

    // ── Trait-based path (all backends) ───────────────────────────────
    if use_trait_path(registry) {
        use crate::cache::CacheManager;

        let reg = registry.expect("use_trait_path guarantees Some");
        let vault_name = resolve_vault_for_trait(&config, registry).await?;
        let cache_manager = CacheManager::from_config(&config);
        // `config.effective_backend_name()`, not `reg.active().name()` (the
        // backend kind) — see `resolve_workspace_or_default`'s doc comment.
        let cache_key = trait_secret_cache_key(config.effective_backend_name(), &vault_name);
        let use_cache = cache_manager.is_enabled() && !no_cache;

        // Try cache (skip for expiry filters — they need per-secret API calls)
        if use_cache && expiring.is_none() && !expired {
            if let Some(cached) =
                cache_manager.get::<Vec<crate::secret::manager::SecretSummary>>(&cache_key)
            {
                return display_cached_secret_list(
                    cached,
                    group,
                    all,
                    &path,
                    long,
                    recursive,
                    sort,
                    pagination,
                    pager,
                    &vault_name,
                    &config,
                    names_only,
                    type_filter.as_deref(),
                    filter.as_deref(),
                    false,
                );
            }
        }

        // Fetch the full unfiltered list for the cache. For expiry filters,
        // derive the display set from this cached dataset after applying the
        // cheap group/enabled filters so we only call get_secret for rows that
        // can actually be displayed.
        let all_secrets = reg
            .active()
            .secrets()
            .list_secrets(&vault_name, None)
            .await?;

        // Cache the unfiltered list so subsequent calls see the full dataset.
        if use_cache {
            cache_manager.set(&cache_key, &all_secrets);
        }

        // Apply expiry filtering if requested (requires per-secret trait calls)
        let secrets = if expired || expiring.is_some() {
            use crate::utils::datetime::{is_expired, is_expiring_within};

            let display_candidates =
                filter_secret_summaries_for_display(all_secrets, group.as_deref(), all);
            let mut filtered_secrets = Vec::new();
            for secret_summary in display_candidates {
                match reg
                    .active()
                    .secrets()
                    .get_secret(&vault_name, &secret_summary.name, false)
                    .await
                {
                    Ok(secret_props) => {
                        let should_include = if expired {
                            is_expired(secret_props.expires_on)
                        } else if let Some(ref duration) = expiring {
                            match is_expiring_within(secret_props.expires_on, duration) {
                                Ok(is_exp) => is_exp,
                                Err(e) => {
                                    eprintln!("Warning: Invalid duration '{}': {}", duration, e);
                                    false
                                }
                            }
                        } else {
                            true
                        };
                        if should_include {
                            filtered_secrets.push(secret_summary);
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "Warning: Failed to get details for secret '{}': {}",
                            secret_summary.name, e
                        );
                    }
                }
            }
            filtered_secrets
        } else {
            all_secrets
        };

        return display_cached_secret_list(
            secrets,
            if expired || expiring.is_some() {
                None
            } else {
                group
            },
            if expired || expiring.is_some() {
                true
            } else {
                all
            },
            &path,
            long,
            recursive,
            sort,
            pagination,
            pager,
            &vault_name,
            &config,
            names_only,
            type_filter.as_deref(),
            filter.as_deref(),
            false,
        );
    }

    Err(CrosstacheError::config(
        "No backend registry available. Run 'xv config show' to check your configuration.",
    ))
}

pub(crate) async fn execute_secret_delete_direct(
    name: Option<String>,
    group: Option<String>,
    force: bool,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    // ── Trait-based path (non-Azure backends) ──────────────────────────
    if use_trait_path(registry) {
        if let Some(group_name) = group {
            // Group delete: not a single addressable secret name, so there's
            // nothing to alias-qualify — an empty raw always resolves to the
            // workspace's default vault (Write mode never searches), same
            // as every other unqualified write. No workspace attached ⇒
            // byte-identical to the pre-workspace resolution.
            let (backend, backend_name, vault_name, _) =
                crate::cli::helpers::resolve_workspace_or_default(
                    "",
                    &config,
                    crate::workspace::TargetMode::Write,
                )
                .await?;
            // List, filter by group, delete matching
            let secrets = backend
                .secrets()
                .list_secrets(&vault_name, Some(&group_name))
                .await?;
            if secrets.is_empty() {
                output::info(&format!("No secrets found in group '{group_name}'"));
                return Ok(());
            }
            if !confirm_destructive(
                force,
                &format!(
                    "Delete {} secret(s) in group '{group_name}'?",
                    secrets.len()
                ),
            )? {
                output::info("Aborted; no secrets deleted.");
                return Ok(());
            }
            for s in &secrets {
                backend
                    .secrets()
                    .delete_secret(&vault_name, &s.name)
                    .await?;
                output::success(&format!("Deleted '{}'", s.name));
            }
            invalidate_trait_secret_cache(&config, &backend_name, &vault_name);
        } else if let Some(secret_name) = name {
            // Workspace-aware resolution (unqualified → default vault, never
            // searched; `alias:name` targets that vault directly). No
            // workspace attached ⇒ byte-identical to the pre-workspace path.
            let (backend, backend_name, vault_name, resolved_name) =
                crate::cli::helpers::resolve_workspace_or_default(
                    &secret_name,
                    &config,
                    crate::workspace::TargetMode::Write,
                )
                .await?;
            if !confirm_destructive(force, &format!("Delete secret '{resolved_name}'?"))? {
                output::info("Aborted; secret not deleted.");
                return Ok(());
            }
            backend
                .secrets()
                .delete_secret(&vault_name, &resolved_name)
                .await?;
            output::success(&format!("Successfully deleted secret '{resolved_name}'"));
            invalidate_trait_secret_cache(&config, &backend_name, &vault_name);
        } else {
            return Err(CrosstacheError::invalid_argument(
                "Either secret name or --group must be specified",
            ));
        }

        return Ok(());
    }

    Err(CrosstacheError::config(
        "No backend registry available. Run 'xv config show' to check your configuration.",
    ))
}

pub(crate) async fn execute_secret_history_direct(
    name: &str,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    // ── Trait-based path (non-Azure backends) ──────────────────────────
    if use_trait_path(registry) {
        // Workspace-aware resolution (Read mode: searches attached vaults
        // on an unqualified name; ambiguous → exit 13). No workspace
        // attached ⇒ byte-identical to the pre-workspace path.
        let (backend, _backend_name, vault_name, resolved_name) =
            crate::cli::helpers::resolve_workspace_or_default(
                name,
                &config,
                crate::workspace::TargetMode::Read,
            )
            .await?;
        let name = resolved_name.as_str();

        // Capability check: history requires versioning support. Gated on
        // the RESOLVED target's backend (Bugbot round-3 fix) — a workspace
        // entry can differ in capabilities from the process's top-level
        // active backend, so checking `registry.active()` here (as before)
        // could reject a resolved vault that actually supports versioning,
        // or approve one that doesn't. No workspace attached ⇒ resolved
        // backend == active backend, so this check is unchanged there.
        if !backend.capabilities().has_versioning {
            return Err(CrosstacheError::InvalidArgument(format!(
                "The {} backend does not support version history.",
                backend.name()
            )));
        }

        let versions = backend.secrets().list_versions(&vault_name, name).await?;
        if versions.is_empty() {
            let fmt = config.runtime_output_format;
            use crate::utils::format::TableFormatter;
            let formatter = TableFormatter::new(
                fmt,
                config.no_color,
                config.template.clone(),
                config.runtime_columns.clone(),
            );
            if matches!(
                fmt,
                OutputFormat::Table | OutputFormat::Plain | OutputFormat::Raw
            ) {
                formatter.validate_columns::<crate::secret::manager::SecretProperties>()?;
                output::info(&format!("No version history for '{name}'"));
            } else {
                // Valid-empty machine output on stdout (e.g. `[]` for JSON).
                println!("{}", formatter.format_table(&versions)?);
            }
        } else {
            use crate::utils::format::TableFormatter;
            let formatter = TableFormatter::new(
                config.runtime_output_format,
                config.no_color,
                config.template.clone(),
                config.runtime_columns.clone(),
            );
            let table = formatter.format_table(&versions)?;
            println!("{table}");
            let fmt = config.runtime_output_format;
            if matches!(
                fmt,
                OutputFormat::Table | OutputFormat::Plain | OutputFormat::Raw
            ) {
                println!(
                    "{} of '{name}'",
                    crate::utils::list_output::count_label(
                        versions.len(),
                        versions.len(),
                        "version",
                        "versions",
                        None,
                        false
                    )
                );
            }
        }
        return Ok(());
    }

    Err(CrosstacheError::config(
        "No backend registry available. Run 'xv config show' to check your configuration.",
    ))
}

pub(crate) async fn execute_secret_rollback_direct(
    name: &str,
    version: &str,
    force: bool,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    // Every backend resolves through the trait path now — including Azure,
    // whose `SecretBackend::rollback` resolves friendly version numbers
    // ("v6") to the underlying GUID internally. registry==None rebuilds from
    // config so a startup init failure surfaces as a clean error.
    let rebuilt_registry;
    let _reg = match registry {
        Some(r) => r,
        None => {
            rebuilt_registry = BackendRegistry::from_config(&config)
                .map_err(|e| CrosstacheError::config(e.to_string()))?;
            &rebuilt_registry
        }
    };

    // Workspace-aware resolution: rollback is a read-resolution verb (searches
    // attached vaults on an unqualified name; ambiguous → exit 13), even
    // though it mutates state once the target secret is found.
    let (backend, backend_name, vault_name, resolved_name) =
        crate::cli::helpers::resolve_workspace_or_default(
            name,
            &config,
            crate::workspace::TargetMode::Read,
        )
        .await?;

    // Capability check: rollback requires versioning support.
    if !backend.capabilities().has_versioning {
        return Err(CrosstacheError::InvalidArgument(format!(
            "The {} backend does not support version rollback.",
            backend.name()
        )));
    }

    let name = resolved_name.as_str();
    if !confirm_destructive(
        force,
        &format!("Roll back secret '{name}' to version {version}?"),
    )? {
        output::info("Aborted; no rollback performed.");
        return Ok(());
    }
    let props = backend
        .secrets()
        .rollback(&vault_name, name, version)
        .await?;
    output::success(&format!(
        "Successfully rolled back '{}' to version {version}",
        props.original_name
    ));
    invalidate_trait_secret_cache(&config, &backend_name, &vault_name);
    Ok(())
}

/// Resolve rotate's `(backend, backend_name, vault, secret_name)` target,
/// implementing the A4 `--vault` composition semantics.
///
/// With no `--vault`, this is the pre-A4 path: the raw secret `name` resolves
/// through the workspace seam (unqualified → default entry, never searched).
/// With an explicit `--vault`, the flag overrides the degenerate default entry
/// — it resolves to an attached workspace alias's backend+vault when it names
/// one, else a literal vault on the effective backend — and the secret name is
/// taken literally in that vault (never adds an entry, never errors merely for
/// the override).
async fn resolve_rotate_target(
    name: &str,
    vault: Option<String>,
    config: &Config,
    reg: &BackendRegistry,
) -> Result<(Arc<dyn crate::backend::Backend>, String, String, String)> {
    match vault {
        Some(v) => {
            let (ws, ws_registry) =
                crate::cli::helpers::resolve_workspace_and_registry(config).await?;
            let (backend, backend_name, vault_name) =
                crate::cli::helpers::resolve_vault_ref_with_workspace(
                    &v,
                    ws.as_ref(),
                    ws_registry.as_ref(),
                    reg,
                    config,
                )
                .await?;
            Ok((backend, backend_name, vault_name, name.to_string()))
        }
        None => {
            crate::cli::helpers::resolve_workspace_or_default(
                name,
                config,
                crate::workspace::TargetMode::Write,
            )
            .await
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_secret_rotate_direct(
    name: &str,
    vault: Option<String>,
    length: usize,
    charset: CharsetType,
    generator: Option<String>,
    native: bool,
    show_value: bool,
    force: bool,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    // ── Native rotation path (--native) ─────────────────────────────────
    if native {
        return execute_secret_rotate_native(name, vault, force, config, registry).await;
    }

    // Route through the active backend trait so default (client-side) rotation
    // works on every backend; the Azure trait impl delegates to the same ops.
    let reg = registry.ok_or_else(|| {
        CrosstacheError::config(
            "No backend registry available. Run 'xv config show' to check your configuration.",
        )
    })?;

    // Resolution: rotate is a write/destructive verb — unqualified names
    // always target the default entry, never searched. No workspace attached ⇒
    // byte-identical to the pre-workspace path.
    //
    // A4 --vault composition: an explicit `--vault` overrides the degenerate
    // default entry (never adds an entry, never errors merely for the
    // override). It resolves to an attached workspace alias's backend+vault
    // when it matches one, else a literal vault on the effective backend; the
    // secret name is then used literally in that vault. Without the flag,
    // behavior is unchanged.
    let (backend, backend_name, vault_name, resolved_name) =
        resolve_rotate_target(name, vault, &config, reg).await?;
    let local_registry = BackendRegistry::new(backend);

    execute_secret_rotate(
        &local_registry,
        &backend_name,
        &resolved_name,
        Some(vault_name.clone()),
        length,
        charset,
        generator,
        show_value,
        force,
        &config,
    )
    .await?;

    // Invalidate the secrets list cache for the resolved vault. Must use
    // the RESOLVED workspace entry's registry name (`backend_name`), not
    // `local_registry.active().name()` — the latter is the backend's
    // hardcoded KIND (e.g. "local"), which silently invalidates the wrong
    // `(backend, vault)` cache path whenever the entry's registry name
    // differs from its kind (any named backend) — Bugbot review.
    let cache_manager = crate::cache::CacheManager::from_config(&config);
    cache_manager.invalidate(&crate::cache::CacheKey::SecretsList {
        backend: backend_name,
        vault_name,
    });

    Ok(())
}

/// Capability error for `xv rotate --native` on a backend without native
/// rotation support.
fn rotate_native_unsupported_error(backend_name: &str) -> CrosstacheError {
    CrosstacheError::InvalidArgument(format!(
        "The {backend_name} backend does not support native rotation. Native rotation is \
         currently available on the aws backend only; without --native, 'xv rotate' generates \
         a new value client-side on any backend."
    ))
}

/// Trigger the backend's native rotation mechanism (`xv rotate --native`).
///
/// Unlike the default rotate path (which generates a new value client-side
/// and writes it as a new version), this delegates rotation entirely to the
/// backend — on AWS, `RotateSecret` invokes the rotation Lambda configured
/// on the secret and completes asynchronously.
async fn execute_secret_rotate_native(
    name: &str,
    vault: Option<String>,
    force: bool,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    use crate::utils::interactive::InteractivePrompt;

    // Capability check: native rotation requires backend support. When the
    // registry is missing (the requested backend failed to initialize),
    // resolve the requested backend from config so non-rotation backends
    // still get the capability hint instead of a generic config error.
    let Some(reg) = registry else {
        if let Some(kind) = crate::cli::helpers::requested_backend_kind(&config) {
            if kind != BackendKind::Aws {
                return Err(rotate_native_unsupported_error(
                    config.effective_backend_name(),
                ));
            }
        }
        return Err(CrosstacheError::config(
            "No backend registry available. Run 'xv config show' to check your configuration.",
        ));
    };

    // Resolution: native rotate is a write/destructive verb — unqualified
    // names always target the default entry, never searched. No workspace
    // attached ⇒ byte-identical to the pre-workspace path. A4 --vault
    // composition applies the same way it does for the default rotate path
    // (see `resolve_rotate_target`).
    let (backend, backend_name, vault_name, resolved_name) =
        resolve_rotate_target(name, vault, &config, reg).await?;
    let name = resolved_name.as_str();

    // Capability check: native rotation requires backend support. Gated on
    // the RESOLVED target's backend (Bugbot round-3 fix), not the
    // process's top-level active backend. No workspace attached ⇒ resolved
    // backend == active backend, so this check is unchanged there.
    if !backend.capabilities().has_secret_rotation {
        return Err(rotate_native_unsupported_error(backend.name()));
    }

    // Confirm rotation unless force flag is used (mirrors the default path)
    if !force {
        let prompt = InteractivePrompt::new();
        let confirm = prompt.confirm(
            &format!(
                "Are you sure you want to trigger native rotation for secret '{name}'? \
                 This invokes the rotation mechanism configured on the backend."
            ),
            false,
        )?;

        if !confirm {
            println!("Rotation cancelled.");
            return Ok(());
        }
    }

    output::step(&format!("Requesting native rotation for secret: {name}"));
    backend.secrets().native_rotate(&vault_name, name).await?;

    output::success(&format!("Rotation request accepted for secret '{name}'"));
    println!(
        "Rotation runs asynchronously — the backend's rotation function creates the new \
         version once it completes."
    );
    output::hint(&format!(
        "Use 'xv history {name}' to check for the new version"
    ));

    // Invalidate the secrets list cache for the resolved vault
    invalidate_trait_secret_cache(&config, &backend_name, &vault_name);

    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_secret_run_direct(
    vault: Option<String>,
    group: Vec<String>,
    include: Vec<String>,
    exclude: Vec<String>,
    no_masking: bool,
    inherit_env: bool,
    best_effort: bool,
    command: Vec<String>,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    // Route through the active backend trait so `xv run` works on every backend
    // (azure/local/aws). The Azure trait impl delegates to the same secret ops
    // the legacy path used, so Azure behaviour is unchanged.
    let reg = registry.ok_or_else(|| {
        CrosstacheError::config(
            "No backend registry available. Run 'xv config show' to check your configuration.",
        )
    })?;

    execute_secret_run(
        reg,
        vault,
        group,
        include,
        exclude,
        no_masking,
        inherit_env,
        best_effort,
        command,
        &config,
    )
    .await
}

pub(crate) async fn execute_secret_inject_direct(
    vault: Option<String>,
    template: Option<String>,
    out: Option<String>,
    group: Vec<String>,
    best_effort: bool,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    // Route through the active backend trait so `xv inject` works on every
    // backend (azure/local/aws), not just Azure.
    let reg = registry.ok_or_else(|| {
        CrosstacheError::config(
            "No backend registry available. Run 'xv config show' to check your configuration.",
        )
    })?;

    execute_secret_inject(reg, vault, template, out, group, best_effort, &config).await
}

/// Like [`crate::cli::helpers::confirm_proceed`], but exits with code 3
/// (`CrosstacheError::config`) instead of 2 — record-types plan Task 9
/// requires `xv update --untype` (when it would drop non-primary secret
/// fields) to exit 3 without `--yes` on a non-interactive session,
/// consistent with every other record-types validation failure, even
/// though it mirrors `mv`'s bulk-confirm *behavior* (prompt unless `--yes`,
/// hard-fail without a TTY).
fn confirm_record_action(yes: bool, prompt: &str) -> Result<bool> {
    use std::io::IsTerminal;

    if yes {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        return Err(CrosstacheError::config(format!(
            "Refusing to proceed without confirmation in a non-interactive session ({prompt}). \
             Re-run with --yes to confirm."
        )));
    }
    crate::utils::interactive::InteractivePrompt::new().confirm(prompt, false)
}

/// Non-reserved, non-`f.*` user tag entries of `tags` — the same "everything
/// else" set the tag-budget check counts as `user_tags` on `xv set --type`.
fn user_tags_of(tags: &std::collections::HashMap<String, String>) -> BTreeMap<String, String> {
    tags.iter()
        .filter(|(k, _)| {
            let k = k.as_str();
            k != TYPE_TAG
                && !k.starts_with(FIELD_TAG_PREFIX)
                && k != crate::backend::TAG_ORIGINAL_NAME
                && k != crate::backend::TAG_CREATED_BY
                && k != "groups"
                && k != "note"
                && k != "folder"
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// `xv update <name> --field NAME=VALUE [--field-secret NAME=VALUE ...]`
/// (record-types plan Task 8). A metadata-field change is tag-only
/// (no new version); a secret-field change fetches the envelope, merges,
/// re-encodes, and writes a new version. Both re-run the tag budget with
/// the same collision/required-field rules as `xv set --type`.
async fn execute_record_field_update(
    name: &str,
    fields: &[(String, String)],
    secret_fields: &[(String, String)],
    vault_name: &str,
    config: &Config,
    reg: &BackendRegistry,
    backend_name: &str,
) -> Result<()> {
    let secret = reg
        .active()
        .secrets()
        .get_secret(vault_name, name, true)
        .await?;
    if !crate::records::is_record(&secret.content_type) {
        return Err(CrosstacheError::config(format!(
            "secret '{name}' is not a typed record (value is not marked {}); --field/--field-secret \
             only apply to typed records. Use 'xv update {name} --type <type>' to convert it.",
            crate::records::RECORD_CONTENT_TYPE
        )));
    }

    let type_name = secret.tags.get(TYPE_TAG).cloned().unwrap_or_default();
    let types = config.resolve_record_types().await?;
    let Some(record_type) = find_type(&types, &type_name) else {
        return Err(CrosstacheError::config(format!(
            "secret '{name}' has type '{type_name}', which has no resolvable type definition \
             (check your [types.*] config); --field/--field-secret can't be validated against it."
        )));
    };

    let (metadata_updates, secret_updates) = route_fields(record_type, fields, secret_fields)?;

    // Required-field-emptying guard, matching `xv set --type`'s
    // has_non_blank_value rule: an explicit empty/whitespace value on a
    // required field isn't meaningfully "set".
    for (field_name, value) in metadata_updates.iter().chain(secret_updates.iter()) {
        if let Some(def) = record_type.field(field_name) {
            if def.required && value.trim().is_empty() {
                return Err(CrosstacheError::config(format!(
                    "field '{field_name}' is required for type '{type_name}' and cannot be set to \
                     an empty value"
                )));
            }
        }
    }

    let props = apply_record_field_changes(
        name,
        &secret,
        &metadata_updates,
        &secret_updates,
        None,
        vault_name,
        config,
        reg.active(),
        backend_name,
    )
    .await?;
    output::success(&format!(
        "Successfully updated field(s) on record '{}'",
        props.original_name
    ));
    Ok(())
}

/// Shared record write-back machinery: given a `secret` already known to be
/// a record, applies `metadata_updates`
/// (tag-only, no new version) and/or `secret_updates` (rewrites the
/// envelope, writes a new version), re-running the same tag-budget check
/// `xv set --type` uses. Used by `--field`/`--field-secret` edits
/// (`execute_record_field_update`) and by bare-value writes against a
/// record's primary field — `xv update <name> <value>`/`--stdin` and
/// `xv rotate <name>` (`execute_record_primary_update`) — so all three
/// paths share one write-back implementation instead of drifting apart
/// (record-types plan; fixes #330).
///
/// Tags + content type + replace_tags/replace_groups + groups/note/
/// folder. Two distinct write shapes:
///
/// - Secret-field edit (`secret_updates` non-empty): this forces the
///   full-PUT path on Azure's `update_secret` (it only takes the lighter
///   attributes-only PATCH when `request.value.is_none()` — see
///   `AzureSecretBackendCompat::update_secret`, src/backend/azure/secrets.rs
///   ~L214-265). The full-PUT branch writes `content_type`/`tags` from the
///   request EXACTLY as given — `None` there does not mean "leave
///   unchanged" the way it does on the attributes-only PATCH; it means
///   "no content type" / "rebuild tags from empty" respectively. Relying
///   on backend merge-on-`None` semantics here silently destroyed the
///   record's content-type marker and dropped xv-type/f.*/groups/etc.
///   (Bugbot review, round 1). So whenever a secret-field edit is in
///   play we build the COMPLETE desired tag map ourselves (existing tags
///   + the metadata overlay) and set `replace_tags: true`, exactly like
///     `execute_record_type_conversion`/`execute_record_untype` already do
///     — never a partial delta.
///
///   That full-PUT path ALSO does not treat `groups: None` as
///   "unchanged": Azure's `prepare_secret_request` (src/secret/manager.rs)
///   only re-adds the `groups` tag when `SecretRequest.groups` is `Some`,
///   so sending `None` after stripping the denormalized `groups` key out
///   of the tags map above silently ERASED group membership on Azure —
///   `note`/`folder` happened to survive only because they ride
///   `FieldUpdate::Unchanged`, which Azure's translation layer explicitly
///   re-fetches and carries forward before reaching `prepare_secret_
///   request`; `groups` has no such tri-state fallback (Bugbot review,
///   round 3). Fix: use the tuple `split_denormalized_tags` returns —
///   `groups` goes into the request's dedicated field as `Some(vec)`
///   with `replace_groups: true` (exact carry-forward, mirrors
///   `rename_request_from_properties`'s use of the same tuple); `note`/
///   `folder` switch from `Unchanged` to `Set`-when-known so they no
///   longer rely on any backend's "Unchanged re-fetches current"
///   behavior specifically — they're asserted directly from the same
///   `secret` this function already fetched.
/// - Metadata-only edit (`secret_updates` empty): stays on Azure's
///   attributes-only PATCH, which already merges a partial tag delta
///   against the current tags itself (`build_patched_tags`) and already
///   correctly leaves groups/note/folder alone when their request fields
///   are None/Unchanged — untouched, still correct.
///
/// `enabled_override` is threaded straight into the request's `enabled`
/// field: `None` leaves the current enabled state alone (the right default
/// for `--field`/`--field-secret` edits and for bare-value `update`, which
/// don't force re-enabling on untyped secrets either); `xv rotate` passes
/// `Some(true)` so a record's rotate re-enables it exactly like the classic
/// (untyped) rotate path already does — see the call site in
/// `execute_secret_rotate` for the asymmetry this closes (Bugbot review).
#[allow(clippy::too_many_arguments)]
async fn apply_record_field_changes(
    name: &str,
    secret: &crate::secret::manager::SecretProperties,
    metadata_updates: &BTreeMap<String, String>,
    secret_updates: &BTreeMap<String, String>,
    enabled_override: Option<bool>,
    vault_name: &str,
    config: &Config,
    backend: &dyn crate::backend::Backend,
    backend_name: &str,
) -> Result<crate::secret::manager::SecretProperties> {
    let mut new_value: Option<Zeroizing<String>> = None;
    if !secret_updates.is_empty() {
        let raw = secret.value.as_deref().map(|s| s.as_str()).unwrap_or("");
        let mut envelope = parse_record_envelope_or_fail(name, &secret.content_type, raw)?;
        for (k, v) in secret_updates {
            envelope.insert(k.clone(), v.clone());
        }
        new_value = Some(Zeroizing::new(encode_envelope(&envelope)?));
    }

    let (new_tags, content_type, replace_tags, groups, note, folder, replace_groups) =
        if !secret_updates.is_empty() {
            let mut full = secret.tags.clone();
            for (k, v) in metadata_updates {
                full.insert(format!("{FIELD_TAG_PREFIX}{k}"), v.clone());
            }
            // `secret.tags` is DENORMALIZED for display (groups/note/folder
            // folded into plain keys by every backend's get_secret) — strip
            // them before this map becomes a literal `replace_tags: true`
            // write, or they'd land as extra plain user tags on AWS / the
            // wrong SecretMeta field on local (Bugbot review, round 2; same
            // helper `rename_request_from_properties` already relies on).
            let (groups, note, folder) = crate::backend::secret::split_denormalized_tags(&mut full);
            (
                Some(full),
                Some(RECORD_CONTENT_TYPE.to_string()),
                true,
                groups,
                note.map(crate::secret::manager::FieldUpdate::Set)
                    .unwrap_or(crate::secret::manager::FieldUpdate::Unchanged),
                folder
                    .map(crate::secret::manager::FieldUpdate::Set)
                    .unwrap_or(crate::secret::manager::FieldUpdate::Unchanged),
                true,
            )
        } else if !metadata_updates.is_empty() {
            let mut t = std::collections::HashMap::new();
            for (k, v) in metadata_updates {
                t.insert(format!("{FIELD_TAG_PREFIX}{k}"), v.clone());
            }
            (
                Some(t),
                None,
                false,
                None,
                crate::secret::manager::FieldUpdate::Unchanged,
                crate::secret::manager::FieldUpdate::Unchanged,
                false,
            )
        } else {
            (
                None,
                None,
                false,
                None,
                crate::secret::manager::FieldUpdate::Unchanged,
                crate::secret::manager::FieldUpdate::Unchanged,
                false,
            )
        };

    // Re-run the tag budget: projected f.* tags (existing, minus any this
    // write overwrites, plus the new values) + existing user tags. This
    // write never adds a *new* user tag, but a field update can still push
    // an existing f.* tag over the backend's per-value length cap.
    let mut projected_field_tags: BTreeMap<String, String> = secret
        .tags
        .iter()
        .filter_map(|(k, v)| {
            k.strip_prefix(FIELD_TAG_PREFIX)
                .map(|f| (f.to_string(), v.clone()))
        })
        .collect();
    for (k, v) in metadata_updates {
        projected_field_tags.insert(k.clone(), v.clone());
    }
    let user_tags = user_tags_of(&secret.tags);
    let reserved_count = crate::records::predicted_reserved_tag_count(
        backend.kind(),
        true,
        secret.tags.contains_key("groups"),
        secret.tags.contains_key("note"),
        secret.tags.contains_key("folder"),
        secret.expires_on.is_some(),
    );
    crate::records::check_tag_budget(
        &backend.capabilities(),
        reserved_count,
        &projected_field_tags,
        &user_tags,
    )?;

    let request = crate::secret::manager::SecretUpdateRequest {
        name: name.to_string(),
        value: new_value,
        content_type,
        enabled: enabled_override,
        expires_on: crate::secret::manager::FieldUpdate::Unchanged,
        not_before: crate::secret::manager::FieldUpdate::Unchanged,
        tags: new_tags,
        groups,
        note,
        folder,
        replace_tags,
        replace_groups,
    };
    let props = backend
        .secrets()
        .update_secret(vault_name, name, request)
        .await?;
    invalidate_trait_secret_cache(config, backend_name, vault_name);
    Ok(props)
}

/// Resolves the record's declared primary field, erroring with guidance
/// when the type can't be resolved — the primary field name is unknowable
/// in that case, so a bare-value write can't safely target it (record-types
/// plan; fixes #330).
fn resolve_primary_field<'a>(
    name: &str,
    secret: &crate::secret::manager::SecretProperties,
    types: &'a [RecordType],
) -> Result<&'a RecordType> {
    let type_name = secret.tags.get(TYPE_TAG).cloned().unwrap_or_default();
    find_type(types, &type_name).ok_or_else(|| {
        CrosstacheError::config(format!(
            "secret '{name}' has type '{type_name}', which has no resolvable type definition \
             (check your [types.*] config); its primary field can't be determined, so a bare-value \
             write can't set it safely. Use 'xv get {name} --record' to inspect its raw fields, or \
             fix the type definition, then retry."
        ))
    })
}

/// `xv update <name> <value>` / `xv update <name> --stdin` against a typed
/// record: sets the primary field inside the envelope instead of
/// overwriting the whole secret value (which would corrupt the envelope —
/// see #330). Also used by `xv rotate <name>` on a record, so the generated
/// value becomes the new primary the same way. Reuses
/// `apply_record_field_changes`, so tags/groups/note/folder and every other
/// envelope field are preserved exactly like a `--field`/`--field-secret`
/// edit.
#[allow(clippy::too_many_arguments)]
async fn execute_record_primary_update(
    name: &str,
    new_primary_value: &str,
    secret: &crate::secret::manager::SecretProperties,
    enabled_override: Option<bool>,
    vault_name: &str,
    config: &Config,
    reg: &BackendRegistry,
    backend_name: &str,
) -> Result<crate::secret::manager::SecretProperties> {
    let types = config.resolve_record_types().await?;
    let record_type = resolve_primary_field(name, secret, &types)?;
    let primary_name = record_type.primary().name.clone();
    if new_primary_value.trim().is_empty() {
        return Err(CrosstacheError::config(format!(
            "the primary field '{primary_name}' of type '{}' is required and cannot be set to an \
             empty value",
            record_type.name
        )));
    }
    let mut secret_updates = BTreeMap::new();
    secret_updates.insert(primary_name, new_primary_value.to_string());
    apply_record_field_changes(
        name,
        secret,
        &BTreeMap::new(),
        &secret_updates,
        enabled_override,
        vault_name,
        config,
        reg.active(),
        backend_name,
    )
    .await
}

/// `xv update <name> --type <type>` (record-types plan Task 9): explicit
/// conversion of a bare secret into a typed record. The current value
/// becomes the primary field; existing groups/note/folder/user tags are
/// untouched. Errors if the secret is already a record.
async fn execute_record_type_conversion(
    name: &str,
    type_name: &str,
    vault_name: &str,
    config: &Config,
    backend: &dyn crate::backend::Backend,
    backend_name: &str,
) -> Result<()> {
    let secret = backend.secrets().get_secret(vault_name, name, true).await?;
    if crate::records::is_record(&secret.content_type) {
        return Err(CrosstacheError::config(format!(
            "secret '{name}' is already a typed record (type: {}); use --field/--field-secret to \
             edit it, or --untype first to convert it back to a bare secret.",
            secret.tags.get(TYPE_TAG).cloned().unwrap_or_default()
        )));
    }

    let types = config.resolve_record_types().await?;
    let Some(record_type) = find_type(&types, type_name) else {
        let mut known: Vec<&str> = types.iter().map(|t| t.name.as_str()).collect();
        known.sort_unstable();
        return Err(CrosstacheError::config(format!(
            "unknown type '{type_name}'. Known types: {}",
            known.join(", ")
        )));
    };

    let current_value = secret.value.clone().unwrap_or_default();
    if current_value.is_empty() {
        return Err(CrosstacheError::config(format!(
            "secret '{name}' has no value to convert"
        )));
    }

    let mut envelope = BTreeMap::new();
    envelope.insert(
        record_type.primary().name.clone(),
        current_value.as_str().to_string(),
    );
    let envelope_value = encode_envelope(&envelope)?;

    // Tag budget: existing tags (unchanged) + the new xv-type tag. No f.*
    // fields are added by a bare `--type` conversion (only the primary is
    // set, and the primary never gets an f.* tag).
    let user_tags = user_tags_of(&secret.tags);
    let reserved_count = crate::records::predicted_reserved_tag_count(
        backend.kind(),
        true,
        secret.tags.contains_key("groups"),
        secret.tags.contains_key("note"),
        secret.tags.contains_key("folder"),
        secret.expires_on.is_some(),
    );
    crate::records::check_tag_budget(
        &backend.capabilities(),
        reserved_count,
        &BTreeMap::new(),
        &user_tags,
    )?;

    let mut new_tags = secret.tags.clone();
    // Strip denormalized groups/note/folder (see
    // `split_denormalized_tags`'s doc comment) before this becomes a
    // `replace_tags: true` write, and USE the extracted values via the
    // request's dedicated fields (`groups: Some(vec)` +
    // `replace_groups: true`, `note`/`folder` as `Set`-when-known) rather
    // than `None`/`Unchanged` — a value-changing update takes Azure's
    // full-PUT path, which does not treat `groups: None` as "leave
    // unchanged" the way local/AWS's delta model does: `prepare_secret_
    // request` only re-adds the `groups` tag when the request field is
    // `Some`, so `None` here previously erased group membership on Azure
    // (Bugbot review, round 3).
    let (groups, note, folder) = crate::backend::secret::split_denormalized_tags(&mut new_tags);
    new_tags.insert(TYPE_TAG.to_string(), record_type.name.clone());

    let request = crate::secret::manager::SecretUpdateRequest {
        name: name.to_string(),
        value: Some(Zeroizing::new(envelope_value)),
        content_type: Some(RECORD_CONTENT_TYPE.to_string()),
        enabled: None,
        expires_on: crate::secret::manager::FieldUpdate::Unchanged,
        not_before: crate::secret::manager::FieldUpdate::Unchanged,
        tags: Some(new_tags),
        groups,
        note: note
            .map(crate::secret::manager::FieldUpdate::Set)
            .unwrap_or(crate::secret::manager::FieldUpdate::Unchanged),
        folder: folder
            .map(crate::secret::manager::FieldUpdate::Set)
            .unwrap_or(crate::secret::manager::FieldUpdate::Unchanged),
        replace_tags: true,
        replace_groups: true,
    };
    let props = backend
        .secrets()
        .update_secret(vault_name, name, request)
        .await?;
    output::success(&format!(
        "Successfully converted '{}' to type '{}'",
        props.original_name, record_type.name
    ));
    invalidate_trait_secret_cache(config, backend_name, vault_name);
    Ok(())
}

/// `xv update <name> --untype` (record-types plan Task 9): flattens a
/// typed record back to a bare secret holding the primary field's value.
/// Non-primary secret fields are dropped with an interactive confirmation
/// (or `--yes`); metadata fields are removed from tags. Non-TTY without
/// `--yes` when fields would be dropped exits 3.
async fn execute_record_untype(
    name: &str,
    yes: bool,
    vault_name: &str,
    config: &Config,
    reg: &BackendRegistry,
    backend_name: &str,
) -> Result<()> {
    let secret = reg
        .active()
        .secrets()
        .get_secret(vault_name, name, true)
        .await?;
    if !crate::records::is_record(&secret.content_type) {
        return Err(CrosstacheError::config(format!(
            "secret '{name}' is not a typed record; nothing to untype."
        )));
    }

    let type_name = secret.tags.get(TYPE_TAG).cloned().unwrap_or_default();
    let raw = secret.value.as_deref().map(|s| s.as_str()).unwrap_or("");
    let envelope = parse_record_envelope_or_fail(name, &secret.content_type, raw)?;

    let types = config.resolve_record_types().await?;
    let Some(record_type) = find_type(&types, &type_name) else {
        return Err(CrosstacheError::config(format!(
            "secret '{name}' has type '{type_name}', which has no resolvable type definition \
             (check your [types.*] config); its primary field can't be determined, so it can't be \
             untyped automatically. Use 'xv get {name} --record' to inspect its raw fields."
        )));
    };

    let primary_name = &record_type.primary().name;
    let Some(primary_value) = envelope.get(primary_name) else {
        return Err(CrosstacheError::config(format!(
            "secret '{name}' is missing its primary field '{primary_name}' in the record envelope"
        )));
    };
    let primary_value = primary_value.clone();

    let mut dropped: Vec<String> = envelope
        .keys()
        .filter(|k| *k != primary_name)
        .cloned()
        .collect();
    dropped.sort();

    if !dropped.is_empty() {
        let prompt = format!(
            "Untyping '{name}' will permanently drop {} non-primary secret field(s): {}. Continue?",
            dropped.len(),
            dropped.join(", ")
        );
        if !confirm_record_action(yes, &prompt)? {
            output::info("Aborted; secret not untyped.");
            return Ok(());
        }
        output::warn(&format!("Dropped field(s): {}", dropped.join(", ")));
    }

    let mut new_tags = secret.tags.clone();
    new_tags.remove(TYPE_TAG);
    new_tags.retain(|k, _| !k.starts_with(FIELD_TAG_PREFIX));
    // Strip denormalized groups/note/folder — see
    // `split_denormalized_tags`'s doc comment — and USE the extracted
    // values via the request's dedicated fields. Untyping is a
    // value-changing update (Azure's full-PUT path), which does not treat
    // `groups: None` as "leave unchanged": `prepare_secret_request` only
    // re-adds the `groups` tag when the request field is `Some`, so
    // `None` here previously erased group membership on Azure (Bugbot
    // review, round 3).
    let (groups, note, folder) = crate::backend::secret::split_denormalized_tags(&mut new_tags);

    let request = crate::secret::manager::SecretUpdateRequest {
        name: name.to_string(),
        value: Some(Zeroizing::new(primary_value)),
        content_type: Some(String::new()),
        enabled: None,
        expires_on: crate::secret::manager::FieldUpdate::Unchanged,
        not_before: crate::secret::manager::FieldUpdate::Unchanged,
        tags: Some(new_tags),
        groups,
        note: note
            .map(crate::secret::manager::FieldUpdate::Set)
            .unwrap_or(crate::secret::manager::FieldUpdate::Unchanged),
        folder: folder
            .map(crate::secret::manager::FieldUpdate::Set)
            .unwrap_or(crate::secret::manager::FieldUpdate::Unchanged),
        replace_tags: true,
        replace_groups: true,
    };
    let props = reg
        .active()
        .secrets()
        .update_secret(vault_name, name, request)
        .await?;
    output::success(&format!(
        "Successfully untyped '{}' (was type '{type_name}')",
        props.original_name
    ));
    invalidate_trait_secret_cache(config, backend_name, vault_name);
    Ok(())
}

/// Every classic (non-record) `xv update` metadata flag that was actually
/// supplied, in CLI-flag form, for the "combining a bare value with a
/// classic flag on a record" rejection below (Bugbot review MAJOR: this
/// combination used to silently drop every one of these instead of
/// applying or rejecting them). Mirrors the flag set clap's
/// `conflicts_with_all` already rejects on `--field`/`--field-secret`/
/// `--type`/`--untype` — a bare-value record write is the same kind of
/// standalone operation, just not clap-expressible (clap can't make a
/// positional/`--stdin` value conflict with itself).
#[allow(clippy::too_many_arguments)]
fn classic_flags_present(
    tags: &[(String, String)],
    groups: &[String],
    rename: &Option<String>,
    note: &Option<String>,
    folder: &Option<String>,
    replace_tags: bool,
    replace_groups: bool,
    expires: &Option<String>,
    not_before: &Option<String>,
    clear_expires: bool,
    clear_not_before: bool,
    clear_note: bool,
    clear_folder: bool,
    enabled: Option<bool>,
) -> Vec<&'static str> {
    let mut present = Vec::new();
    if !tags.is_empty() {
        present.push("--tags");
    }
    if !groups.is_empty() {
        present.push("--group");
    }
    if rename.is_some() {
        present.push("--rename");
    }
    if note.is_some() {
        present.push("--note");
    }
    if folder.is_some() {
        present.push("--folder");
    }
    if replace_tags {
        present.push("--replace-tags");
    }
    if replace_groups {
        present.push("--replace-groups");
    }
    if expires.is_some() {
        present.push("--expires");
    }
    if not_before.is_some() {
        present.push("--not-before");
    }
    if clear_expires {
        present.push("--clear-expires");
    }
    if clear_not_before {
        present.push("--clear-not-before");
    }
    if clear_note {
        present.push("--clear-note");
    }
    if clear_folder {
        present.push("--clear-folder");
    }
    if enabled.is_some() {
        present.push("--enabled");
    }
    present
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_secret_update_direct(
    name: &str,
    value: Option<String>,
    stdin: bool,
    trim: bool,
    tags: Vec<(String, String)>,
    groups: Vec<String>,
    rename: Option<String>,
    note: Option<String>,
    folder: Option<String>,
    replace_tags: bool,
    replace_groups: bool,
    expires: Option<String>,
    not_before: Option<String>,
    clear_expires: bool,
    clear_not_before: bool,
    clear_note: bool,
    clear_folder: bool,
    enabled: Option<bool>,
    fields: Vec<(String, String)>,
    secret_fields: Vec<(String, String)>,
    type_name: Option<String>,
    untype: bool,
    yes: bool,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    // ── Trait-based path (non-Azure backends) ──────────────────────────
    if use_trait_path(registry) {
        use crate::secret::manager::FieldUpdate;
        use crate::utils::datetime::parse_datetime_or_duration;

        // Workspace-aware resolution (unqualified → default vault, never
        // searched; `alias:name` targets that vault directly). No workspace
        // attached ⇒ byte-identical to the pre-workspace path. Resolved once
        // up front and reused by EVERY sub-path below (record conversion,
        // untype, field update, bare-value-on-record, and the classic
        // metadata update) via a single-backend registry wrapping the
        // resolved backend, so none of those helpers need to change shape.
        let (resolved_backend, backend_name, vault_name, resolved_name) =
            crate::cli::helpers::resolve_workspace_or_default(
                name,
                &config,
                crate::workspace::TargetMode::Write,
            )
            .await?;
        let name = resolved_name.as_str();
        let local_registry = BackendRegistry::new(resolved_backend);
        let reg = &local_registry;

        // Record-types edit/conversion paths (record-types plan Tasks 8/9)
        // take over completely — clap's `conflicts_with_all` on --type/
        // --untype/--field/--field-secret already guarantees at most one
        // of these fires alongside the classic metadata-update flags below.
        {
            if let Some(type_name) = type_name {
                return execute_record_type_conversion(
                    name,
                    &type_name,
                    &vault_name,
                    &config,
                    reg.active(),
                    &backend_name,
                )
                .await;
            }
            if untype {
                return execute_record_untype(name, yes, &vault_name, &config, reg, &backend_name)
                    .await;
            }
            if !fields.is_empty() || !secret_fields.is_empty() {
                return execute_record_field_update(
                    name,
                    &fields,
                    &secret_fields,
                    &vault_name,
                    &config,
                    reg,
                    &backend_name,
                )
                .await;
            }

            // Bare-value write (`xv update <name> <value>` / `--stdin`)
            // against a typed record: set the primary field inside the
            // envelope instead of overwriting the whole secret value, which
            // would leave `content_type` claiming a record while the value
            // is a bare string (#330). Untyped secrets fall through
            // untouched to the classic path below.
            if value.is_some() || stdin {
                // Metadata-only probe (Bugbot review MINOR): checking
                // `is_record` never needs the plaintext value, so the
                // (common, untyped) case that falls through to the classic
                // path below never pays for a decrypt/fetch of a value it
                // then discards. Only a confirmed record pays for the
                // second, value-including fetch below.
                let probe = reg
                    .active()
                    .secrets()
                    .get_secret(&vault_name, name, false)
                    .await?;
                if crate::records::is_record(&probe.content_type) {
                    // Bugbot review MAJOR: this branch used to apply the
                    // primary-field write and `return Ok(())` unconditionally,
                    // silently discarding every classic metadata flag
                    // (--note/--group/--tags/--rename/--expires/--not-before/
                    // --enabled/--folder/--clear-*) supplied alongside the
                    // bare value — reproduced against --note/--group/--rename.
                    // A record's primary-field write is a standalone
                    // operation in v1 (matching --field/--field-secret's own
                    // `conflicts_with_all`, which clap can't extend to a
                    // positional/--stdin value), so reject loud instead,
                    // naming every flag actually present.
                    let extra_flags = classic_flags_present(
                        &tags,
                        &groups,
                        &rename,
                        &note,
                        &folder,
                        replace_tags,
                        replace_groups,
                        &expires,
                        &not_before,
                        clear_expires,
                        clear_not_before,
                        clear_note,
                        clear_folder,
                        enabled,
                    );
                    if !extra_flags.is_empty() {
                        return Err(CrosstacheError::invalid_argument(format!(
                            "secret '{name}' is a typed record; a bare-value update/--stdin can't be \
                             combined with {} in v1 — a record's primary-field write is a standalone \
                             operation (same rule as --field/--field-secret). Run 'xv update {name} \
                             <value>' (or --stdin) and a separate 'xv update {name} ...' for the \
                             metadata instead.",
                            extra_flags.join(", ")
                        )));
                    }

                    let resolved_value = if stdin {
                        let stdin_value = read_secret_value_from_stdin(trim)?;
                        if stdin_value.is_empty() {
                            return Err(CrosstacheError::config("Secret value cannot be empty"));
                        }
                        stdin_value
                    } else {
                        value.clone().unwrap_or_default()
                    };
                    let existing = reg
                        .active()
                        .secrets()
                        .get_secret(&vault_name, name, true)
                        .await?;
                    let props = execute_record_primary_update(
                        name,
                        &resolved_value,
                        &existing,
                        None,
                        &vault_name,
                        &config,
                        reg,
                        &backend_name,
                    )
                    .await?;
                    output::success(&format!(
                        "Successfully updated secret '{}'",
                        props.original_name
                    ));
                    return Ok(());
                }
            }
        }

        // Parse value from stdin if requested
        let resolved_value = if stdin {
            let stdin_value = read_secret_value_from_stdin(trim)?;
            if stdin_value.is_empty() {
                return Err(CrosstacheError::config("Secret value cannot be empty"));
            }
            Some(Zeroizing::new(stdin_value))
        } else {
            value.map(Zeroizing::new)
        };

        // Tri-state metadata updates: omitted = Unchanged, value = Set, --clear-* = Clear
        let expires_update = FieldUpdate::from_flags(
            expires
                .as_deref()
                .map(parse_datetime_or_duration)
                .transpose()?,
            clear_expires,
            "expiration date",
        )?;
        let not_before_update = FieldUpdate::from_flags(
            not_before
                .as_deref()
                .map(parse_datetime_or_duration)
                .transpose()?,
            clear_not_before,
            "not-before date",
        )?;
        let note_update = FieldUpdate::from_flags(note, clear_note, "note")?;
        let folder_update = FieldUpdate::from_flags(folder, clear_folder, "folder")?;

        let merged_tags = if tags.is_empty() {
            None
        } else {
            Some(
                tags.into_iter()
                    .collect::<std::collections::HashMap<_, _>>(),
            )
        };
        let merged_groups = if groups.is_empty() {
            None
        } else {
            Some(groups)
        };

        let renaming = rename.is_some();
        let has_other_updates = resolved_value.is_some()
            || merged_tags.is_some()
            || merged_groups.is_some()
            || enabled.is_some()
            || !expires_update.is_unchanged()
            || !not_before_update.is_unchanged()
            || !note_update.is_unchanged()
            || !folder_update.is_unchanged();

        // Apply in-place updates first, under the old name; then rename.
        // `--rename` alone skips the no-op update round-trip, and a bare
        // `xv update NAME` keeps its historical all-unchanged update call.
        if has_other_updates || !renaming {
            let request = crate::secret::manager::SecretUpdateRequest {
                name: name.to_string(),
                value: resolved_value,
                content_type: None,
                enabled,
                expires_on: expires_update,
                not_before: not_before_update,
                tags: merged_tags,
                groups: merged_groups,
                note: note_update,
                folder: folder_update,
                replace_tags,
                replace_groups,
            };
            let props = reg
                .active()
                .secrets()
                .update_secret(&vault_name, name, request)
                .await?;
            output::success(&format!(
                "Successfully updated secret '{}'",
                props.original_name
            ));
            // The in-place update just mutated state (value/tags/groups/etc.);
            // invalidate immediately so a rename-phase failure below can't
            // leave a stale cached list behind.
            invalidate_trait_secret_cache(&config, &backend_name, &vault_name);
        }

        if let Some(ref new_name) = rename {
            let rename_result = reg
                .active()
                .secrets()
                .rename_secret(&vault_name, name, new_name)
                .await;
            // Rename may mutate state even when it errors (e.g. RenameIncomplete:
            // the new name was created but deleting the old one failed), so
            // invalidate unconditionally before inspecting the result.
            invalidate_trait_secret_cache(&config, &backend_name, &vault_name);
            match rename_result {
                Ok(props) => {
                    output::success(&format!(
                        "Successfully renamed secret '{name}' to '{}'",
                        props.original_name
                    ));
                }
                Err(e) => {
                    if has_other_updates {
                        // RenameIncomplete means the new copy WAS created and
                        // only the old-name delete failed — both names exist,
                        // so don't claim the name is unchanged.
                        let msg = if matches!(
                            e,
                            crate::backend::error::BackendError::RenameIncomplete { .. }
                        ) {
                            "the metadata update was applied; the rename did not complete cleanly — both names currently exist (see the error below for recovery)"
                        } else {
                            "the metadata update was applied; the rename did not complete — the secret keeps its original name"
                        };
                        output::warn(msg);
                    }
                    return Err(e.into());
                }
            }
        }

        return Ok(());
    }

    Err(CrosstacheError::config(
        "No backend registry available. Run 'xv config show' to check your configuration.",
    ))
}

pub(crate) async fn execute_secret_purge_direct(
    name: &str,
    force: bool,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    // Every backend — including Azure, whose `SecretBackend` impl covers
    // purge — resolves through the trait path now. When startup backend
    // construction failed the registry is `None`; rebuild it from config so
    // that failure surfaces as a clean error rather than a legacy fallback.
    let rebuilt_registry;
    let _reg = match registry {
        Some(r) => r,
        None => {
            rebuilt_registry = BackendRegistry::from_config(&config)
                .map_err(|e| CrosstacheError::config(e.to_string()))?;
            &rebuilt_registry
        }
    };

    // Workspace-aware resolution: purge is a write/destructive verb —
    // unqualified names always target the default entry, never searched. The
    // capability check and the operation below act on the RESOLVED backend.
    let (backend, backend_name, vault_name, resolved_name) =
        crate::cli::helpers::resolve_workspace_or_default(
            name,
            &config,
            crate::workspace::TargetMode::Write,
        )
        .await?;

    // Capability check: purge requires soft-delete support.
    if !backend.capabilities().has_soft_delete {
        return Err(CrosstacheError::InvalidArgument(format!(
            "The {} backend does not support purge (soft-delete not available).",
            backend.name()
        )));
    }

    let name = resolved_name.as_str();
    if !confirm_destructive(
        force,
        &format!("PERMANENTLY DELETE secret '{name}'? This cannot be undone."),
    )? {
        output::info("Aborted; secret not purged.");
        return Ok(());
    }
    backend.secrets().purge_secret(&vault_name, name).await?;
    output::success(&format!("Successfully purged secret '{name}'"));
    invalidate_trait_secret_cache(&config, &backend_name, &vault_name);
    Ok(())
}

pub(crate) async fn execute_secret_restore_direct(
    name: &str,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    // Every backend — including Azure, whose `SecretBackend` impl covers
    // restore — resolves through the trait path. When startup backend
    // construction failed the registry is `None`; rebuild it from config so
    // that failure surfaces as a clean error rather than a legacy fallback.
    let rebuilt_registry;
    let _reg = match registry {
        Some(r) => r,
        None => {
            rebuilt_registry = BackendRegistry::from_config(&config)
                .map_err(|e| CrosstacheError::config(e.to_string()))?;
            &rebuilt_registry
        }
    };

    // Workspace-aware resolution: restore is a write/destructive verb —
    // unqualified names always target the default entry, never searched.
    let (backend, backend_name, vault_name, resolved_name) =
        crate::cli::helpers::resolve_workspace_or_default(
            name,
            &config,
            crate::workspace::TargetMode::Write,
        )
        .await?;

    // Capability check: restore requires soft-delete support.
    if !backend.capabilities().has_soft_delete {
        return Err(CrosstacheError::InvalidArgument(format!(
            "The {} backend does not support restore (soft-delete not available).",
            backend.name()
        )));
    }

    let props = backend
        .secrets()
        .restore_secret(&vault_name, &resolved_name)
        .await?;
    output::success(&format!(
        "Successfully restored secret '{}'",
        props.original_name
    ));
    invalidate_trait_secret_cache(&config, &backend_name, &vault_name);
    Ok(())
}

pub(crate) async fn execute_diff_command(
    vault1: &str,
    vault2: &str,
    show_values: bool,
    group: Option<String>,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    use std::collections::BTreeSet;

    // Every backend resolves through the trait path now; registry==None
    // rebuilds from config so a startup init failure surfaces as a clean error.
    let rebuilt_registry;
    let reg = match registry {
        Some(r) => r,
        None => {
            rebuilt_registry = BackendRegistry::from_config(&config)
                .map_err(|e| CrosstacheError::config(e.to_string()))?;
            &rebuilt_registry
        }
    };

    // Workspace-aware: each vault argument resolves against attached aliases
    // first, else a literal vault on the active backend (spec §Addressing) —
    // so `xv diff work stage` can span backends, while raw names with no
    // workspace or alias match behave exactly as before.
    let (ws, ws_registry) = crate::cli::helpers::resolve_workspace_and_registry(&config).await?;
    let (backend_a, _backend_a_name, vault1_resolved) =
        crate::cli::helpers::resolve_vault_ref_with_workspace(
            vault1,
            ws.as_ref(),
            ws_registry.as_ref(),
            reg,
            &config,
        )
        .await?;
    let (backend_b, _backend_b_name, vault2_resolved) =
        crate::cli::helpers::resolve_vault_ref_with_workspace(
            vault2,
            ws.as_ref(),
            ws_registry.as_ref(),
            reg,
            &config,
        )
        .await?;

    // List secrets from both vaults
    let secrets_a = backend_a
        .secrets()
        .list_secrets(&vault1_resolved, group.as_deref())
        .await?;
    let secrets_b = backend_b
        .secrets()
        .list_secrets(&vault2_resolved, group.as_deref())
        .await?;

    // Build name sets
    let names_a: BTreeSet<String> = secrets_a.iter().map(|s| s.name.clone()).collect();
    let names_b: BTreeSet<String> = secrets_b.iter().map(|s| s.name.clone()).collect();
    let all_names: BTreeSet<String> = names_a.union(&names_b).cloned().collect();

    // Fetch values from both vaults for comparison
    let mut values_a = std::collections::HashMap::new();
    let mut values_b = std::collections::HashMap::new();

    for name in &names_a {
        match backend_a
            .secrets()
            .get_secret(&vault1_resolved, name, true)
            .await
        {
            Ok(props) => {
                if let Some(val) = props.value {
                    values_a.insert(name.clone(), val);
                }
            }
            Err(e) => {
                output::warn(&format!("Failed to get '{}' from {}: {}", name, vault1, e));
            }
        }
    }

    for name in &names_b {
        match backend_b
            .secrets()
            .get_secret(&vault2_resolved, name, true)
            .await
        {
            Ok(props) => {
                if let Some(val) = props.value {
                    values_b.insert(name.clone(), val);
                }
            }
            Err(e) => {
                output::warn(&format!("Failed to get '{}' from {}: {}", name, vault2, e));
            }
        }
    }

    // Compare and output
    println!("Comparing {} → {}", vault1, vault2);
    println!();

    let mut added = 0u32;
    let mut removed = 0u32;
    let mut changed = 0u32;
    let mut identical = 0u32;

    // Find max name length for alignment
    let max_len = all_names.iter().map(|n| n.len()).max().unwrap_or(0);

    for name in &all_names {
        let in_a = names_a.contains(name);
        let in_b = names_b.contains(name);

        match (in_a, in_b) {
            (false, true) => {
                println!(
                    "  + {:<width$}  (only in {})",
                    name,
                    vault2,
                    width = max_len
                );
                added += 1;
            }
            (true, false) => {
                println!(
                    "  - {:<width$}  (only in {})",
                    name,
                    vault1,
                    width = max_len
                );
                removed += 1;
            }
            (true, true) => {
                let val_a = values_a.get(name);
                let val_b = values_b.get(name);
                if val_a == val_b {
                    println!("  = {:<width$}  (identical)", name, width = max_len);
                    identical += 1;
                } else {
                    println!("  ~ {:<width$}  (value differs)", name, width = max_len);
                    if show_values {
                        let a_str = val_a.map(|v| v.as_str()).unwrap_or("<empty>");
                        let b_str = val_b.map(|v| v.as_str()).unwrap_or("<empty>");
                        println!("      {} : {}", vault1, a_str);
                        println!("      {} : {}", vault2, b_str);
                    }
                    changed += 1;
                }
            }
            (false, false) => unreachable!(),
        }
    }

    println!();
    println!(
        "Summary: {} added, {} removed, {} changed, {} identical",
        added, removed, changed, identical
    );

    Ok(())
}

pub(crate) async fn execute_secret_copy_direct(
    name: &str,
    from_vault: &str,
    to_vault: &str,
    new_name: Option<String>,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    // Every backend resolves through the trait path now; registry==None
    // rebuilds from config so a startup init failure surfaces as a clean error.
    let rebuilt_registry;
    let reg = match registry {
        Some(r) => r,
        None => {
            rebuilt_registry = BackendRegistry::from_config(&config)
                .map_err(|e| CrosstacheError::config(e.to_string()))?;
            &rebuilt_registry
        }
    };

    execute_secret_copy(reg, name, from_vault, to_vault, new_name, false, &config).await?;

    // Invalidate the secrets list cache for both source and destination
    // vaults, each keyed by ITS OWN resolved backend name — a workspace
    // alias may resolve `from_vault`/`to_vault` to DIFFERENT backends, so
    // invalidating both under a single `config.effective_backend_name()`
    // would silently miss the actual cache entries a cross-backend copy
    // touches (the "ONE identifier" convention: see
    // `resolve_workspace_or_default`'s doc comment). Falls back to
    // `config.effective_backend_name()` unchanged when no workspace is
    // attached or the argument isn't an attached alias. Only a REAL
    // (configured) workspace participates in per-side alias resolution:
    // `resolve_configured_workspace` returns `None` with no configured
    // workspace, so the degenerate single-vault case keys the cache exactly as
    // the no-workspace path did (`config.effective_backend_name()`).
    let ws = crate::workspace::resolve_configured_workspace(&config).await?;
    let (from_backend_name, from_vault_resolved) =
        crate::cli::helpers::vault_ref_cache_identity(from_vault, ws.as_ref(), &config);
    let (to_backend_name, to_vault_resolved) =
        crate::cli::helpers::vault_ref_cache_identity(to_vault, ws.as_ref(), &config);
    let cache_manager = crate::cache::CacheManager::from_config(&config);
    cache_manager.invalidate(&crate::cache::CacheKey::SecretsList {
        backend: from_backend_name,
        vault_name: from_vault_resolved,
    });
    cache_manager.invalidate(&crate::cache::CacheKey::SecretsList {
        backend: to_backend_name,
        vault_name: to_vault_resolved,
    });

    Ok(())
}

pub(crate) async fn execute_secret_move_direct(
    name: &str,
    from_vault: &str,
    to_vault: &str,
    new_name: Option<String>,
    force: bool,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    // Every backend resolves through the trait path now; registry==None
    // rebuilds from config so a startup init failure surfaces as a clean error.
    let rebuilt_registry;
    let reg = match registry {
        Some(r) => r,
        None => {
            rebuilt_registry = BackendRegistry::from_config(&config)
                .map_err(|e| CrosstacheError::config(e.to_string()))?;
            &rebuilt_registry
        }
    };

    execute_secret_move(reg, name, from_vault, to_vault, new_name, force, &config).await?;

    // Invalidate the secrets list cache for both source and destination
    // vaults — same reasoning as `execute_secret_copy_direct` above: keyed
    // by `config.effective_backend_name()`, not the backend kind — same
    // per-side alias resolution as `execute_secret_copy_direct` above.
    // `resolve_configured_workspace` returns `None` with no configured
    // workspace, so the degenerate single-vault case keys cache identity
    // byte-identically to the no-workspace path.
    let ws = crate::workspace::resolve_configured_workspace(&config).await?;
    let (from_backend_name, from_vault_resolved) =
        crate::cli::helpers::vault_ref_cache_identity(from_vault, ws.as_ref(), &config);
    let (to_backend_name, to_vault_resolved) =
        crate::cli::helpers::vault_ref_cache_identity(to_vault, ws.as_ref(), &config);
    let cache_manager = crate::cache::CacheManager::from_config(&config);
    cache_manager.invalidate(&crate::cache::CacheKey::SecretsList {
        backend: from_backend_name,
        vault_name: from_vault_resolved,
    });
    cache_manager.invalidate(&crate::cache::CacheKey::SecretsList {
        backend: to_backend_name,
        vault_name: to_vault_resolved,
    });

    Ok(())
}

pub(crate) async fn execute_secret_parse_direct(
    connection_string: &str,
    format: &str,
    config: Config,
    _registry: Option<&BackendRegistry>,
) -> Result<()> {
    // `xv secret parse` is a pure string-parsing command — no backend needed.
    execute_secret_parse(connection_string, format, &config).await
}

pub(crate) async fn execute_secret_share_direct(
    command: ShareCommands,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    // Fast capability pre-gate on the active/requested backend, answered
    // WITHOUT resolving the workspace so a non-RBAC backend rejects immediately.
    // With a registry, check the active backend; without one (startup init
    // failed), answer a non-Azure requested backend from its kind WITHOUT
    // constructing (e.g. AWS returns its IAM guidance instead of a build error).
    // `kind` only selects the message text.
    match registry {
        Some(reg) => {
            let active = reg.active();
            if !active.capabilities().has_rbac {
                return Err(share_unsupported_error(
                    active.kind(),
                    active.name(),
                    "access sharing",
                ));
            }
        }
        None => {
            if let Some(kind) = crate::cli::helpers::requested_backend_kind(&config) {
                if kind != BackendKind::Azure {
                    return Err(share_unsupported_error(
                        kind,
                        config.effective_backend_name(),
                        "access sharing",
                    ));
                }
            }
        }
    }

    // The workspace default entry supplies BOTH the vault name AND the backend
    // the RBAC calls run against — they MUST come from the SAME entry. In a
    // multi-vault workspace the default entry's backend can differ from the
    // process-active backend, so resolving the RBAC client from the active
    // backend (while taking the vault name from the entry) would run
    // grants/revokes/lists against the wrong Key Vault (Bugbot PR #346).
    let ws = crate::workspace::resolve_workspace(&config)
        .await?
        .ok_or_else(|| {
            CrosstacheError::config(
                "internal error: resolve_workspace returned None; the degenerate \
                 workspace-of-one must always yield Some or Err",
            )
        })?;
    let entry = ws.default_entry()?;
    let vault_name = entry.vault.clone();

    // Materialize the default entry's backend, so the RBAC client is the one
    // that owns the entry's vault. Reuse the process registry when it already
    // holds that backend (the common degenerate / single-backend case);
    // otherwise build it from config via a workspace-scoped lazy registry (a
    // configured workspace whose default entry lives on a backend the process
    // registry didn't materialize).
    let backend = match registry.and_then(|reg| reg.materialize(&entry.backend).ok()) {
        Some(b) => b,
        None => {
            let ws_registry =
                BackendRegistry::with_lazy(&config, std::slice::from_ref(&entry.backend))
                    .map_err(|e| CrosstacheError::config(e.to_string()))?;
            ws_registry
                .materialize(&entry.backend)
                .map_err(|e| CrosstacheError::config(e.to_string()))?
        }
    };

    // Second capability gate on the RESOLVED entry backend: a configured
    // workspace's default entry can be a non-RBAC backend even when the active
    // backend supports RBAC.
    if !backend.capabilities().has_rbac {
        return Err(share_unsupported_error(
            backend.kind(),
            backend.name(),
            "access sharing",
        ));
    }
    let vault_backend = backend
        .vaults()
        .ok_or_else(|| share_unsupported_error(backend.kind(), backend.name(), "access sharing"))?;

    execute_secret_share(vault_backend, &vault_name, command, &config).await
}

/// One `xv find` result as rendered by every output format. Serde keys match
/// the pre-unification JSON envelope (`name`/`score`/`folder`/`groups`);
/// `score` is a 2-decimal string for stable CSV/table output and `folder`/
/// `groups` are empty strings instead of null (changelog-documented).
#[derive(tabled::Tabled, serde::Serialize)]
struct FindRow {
    #[tabled(rename = "Name")]
    #[serde(rename = "name")]
    name: String,
    #[tabled(rename = "Score")]
    #[serde(rename = "score")]
    score: String,
    #[tabled(rename = "Folder")]
    #[serde(rename = "folder")]
    folder: String,
    #[tabled(rename = "Groups")]
    #[serde(rename = "groups")]
    groups: String,
}

/// Empty-state wording for `xv find`, shared by the trait and legacy paths.
fn find_empty_message(
    pattern: Option<&str>,
    all_vaults: bool,
    vault_name: Option<&str>,
    folder_scope: Option<&str>,
) -> String {
    match (all_vaults, pattern, vault_name, folder_scope) {
        // All vaults cases
        (true, Some(p), _, Some(f)) => {
            format!("No secrets match '{p}' in folder '{f}' across all vaults.")
        }
        (true, Some(p), _, None) => format!("No secrets match '{p}' across all vaults."),
        (true, None, _, Some(f)) => format!("No secrets found in folder '{f}' across all vaults."),
        (true, None, _, None) => "No secrets found across all vaults.".to_string(),
        // Single vault cases
        (false, Some(p), Some(v), Some(f)) => {
            format!("No secrets match '{p}' in folder '{f}' of vault '{v}'.")
        }
        (false, Some(p), Some(v), None) => format!("No secrets match '{p}' in vault '{v}'."),
        (false, None, Some(v), Some(f)) => {
            format!("No secrets found in folder '{f}' of vault '{v}'.")
        }
        (false, None, Some(v), None) => format!("No secrets in vault '{v}'."),
        (false, _, None, _) => "No matching secrets found.".to_string(),
    }
}

/// Render find matches through the shared TableFormatter: all formats work
/// (CSV included), `--columns`/`--no-color` inherited, machine formats emit
/// valid-empty output on stdout when nothing matched.
fn render_find_matches(
    matches: &[crate::utils::fuzzy::Match<'_>],
    format: crate::utils::format::OutputFormat,
    empty_msg: &str,
    config: &Config,
) -> Result<()> {
    let rows: Vec<FindRow> = matches
        .iter()
        .map(|m| FindRow {
            name: m.item.name.clone(),
            score: format!("{:.2}", m.score as f64),
            folder: m.item.folder.clone().unwrap_or_default(),
            groups: m.item.groups.clone().unwrap_or_default(),
        })
        .collect();

    let fmt = format.resolve_for_stdout();
    let human_table_like = matches!(
        fmt,
        OutputFormat::Table | OutputFormat::Plain | OutputFormat::Raw
    );

    let formatter = crate::utils::format::TableFormatter::new(
        fmt,
        config.no_color,
        config.template.clone(),
        config.runtime_columns.clone(),
    );

    if rows.is_empty() && human_table_like {
        formatter.validate_columns::<FindRow>()?;
        output::info(empty_msg);
        return Ok(());
    }

    // Non-empty rows render for every format; empty rows reach here only on
    // machine formats, where format_table emits valid-empty output.
    println!("{}", formatter.format_table(&rows)?);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_secret_find_direct(
    pattern: Option<String>,
    in_fields: Vec<String>,
    limit: usize,
    min_score: f32,
    folder: Option<String>,
    all_vaults: bool,
    names_only: bool,
    format: crate::utils::format::OutputFormat,
    filter: Option<String>,
    config: Config,
    registry: Option<&crate::backend::BackendRegistry>,
) -> Result<()> {
    // Validate the glob before any backend call — the hard pre-filter is
    // applied on the candidate set before fuzzy scoring (see below).
    if let Some(pattern) = filter.as_deref() {
        crate::utils::helpers::compile_name_glob(pattern)?;
    }

    // Normalize --folder: trim trailing '/', validate, treat empty as absent.
    let folder_scope: Option<String> = match folder {
        Some(raw) => {
            let trimmed = raw.trim_end_matches('/').to_string();
            if trimmed.is_empty() {
                None
            } else {
                crate::utils::helpers::validate_folder_path(&trimmed)?;
                Some(trimmed)
            }
        }
        None => None,
    };

    // Workspace union `find` (multi-vault workspaces plan, Phase B Task 8):
    // consulted ONLY when a REAL (configured) workspace is attached AND
    // `--all-vaults` was NOT requested. `resolve_configured_workspace` returns
    // `None` with no configured workspace, so the degenerate single-vault case
    // falls through to the single-vault path below (which also performs the
    // `--in` field validation), byte-identical. `--all-vaults` keeps its
    // existing, documented meaning ("every vault the active backend can list")
    // — a strict superset of "every ATTACHED vault" — so it takes priority and
    // falls through to its own unchanged branch below even with a workspace
    // present.
    if !all_vaults {
        if let Some(ws) = crate::workspace::resolve_configured_workspace(&config).await? {
            use crate::utils::fuzzy::{score_matches, CandidateItem, FuzzyField};

            let mut fields: Vec<FuzzyField> = vec![FuzzyField::Name];
            for raw in &in_fields {
                let parsed = match raw.to_ascii_lowercase().as_str() {
                    "name" => FuzzyField::Name,
                    "folder" => FuzzyField::Folder,
                    "groups" => FuzzyField::Groups,
                    "note" => FuzzyField::Note,
                    "tags" => FuzzyField::Tags,
                    other => {
                        return Err(CrosstacheError::invalid_argument(format!(
                        "unknown --in field: '{other}' (allowed: name, folder, groups, note, tags)"
                    )));
                    }
                };
                if !fields.contains(&parsed) {
                    fields.push(parsed);
                }
            }

            let backend_names: Vec<String> = ws.entries.iter().map(|e| e.backend.clone()).collect();
            let ws_registry = crate::backend::BackendRegistry::with_lazy(&config, &backend_names)
                .map_err(|e| CrosstacheError::config(e.to_string()))?;

            // Candidate set = union of every ATTACHED vault (spec §Read
            // semantics for `find`): fail loud naming vault+backend on any
            // error, mirroring union `ls` — no partial unions, no silently
            // dropped vault. Rows are prefixed `alias/`, mirroring today's
            // `--all-vaults` vault-prefix style.
            let mut items: Vec<CandidateItem> = Vec::new();
            for entry in &ws.entries {
                let backend = ws_registry.materialize(&entry.backend).map_err(|e| {
                    CrosstacheError::config(format!(
                        "workspace vault '{}' (backend '{}') is unavailable: {e}",
                        entry.alias, entry.backend
                    ))
                })?;
                let secrets = backend
                    .secrets()
                    .list_secrets(&entry.vault, None)
                    .await
                    .map_err(|e| {
                        CrosstacheError::config(format!(
                            "workspace vault '{}' (backend '{}') failed to list secrets: {e}",
                            entry.alias, entry.backend
                        ))
                    })?;
                let secrets = filter_secrets_by_glob(secrets, filter.as_deref())?;
                for s in &secrets {
                    let mut item = CandidateItem::from_secret_summary(s);
                    item.name = format!("{}/{}", entry.alias, item.name);
                    items.push(item);
                }
            }

            let items: Vec<CandidateItem> = match folder_scope.as_deref() {
                Some(path) => items
                    .into_iter()
                    .filter(|i| {
                        crate::cli::ls_view::folder_in_scope(
                            i.folder.as_deref().unwrap_or(""),
                            path,
                        )
                    })
                    .collect(),
                None => items,
            };

            let pattern_str = pattern.as_deref().unwrap_or("");
            let mut matches = score_matches(pattern_str, &items, &fields);
            if !pattern_str.is_empty() && !matches.is_empty() {
                let top = matches[0].score as f32;
                if top > 0.0 {
                    let cutoff = (top * min_score).ceil() as u32;
                    matches.retain(|m| m.score >= cutoff);
                }
            }
            matches.truncate(limit);

            if names_only {
                for m in &matches {
                    println!("{}", m.item.name);
                }
                return Ok(());
            }

            let empty_msg = match (pattern.as_deref(), folder_scope.as_deref()) {
                (Some(p), Some(f)) => {
                    format!("No secrets match '{p}' in folder '{f}' across the attached workspace vaults.")
                }
                (Some(p), None) => {
                    format!("No secrets match '{p}' across the attached workspace vaults.")
                }
                (None, Some(f)) => {
                    format!(
                        "No secrets found in folder '{f}' across the attached workspace vaults."
                    )
                }
                (None, None) => {
                    "No secrets found across the attached workspace vaults.".to_string()
                }
            };
            render_find_matches(&matches, format, &empty_msg, &config)?;
            return Ok(());
        }
    }

    // ── Trait-based path (non-Azure backends) ──────────────────────────
    if use_trait_path(registry) {
        let reg = registry.expect("use_trait_path guarantees Some");
        use crate::utils::fuzzy::{score_matches, CandidateItem, FuzzyField};

        // Parse --in fields
        let mut fields: Vec<FuzzyField> = vec![FuzzyField::Name];
        for raw in &in_fields {
            let parsed = match raw.to_ascii_lowercase().as_str() {
                "name" => FuzzyField::Name,
                "folder" => FuzzyField::Folder,
                "groups" => FuzzyField::Groups,
                "note" => FuzzyField::Note,
                "tags" => FuzzyField::Tags,
                other => {
                    return Err(CrosstacheError::invalid_argument(format!(
                        "unknown --in field: '{other}' (allowed: name, folder, groups, note, tags)"
                    )));
                }
            };
            if !fields.contains(&parsed) {
                fields.push(parsed);
            }
        }

        let mut scope_vault: Option<String> = None;
        let items: Vec<CandidateItem> = if all_vaults {
            // List all vaults and collect secrets
            let mut combined = Vec::new();
            // `find --all-vaults` deliberately fans out over the ACTIVE backend's
            // full vault list — this is a legitimate active-backend read, not a
            // per-name resolution, so it stays on `reg.active()` (there is no
            // single vault to resolve here).
            if let Some(vaults_backend) = reg.active().vaults() {
                let vaults = vaults_backend.list_vaults(None).await?;
                for v in &vaults {
                    match reg.active().secrets().list_secrets(&v.name, None).await {
                        Ok(secrets) => {
                            let secrets = filter_secrets_by_glob(secrets, filter.as_deref())?;
                            for s in &secrets {
                                let mut item = CandidateItem::from_secret_summary(s);
                                item.name = format!("{}/{}", v.name, item.name);
                                combined.push(item);
                            }
                        }
                        Err(e) => {
                            tracing::debug!("list_secrets failed for vault {}: {e}", v.name);
                        }
                    }
                }
            }
            combined
        } else {
            let vault_name = resolve_vault_for_trait(&config, registry).await?;
            scope_vault = Some(vault_name.clone());
            let all_secrets = reg
                .active()
                .secrets()
                .list_secrets(&vault_name, None)
                .await?;
            let all_secrets = filter_secrets_by_glob(all_secrets, filter.as_deref())?;
            all_secrets
                .iter()
                .map(CandidateItem::from_secret_summary)
                .collect()
        };

        let items: Vec<CandidateItem> = match folder_scope.as_deref() {
            Some(path) => items
                .into_iter()
                .filter(|i| {
                    crate::cli::ls_view::folder_in_scope(i.folder.as_deref().unwrap_or(""), path)
                })
                .collect(),
            None => items,
        };

        let pattern_str = pattern.as_deref().unwrap_or("");
        let mut matches = score_matches(pattern_str, &items, &fields);

        if !pattern_str.is_empty() && !matches.is_empty() {
            let top = matches[0].score as f32;
            if top > 0.0 {
                let cutoff = (top * min_score).ceil() as u32;
                matches.retain(|m| m.score >= cutoff);
            }
        }
        matches.truncate(limit);

        if names_only {
            for m in &matches {
                println!("{}", m.item.name);
            }
            return Ok(());
        }

        let empty_msg = find_empty_message(
            pattern.as_deref(),
            all_vaults,
            scope_vault.as_deref(),
            folder_scope.as_deref(),
        );
        render_find_matches(&matches, format, &empty_msg, &config)?;
        return Ok(());
    }

    // Non-union find: single-vault or `--all-vaults`, both against the active
    // backend, resolved through the trait now. registry==None rebuilds from
    // config (clean error if it still fails).
    let rebuilt_registry;
    let reg = match registry {
        Some(r) => r,
        None => {
            rebuilt_registry = BackendRegistry::from_config(&config)
                .map_err(|e| CrosstacheError::config(e.to_string()))?;
            &rebuilt_registry
        }
    };
    let (backend, single_vault) = if all_vaults {
        // `--all-vaults` must NOT require a resolvable default vault — it scans
        // every vault the active backend can list.
        (reg.active_arc(), None)
    } else {
        // Single-vault: resolve the default vault through the workspace seam
        // (empty name → the default write target, no search).
        let (backend, _backend_name, vault_name, _path) =
            crate::cli::helpers::resolve_workspace_or_default(
                "",
                &config,
                crate::workspace::TargetMode::Write,
            )
            .await?;
        // Track context usage for the resolved vault (parity with the former
        // legacy single-vault path).
        let mut context_manager = crate::config::ContextManager::load()
            .await
            .unwrap_or_default();
        let _ = context_manager.update_usage(&vault_name).await;
        (backend, Some(vault_name))
    };
    execute_secret_find(
        &backend,
        single_vault,
        pattern.as_deref(),
        in_fields,
        limit,
        min_score,
        folder_scope.as_deref(),
        all_vaults,
        names_only,
        format,
        filter.as_deref(),
        &config,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn execute_secret_find(
    backend: &std::sync::Arc<dyn crate::backend::Backend>,
    single_vault: Option<String>,
    pattern: Option<&str>,
    in_fields: Vec<String>,
    limit: usize,
    min_score: f32,
    folder: Option<&str>,
    all_vaults: bool,
    names_only: bool,
    format: crate::utils::format::OutputFormat,
    filter: Option<&str>,
    config: &Config,
) -> Result<()> {
    use crate::utils::fuzzy::{score_matches, CandidateItem, FuzzyField};

    // Parse --in fields first so argument errors fire before vault resolution.
    let mut fields: Vec<FuzzyField> = vec![FuzzyField::Name];
    for raw in &in_fields {
        let parsed = match raw.to_ascii_lowercase().as_str() {
            "name" => FuzzyField::Name,
            "folder" => FuzzyField::Folder,
            "groups" => FuzzyField::Groups,
            "note" => FuzzyField::Note,
            "tags" => FuzzyField::Tags,
            other => {
                return Err(CrosstacheError::invalid_argument(format!(
                    "unknown --in field: '{other}' (allowed: name, folder, groups, note, tags)"
                )));
            }
        };
        if !fields.contains(&parsed) {
            fields.push(parsed);
        }
    }

    let items: Vec<CandidateItem> = if all_vaults {
        // `--all-vaults`: every vault the active backend can list. Vault
        // listing (and its subscription/RG scoping on Azure) lives inside
        // the backend's VaultBackend impl.
        let vaults_backend = backend.vaults().ok_or_else(|| {
            CrosstacheError::invalid_argument(format!(
                "the {} backend does not support listing vaults (required for --all-vaults)",
                backend.name()
            ))
        })?;
        let vaults = vaults_backend.list_vaults(None).await?;

        let progress = crate::utils::interactive::ProgressIndicator::new(&format!(
            "Searching {} vaults...",
            vaults.len()
        ));
        let mut combined: Vec<CandidateItem> = Vec::new();
        for v in &vaults {
            // Per-vault list — failures here are non-fatal; log + skip.
            match backend.secrets().list_secrets(&v.name, None).await {
                Ok(secrets) => {
                    let secrets = filter_secrets_by_glob(secrets, filter)?;
                    for s in &secrets {
                        let mut item = CandidateItem::from_secret_summary(s);
                        // Prefix the vault name into the displayed name so
                        // results are unambiguous: e.g. "myvault/SECRET".
                        item.name = format!("{}/{}", v.name, item.name);
                        combined.push(item);
                    }
                }
                Err(e) => {
                    tracing::debug!("list_secrets failed for vault {}: {e}", v.name);
                }
            }
        }
        progress.finish_clear();
        combined
    } else {
        // Single-vault path — the vault was resolved by the caller through the
        // workspace seam.
        let vault_name = single_vault.as_ref().ok_or_else(|| {
            CrosstacheError::config("vault name not resolved for single-vault search".to_string())
        })?;
        let progress = crate::utils::interactive::ProgressIndicator::new("Loading secrets...");
        let all_secrets = backend.secrets().list_secrets(vault_name, None).await;
        progress.finish_clear();
        let all_secrets = all_secrets?;
        let all_secrets = filter_secrets_by_glob(all_secrets, filter)?;
        all_secrets
            .iter()
            .map(CandidateItem::from_secret_summary)
            .collect()
    };

    let items: Vec<CandidateItem> = match folder {
        Some(path) => items
            .into_iter()
            .filter(|i| {
                crate::cli::ls_view::folder_in_scope(i.folder.as_deref().unwrap_or(""), path)
            })
            .collect(),
        None => items,
    };

    let pattern_str = pattern.unwrap_or("");
    let mut matches = score_matches(pattern_str, &items, &fields);

    // Apply min_score (relative to the top score, so 0.3 means 30% of
    // top). Empty pattern → every score is 0; skip filtering.
    if !pattern_str.is_empty() && !matches.is_empty() {
        let top = matches[0].score as f32;
        if top > 0.0 {
            let cutoff = (top * min_score).ceil() as u32;
            matches.retain(|m| m.score >= cutoff);
        }
    }

    // Apply limit.
    matches.truncate(limit);

    // Render: --names-only beats everything (pipe-friendly).
    if names_only {
        for m in &matches {
            println!("{}", m.item.name);
        }
        return Ok(());
    }

    let empty_msg = find_empty_message(pattern, all_vaults, single_vault.as_deref(), folder);
    render_find_matches(&matches, format, &empty_msg, config)
}

#[allow(dead_code)] // called from src/main.rs::run_complete_secrets (binary-only path)
pub(crate) async fn execute_complete_secrets(config: Config) -> Result<()> {
    use crate::cache::{CacheKey, CacheManager};

    // Workspace-seam vault resolution, cache-key identity only: this is a
    // cache-ONLY completion path that must never materialize a backend or make
    // a round-trip on a Tab press, so it resolves the workspace default entry
    // (non-materializing) rather than the full secret seam. The default entry's
    // (backend, vault) keys the cache exactly as the `ls` write path does; the
    // degenerate workspace-of-one reproduces the pre-workspace default.
    let ws = crate::workspace::resolve_workspace(&config)
        .await?
        .ok_or_else(|| {
            CrosstacheError::config(
                "internal error: resolve_workspace returned None; the degenerate \
             workspace-of-one must always yield Some or Err",
            )
        })?;
    let entry = ws.default_entry()?;

    // Cache-only path. If cache is cold, exit silently — the user got
    // no completions, which is the right UX for a Tab press (no Azure
    // round-trip on every keystroke).
    let cache_manager = CacheManager::from_config(&config);
    if !cache_manager.is_enabled() {
        return Ok(());
    }
    let cache_key = CacheKey::SecretsList {
        backend: entry.backend.clone(),
        vault_name: entry.vault.clone(),
    };
    if let Some(cached) =
        cache_manager.get::<Vec<crate::secret::manager::SecretSummary>>(&cache_key)
    {
        for s in &cached {
            let display = if s.original_name.is_empty() {
                &s.name
            } else {
                &s.original_name
            };
            println!("{}", crate::utils::format::sanitize_control_chars(display));
        }
    }
    Ok(())
}

/// Distinct folder paths (including ancestor prefixes, so `prod/db` also
/// offers `prod`), sorted — the completion feed for FOLDER-taking args.
fn folder_completion_paths(secrets: &[crate::secret::manager::SecretSummary]) -> Vec<String> {
    let mut folders = std::collections::BTreeSet::new();
    for s in secrets {
        let Some(folder) = s.folder.as_deref().filter(|f| !f.is_empty()) else {
            continue;
        };
        let mut prefix = String::new();
        for seg in folder.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(seg);
            folders.insert(prefix.clone());
        }
    }
    folders.into_iter().collect()
}

#[allow(dead_code)] // called from src/main.rs::run_complete_folders (binary-only path)
pub(crate) async fn execute_complete_folders(config: Config) -> Result<()> {
    use crate::cache::{CacheKey, CacheManager};

    // Workspace-seam vault resolution, cache-key identity only (see
    // `execute_complete_secrets`): cache-ONLY path, non-materializing default
    // entry keyed exactly as the `ls` write path.
    let ws = crate::workspace::resolve_workspace(&config)
        .await?
        .ok_or_else(|| {
            CrosstacheError::config(
                "internal error: resolve_workspace returned None; the degenerate \
             workspace-of-one must always yield Some or Err",
            )
        })?;
    let entry = ws.default_entry()?;

    // Cache-only path, mirroring execute_complete_secrets: a cold cache
    // means no completions — never a backend round-trip on a Tab press.
    let cache_manager = CacheManager::from_config(&config);
    if !cache_manager.is_enabled() {
        return Ok(());
    }
    let cache_key = CacheKey::SecretsList {
        backend: entry.backend.clone(),
        vault_name: entry.vault.clone(),
    };
    if let Some(cached) =
        cache_manager.get::<Vec<crate::secret::manager::SecretSummary>>(&cache_key)
    {
        for f in folder_completion_paths(&cached) {
            println!("{}", crate::utils::format::sanitize_control_chars(&f));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn execute_secret_rotate(
    reg: &BackendRegistry,
    backend_name: &str,
    name: &str,
    vault: Option<String>,
    length: usize,
    charset: CharsetType,
    custom_generator: Option<String>,
    show_value: bool,
    force: bool,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;
    use crate::secret::manager::SecretRequest;
    use crate::utils::interactive::InteractivePrompt;

    // The vault was already resolved through the workspace seam by
    // `execute_secret_rotate_direct` (rotate's sole caller) and handed in.
    let vault_name =
        vault.expect("execute_secret_rotate is only called with an already-resolved vault");

    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Check if the secret exists first
    let existing_secret = reg
        .active()
        .secrets()
        .get_secret(&vault_name, name, true)
        .await
        .map_err(|e| {
            CrosstacheError::config(format!(
                "Failed to verify secret exists: {}. Use 'xv set' to create a new secret.",
                e
            ))
        })?;

    output::step(&format!("Rotating secret: {}", name));

    // Show generation parameters
    if let Some(ref script) = custom_generator {
        println!("  Generator: {} (length: {})", script, length);
    } else {
        println!("  Character set: {:?}", charset);
        println!("  Length: {}", length);
    }

    // Confirm rotation unless force flag is used
    if !force {
        let prompt = InteractivePrompt::new();
        let confirm = prompt.confirm(
            &format!(
                "Are you sure you want to rotate secret '{}'? This will generate a new value and increment the version.",
                name
            ),
            false,
        )?;

        if !confirm {
            println!("Rotation cancelled.");
            return Ok(());
        }
    }

    // Generate the new value
    let new_value = generate_random_value(length, charset, custom_generator)?;

    // A typed record's generated value becomes the new primary field inside
    // the envelope, via the same write-back path as `xv update <name>
    // <value>`/`--stdin` (record-types plan; fixes #330). The naive
    // `set_secret` overwrite below is only correct for untyped secrets: on
    // a record it would leave `content_type` claiming a record while the
    // value becomes the bare generated string, corrupting it identically
    // to the bare-value `update` bug.
    //
    // `enabled: Some(true)` mirrors the untyped branch below, which always
    // forces the rotated secret back to enabled — without this a record
    // rotate would leave a previously-disabled record disabled (Bugbot
    // review NIT): the untyped path's `SecretRequest.enabled: Some(true)`
    // and this `SecretUpdateRequest.enabled` override are the same
    // deliberate choice, made consistent across both code shapes.
    let new_version = if crate::records::is_record(&existing_secret.content_type) {
        let props = execute_record_primary_update(
            name,
            new_value.as_str(),
            &existing_secret,
            Some(true),
            &vault_name,
            config,
            reg,
            backend_name,
        )
        .await?;
        props.version
    } else {
        // Preserve existing secret metadata
        let set_request = SecretRequest {
            name: name.to_string(),
            value: new_value.clone(),
            content_type: if existing_secret.content_type.is_empty() {
                None
            } else {
                Some(existing_secret.content_type)
            },
            enabled: Some(true),
            expires_on: existing_secret.expires_on,
            not_before: existing_secret.not_before,
            tags: if existing_secret.tags.is_empty() {
                None
            } else {
                Some(existing_secret.tags)
            },
            groups: None, // Groups are managed via tags
            note: None,
            folder: None,
        };

        // Set the rotated secret
        let result = reg
            .active()
            .secrets()
            .set_secret(&vault_name, set_request)
            .await
            .map_err(CrosstacheError::from)?;
        result.version
    };

    output::success(&format!("Successfully rotated secret '{}'", name));
    println!("New version: {}", new_version);

    if show_value {
        println!("Generated value: {}", new_value.as_str());
    } else {
        println!("Generated value: [hidden] (use --show-value to display)");
    }

    output::hint(&format!("Use 'xv history {}' to see version history", name));

    Ok(())
}

/// Reject a cross-vault copy/move BEFORE issuing any write when the
/// destination backend's tag budget (`BackendCapabilities::max_tags`, e.g.
/// Azure's 15) can't hold the request's tags — reuses
/// [`crate::records::check_tag_budget`] (the same pre-check `xv set --type`
/// already runs) rather than duplicating its counting logic, with the
/// request's `groups`/`note`/`folder` presence added to the reserved count
/// alongside crosstache's two always-written metadata tags
/// (`original_name`/`created_by`) — every one of those, when present,
/// consumes its own tag slot on write, exactly like a record's reserved tags
/// (Phase C Task 12).
pub(crate) fn check_dest_tag_budget(
    dest: &dyn crate::backend::Backend,
    request: &crate::secret::manager::SecretRequest,
) -> Result<()> {
    let reserved = crate::backend::ALWAYS_WRITTEN_TAGS.len()
        + usize::from(request.groups.as_ref().is_some_and(|g| !g.is_empty()))
        + usize::from(request.note.is_some())
        + usize::from(request.folder.is_some());
    let user_tags: std::collections::BTreeMap<String, String> = request
        .tags
        .clone()
        .unwrap_or_default()
        .into_iter()
        .collect();
    crate::records::check_tag_budget(
        &dest.capabilities(),
        reserved,
        &std::collections::BTreeMap::new(),
        &user_tags,
    )
}

/// Resolve a `{{ secret:TOKEN }}` template reference whose `TOKEN` carries a
/// colon prefix that parses as an attached workspace alias (Bugbot HIGH
/// fix, round 4): `{{ secret:work:DB_PASSWORD }}`,
/// `{{ secret:work:app/db/pass }}` (the slash slot stays folder-only —
/// unaffected by this; only the alias slot before the FIRST colon is new),
/// and `{{ secret:work:mail-cred.username }}` (record field access
/// composes with alias resolution).
///
/// Exact-name-first (mirroring [`crate::workspace::resolve_secret_target`]'s
/// Read-mode rule, applied inside it): the FULL raw `ref_token` — colon
/// included — is probed as a literal secret name across every attached
/// vault BEFORE alias interpretation, so a pre-existing secret literally
/// named `work:x` still wins. Once resolved to a target vault, the
/// alias-stripped path is tried as a literal name in THAT vault first (the
/// same exact-name-before-dot-split order the bare-name path above uses);
/// only on a miss does it split on the LAST dot for a record field
/// reference. An unknown alias surfaces
/// [`crate::workspace::resolve_secret_target`]'s own error (naming every
/// attached alias), not a generic not-found.
///
/// Only called when `crate::workspace::parse_address(ref_token).alias` is
/// `Some` (checked by the caller) — a bare `name`/`name.field` reference
/// never reaches this function, so that path stays byte-identical.
async fn resolve_workspace_template_ref(
    ref_token: &str,
    ws: &crate::workspace::Workspace,
    ws_registry: &BackendRegistry,
    config: &Config,
    record_types_cache: &mut Option<std::result::Result<Vec<RecordType>, String>>,
) -> Result<Zeroizing<String>> {
    let (target, path) = crate::workspace::resolve_secret_target(
        ref_token,
        ws,
        ws_registry,
        crate::workspace::TargetMode::Read,
    )
    .await?;

    let get_result = target
        .backend
        .secrets()
        .get_secret(&target.entry.vault, &path, true)
        .await
        .map_err(CrosstacheError::from);

    match get_result {
        Ok(secret_props) => {
            let types = if crate::records::is_record(&secret_props.content_type) {
                resolve_types_lazily(record_types_cache, config).await?
            } else {
                Vec::new()
            };
            record_field_value(&path, &secret_props, None, &types)
        }
        Err(CrosstacheError::SecretNotFound { .. }) => {
            let Some(dot) = path.rfind('.') else {
                return Err(CrosstacheError::secret_not_found(format!(
                    "{path} (in workspace vault '{}')",
                    target.entry.alias
                )));
            };
            let base = &path[..dot];
            let field = &path[dot + 1..];
            let secret_props = target
                .backend
                .secrets()
                .get_secret(&target.entry.vault, base, true)
                .await
                .map_err(CrosstacheError::from)?;
            let types = if crate::records::is_record(&secret_props.content_type) {
                resolve_types_lazily(record_types_cache, config).await?
            } else {
                Vec::new()
            };
            record_field_value(base, &secret_props, Some(field), &types)
        }
        Err(e) => Err(e),
    }
}

/// Resolve a single `xv://` reference's vault segment against workspace
/// aliases first, falling back to [`resolve_uri_secret`]'s raw-vault-name /
/// backend-kind behavior unchanged (spec §Addressing, Phase C Task 11).
///
/// - `xv://backend:vault/name` (an explicit backend prefix, i.e.
///   `backend_ref.backend.is_some()`) bypasses alias resolution entirely —
///   checked first, before consulting the workspace at all.
/// - Otherwise, when a workspace is attached and `backend_ref.vault` matches
///   an attached alias, resolves directly to that entry's `(backend, vault)`
///   via the lazy workspace registry — no `active_kind`/`cross_backends`
///   involvement, since a workspace entry may be a *named* backend that
///   isn't a bare [`BackendKind`].
/// - No workspace, or no alias match: falls straight through to
///   [`resolve_uri_secret`], today's raw-vault-name meaning, byte-identical.
#[allow(clippy::too_many_arguments)]
async fn resolve_uri_secret_workspace_aware(
    backend_ref: &BackendRef,
    secret_name: &str,
    active_secrets: &dyn crate::backend::SecretBackend,
    config: &Config,
    active_kind: BackendKind,
    cross_backends: &mut std::collections::HashMap<BackendKind, Arc<dyn crate::backend::Backend>>,
    ws: Option<&crate::workspace::Workspace>,
    ws_registry: Option<&BackendRegistry>,
) -> Result<crate::secret::manager::SecretProperties> {
    if backend_ref.backend.is_none() {
        if let (Some(ws), Some(ws_registry)) = (ws, ws_registry) {
            if let Some(entry) = ws.entry(&backend_ref.vault) {
                let backend = ws_registry.materialize(&entry.backend).map_err(|e| {
                    CrosstacheError::config(format!(
                        "workspace vault '{}' (backend '{}') is unavailable: {e}",
                        entry.alias, entry.backend
                    ))
                })?;
                return backend
                    .secrets()
                    .get_secret(&entry.vault, secret_name, true)
                    .await
                    .map_err(CrosstacheError::from);
            }
        }
    }
    resolve_uri_secret(
        backend_ref,
        secret_name,
        active_secrets,
        config,
        active_kind,
        cross_backends,
    )
    .await
}

/// Resolve a single `xv://` URI reference to its secret, dispatching to the
/// active backend or a cross-backend instance as needed.
///
/// `cross_backends` caches freshly-created backends by kind so the SDK is not
/// re-initialised per URI. Shared by `execute_secret_run` and
/// `execute_secret_inject` to keep cross-backend resolution logic in one place.
async fn resolve_uri_secret(
    backend_ref: &BackendRef,
    secret_name: &str,
    active_secrets: &dyn crate::backend::SecretBackend,
    config: &Config,
    active_kind: BackendKind,
    cross_backends: &mut std::collections::HashMap<BackendKind, Arc<dyn crate::backend::Backend>>,
) -> Result<crate::secret::manager::SecretProperties> {
    if let Some(backend_kind) = backend_ref.backend {
        if backend_kind != active_kind {
            // Cross-backend: reuse or create a cached backend instance
            if let std::collections::hash_map::Entry::Vacant(e) = cross_backends.entry(backend_kind)
            {
                let b = BackendRegistry::create_for_kind(backend_kind, config)
                    .await
                    .map_err(CrosstacheError::from)?;
                e.insert(b);
            }
            return cross_backends[&backend_kind]
                .secrets()
                .get_secret(&backend_ref.vault, secret_name, true)
                .await
                .map_err(CrosstacheError::from);
        }
    }
    active_secrets
        .get_secret(&backend_ref.vault, secret_name, true)
        .await
        .map_err(CrosstacheError::from)
}

/// Resolve the `(workspace, workspace registry, target backend, vault)` a
/// `run`/`inject` invocation acts on, implementing A4 `--vault` composition.
///
/// `ws`/`ws_registry` are returned for the caller's `xv://alias/...` URI
/// resolution (`None` when no configured workspace is attached, so URI
/// resolution stays byte-identical to pre-workspace behavior).
///
/// With no `--vault`, the target vault is the context/config default on the
/// active backend (unchanged). With an explicit `--vault`, the flag overrides
/// the degenerate default entry: it resolves to an attached workspace alias's
/// backend+vault when it names one, else a literal vault on the effective
/// backend (never adds an entry, never errors merely for the override).
async fn resolve_run_inject_target(
    reg: &BackendRegistry,
    vault: Option<String>,
    config: &Config,
) -> Result<(
    Option<crate::workspace::Workspace>,
    Option<BackendRegistry>,
    Arc<dyn crate::backend::Backend>,
    String,
)> {
    let (ws, ws_registry) = crate::cli::helpers::resolve_workspace_and_registry(config).await?;
    let (target_backend, vault_name) = match vault {
        Some(v) => {
            let (backend, _backend_name, vault_name) =
                crate::cli::helpers::resolve_vault_ref_with_workspace(
                    &v,
                    ws.as_ref(),
                    ws_registry.as_ref(),
                    reg,
                    config,
                )
                .await?;
            (backend, vault_name)
        }
        None => (reg.active_arc(), config.resolve_vault_name(None).await?),
    };
    Ok((ws, ws_registry, target_backend, vault_name))
}

#[allow(clippy::too_many_arguments)]
async fn execute_secret_run(
    reg: &BackendRegistry,
    vault: Option<String>,
    groups: Vec<String>,
    include: Vec<String>,
    exclude: Vec<String>,
    no_masking: bool,
    inherit_env: bool,
    best_effort: bool,
    command: Vec<String>,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;
    use crate::utils::helpers::to_env_var_name;
    use regex::Regex;
    use std::collections::HashMap;
    use std::process::{Command, Stdio};

    if command.is_empty() {
        return Err(CrosstacheError::config("No command specified"));
    }

    // No `--group` given on the CLI: fall back to the active env profile's
    // `group` default as the injection filter. An explicit `--group` (even
    // repeated) always wins; the profile default never adds to it. Track
    // whether the filter came from the profile so a fail-loud "nothing
    // matched" error can be attributed correctly (the user never typed it).
    let mut group_from_profile_default = false;
    let groups = if groups.is_empty() {
        match config.resolve_group(None).await? {
            Some(g) => {
                group_from_profile_default = true;
                vec![g]
            }
            None => groups,
        }
    } else {
        groups
    };

    // Resolve the run target (A4 --vault composition) and the workspace pair
    // used below for `xv://alias/...` URI resolution. Without `--vault` this is
    // the context/config default vault on the active backend, byte-identical to
    // the pre-workspace path. All backend reads below go through the RESOLVED
    // target backend via `reg`.
    let (ws, ws_registry, target_backend, vault_name) =
        resolve_run_inject_target(reg, vault, config).await?;
    let target_registry = BackendRegistry::new(target_backend);
    let reg = &target_registry;

    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Parse current environment for xv:// URI references (supports optional backend prefix).
    // Only done when the parent environment is inherited: in clean-env mode
    // (`inherit_env == false`) parent variables never reach the child, and
    // resolving/re-adding them would silently reintroduce parent-controlled
    // variables after `env_clear()` — an isolation bypass.
    let mut uri_refs: Vec<(String, BackendRef)> = Vec::new(); // (original_uri, parsed_ref)
    let uri_regex = Regex::new(r"xv://([^/\s]+)/([^/\s]+)").unwrap();

    if inherit_env {
        for (_env_name, env_value) in std::env::vars() {
            for captures in uri_regex.captures_iter(&env_value) {
                let vault_part = captures.get(1).map_or("", |m| m.as_str());
                let secret_part = captures.get(2).map_or("", |m| m.as_str());
                let uri_key = format!("xv://{vault_part}/{secret_part}");
                if uri_refs.iter().any(|(uri, _)| uri == &uri_key) {
                    continue;
                }
                match BackendRef::parse(&format!("{vault_part}/{secret_part}")) {
                    Ok(r) => uri_refs.push((uri_key, r)),
                    Err(e) => output::warn(&format!("Skipping invalid URI '{uri_key}': {e}")),
                }
            }
        }
    }

    // Get all secrets from the active backend (trait path — works for azure,
    // local, and aws alike).
    let progress = crate::utils::interactive::ProgressIndicator::new("Loading secrets...");
    let secrets = reg.active().secrets().list_secrets(&vault_name, None).await;
    progress.finish_clear();
    let secrets = secrets.map_err(CrosstacheError::from)?;

    // Filter secrets by groups if specified
    let filtered_secrets = if !groups.is_empty() {
        secrets
            .into_iter()
            .filter(|secret| {
                if let Some(secret_groups) = &secret.groups {
                    // Secret can have multiple groups (comma-separated)
                    let secret_group_list: Vec<&str> =
                        secret_groups.split(',').map(|g| g.trim()).collect();
                    groups
                        .iter()
                        .any(|filter_group| secret_group_list.contains(&filter_group.as_str()))
                } else {
                    false
                }
            })
            .collect()
    } else {
        secrets
    };

    // Apply name-based --include / --exclude on top of the group filter.
    // --include restricts to the named secrets; --exclude removes them.
    //
    // Match against EITHER the user-facing original name (what `xv list` prints
    // and what the flag help documents) OR the backend name, so a name copied
    // from list output always resolves — `original_name` falls back to `name`
    // when unset, mirroring the list display logic.
    let name_matches = |secret: &crate::secret::manager::SecretSummary, n: &str| -> bool {
        n == secret.name || (!secret.original_name.is_empty() && n == secret.original_name)
    };

    // Positive selection first: --group (already applied above) plus --include.
    // If a positive selector was given but matched nothing, that's almost always
    // a mistake (typo'd group/name, wrong vault); silently running the child with
    // no secrets — and exiting 0 — is dangerous in scripts/CI, so fail loud.
    let selected: Vec<_> = filtered_secrets
        .into_iter()
        .filter(|secret| include.is_empty() || include.iter().any(|n| name_matches(secret, n)))
        .collect();
    let positive_selector = !groups.is_empty() || !include.is_empty();
    if selected.is_empty() && positive_selector {
        let group_source = if group_from_profile_default {
            " (from env profile default)"
        } else {
            ""
        };
        return Err(CrosstacheError::invalid_argument(format!(
            "No secrets matched the requested selection in vault '{vault_name}' \
             (group={groups:?}{group_source}, include={include:?}). \
             Refusing to run the command with nothing injected — check the values."
        )));
    }

    // Negative filter: --exclude. Excluding every selected secret (leaving
    // nothing to inject) is a legitimate "run without these secrets" workflow,
    // so it behaves like an empty vault: warn, but still run the command.
    let filtered_secrets: Vec<_> = selected
        .into_iter()
        .filter(|secret| !exclude.iter().any(|n| name_matches(secret, n)))
        .collect();

    if filtered_secrets.is_empty() {
        output::warn(&format!(
            "No secrets to inject in vault '{vault_name}'; running command with no injected secrets."
        ));
    } else {
        output::step(&format!(
            "Injecting {} secret(s) as environment variables...",
            filtered_secrets.len()
        ));
    }

    // Fetch secret values and build environment map
    let mut env_vars: HashMap<String, Zeroizing<String>> = HashMap::new();
    let mut secret_values: Vec<Zeroizing<String>> = Vec::new(); // For masking
    let mut uri_values: HashMap<String, Zeroizing<String>> = HashMap::new(); // URI -> value mapping

    // Fetch failures are collected rather than aborting immediately, so the
    // user sees every failing secret/reference in one shot. By default any
    // failure aborts before the child is spawned; `--best-effort` restores
    // the old warn-and-continue behavior.
    let mut fetch_failures: Vec<String> = Vec::new();

    // Record types: resolved lazily, at most once, only when a fetched
    // secret is actually a record — so a typed record injects its primary
    // field value under its name (`xv run` never expands other fields,
    // spec §9 out of scope) without leaking the raw JSON envelope, while an
    // all-untyped selection never pays for (or fails on) type resolution at
    // all (Bugbot round 2 on #321 Phase C).
    let mut record_types_cache: Option<std::result::Result<Vec<RecordType>, String>> = None;

    // Fetch secrets from current vault (group-filtered)
    for secret in filtered_secrets {
        // Get the secret value
        match reg
            .active()
            .secrets()
            .get_secret(&vault_name, &secret.name, true)
            .await
        {
            Ok(secret_props) => {
                let resolved = if crate::records::is_record(&secret_props.content_type) {
                    resolve_types_lazily(&mut record_types_cache, config)
                        .await
                        .and_then(|types| {
                            record_field_value(&secret.name, &secret_props, None, &types)
                        })
                } else {
                    record_field_value(&secret.name, &secret_props, None, &[])
                };
                match resolved {
                    Ok(value) => {
                        let env_name = to_env_var_name(&secret.name);
                        env_vars.insert(env_name, value.clone());

                        // Store for masking (if enabled)
                        if !no_masking && !value.is_empty() {
                            secret_values.push(value.clone());
                        }
                    }
                    Err(e) => {
                        let msg = format!("Failed to resolve secret '{}': {}", secret.name, e);
                        output::warn(&msg);
                        fetch_failures.push(msg);
                    }
                }
            }
            Err(e) => {
                let msg = format!("Failed to get value for secret '{}': {}", secret.name, e);
                output::warn(&msg);
                fetch_failures.push(msg);
            }
        }
    }

    // Fetch URI-referenced secrets from environment variables
    if !uri_refs.is_empty() {
        output::info(&format!(
            "Found {} URI reference(s) in environment",
            uri_refs.len()
        ));

        // URI resolution's "same backend" default reads the RESOLVED
        // run/inject target (A4): with `--vault` this is the override's
        // backend, not the process active backend.
        let active_kind: BackendKind = reg.active().kind();

        // Cache backends by kind — avoids re-initialising the SDK per URI
        let mut cross_backends: std::collections::HashMap<
            BackendKind,
            Arc<dyn crate::backend::Backend>,
        > = std::collections::HashMap::new();

        for (uri, backend_ref) in &uri_refs {
            let secret_name = match &backend_ref.secret {
                Some(s) => s.clone(),
                None => {
                    output::warn(&format!("URI '{uri}' has no secret segment — skipping"));
                    continue;
                }
            };

            let fetch_result = resolve_uri_secret_workspace_aware(
                backend_ref,
                &secret_name,
                reg.active().secrets(),
                config,
                active_kind,
                &mut cross_backends,
                ws.as_ref(),
                ws_registry.as_ref(),
            )
            .await;

            match fetch_result {
                Ok(secret_props) => {
                    let resolved = if crate::records::is_record(&secret_props.content_type) {
                        resolve_types_lazily(&mut record_types_cache, config)
                            .await
                            .and_then(|types| {
                                record_field_value(&secret_name, &secret_props, None, &types)
                            })
                    } else {
                        record_field_value(&secret_name, &secret_props, None, &[])
                    };
                    match resolved {
                        Ok(value) => {
                            uri_values.insert(uri.clone(), value.clone());
                            if !no_masking && !value.is_empty() {
                                secret_values.push(value);
                            }
                        }
                        Err(e) => {
                            let msg = format!("Failed to resolve URI '{uri}': {e}");
                            output::warn(&msg);
                            fetch_failures.push(msg);
                        }
                    }
                }
                Err(e) => {
                    let msg = format!("Failed to resolve URI '{uri}': {e}");
                    output::warn(&msg);
                    fetch_failures.push(msg);
                }
            }
        }
    }

    // Abort before the child is built/spawned if any secret or xv:// reference
    // failed to fetch, unless --best-effort was requested (the previous
    // default: warn-and-continue).
    if !fetch_failures.is_empty() && !best_effort {
        output::error(&format!(
            "Aborting: {} secret(s)/reference(s) failed to fetch. Use --best-effort to launch anyway.",
            fetch_failures.len()
        ));
        for failure in &fetch_failures {
            output::error(&format!("  - {failure}"));
        }
        return Err(CrosstacheError::config(format!(
            "xv run aborted: {} secret(s)/reference(s) failed to fetch: {}",
            fetch_failures.len(),
            fetch_failures.join("; ")
        )));
    }

    // Set up the command
    let mut cmd = Command::new(&command[0]);
    if command.len() > 1 {
        cmd.args(&command[1..]);
    }

    // Set environment variables from vault secrets
    if !inherit_env {
        cmd.env_clear();
    }
    cmd.envs(&env_vars);

    // Resolve URI references in existing environment variables.
    // `uri_values` is only populated when `inherit_env` is true (see the scan
    // above), so this never re-adds parent variables after `env_clear()`.
    if inherit_env && !uri_values.is_empty() {
        for (env_name, env_value) in std::env::vars() {
            let mut resolved_value = env_value.clone();

            // Replace any xv:// URIs with actual secret values
            for (uri, secret_value) in &uri_values {
                resolved_value = resolved_value.replace(uri, secret_value);
            }

            // Only set if the value changed (had URI references)
            if resolved_value != env_value {
                cmd.env(env_name, resolved_value);
            }
        }
    }

    output::step(&format!("Executing: {}", command.join(" ")));

    if no_masking {
        // Direct passthrough — use .status() so inherited stdio works correctly
        cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());

        let status = cmd.status().map_err(|e| {
            CrosstacheError::config(format!("Failed to execute command '{}': {}", command[0], e))
        })?;

        // Explicitly drop secret-holding variables to zeroize them after child exits
        drop(env_vars);
        drop(uri_values);
        drop(secret_values);

        std::process::exit(status.code().unwrap_or(1));
    } else {
        // Stream output line-by-line with masking
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let child = cmd.spawn().map_err(|e| {
            CrosstacheError::config(format!("Failed to execute command '{}': {}", command[0], e))
        })?;

        // Drop env vars now — they're already set on the child process
        drop(env_vars);
        drop(uri_values);

        // secret_values is moved into stream_and_mask, which wraps it in Arc.
        // After threads join, Arc drop triggers Zeroizing::drop on each secret.
        let exit_code = stream_and_mask(child, secret_values)?;
        std::process::exit(exit_code);
    }
}

/// Stream child process stdout/stderr line-by-line, masking secret values in each line.
/// Returns the child's exit code.
///
/// `secret_values` is moved into an `Arc` and shared across two reader threads.
/// After both threads join, this function holds the last `Arc` reference —
/// dropping it triggers `Zeroizing::drop` on each secret value.
fn stream_and_mask(
    mut child: std::process::Child,
    secret_values: Vec<Zeroizing<String>>,
) -> Result<i32> {
    use std::io::Write;

    let stdout = child.stdout.take().ok_or_else(|| {
        CrosstacheError::config("failed to capture child stdout: pipe was not set")
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        CrosstacheError::config("failed to capture child stderr: pipe was not set")
    })?;

    // Move secret_values into Arc for sharing across threads.
    // After threads join, the Arc in this function is the last reference.
    let secrets = Arc::new(secret_values);
    let secrets_for_stderr = Arc::clone(&secrets);

    // Thread 1: stream stdout
    let stdout_thread = std::thread::spawn(move || {
        let mut out = std::io::stdout();
        mask_stream_bounded(stdout, &secrets, |masked| {
            let _ = out.write_all(masked.as_bytes());
        });
    });

    // Thread 2: stream stderr
    let stderr_thread = std::thread::spawn(move || {
        let mut err = std::io::stderr();
        mask_stream_bounded(stderr, &secrets_for_stderr, |masked| {
            let _ = err.write_all(masked.as_bytes());
        });
    });

    // Wait for child to exit
    let status = child
        .wait()
        .map_err(|e| CrosstacheError::config(format!("failed to wait on child process: {e}")))?;

    // Join threads (they'll finish once child closes pipe write-ends)
    let _ = stdout_thread.join();
    let _ = stderr_thread.join();

    // Flush before process::exit (which does not flush stdio buffers)
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();

    Ok(status.code().unwrap_or(1))
}

/// Read `src` in bounded chunks, mask any secret values, and hand each masked
/// chunk to `emit`. Memory is bounded regardless of the child's output shape:
/// the old implementation used `read_until(b'\n', ..)`, which buffers an entire
/// "line" in RAM — a child that emits gigabytes with no newline (binary output,
/// a hung process spewing) would OOM. Here we cap the working buffer and flush
/// in fixed-size chunks, carrying an overlap of `longest_secret - 1` bytes
/// across flush boundaries so a secret straddling two chunks is still masked.
fn mask_stream_bounded<R: std::io::Read>(
    src: R,
    secrets: &[Zeroizing<String>],
    mut emit: impl FnMut(&str),
) {
    // Read granularity. Small enough to bound memory, large enough to amortize
    // syscalls. The working buffer never exceeds CHUNK + a small carry.
    const CHUNK: usize = 64 * 1024;

    // The maximum maskable secret length. A secret can straddle a read
    // boundary, so we always retain at least this many trailing bytes as a
    // carry until they're confirmed not to start a secret completed by the
    // next read.
    let longest = secrets.iter().map(|s| s.len()).max().unwrap_or(0);

    let mut reader = src;
    let mut read_buf = [0u8; CHUNK];
    // `carry` holds raw (unmasked) bytes retained from the previous flush.
    let mut carry: Vec<u8> = Vec::new();

    loop {
        let n = match reader.read(&mut read_buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };

        // Working window = carried-over tail + freshly read bytes.
        let mut window = std::mem::take(&mut carry);
        window.extend_from_slice(&read_buf[..n]);

        // Provisional split: hold back the last `longest` bytes, which could be
        // the start of a secret completed by the next read.
        let mut split = window.len().saturating_sub(longest);

        // A naive prefix mask would cut any secret occurrence that *straddles*
        // `split` (starts before it, ends after it) — masking the prefix alone
        // wouldn't see the full value and would leak it. Move `split` left to
        // the start of any straddling occurrence so the whole occurrence stays
        // in the carry and gets masked once it's fully buffered.
        split = clean_split(&window, secrets, split);

        if split > 0 {
            let masked = mask_secrets(&String::from_utf8_lossy(&window[..split]), secrets);
            emit(&masked);
            carry = window[split..].to_vec();
        } else {
            // Nothing safely committable yet; keep accumulating.
            carry = window;
        }
    }

    // Flush whatever remains (EOF: everything is now complete).
    if !carry.is_empty() {
        let masked = mask_secrets(&String::from_utf8_lossy(&carry), secrets);
        emit(&masked);
    }
}

/// Find a flush boundary at or before `split` that does not fall inside any
/// secret occurrence in `window`. If a secret value occurs straddling `split`
/// (its bytes start before `split` and end after it), the boundary is moved
/// left to the occurrence's start so the entire value stays together (in the
/// carry) and is masked once fully buffered. Returns the adjusted split.
fn clean_split(window: &[u8], secrets: &[Zeroizing<String>], mut split: usize) -> usize {
    if split == 0 || split >= window.len() {
        return split.min(window.len());
    }
    // Iterate to a fixed point: moving the split left can expose a different
    // straddling occurrence. Bounded by the number of secrets per pass.
    let mut changed = true;
    while changed {
        changed = false;
        for s in secrets {
            let v = s.as_bytes();
            let len = v.len();
            // mask_secrets ignores values < 4 bytes.
            if len < 4 {
                continue;
            }
            // A straddling occurrence has start `p` with p < split < p+len.
            // Candidate starts are p in [split-len+1, split-1].
            let lo = split.saturating_sub(len - 1);
            let mut new_split = None;
            for p in lo..split {
                if p + len <= window.len() && &window[p..p + len] == v && p + len > split {
                    // Occurrence straddles `split`; move boundary to its start.
                    new_split = Some(p);
                    break;
                }
            }
            if let Some(p) = new_split {
                split = p;
                changed = true;
                break;
            }
        }
    }
    split
}

#[allow(clippy::too_many_arguments)]
async fn execute_secret_inject(
    reg: &BackendRegistry,
    vault: Option<String>,
    template_file: Option<String>,
    output_file: Option<String>,
    groups: Vec<String>,
    best_effort: bool,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;
    use regex::Regex;
    use std::collections::HashMap;
    use std::fs;
    use std::io::{self, Read};

    // Resolve the inject target (A4 --vault composition) and the workspace pair
    // used below for `xv://alias/...` URI resolution. Without `--vault` this is
    // the context/config default vault on the active backend, byte-identical to
    // the pre-workspace path. All backend reads below go through the RESOLVED
    // target backend via `reg`.
    let (ws, ws_registry, target_backend, vault_name) =
        resolve_run_inject_target(reg, vault, config).await?;
    let target_registry = BackendRegistry::new(target_backend);
    let reg = &target_registry;

    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Read template content
    let template_content = match template_file {
        Some(path) => fs::read_to_string(&path).map_err(|e| {
            CrosstacheError::config(format!("Failed to read template file '{}': {}", path, e))
        })?,
        None => {
            // Read from stdin
            let mut buffer = String::new();
            io::stdin().read_to_string(&mut buffer).map_err(|e| {
                CrosstacheError::config(format!("Failed to read from stdin: {}", e))
            })?;
            buffer
        }
    };

    // Parse template for secret references
    // Supports: {{ secret:name }}, {{ secret:name.field }}, and
    // xv://[backend:]vault/secret[#field]. The `.field` split (on the LAST
    // dot) is resolved later, after the vault's secrets are loaded: an exact
    // name match always wins first, so an untyped secret literally named
    // `a.b` keeps resolving as itself (record-types plan Task 12). The `#`
    // fragment is unambiguous up front since `#` is invalid in secret names
    // on every backend.
    let secret_regex = Regex::new(r"\{\{\s*secret:([^}\s]+)\s*\}\}").unwrap();
    let uri_regex = Regex::new(r"xv://([^/\s]+)/([^/\s#]+)(?:#([A-Za-z0-9._-]+))?").unwrap();

    let mut required_secrets: Vec<String> = Vec::new();
    // (original_uri, parsed_ref, optional #field)
    let mut cross_vault_refs: Vec<(String, BackendRef, Option<String>)> = Vec::new();

    // Failures collected across template parsing and secret fetching. By
    // default any failure aborts rendering (and the output write) before it
    // happens; `--best-effort` restores the old warn-and-continue behavior
    // (matching `xv run`'s fetch_failures pattern, #314).
    //
    // Unlike `xv run`'s scan of arbitrary parent-environment values (which
    // can incidentally contain `xv://`-shaped substrings unrelated to secret
    // references — a genuine "lookalike"), every reference here comes from a
    // template the user authored specifically for `xv inject`. Both
    // `{{ secret:name }}` and `xv://vault/secret` are unambiguously
    // intentional in that context, so an unparseable `xv://` reference is
    // treated as a failure too, not silently skipped.
    let mut fetch_failures: Vec<String> = Vec::new();

    // Find {{ secret:name }} references (current vault)
    for captures in secret_regex.captures_iter(&template_content) {
        if let Some(secret_name) = captures.get(1) {
            let name = secret_name.as_str().to_string();
            if !required_secrets.contains(&name) {
                required_secrets.push(name);
            }
        }
    }

    // Find xv://[backend:]vault/secret[#field] URI references
    for captures in uri_regex.captures_iter(&template_content) {
        let vault_part = captures.get(1).map_or("", |m| m.as_str());
        let secret_part = captures.get(2).map_or("", |m| m.as_str());
        let field_part = captures.get(3).map(|m| m.as_str().to_string());
        let uri_key = match &field_part {
            Some(f) => format!("xv://{vault_part}/{secret_part}#{f}"),
            None => format!("xv://{vault_part}/{secret_part}"),
        };
        if cross_vault_refs.iter().any(|(uri, _, _)| uri == &uri_key) {
            continue;
        }
        match BackendRef::parse(&format!("{vault_part}/{secret_part}")) {
            Ok(r) => cross_vault_refs.push((uri_key, r, field_part)),
            Err(e) => {
                let msg = format!("Invalid URI '{uri_key}': {e}");
                output::warn(&msg);
                fetch_failures.push(msg);
            }
        }
    }

    if required_secrets.is_empty() && cross_vault_refs.is_empty() && fetch_failures.is_empty() {
        output::warn("No secret references found in template");
        println!("    Use {{ secret:name }} syntax or xv://[backend:]vault/secret URIs");

        // Still write the template content as-is to output
        match output_file {
            Some(path) => {
                crate::utils::helpers::write_sensitive_file(
                    std::path::Path::new(&path),
                    template_content.as_bytes(),
                )
                .map_err(|e| {
                    CrosstacheError::config(format!(
                        "Failed to write to output file '{}': {}",
                        path, e
                    ))
                })?;
                println!("Template written to '{}'", path);
            }
            None => {
                print!("{}", template_content);
            }
        }
        return Ok(());
    }

    let total_references = required_secrets.len() + cross_vault_refs.len();
    output::info(&format!(
        "Found {} secret reference(s) in template",
        total_references
    ));

    if !required_secrets.is_empty() {
        println!(
            "  Current vault ({}): {} secret(s)",
            vault_name,
            required_secrets.len()
        );
    }
    if !cross_vault_refs.is_empty() {
        println!(
            "  Cross-vault/backend: {} secret(s)",
            cross_vault_refs.len()
        );
    }

    // Get all secrets from the active backend (trait path — works for azure,
    // local, and aws alike).
    let progress = crate::utils::interactive::ProgressIndicator::new("Loading secrets...");
    let secrets = reg.active().secrets().list_secrets(&vault_name, None).await;
    progress.finish_clear();
    let secrets = secrets.map_err(CrosstacheError::from)?;

    // Filter secrets by groups if specified
    let available_secrets = if !groups.is_empty() {
        secrets
            .into_iter()
            .filter(|secret| {
                if let Some(secret_groups) = &secret.groups {
                    let secret_group_list: Vec<&str> =
                        secret_groups.split(',').map(|g| g.trim()).collect();
                    groups
                        .iter()
                        .any(|filter_group| secret_group_list.contains(&filter_group.as_str()))
                } else {
                    false
                }
            })
            .collect()
    } else {
        secrets
    };

    // Build a map of secret names/URIs to values
    let mut secret_values: HashMap<String, Zeroizing<String>> = HashMap::new();
    let mut cross_vault_values: HashMap<String, Zeroizing<String>> = HashMap::new(); // URI -> value

    // Record types: resolved lazily, at most once, only when a fetched
    // secret is actually a record — an all-untyped template must render
    // successfully even with a broken `[types.*]` config block that no
    // referenced secret actually uses (Bugbot round 2 on #321 Phase C).
    let mut record_types_cache: Option<std::result::Result<Vec<RecordType>, String>> = None;

    // Fetch secrets from current vault. `ref_token` is the raw text captured
    // from `{{ secret:TOKEN }}`: either a bare name (record primary or a
    // plain value) or a `name.field` reference. Exact-name match is tried
    // FIRST so an existing secret literally named `a.b` always resolves as
    // itself; only when there is no exact match do we fall back to
    // splitting on the LAST dot and treating the suffix as a field
    // reference on the base record (record-types plan Task 12).
    for ref_token in &required_secrets {
        if let Some(secret_summary) = available_secrets.iter().find(|s| s.name == *ref_token) {
            match reg
                .active()
                .secrets()
                .get_secret(&vault_name, &secret_summary.name, true)
                .await
            {
                Ok(secret_props) => {
                    let resolved = if crate::records::is_record(&secret_props.content_type) {
                        resolve_types_lazily(&mut record_types_cache, config)
                            .await
                            .and_then(|types| {
                                record_field_value(ref_token, &secret_props, None, &types)
                            })
                    } else {
                        record_field_value(ref_token, &secret_props, None, &[])
                    };
                    match resolved {
                        Ok(value) => {
                            secret_values.insert(ref_token.clone(), value);
                        }
                        Err(e) => {
                            let msg = format!("Failed to resolve '{ref_token}': {e}");
                            output::warn(&msg);
                            fetch_failures.push(msg);
                        }
                    }
                }
                Err(e) => {
                    let msg = format!(
                        "Failed to get value for secret '{}' from vault '{}': {}",
                        ref_token, vault_name, e
                    );
                    output::warn(&msg);
                    fetch_failures.push(msg);
                }
            }
            continue;
        }

        // Bugbot HIGH fix, round 4: a colon-shaped, charset-valid alias
        // prefix (`{{ secret:work:DB_PASSWORD }}`) resolves against an
        // attached workspace alias — see `resolve_workspace_template_ref`'s
        // doc comment for the exact-name-first/field-composition rules. A
        // colon-shaped token with no workspace attached, or whose prefix
        // isn't charset-valid (`parse_address` returns `alias: None`),
        // falls through UNCHANGED to the dot-split logic below — the slash
        // form (`{{ secret:app/db/pass }}`) is a folder path, not touched
        // by this at all, since it carries no colon.
        if let Some(ws) = &ws {
            if crate::workspace::parse_address(ref_token).alias.is_some() {
                let ws_registry = ws_registry.as_ref().expect(
                    "ws_registry is Some whenever ws is Some (resolve_workspace_and_registry)",
                );
                match resolve_workspace_template_ref(
                    ref_token,
                    ws,
                    ws_registry,
                    config,
                    &mut record_types_cache,
                )
                .await
                {
                    Ok(value) => {
                        secret_values.insert(ref_token.clone(), value);
                    }
                    Err(e) => {
                        let msg = format!("Failed to resolve '{ref_token}': {e}");
                        output::warn(&msg);
                        fetch_failures.push(msg);
                    }
                }
                continue;
            }
        }

        let mut resolved = false;
        if let Some(dot) = ref_token.rfind('.') {
            let base = &ref_token[..dot];
            let field = &ref_token[dot + 1..];
            if let Some(secret_summary) = available_secrets.iter().find(|s| s.name == base) {
                resolved = true;
                match reg
                    .active()
                    .secrets()
                    .get_secret(&vault_name, &secret_summary.name, true)
                    .await
                {
                    Ok(secret_props) => {
                        let resolved = if crate::records::is_record(&secret_props.content_type) {
                            resolve_types_lazily(&mut record_types_cache, config)
                                .await
                                .and_then(|types| {
                                    record_field_value(base, &secret_props, Some(field), &types)
                                })
                        } else {
                            record_field_value(base, &secret_props, Some(field), &[])
                        };
                        match resolved {
                            Ok(value) => {
                                secret_values.insert(ref_token.clone(), value);
                            }
                            Err(e) => {
                                let msg = format!("Failed to resolve '{ref_token}': {e}");
                                output::warn(&msg);
                                fetch_failures.push(msg);
                            }
                        }
                    }
                    Err(e) => {
                        let msg = format!(
                            "Failed to get value for secret '{}' from vault '{}': {}",
                            base, vault_name, e
                        );
                        output::warn(&msg);
                        fetch_failures.push(msg);
                    }
                }
            }
        }

        if !resolved {
            let msg = format!("Secret '{ref_token}' not found in vault '{vault_name}'");
            output::warn(&msg);
            fetch_failures.push(msg);
        }
    }

    // Fetch URI-referenced secrets (supports optional backend prefix)
    {
        // URI resolution's "same backend" default reads the RESOLVED
        // run/inject target (A4): with `--vault` this is the override's
        // backend, not the process active backend.
        let active_kind: BackendKind = reg.active().kind();

        // Cache backends by kind — avoids re-initialising the SDK per URI
        let mut cross_backends: std::collections::HashMap<
            BackendKind,
            Arc<dyn crate::backend::Backend>,
        > = std::collections::HashMap::new();

        for (uri, backend_ref, field_opt) in &cross_vault_refs {
            let secret_name = match &backend_ref.secret {
                Some(s) => s.clone(),
                None => {
                    let msg = format!("URI '{uri}' has no secret segment — skipping");
                    output::warn(&msg);
                    fetch_failures.push(msg);
                    continue;
                }
            };

            let fetch_result = resolve_uri_secret_workspace_aware(
                backend_ref,
                &secret_name,
                reg.active().secrets(),
                config,
                active_kind,
                &mut cross_backends,
                ws.as_ref(),
                ws_registry.as_ref(),
            )
            .await;

            match fetch_result {
                Ok(secret_props) => {
                    let resolved = if crate::records::is_record(&secret_props.content_type) {
                        resolve_types_lazily(&mut record_types_cache, config)
                            .await
                            .and_then(|types| {
                                record_field_value(
                                    &secret_name,
                                    &secret_props,
                                    field_opt.as_deref(),
                                    &types,
                                )
                            })
                    } else {
                        record_field_value(&secret_name, &secret_props, field_opt.as_deref(), &[])
                    };
                    match resolved {
                        Ok(value) => {
                            cross_vault_values.insert(uri.clone(), value);
                        }
                        Err(e) => {
                            let msg = format!("Failed to resolve URI '{uri}': {e}");
                            output::warn(&msg);
                            fetch_failures.push(msg);
                        }
                    }
                }
                Err(e) => {
                    let msg = format!("Failed to resolve URI '{uri}': {e}");
                    output::warn(&msg);
                    fetch_failures.push(msg);
                }
            }
        }
    }

    // Abort before rendering/writing if any reference failed to resolve,
    // unless --best-effort was requested (the previous default:
    // warn-and-continue, leaving unresolved placeholders in the output).
    if !fetch_failures.is_empty() && !best_effort {
        output::error(&format!(
            "Aborting: {} secret(s)/reference(s) failed to resolve. Use --best-effort to render anyway.",
            fetch_failures.len()
        ));
        for failure in &fetch_failures {
            output::error(&format!("  - {failure}"));
        }
        return Err(CrosstacheError::config(format!(
            "xv inject aborted: {} secret(s)/reference(s) failed to resolve: {}",
            fetch_failures.len(),
            fetch_failures.join("; ")
        )));
    }

    let total_injected = secret_values.len() + cross_vault_values.len();
    output::step(&format!(
        "Injecting {} secret(s) into template...",
        total_injected
    ));

    // Replace secret references with actual values
    let mut result_content = Zeroizing::new(template_content);

    // Replace {{ secret:name }} references (current vault)
    for (secret_name, secret_value) in &secret_values {
        let pattern = format!(r"\{{\{{\s*secret:{}\s*\}}\}}", regex::escape(secret_name));
        let regex_pattern = Regex::new(&pattern).unwrap();
        *result_content = regex_pattern
            .replace_all(&result_content, secret_value.as_str())
            .to_string();
    }

    // Replace xv://vault/secret URI references. Longest-key-first: a bare
    // `xv://vault/name` is a strict prefix of its own `#field` form (and,
    // more generally, of any other URI key that happens to extend it), so
    // substituting in `HashMap` iteration order (nondeterministic) can
    // rewrite part of a longer reference before it's ever matched whole —
    // e.g. `xv://vault/name` replacing inside `xv://vault/name#username`
    // and leaving a mangled `<value>#username` behind. Sorting by
    // descending key length guarantees every longer (more specific) URI is
    // substituted before any of its prefixes.
    let mut cross_vault_entries: Vec<(&String, &Zeroizing<String>)> =
        cross_vault_values.iter().collect();
    cross_vault_entries.sort_by_key(|(uri, _)| std::cmp::Reverse(uri.len()));
    for (uri, secret_value) in cross_vault_entries {
        *result_content = result_content.replace(uri, secret_value.as_str());
    }

    // Write result
    match output_file {
        Some(path) => {
            crate::utils::helpers::write_sensitive_file(
                std::path::Path::new(&path),
                result_content.as_bytes(),
            )
            .map_err(|e| {
                CrosstacheError::config(format!("Failed to write to output file '{}': {}", path, e))
            })?;
            output::success(&format!(
                "Template resolved and written to '{}' (permissions: owner-only)",
                path
            ));
            output::warn("Output file contains resolved secrets -- treat as sensitive");
        }
        None => {
            print!("{}", result_content.as_str());
        }
    }

    Ok(())
}

async fn execute_secret_copy(
    reg: &BackendRegistry,
    name: &str,
    from_vault: &str,
    to_vault: &str,
    new_name: Option<String>,
    force: bool,
    config: &Config,
) -> Result<()> {
    use crate::backend::secret::rename_request_from_properties;

    // Determine target name (use new_name if provided, otherwise use original)
    let target_name = new_name.as_deref().unwrap_or(name);

    println!(
        "Copying secret '{}' from vault '{}' to vault '{}' as '{}'...",
        name, from_vault, to_vault, target_name
    );

    // Workspace-aware: `from_vault`/`to_vault` resolve against attached
    // aliases FIRST, falling back to raw-vault-name meaning on the
    // active backend unchanged (spec §Addressing, Phase C Task 12) — so
    // `xv copy secret --from work --to stage` can span backends, while
    // `xv copy secret --from vault-a --to vault-b` with no workspace (or
    // no alias match) behaves exactly as before.
    let (ws, ws_registry) = crate::cli::helpers::resolve_workspace_and_registry(config).await?;
    let (from_backend, _from_backend_name, from_vault_resolved) =
        crate::cli::helpers::resolve_vault_ref_with_workspace(
            from_vault,
            ws.as_ref(),
            ws_registry.as_ref(),
            reg,
            config,
        )
        .await?;
    let (to_backend, to_backend_name, to_vault_resolved) =
        crate::cli::helpers::resolve_vault_ref_with_workspace(
            to_vault,
            ws.as_ref(),
            ws_registry.as_ref(),
            reg,
            config,
        )
        .await?;

    let source_secret = from_backend
        .secrets()
        .get_secret(&from_vault_resolved, name, true)
        .await?;

    if to_backend
        .secrets()
        .get_secret(&to_vault_resolved, target_name, false)
        .await
        .is_ok()
    {
        if !force {
            return Err(CrosstacheError::config(format!(
                "Secret '{}' already exists in vault '{}'. Use 'xv move' with --force or delete the target secret first.",
                target_name, to_vault
            )));
        }
        output::warn(&format!(
            "Overwriting existing secret '{}' in vault '{}'",
            target_name, to_vault
        ));
    }

    // Lift groups/note/folder out of the canonical tag encoding into the
    // dedicated `SecretRequest` fields so every backend (local, AWS, ...)
    // re-encodes them the way its own reader expects — the local and AWS
    // backends only read those attributes from the dedicated fields, not
    // from raw tags, so building the request by hand here previously
    // silently dropped group membership, folder, and note on copy/move.
    let secret_request = rename_request_from_properties(target_name, &source_secret)?;

    // Destination tag-budget check BEFORE any write: a lower-capped
    // destination (e.g. Azure's 15-tag limit) must reject an oversized
    // cross-vault copy/move up front rather than letting the API reject
    // it mid-flight, leaving nothing written but a confusing error.
    check_dest_tag_budget(to_backend.as_ref(), &secret_request)?;

    let copied_secret = to_backend
        .secrets()
        .set_secret(&to_vault_resolved, secret_request)
        .await?;
    invalidate_trait_secret_cache(config, &to_backend_name, &to_vault_resolved);

    output::success(&format!(
        "Successfully copied secret '{}' to vault '{}'",
        copied_secret.original_name, to_vault
    ));
    println!("   Source: {}/{}", from_vault, name);
    println!("   Target: {}/{}", to_vault, target_name);
    println!("   Version: {}", copied_secret.version);
    println!("   Enabled: {}", copied_secret.enabled);

    if let Some(expires_on) = copied_secret.expires_on {
        use crate::utils::datetime::format_datetime;
        println!("   Expires: {}", format_datetime(Some(expires_on)));
    }

    Ok(())
}

async fn execute_secret_move(
    reg: &BackendRegistry,
    name: &str,
    from_vault: &str,
    to_vault: &str,
    new_name: Option<String>,
    force: bool,
    config: &Config,
) -> Result<()> {
    use crate::utils::interactive::InteractivePrompt;

    // Determine target name (use new_name if provided, otherwise use original)
    let target_name = new_name.as_deref().unwrap_or(name);

    println!(
        "Moving secret '{}' from vault '{}' to vault '{}' as '{}'...",
        name, from_vault, to_vault, target_name
    );

    // Check if target secret already exists and fail fast with a move-specific
    // message when not forced, *before* prompting for confirmation — there is
    // no point asking the user to confirm an operation that is guaranteed to
    // fail. When forced, `execute_secret_copy` below emits its own
    // "Overwriting existing secret" warning and performs the overwrite.
    let target_exists = {
        let (ws, ws_registry) = crate::cli::helpers::resolve_workspace_and_registry(config).await?;
        let (to_backend, _to_backend_name, to_vault_resolved) =
            crate::cli::helpers::resolve_vault_ref_with_workspace(
                to_vault,
                ws.as_ref(),
                ws_registry.as_ref(),
                reg,
                config,
            )
            .await?;
        to_backend
            .secrets()
            .get_secret(&to_vault_resolved, target_name, false)
            .await
            .is_ok()
    };
    if !force && target_exists {
        return Err(CrosstacheError::config(format!(
            "Secret '{}' already exists in vault '{}'. Use --force to overwrite.",
            target_name, to_vault
        )));
    }

    // Confirmation prompt if not forced
    if !force {
        let prompt = InteractivePrompt::new();
        let message = format!(
            "This will delete secret '{}' from vault '{}' after copying it to vault '{}'. Continue?",
            name, from_vault, to_vault
        );
        if !prompt.confirm(&message, false)? {
            println!("Move operation cancelled.");
            return Ok(());
        }
    }

    // First copy the secret (source is only deleted after this succeeds)
    execute_secret_copy(
        reg,
        name,
        from_vault,
        to_vault,
        new_name.clone(),
        force,
        config,
    )
    .await?;

    // Then delete from source
    println!(
        "Deleting source secret '{}' from vault '{}'...",
        name, from_vault
    );
    {
        let (ws, ws_registry) = crate::cli::helpers::resolve_workspace_and_registry(config).await?;
        let (from_backend, from_backend_name, from_vault_resolved) =
            crate::cli::helpers::resolve_vault_ref_with_workspace(
                from_vault,
                ws.as_ref(),
                ws_registry.as_ref(),
                reg,
                config,
            )
            .await?;
        from_backend
            .secrets()
            .delete_secret(&from_vault_resolved, name)
            .await?;
        invalidate_trait_secret_cache(config, &from_backend_name, &from_vault_resolved);
    }

    output::success(&format!(
        "Successfully moved secret '{}' from '{}' to '{}'",
        name, from_vault, to_vault
    ));

    Ok(())
}

async fn execute_secret_parse(
    connection_string: &str,
    format: &str,
    config: &Config,
) -> Result<()> {
    let components = crate::secret::manager::parse_connection_components(connection_string);

    match format.to_lowercase().as_str() {
        "json" => {
            let json_output = serde_json::to_string_pretty(&components).map_err(|e| {
                CrosstacheError::serialization(format!("Failed to serialize components: {e}"))
            })?;
            println!("{json_output}");
        }
        "table" => {
            if components.is_empty() {
                println!("No components found in connection string");
            } else {
                let formatter = crate::utils::format::TableFormatter::new(
                    crate::utils::format::OutputFormat::Table,
                    config.no_color,
                    None,
                    None,
                );
                println!("{}", formatter.format_table(&components)?);
            }
        }
        _ => {
            return Err(CrosstacheError::invalid_argument(format!(
                "Unsupported format '{format}' for this command. Use 'json' or 'table'."
            )));
        }
    }

    Ok(())
}

async fn execute_secret_share(
    vault_backend: &dyn crate::backend::vault::VaultBackend,
    vault_name: &str,
    command: ShareCommands,
    config: &Config,
) -> Result<()> {
    use crate::vault::models::AccessLevel;

    match command {
        ShareCommands::Grant {
            secret_name,
            user,
            level,
        } => {
            let object_id = vault_backend.resolve_principal(&user).await?;
            if object_id != user {
                println!("Resolved '{}' to object ID '{}'", user, object_id);
            }

            let access_level = match level.to_lowercase().as_str() {
                "reader" | "read" => AccessLevel::Reader,
                "contributor" | "write" => AccessLevel::Contributor,
                "admin" | "administrator" => AccessLevel::Admin,
                _ => {
                    return Err(CrosstacheError::invalid_argument(format!(
                        "Invalid access level: {level}"
                    )));
                }
            };

            vault_backend
                .grant_secret_access(vault_name, &secret_name, &object_id, access_level)
                .await?;

            println!(
                "Successfully granted {} access to secret '{}' for '{}' in vault '{}'",
                level, secret_name, user, vault_name
            );
        }
        ShareCommands::Revoke { secret_name, user } => {
            let object_id = vault_backend.resolve_principal(&user).await?;
            if object_id != user {
                println!("Resolved '{}' to object ID '{}'", user, object_id);
            }

            vault_backend
                .revoke_secret_access(vault_name, &secret_name, &object_id)
                .await?;

            println!(
                "Successfully revoked access to secret '{}' for '{}' in vault '{}'",
                secret_name, user, vault_name
            );
        }
        ShareCommands::List {
            secret_name,
            all,
            page,
            page_size,
            pager,
        } => {
            use crate::utils::pagination::{paginate_slice, pagination_footer_text, Pagination};
            use std::fmt::Write as _;

            let pager = pager
                .map(crate::cli::commands::PagerWhen::wants_pager)
                .unwrap_or(false);
            let mut roles = vault_backend
                .list_secret_access(vault_name, &secret_name)
                .await?;

            crate::cli::helpers::enrich_and_filter_roles(vault_backend, &mut roles, all).await;

            let pagination = Pagination::from_args(page, page_size)?;
            let paged = paginate_slice(&roles, pagination);

            let fmt = config.runtime_output_format;
            let human_table_like = matches!(
                fmt,
                crate::utils::format::OutputFormat::Table
                    | crate::utils::format::OutputFormat::Plain
                    | crate::utils::format::OutputFormat::Raw
            );
            let formatter = crate::utils::format::TableFormatter::new(
                fmt,
                config.no_color,
                config.template.clone(),
                config.runtime_columns.clone(),
            );

            if roles.is_empty() {
                if human_table_like {
                    formatter.validate_columns::<crate::vault::models::VaultRole>()?;
                    // Chrome goes to stderr; stdout stays clean for pipes.
                    crate::utils::output::info(&format!(
                        "No access assignments found for secret '{secret_name}' in vault '{vault_name}'"
                    ));
                } else {
                    // Machine formats emit valid empty output (e.g. `[]`).
                    println!("{}", formatter.format_table(&paged.items)?);
                }
            } else {
                let mut output = String::new();
                if human_table_like {
                    let _ = writeln!(
                        output,
                        "Access assignments for secret '{secret_name}' in vault '{vault_name}':"
                    );
                }
                let table_output = formatter.format_table(&paged.items)?;
                output.push_str(&table_output);
                if human_table_like {
                    output.push('\n');
                    output.push_str(&crate::utils::list_output::count_label(
                        paged.items.len(),
                        paged.total_items,
                        "assignment",
                        "assignments",
                        None,
                        paged.page_size.is_some(),
                    ));
                }
                if let Some(footer) =
                    pagination_footer_text(&paged, "assignment", "assignments", fmt)
                {
                    output.push('\n');
                    output.push_str(&footer);
                }
                crate::utils::pager::print_output(&output, pager)?;
            }
        }
    }

    Ok(())
}

/// Parse bulk set arguments into (key, value) pairs.
/// Supports `KEY=value` and `KEY=@/path/to/file` syntax.
fn parse_bulk_set_args(args: Vec<String>) -> Result<Vec<(String, String)>> {
    let mut pairs = Vec::new();
    for arg in args {
        if let Some(pos) = arg.find('=') {
            let key = arg[..pos].trim();
            let value_part = arg[pos + 1..].trim();
            if key.is_empty() {
                return Err(CrosstacheError::invalid_argument(format!(
                    "Invalid KEY=value pair: empty key in '{arg}'"
                )));
            }
            let value = if value_part.starts_with('@') {
                let file_path = value_part.strip_prefix('@').unwrap();
                if !std::path::Path::new(file_path).exists() {
                    return Err(CrosstacheError::config(format!(
                        "File not found: {file_path}"
                    )));
                }
                std::fs::read_to_string(file_path).map_err(|e| {
                    CrosstacheError::config(format!("Failed to read file '{file_path}': {e}"))
                })?
            } else {
                value_part.to_string()
            };
            if value.is_empty() {
                return Err(CrosstacheError::config(format!(
                    "Secret value cannot be empty for key '{key}'"
                )));
            }
            pairs.push((key.to_string(), value));
        } else {
            return Err(CrosstacheError::invalid_argument(format!(
                "Invalid format: '{arg}'. Expected KEY=value or KEY=@/path/to/file"
            )));
        }
    }
    if pairs.is_empty() {
        return Err(CrosstacheError::invalid_argument(
            "No valid KEY=value pairs provided",
        ));
    }
    Ok(pairs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::error::BackendError;
    use crate::backend::secret::SecretBackend;
    use crate::backend::{Backend, BackendCapabilities, BackendKind, NameCharset};
    use std::process::{Command, Stdio};
    use std::sync::OnceLock;
    use tokio::sync::Mutex as AsyncMutex;

    /// Serializes tests that mutate the process-global `XV_CACHE_DIR`
    /// env var, so parallel test threads don't stomp on each other's
    /// override while it is set. Async-aware so the guard can be held
    /// across `.await` points.
    ///
    /// This only protects callers that opt in by acquiring the lock: any
    /// future test that *reads* the cache dir (e.g. builds a cache-enabled
    /// `CacheManager::from_config`) must also acquire this lock for the
    /// duration it relies on `XV_CACHE_DIR`/the default resolution being
    /// stable, or it can observe another test's temporary override.
    fn cache_dir_env_lock() -> &'static AsyncMutex<()> {
        static LOCK: OnceLock<AsyncMutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| AsyncMutex::new(()))
    }

    // `context_dir_env_lock` is intentionally NOT redefined here — it lives
    // once, crate-wide, in `crate::config::context::test_support` (imported
    // below) and is shared with `crate::config::context::tests`. Two
    // independently-defined per-module locks guarding the SAME process-global
    // `XV_CONTEXT_DIR` env var would let a test here and a test in
    // `config::context::tests` both believe they hold exclusive access while
    // racing on the same var — cargo runs lib tests in parallel by default
    // (Bugbot review, LOW, PR #343).
    use crate::config::context::test_support::context_dir_env_lock;

    /// RAII guard that sets an env var for its lifetime and restores the
    /// previous value (or removes it, if previously unset) on drop — including
    /// during a panic unwind, since `tokio::sync::Mutex` does not poison.
    /// Callers must hold `cache_dir_env_lock()` for the guard's lifetime when
    /// mutating `XV_CACHE_DIR`.
    struct EnvVarGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    struct TestBackend {
        kind: BackendKind,
    }

    impl TestBackend {
        fn azure() -> Self {
            Self {
                kind: BackendKind::Azure,
            }
        }

        fn local() -> Self {
            Self {
                kind: BackendKind::Local,
            }
        }

        fn aws() -> Self {
            Self {
                kind: BackendKind::Aws,
            }
        }
    }

    #[async_trait::async_trait]
    impl SecretBackend for TestBackend {
        async fn set_secret(
            &self,
            _vault: &str,
            _request: crate::secret::manager::SecretRequest,
        ) -> std::result::Result<crate::secret::manager::SecretProperties, BackendError> {
            Err(BackendError::Unsupported("test backend".into()))
        }

        async fn get_secret(
            &self,
            _vault: &str,
            _name: &str,
            _include_value: bool,
        ) -> std::result::Result<crate::secret::manager::SecretProperties, BackendError> {
            Err(BackendError::Unsupported("test backend".into()))
        }

        async fn get_secret_version(
            &self,
            _vault: &str,
            _name: &str,
            _version: &str,
            _include_value: bool,
        ) -> std::result::Result<crate::secret::manager::SecretProperties, BackendError> {
            Err(BackendError::Unsupported("test backend".into()))
        }

        async fn list_secrets(
            &self,
            _vault: &str,
            _group_filter: Option<&str>,
        ) -> std::result::Result<Vec<crate::secret::manager::SecretSummary>, BackendError> {
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
            _request: crate::secret::manager::SecretUpdateRequest,
        ) -> std::result::Result<crate::secret::manager::SecretProperties, BackendError> {
            Err(BackendError::Unsupported("test backend".into()))
        }

        async fn native_rotate(
            &self,
            _vault: &str,
            _name: &str,
        ) -> std::result::Result<(), BackendError> {
            if self.kind == BackendKind::Aws {
                Ok(())
            } else {
                Err(BackendError::Unsupported("native rotation".into()))
            }
        }
    }

    #[async_trait::async_trait]
    impl Backend for TestBackend {
        fn name(&self) -> &'static str {
            match self.kind {
                BackendKind::Azure => "azure",
                BackendKind::Local => "local",
                BackendKind::Aws => "aws",
            }
        }

        fn kind(&self) -> BackendKind {
            self.kind
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities {
                has_vaults: self.kind == BackendKind::Local,
                has_file_storage: false,
                has_rbac: false,
                has_audit: false,
                has_versioning: true,
                has_soft_delete: true,
                has_secret_rotation: self.kind == BackendKind::Aws,
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

    /// Helper: run stream_and_mask but redirect its print!/eprint! output to files
    /// so we can verify masking actually happened.
    fn stream_and_mask_to_files(
        mut child: std::process::Child,
        secret_values: Vec<Zeroizing<String>>,
        stdout_file: &std::path::Path,
        stderr_file: &std::path::Path,
    ) -> i32 {
        use std::fs::OpenOptions;
        use std::io::Write;

        let stdout_handle = child.stdout.take().expect("stdout was piped");
        let stderr_handle = child.stderr.take().expect("stderr was piped");

        let secrets = Arc::new(secret_values);
        let secrets_for_stderr = Arc::clone(&secrets);

        let stdout_path = stdout_file.to_path_buf();
        let stderr_path = stderr_file.to_path_buf();

        let stdout_thread = std::thread::spawn(move || {
            let mut out = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&stdout_path)
                .unwrap();
            // Exercise the same bounded path as production.
            mask_stream_bounded(stdout_handle, &secrets, |masked| {
                out.write_all(masked.as_bytes()).unwrap();
            });
        });

        let stderr_thread = std::thread::spawn(move || {
            let mut out = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&stderr_path)
                .unwrap();
            mask_stream_bounded(stderr_handle, &secrets_for_stderr, |masked| {
                out.write_all(masked.as_bytes()).unwrap();
            });
        });

        let status = child.wait().expect("failed to wait on child");
        let _ = stdout_thread.join();
        let _ = stderr_thread.join();
        status.code().unwrap_or(1)
    }

    fn summary_with_groups(groups: Option<&str>) -> crate::secret::manager::SecretSummary {
        summary_named("secret", groups, true)
    }

    fn summary_named(
        name: &str,
        groups: Option<&str>,
        enabled: bool,
    ) -> crate::secret::manager::SecretSummary {
        crate::secret::manager::SecretSummary {
            name: name.to_string(),
            original_name: name.to_string(),
            note: None,
            folder: None,
            groups: groups.map(str::to_string),
            updated_on: "2026-04-28".to_string(),
            enabled,
            content_type: String::new(),
            tags: std::collections::HashMap::new(),
        }
    }

    #[tokio::test]
    async fn azure_trait_vault_resolution_does_not_fallback_to_default() {
        // #342: `resolve_vault_for_trait` -> `Config::resolve_vault_name`
        // reads the REAL global context via `ContextManager::load` unless
        // `XV_CONTEXT_DIR` points somewhere isolated — on a machine with a
        // vault/workspace already current, `resolve_vault_name` would
        // return that instead of erroring, breaking this test's premise.
        let _env_guard = context_dir_env_lock().lock().await;
        let temp_context_dir = tempfile::tempdir().unwrap();
        let _context_dir_guard = EnvVarGuard::set("XV_CONTEXT_DIR", temp_context_dir.path());

        let registry = BackendRegistry::new(Arc::new(TestBackend::azure()));
        let config = Config {
            backend: Some("azure".to_string()),
            default_vault: String::new(),
            ..Default::default()
        };

        let err = resolve_vault_for_trait(&config, Some(&registry))
            .await
            .expect_err("azure should preserve missing-vault config error");
        assert!(err.to_string().contains("No vault specified"));
    }

    #[tokio::test]
    async fn local_trait_vault_resolution_can_fallback_to_local_default() {
        // #342: isolate from the real global context — see the sibling
        // azure test above for why.
        let _env_guard = context_dir_env_lock().lock().await;
        let temp_context_dir = tempfile::tempdir().unwrap();
        let _context_dir_guard = EnvVarGuard::set("XV_CONTEXT_DIR", temp_context_dir.path());

        let registry = BackendRegistry::new(Arc::new(TestBackend::local()));
        let config = Config {
            backend: Some("local".to_string()),
            default_vault: String::new(),
            local: Some(crate::config::settings::LocalConfig {
                store_path: None,
                key_file: None,
                default_vault: Some("local-vault".to_string()),
                encrypt_metadata: None,
                opaque_filenames: None,
            }),
            ..Default::default()
        };

        let resolved = resolve_vault_for_trait(&config, Some(&registry))
            .await
            .unwrap();
        assert_eq!(resolved, "local-vault");
    }

    #[tokio::test]
    async fn aws_share_grant_returns_capability_hint() {
        let registry = BackendRegistry::new(Arc::new(TestBackend::aws()));
        let config = Config {
            backend: Some("aws".to_string()),
            ..Default::default()
        };

        let err = execute_secret_share_direct(
            ShareCommands::Grant {
                secret_name: "api-key".to_string(),
                user: "user@example.com".to_string(),
                level: "read".to_string(),
            },
            config,
            Some(&registry),
        )
        .await
        .expect_err("share grant on aws must be rejected");

        assert_eq!(err.exit_code(), 2);
        let msg = err.to_string();
        assert!(msg.contains("aws"), "should name the backend: {msg}");
        assert!(msg.contains("not supported"), "{msg}");
        assert!(
            msg.contains("aws secretsmanager put-resource-policy"),
            "should suggest the native equivalent: {msg}"
        );
    }

    #[tokio::test]
    async fn aws_share_list_returns_capability_hint() {
        let registry = BackendRegistry::new(Arc::new(TestBackend::aws()));
        let config = Config {
            backend: Some("aws".to_string()),
            ..Default::default()
        };

        let err = execute_secret_share_direct(
            ShareCommands::List {
                secret_name: "api-key".to_string(),
                all: false,
                page: None,
                page_size: None,
                pager: None,
            },
            config,
            Some(&registry),
        )
        .await
        .expect_err("share list on aws must be rejected");

        assert_eq!(err.exit_code(), 2);
        let msg = err.to_string();
        assert!(msg.contains("aws"), "should name the backend: {msg}");
        assert!(msg.contains("not supported"), "{msg}");
        assert!(
            msg.contains("aws secretsmanager put-resource-policy"),
            "should suggest the native equivalent: {msg}"
        );
    }

    #[tokio::test]
    async fn local_share_error_message_unchanged() {
        let registry = BackendRegistry::new(Arc::new(TestBackend::local()));
        let config = Config {
            backend: Some("local".to_string()),
            ..Default::default()
        };

        let err = execute_secret_share_direct(
            ShareCommands::Revoke {
                secret_name: "api-key".to_string(),
                user: "user@example.com".to_string(),
            },
            config,
            Some(&registry),
        )
        .await
        .expect_err("share revoke on local must be rejected");

        assert_eq!(
            err.to_string(),
            "Invalid argument: The local backend does not support access sharing. \
             The azure backend offers RBAC-based sharing."
        );
    }

    /// When `--backend aws` is requested but backend init failed (e.g. no
    /// `[aws]` config block), the registry is `None`. Share must still return
    /// the capability hint rather than fall through to the Azure path.
    #[tokio::test]
    async fn aws_share_without_registry_still_returns_capability_hint() {
        let config = Config {
            backend: Some("aws".to_string()),
            ..Default::default()
        };

        let err = execute_secret_share_direct(
            ShareCommands::Grant {
                secret_name: "api-key".to_string(),
                user: "user@example.com".to_string(),
                level: "read".to_string(),
            },
            config,
            None,
        )
        .await
        .expect_err("share grant on aws without a registry must be rejected");

        assert_eq!(err.exit_code(), 2);
        let msg = err.to_string();
        assert!(msg.contains("aws"), "should name the backend: {msg}");
        assert!(msg.contains("not supported"), "{msg}");
        assert!(
            msg.contains("aws secretsmanager put-resource-policy"),
            "should suggest the native equivalent: {msg}"
        );
    }

    #[tokio::test]
    async fn rotate_native_rejected_on_backend_without_rotation_capability() {
        // #342: `execute_secret_rotate_native` -> `resolve_workspace_or_default`
        // -> `resolve_workspace` reads the REAL global context via
        // `ContextManager::load` unless `XV_CONTEXT_DIR` points somewhere
        // isolated. On a machine with a workspace attached (e.g. one whose
        // default entry is on a DIFFERENT backend, such as azure), that
        // workspace's default entry would hijack resolution instead of the
        // "local" backend this test configures, changing which backend the
        // capability check actually gates on.
        let _env_guard = context_dir_env_lock().lock().await;
        let temp_context_dir = tempfile::tempdir().unwrap();
        let _context_dir_guard = EnvVarGuard::set("XV_CONTEXT_DIR", temp_context_dir.path());

        let registry = BackendRegistry::new(Arc::new(TestBackend::local()));
        let config = Config {
            backend: Some("local".to_string()),
            default_vault: String::new(),
            ..Default::default()
        };

        let err = execute_secret_rotate_native("db-password", None, true, config, Some(&registry))
            .await
            .expect_err("non-rotation backend must reject --native");
        let msg = err.to_string();
        assert!(
            msg.contains("does not support native rotation"),
            "unexpected error: {msg}"
        );
        assert!(msg.contains("local"), "should name the backend: {msg}");
    }

    /// When `--backend local` is requested but backend init failed (registry
    /// is `None`), `--native` must still return the capability hint rather
    /// than a generic "no backend registry" config error.
    #[tokio::test]
    async fn rotate_native_without_registry_still_returns_capability_hint() {
        let config = Config {
            backend: Some("local".to_string()),
            default_vault: String::new(),
            ..Default::default()
        };

        let err = execute_secret_rotate_native("db-password", None, true, config, None)
            .await
            .expect_err("non-rotation backend without a registry must reject --native");
        let msg = err.to_string();
        assert!(
            msg.contains("does not support native rotation"),
            "unexpected error: {msg}"
        );
        assert!(msg.contains("local"), "should name the backend: {msg}");
    }

    // The former `rotate_native_accepted_on_backend_with_rotation_capability`
    // unit test injected a fake rotation-capable `TestBackend::aws()` registry.
    // After resolution convergence the CLI resolves its backend from config through
    // the workspace seam, so a fake backend can no longer be injected — and no
    // HERMETIC backend advertises `has_secret_rotation` (only real AWS does),
    // so a `rotate --native` ACCEPT path can't be exercised without the aws
    // feature + network. Its intent — the `--native` capability gate reads the
    // RESOLVED target's capabilities, not the process active backend's — is
    // covered hermetically by
    // `crate::workspace::resolve::tests::resolved_backend_capabilities_reflect_workspace_entry_not_a_separate_active_backend`
    // and end-to-end by `tests/e2e_workspaces.rs::
    // rotate_native_capability_gate_reflects_resolved_target_not_workspace_default`.
    // The REJECT path stays covered by the sibling
    // `rotate_native_rejected_on_backend_without_rotation_capability` (local,
    // hermetic).

    #[test]
    fn expiry_filter_candidates_apply_group_and_enabled_filters_before_detail_fetches() {
        let candidates = filter_secret_summaries_for_display(
            vec![
                summary_named("prod-enabled", Some("prod"), true),
                summary_named("prod-disabled", Some("prod"), false),
                summary_named("dev-enabled", Some("dev"), true),
                summary_named("ungrouped", None, true),
            ],
            Some("prod"),
            false,
        );

        let names: Vec<_> = candidates.into_iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["prod-enabled"]);
    }

    #[test]
    fn trait_secret_cache_key_and_invalidation_use_same_resolved_vault_name() {
        let key = trait_secret_cache_key("local", "local-vault");
        assert_eq!(key.to_string(), "secrets:local:local-vault");
    }

    #[test]
    fn test_secret_summary_group_filter_is_exact_comma_separated_match() {
        assert!(secret_summary_matches_group(
            &summary_with_groups(Some("prod, infra")),
            "prod"
        ));
        assert!(secret_summary_matches_group(
            &summary_with_groups(Some("prod, infra")),
            "infra"
        ));
        assert!(!secret_summary_matches_group(
            &summary_with_groups(Some("production")),
            "prod"
        ));
        assert!(!secret_summary_matches_group(
            &summary_with_groups(None),
            "prod"
        ));
    }

    #[test]
    fn group_rows_derive_from_comma_separated_tags() {
        fn s(name: &str, groups: Option<&str>) -> crate::secret::manager::SecretSummary {
            crate::secret::manager::SecretSummary {
                name: name.to_string(),
                original_name: name.to_string(),
                note: None,
                folder: None,
                groups: groups.map(str::to_string),
                updated_on: String::new(),
                enabled: true,
                content_type: String::new(),
                tags: std::collections::HashMap::new(),
            }
        }
        let rows = derive_group_rows(&[
            s("a", Some("team-a, team-b")),
            s("b", Some("team-a")),
            s("c", Some(" team-b ,, team-b ")), // dup within one secret counts once
            s("d", None),
        ]);
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].group.as_str(), rows[0].secrets), ("team-a", 2));
        assert_eq!((rows[1].group.as_str(), rows[1].secrets), ("team-b", 2));
    }

    #[test]
    fn human_secret_list_rows_wrap_long_notes() {
        let mut secret = summary_named("api-key", Some("prod"), true);
        secret.note = Some(
            "This note is intentionally long so the secret list display wraps it across multiple lines"
                .to_string(),
        );

        let rows = format_secret_list_rows_for_human(&[secret]);

        assert_eq!(rows.len(), 1);
        assert!(rows[0].note.contains('\n'), "long notes should wrap");
        assert!(
            rows[0]
                .note
                .lines()
                .all(|line| crate::cli::ls_view::display_width(line) <= SECRET_LIST_NOTE_WRAP_WIDTH),
            "wrapped note lines should fit the notes column width: {:?}",
            rows[0].note
        );
    }

    #[test]
    fn test_stream_and_mask_stdout_masks_secrets() {
        let secret = Zeroizing::new("SUPERSECRET".to_string());
        let secrets = vec![secret];
        let dir = tempfile::tempdir().unwrap();
        let stdout_path = dir.path().join("stdout.txt");
        let stderr_path = dir.path().join("stderr.txt");

        let child = Command::new("echo")
            .arg("hello SUPERSECRET world")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn echo");

        let exit_code = stream_and_mask_to_files(child, secrets, &stdout_path, &stderr_path);
        assert_eq!(exit_code, 0);

        let output = std::fs::read_to_string(&stdout_path).unwrap();
        assert!(
            output.contains("[MASKED]"),
            "Expected [MASKED] in stdout, got: {}",
            output
        );
        assert!(
            !output.contains("SUPERSECRET"),
            "Secret should not appear in output"
        );
    }

    #[test]
    fn test_stream_and_mask_both_streams() {
        let secret = Zeroizing::new("TOPSECRET".to_string());
        let secrets = vec![secret];
        let dir = tempfile::tempdir().unwrap();
        let stdout_path = dir.path().join("stdout.txt");
        let stderr_path = dir.path().join("stderr.txt");

        let child = Command::new("sh")
            .arg("-c")
            .arg("echo 'stdout TOPSECRET line'; echo 'stderr TOPSECRET line' >&2")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn sh");

        let exit_code = stream_and_mask_to_files(child, secrets, &stdout_path, &stderr_path);
        assert_eq!(exit_code, 0);

        let stdout_output = std::fs::read_to_string(&stdout_path).unwrap();
        let stderr_output = std::fs::read_to_string(&stderr_path).unwrap();
        assert!(
            stdout_output.contains("[MASKED]"),
            "Expected [MASKED] in stdout"
        );
        assert!(
            stderr_output.contains("[MASKED]"),
            "Expected [MASKED] in stderr"
        );
        assert!(
            !stdout_output.contains("TOPSECRET"),
            "Secret should not appear in stdout"
        );
        assert!(
            !stderr_output.contains("TOPSECRET"),
            "Secret should not appear in stderr"
        );
    }

    #[test]
    fn test_stream_and_mask_exit_code() {
        let secrets = vec![];

        let child = Command::new("sh")
            .arg("-c")
            .arg("exit 42")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn sh");

        let exit_code = stream_and_mask(child, secrets).unwrap();
        assert_eq!(exit_code, 42);
    }

    #[test]
    fn test_stream_and_mask_large_output_no_oom() {
        // Verify streaming works for output larger than typical pipe buffer (64KB)
        let secret = Zeroizing::new("HIDDEN".to_string());
        let secrets = vec![secret];

        let child = Command::new("sh")
            .arg("-c")
            // Use awk for portability (seq not available in all environments)
            .arg("awk 'BEGIN{for(i=1;i<=3000;i++) print \"line \" i \" contains HIDDEN data\"}'")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn sh");

        let exit_code = stream_and_mask(child, secrets).unwrap();
        assert_eq!(exit_code, 0);
    }

    #[test]
    fn mask_stream_bounded_masks_secret_across_chunk_boundary() {
        // Place a secret so it straddles the 64 KiB read boundary: the first
        // half lands at the end of one chunk, the second half at the start of
        // the next. The overlap carry must still mask it.
        let secret = "SUPERSECRETVALUE1234567890";
        let secrets = vec![Zeroizing::new(secret.to_string())];

        // 64 KiB chunk; position the secret to span the boundary.
        let chunk = 64 * 1024;
        let half = secret.len() / 2;
        let prefix_len = chunk - half; // secret begins `half` bytes before the boundary
        let mut input = vec![b'a'; prefix_len];
        input.extend_from_slice(secret.as_bytes());
        input.extend_from_slice(b" trailing\n");

        let mut out = Vec::new();
        mask_stream_bounded(std::io::Cursor::new(input), &secrets, |m| {
            out.extend_from_slice(m.as_bytes())
        });
        let s = String::from_utf8_lossy(&out);
        assert!(
            s.contains("[MASKED]"),
            "boundary-spanning secret not masked"
        );
        assert!(
            !s.contains(secret),
            "secret leaked across the chunk boundary"
        );
    }

    #[test]
    fn mask_stream_bounded_handles_no_newline_input() {
        // A large input with NO newline must still stream/mask without relying
        // on line boundaries (the old read_until-based code would buffer it all).
        let secret = "NOLINESECRET";
        let secrets = vec![Zeroizing::new(secret.to_string())];
        let mut input = vec![b'x'; 200 * 1024];
        input.extend_from_slice(secret.as_bytes()); // no trailing newline

        let mut out = Vec::new();
        mask_stream_bounded(std::io::Cursor::new(input), &secrets, |m| {
            out.extend_from_slice(m.as_bytes())
        });
        let s = String::from_utf8_lossy(&out);
        assert!(
            s.contains("[MASKED]"),
            "secret in newline-less input not masked"
        );
        assert!(!s.contains(secret), "secret leaked in newline-less input");
    }

    #[test]
    fn stdin_value_is_byte_exact_by_default() {
        let pem = "-----BEGIN KEY-----\nabc123\n-----END KEY-----\n";
        let mut reader = std::io::Cursor::new(pem);
        assert_eq!(read_secret_value(&mut reader, false).unwrap(), pem);
    }

    #[test]
    fn stdin_value_preserves_leading_and_trailing_spaces() {
        let padded = "  value with spaces  ";
        let mut reader = std::io::Cursor::new(padded);
        assert_eq!(read_secret_value(&mut reader, false).unwrap(), padded);
    }

    #[test]
    fn stdin_trim_strips_leading_and_trailing_whitespace() {
        let mut reader = std::io::Cursor::new("\n  value with spaces  \n");
        assert_eq!(
            read_secret_value(&mut reader, true).unwrap(),
            "value with spaces"
        );
    }

    #[test]
    fn stdin_trim_preserves_interior_whitespace() {
        let mut reader = std::io::Cursor::new("  line1\nline2  ");
        assert_eq!(
            read_secret_value(&mut reader, true).unwrap(),
            "line1\nline2"
        );
    }

    #[test]
    fn stdin_empty_input_yields_empty_string_for_caller_rejection() {
        let mut reader = std::io::Cursor::new("");
        assert_eq!(read_secret_value(&mut reader, false).unwrap(), "");
        let mut reader = std::io::Cursor::new("  \n  ");
        assert_eq!(read_secret_value(&mut reader, true).unwrap(), "");
    }

    #[test]
    fn wrap_is_display_width_aware_for_cjk() {
        // 6 full-width chars = 12 columns; budget of 4 columns = 2 chars/line.
        let wrapped = wrap_text_to_width("秘密秘密秘密", 4);
        assert_eq!(wrapped.lines().count(), 3, "{wrapped:?}");
        for line in wrapped.lines() {
            assert!(crate::cli::ls_view::display_width(line) <= 4, "{wrapped:?}");
        }
    }

    #[test]
    fn folder_completion_includes_ancestor_prefixes_sorted() {
        fn s(folder: Option<&str>) -> crate::secret::manager::SecretSummary {
            crate::secret::manager::SecretSummary {
                name: "x".to_string(),
                original_name: "x".to_string(),
                note: None,
                folder: folder.map(str::to_string),
                groups: None,
                updated_on: String::new(),
                enabled: true,
                content_type: String::new(),
                tags: std::collections::HashMap::new(),
            }
        }
        let paths = folder_completion_paths(&[
            s(Some("prod/db/replica")),
            s(Some("prod")),
            s(Some("dev")),
            s(None),
            s(Some("")),
        ]);
        assert_eq!(
            paths,
            vec![
                "dev".to_string(),
                "prod".to_string(),
                "prod/db".to_string(),
                "prod/db/replica".to_string(),
            ]
        );
    }

    fn fake_secret_properties(name: &str) -> crate::secret::manager::SecretProperties {
        crate::secret::manager::SecretProperties {
            name: name.to_string(),
            original_name: name.to_string(),
            value: None,
            version: "v1".to_string(),
            version_number: Some(1),
            created_timestamp: 0,
            created_on: "2026-04-28".to_string(),
            updated_on: "2026-04-28".to_string(),
            enabled: true,
            expires_on: None,
            not_before: None,
            tags: std::collections::HashMap::new(),
            content_type: String::new(),
            recovery_level: None,
        }
    }

    /// Regression test for fix-6 finding #1: when the in-place update phase
    /// of a combined `--rename` update succeeds but the rename phase fails,
    /// the secrets-list cache must still be invalidated. Before the fix, both
    /// `update_secret(...).await?` and `rename_secret(...).await?` returned
    /// early past the single trailing `invalidate_trait_secret_cache` call,
    /// leaving a stale cached `xv ls` for up to the cache TTL.
    ///
    /// Post-convergence (Phase 1) the CLI resolves its backend from `config`
    /// through the workspace seam, so this can no longer inject a fake backend.
    /// It drives a REAL hermetic local backend instead and forces the rename
    /// to fail the natural way — renaming onto a name that already exists
    /// yields `BackendError::Conflict` (see `SecretBackend::rename_secret`'s
    /// destination-exists guard), exactly the "update applied, rename failed"
    /// shape the fix guards. Still a unit-level test (not e2e) because every
    /// e2e harness sets `cache_enabled = false`.
    #[tokio::test]
    async fn rename_failure_after_successful_update_invalidates_cache() {
        // Serialize + isolate the two process-global env overrides this test
        // relies on: `XV_CACHE_DIR` (so our `CacheManager` and the one
        // `execute_secret_update_direct` builds internally share a temp dir)
        // and `XV_CONTEXT_DIR` (so `resolve_workspace` -> `ContextManager::load`
        // can't pick up a real workspace and redirect the target vault).
        // `EnvVarGuard` restores both on drop, including on panic unwind.
        let _cache_env_guard = cache_dir_env_lock().lock().await;
        let temp_cache_dir = tempfile::tempdir().unwrap();
        let _cache_dir_guard = EnvVarGuard::set("XV_CACHE_DIR", temp_cache_dir.path());
        let _context_env_guard = context_dir_env_lock().lock().await;
        let temp_context_dir = tempfile::tempdir().unwrap();
        let _context_dir_guard = EnvVarGuard::set("XV_CONTEXT_DIR", temp_context_dir.path());

        // Real hermetic local store the CLI will resolve to from `config`.
        let store = tempfile::tempdir().unwrap();
        let vault_name = "xv-test-rename-cache".to_string();
        let config = Config {
            backend: Some("local".to_string()),
            cache_enabled: true,
            cache_ttl_secs: 300,
            local: Some(crate::config::settings::LocalConfig {
                store_path: Some(store.path().join("store").to_string_lossy().to_string()),
                key_file: Some(store.path().join("key.txt").to_string_lossy().to_string()),
                default_vault: Some(vault_name.clone()),
                encrypt_metadata: None,
                opaque_filenames: None,
            }),
            ..Default::default()
        };

        // Seed the SAME store the CLI resolves to: "src" so the in-place update
        // succeeds, and "existing-dst" so the subsequent rename collides
        // (`rename_secret`'s destination-exists guard -> `Conflict`).
        let registry = BackendRegistry::with_lazy(&config, &["local".to_string()])
            .expect("register local backend");
        let backend = registry.materialize("local").expect("build local backend");
        for name in ["src", "existing-dst"] {
            backend
                .secrets()
                .set_secret(
                    &vault_name,
                    crate::secret::manager::SecretRequest {
                        name: name.to_string(),
                        value: zeroize::Zeroizing::new("seed".to_string()),
                        content_type: None,
                        enabled: None,
                        expires_on: None,
                        not_before: None,
                        tags: None,
                        groups: None,
                        note: None,
                        folder: None,
                    },
                )
                .await
                .expect("seed secret");
        }

        let cache_manager = crate::cache::CacheManager::from_config(&config);
        let cache_key = trait_secret_cache_key("local", &vault_name);

        // Seed a stale cache entry, as a prior `xv ls` would have left behind.
        let stale: Vec<crate::secret::manager::SecretSummary> =
            vec![summary_named("src", None, true)];
        cache_manager.set(&cache_key, &stale);
        assert!(
            cache_manager
                .get::<Vec<crate::secret::manager::SecretSummary>>(&cache_key)
                .is_some(),
            "precondition: stale cache entry must be readable before the update runs"
        );

        let result = execute_secret_update_direct(
            "src",
            None,
            false,
            false,
            Vec::new(),
            Vec::new(),
            Some("existing-dst".to_string()),
            Some("rotated".to_string()),
            None,
            false,
            false,
            None,
            None,
            false,
            false,
            false,
            false,
            None,
            Vec::new(),
            Vec::new(),
            None,
            false,
            false,
            config.clone(),
            Some(&registry),
        )
        .await;

        // Observe the cache while the env override is still active, so both
        // this test's `cache_manager` and the internal invalidation performed
        // by `execute_secret_update_direct` are still looking at the same
        // temp dir. `_cache_dir_guard` and `_env_guard` are dropped at the
        // end of the function (in reverse declaration order — guard, then
        // mutex), restoring `XV_CACHE_DIR` before the lock is released even
        // if an assertion below panics.
        let cache_still_present = cache_manager
            .get::<Vec<crate::secret::manager::SecretSummary>>(&cache_key)
            .is_some();

        let err = match result {
            Ok(()) => panic!("rename phase must fail for this backend"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("Conflict") || err.to_string().contains("already exists"),
            "unexpected error: {err}"
        );

        assert!(
            !cache_still_present,
            "cache must be invalidated once the in-place update applied, \
             even though the following rename failed"
        );
    }

    // ── Record-types Task 6 helpers ─────────────────────────────────────

    fn login_type() -> crate::records::RecordType {
        crate::records::builtin_types()
            .into_iter()
            .find(|t| t.name == "login")
            .unwrap()
    }

    #[test]
    fn prompt_plan_orders_non_primary_then_primary_last() {
        let t = login_type();
        let plan = prompt_plan(&t, &BTreeMap::new());
        let names: Vec<&str> = plan.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["username", "url", "password"]);
        assert!(plan.last().unwrap().primary);
    }

    #[test]
    fn prompt_plan_skips_already_provided_fields() {
        let t = login_type();
        let mut provided = BTreeMap::new();
        provided.insert("username".to_string(), "bob".to_string());
        let plan = prompt_plan(&t, &provided);
        let names: Vec<&str> = plan.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["url", "password"]);
    }

    #[test]
    fn route_fields_declared_metadata_and_adhoc() {
        let t = login_type();
        let fields = vec![
            ("username".to_string(), "bob".to_string()),
            ("custom".to_string(), "x".to_string()),
        ];
        let (metadata, secret) = route_fields(&t, &fields, &[]).unwrap();
        assert_eq!(metadata.get("username"), Some(&"bob".to_string()));
        assert_eq!(metadata.get("custom"), Some(&"x".to_string()));
        assert!(secret.is_empty());
    }

    #[test]
    fn route_fields_rejects_primary_via_field() {
        let t = login_type();
        let fields = vec![("password".to_string(), "x".to_string())];
        assert!(route_fields(&t, &fields, &[]).is_err());
    }

    #[test]
    fn route_fields_secret_flag_always_goes_to_envelope() {
        let t = login_type();
        let secret_fields = vec![("totp".to_string(), "x".to_string())];
        let (metadata, secret) = route_fields(&t, &[], &secret_fields).unwrap();
        assert!(metadata.is_empty());
        assert_eq!(secret.get("totp"), Some(&"x".to_string()));
    }

    #[test]
    fn route_fields_rejects_duplicate_across_field_and_field_secret() {
        // Bugbot follow-up: `--field a=1 --field-secret a=2` must not
        // silently store `a` as both an f.* tag and an envelope entry.
        let t = login_type();
        let fields = vec![("custom".to_string(), "1".to_string())];
        let secret_fields = vec![("custom".to_string(), "2".to_string())];
        let err = route_fields(&t, &fields, &secret_fields).unwrap_err();
        assert!(err.to_string().contains("custom"), "{err}");
    }

    #[test]
    fn route_fields_rejects_duplicate_within_field_alone() {
        let t = login_type();
        let fields = vec![
            ("custom".to_string(), "1".to_string()),
            ("custom".to_string(), "2".to_string()),
        ];
        assert!(route_fields(&t, &fields, &[]).is_err());
    }

    #[test]
    fn missing_required_fields_reports_absent_username() {
        let t = login_type();
        let missing = missing_required_fields(&t, &BTreeMap::new(), &BTreeMap::new());
        let names: Vec<&str> = missing.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["username"]);
    }

    // ── #330: primary-field resolution for bare-value update/rotate ────

    #[test]
    fn resolve_primary_field_finds_declared_primary() {
        let mut secret = fake_secret_properties("cred");
        secret
            .tags
            .insert(TYPE_TAG.to_string(), "login".to_string());
        let types = crate::records::builtin_types();
        let record_type = resolve_primary_field("cred", &secret, &types).unwrap();
        assert_eq!(record_type.primary().name, "password");
    }

    #[test]
    fn resolve_primary_field_errors_on_unknown_type() {
        let mut secret = fake_secret_properties("cred");
        secret
            .tags
            .insert(TYPE_TAG.to_string(), "nosuch".to_string());
        let types = crate::records::builtin_types();
        let err = resolve_primary_field("cred", &secret, &types).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("cred"), "{msg}");
        assert!(msg.contains("nosuch"), "{msg}");
    }

    // ── Empty required fields (Bugbot round 3 follow-up) ────────────────

    #[test]
    fn missing_required_fields_treats_empty_value_as_missing() {
        let t = login_type();
        let mut metadata = BTreeMap::new();
        metadata.insert("username".to_string(), String::new());
        let missing = missing_required_fields(&t, &metadata, &BTreeMap::new());
        let names: Vec<&str> = missing.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["username"], "empty value must count as missing");
    }

    #[test]
    fn missing_required_fields_treats_whitespace_only_value_as_missing() {
        let t = login_type();
        let mut metadata = BTreeMap::new();
        metadata.insert("username".to_string(), "   ".to_string());
        let missing = missing_required_fields(&t, &metadata, &BTreeMap::new());
        let names: Vec<&str> = missing.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["username"],
            "whitespace-only value must count as missing"
        );
    }

    #[test]
    fn missing_required_fields_non_required_field_may_stay_empty() {
        // `url` on `login` is optional metadata; an explicit empty value is
        // a legitimate user choice and must not affect required-field
        // reporting (only `username`, the required field, is missing).
        let t = login_type();
        let mut metadata = BTreeMap::new();
        metadata.insert("url".to_string(), String::new());
        let missing = missing_required_fields(&t, &metadata, &BTreeMap::new());
        let names: Vec<&str> = missing.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["username"]);
    }

    #[test]
    fn has_non_blank_value_true_for_real_value() {
        let mut metadata = BTreeMap::new();
        metadata.insert("username".to_string(), "bob".to_string());
        assert!(has_non_blank_value("username", &metadata, &BTreeMap::new()));
    }

    // ── `xv get --field` clipboard auto-clear (code review follow-up) ──

    #[test]
    fn field_clipboard_outcome_secret_field_schedules_clear_like_plain_get() {
        let (message, schedule_clear) = field_clipboard_outcome("cred", "password", true, 30);
        assert!(schedule_clear, "secret-kind fields must schedule a clear");
        assert!(message.contains("auto-clears in 30s"), "{message}");
        assert!(message.contains("password"), "{message}");
        assert!(message.contains("cred"), "{message}");
    }

    #[test]
    fn field_clipboard_outcome_secret_field_no_clear_when_timeout_disabled() {
        let (message, schedule_clear) = field_clipboard_outcome("cred", "password", true, 0);
        assert!(!schedule_clear, "timeout=0 must never schedule a clear");
        assert!(
            !message.contains("auto-clears"),
            "no affordance text when disabled: {message}"
        );
    }

    #[test]
    fn field_clipboard_outcome_metadata_field_never_schedules_clear() {
        // Even with a non-zero timeout, a metadata-kind field (listable
        // without fetching the secret) intentionally skips the auto-clear.
        let (message, schedule_clear) = field_clipboard_outcome("cred", "username", false, 30);
        assert!(
            !schedule_clear,
            "metadata-kind fields must not schedule a clear"
        );
        assert!(!message.contains("auto-clears"), "{message}");
    }

    // ── Multi-vault workspaces plan (Phase B) union `ls`/`ls --deleted` ──

    fn workspace_secret(name: &str, alias: &str) -> crate::secret::manager::SecretSummary {
        let mut s = crate::secret::manager::SecretSummary {
            name: name.to_string(),
            original_name: name.to_string(),
            note: None,
            folder: None,
            groups: None,
            updated_on: "2026-07-01 00:00:00 UTC".to_string(),
            enabled: true,
            content_type: String::new(),
            tags: std::collections::HashMap::new(),
        };
        s.tags
            .insert(WORKSPACE_ALIAS_TAG.to_string(), alias.to_string());
        s
    }

    #[test]
    fn workspace_alias_of_reads_the_synthetic_tag() {
        let s = workspace_secret("SECRET", "work");
        assert_eq!(workspace_alias_of(&s), Some("work"));
    }

    #[test]
    fn workspace_alias_of_none_for_ordinary_secret() {
        let s = crate::secret::manager::SecretSummary {
            name: "SECRET".to_string(),
            original_name: "SECRET".to_string(),
            note: None,
            folder: None,
            groups: None,
            updated_on: String::new(),
            enabled: true,
            content_type: String::new(),
            tags: std::collections::HashMap::new(),
        };
        assert_eq!(workspace_alias_of(&s), None);
    }

    #[test]
    fn format_secret_list_rows_for_human_vault_reads_alias_into_vault_column() {
        let secrets = vec![workspace_secret("DB_PASSWORD", "stage")];
        let rows = format_secret_list_rows_for_human_vault(&secrets);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "DB_PASSWORD");
        assert_eq!(rows[0].vault, "stage");
    }

    #[test]
    fn deleted_list_capability_skip_note_names_alias_and_backend() {
        let msg = deleted_list_capability_skip_note("personal", "local-b");
        assert!(msg.contains("personal"), "{msg}");
        assert!(msg.contains("local-b"), "{msg}");
        assert!(msg.contains("soft-delete"), "{msg}");
        assert!(
            msg.starts_with("note:"),
            "must be a non-fatal 'note:' prefix, not an error: {msg}"
        );
    }

    /// Bugbot review MEDIUM: `execute_deleted_secret_list_workspace` must
    /// filter BARE per-vault names before applying the `alias/` display
    /// prefix, mirroring the live union `ls` path's ordering. This is
    /// deliberately a unit test on the composed
    /// `filter_deleted_secrets_by_glob` + prefix steps rather than an e2e
    /// test against the local backend: the local backend's `Unrestricted`
    /// name charset means `name` and `original_name` are always identical
    /// there (no sanitization ever runs), so `glob_matches_either_name`'s
    /// OR fallback via the untouched `name` field masks the bug entirely
    /// in any local-only e2e harness — the divergence only bites on a
    /// backend with a restricted charset (e.g. Azure Key Vault, which
    /// disallows underscores), where the SANITIZED `name` differs from
    /// the user-facing `original_name` and only the latter is
    /// glob-matchable. This test constructs that divergence directly.
    #[test]
    fn deleted_union_filter_must_run_on_bare_names_before_alias_prefix() {
        use crate::secret::manager::DeletedSecretSummary;

        let make = || DeletedSecretSummary {
            name: "prod-alpha-a1b2c3".to_string(), // sanitized: doesn't match "PROD_*"
            original_name: "PROD_ALPHA".to_string(), // user-facing: matches "PROD_*"
            deleted_on: None,
            scheduled_purge_on: None,
        };

        // CORRECT order (this function's fix): filter bare names first,
        // THEN apply the alias prefix.
        let mut correct = filter_deleted_secrets_by_glob(vec![make()], Some("PROD_*")).unwrap();
        assert_eq!(
            correct.len(),
            1,
            "must match via the bare original_name before any prefix is applied"
        );
        for s in &mut correct {
            let label = deleted_display_name(s).to_string();
            s.original_name = format!("work/{label}");
        }
        assert_eq!(correct[0].original_name, "work/PROD_ALPHA");

        // BUGGY order (what this test guards against): prefix first, THEN
        // filter — the filter now sees "work/PROD_ALPHA" (fails to match
        // "PROD_*", which requires a "PROD_" prefix) and the untouched
        // sanitized `name` "prod-alpha-a1b2c3" (also doesn't match),
        // silently losing the row.
        let mut buggy = vec![make()];
        for s in &mut buggy {
            let label = deleted_display_name(s).to_string();
            s.original_name = format!("work/{label}");
        }
        let buggy = filter_deleted_secrets_by_glob(buggy, Some("PROD_*")).unwrap();
        assert!(
            buggy.is_empty(),
            "demonstrates the pre-fix bug: filtering after alias-prefixing loses the match"
        );
    }
}
