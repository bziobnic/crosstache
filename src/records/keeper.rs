//! Keeper Security JSON import/export format.
//!
//! Implements the format documented at
//! <https://docs.keeper.io/user-guides/import-records-1/import-json>, used by
//! `xv vault import --fmt keeper` and `xv vault export --fmt keeper`.
//!
//! # Mapping
//!
//! A Keeper record maps onto an xv typed `login` record wherever it can:
//!
//! | Keeper                  | xv                                        |
//! |-------------------------|-------------------------------------------|
//! | `title`                 | secret name (sanitized; original in a tag) |
//! | `login`                 | `f.username` tag                          |
//! | `password`              | envelope primary field (`password`)        |
//! | `login_url`             | `f.url` tag                               |
//! | `notes`                 | `note` tag                                |
//! | `folders[].folder`      | `folder` tag (`\` → `/`)                  |
//! | `custom_fields`         | user tags                                 |
//! | `custom_fields.$oneTimeCode` | envelope `one-time-code` field       |
//!
//! Records that carry no `login` cannot satisfy the `login` type's required
//! `username` field, so they degrade to a plain (untyped) secret rather than
//! being dropped — see [`plan_import`].
//!
//! The one exception is a `$oneTimeCode`: a TOTP seed can only live in a
//! record envelope, which requires the typed shape (both a `login` and a
//! `password`). A record carrying a seed in any other shape is refused, since
//! the alternatives are dropping a second authentication factor or writing it
//! into a listable tag.
//!
//! # Deliberate gaps
//!
//! Keeper's `shared_folders` carry per-user and per-team permissions
//! (`manage_users`, `manage_records`, `can_edit`, `can_share`). xv has no
//! per-folder ACL — it shares whole vaults through backend RBAC (`xv share`)
//! — so a shared folder is imported as an ordinary folder path and the
//! dropped permissions are reported as warnings. They are never silently
//! discarded, and never silently applied either.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::backend::{BackendCapabilities, BackendKind};
use crate::error::{CrosstacheError, Result};
use crate::records::{
    check_tag_budget, encode_envelope, find_type, parse_envelope, predicted_reserved_tag_count,
    RecordType, FIELD_TAG_PREFIX, RECORD_CONTENT_TYPE, TYPE_TAG,
};
use crate::secret::manager::SecretRequest;
use zeroize::Zeroizing;

/// Keeper nests folder paths with a backslash (`Customer1\Folder2`); xv uses
/// `/`.
const KEEPER_PATH_SEP: char = '\\';

/// The Keeper custom field carrying a TOTP seed as an `otpauth://` URI.
const KEEPER_ONE_TIME_CODE: &str = "$oneTimeCode";

/// Envelope field the TOTP seed is stored in. Secret-kind rather than a tag:
/// a TOTP seed is a second authentication factor, so it must not sit in
/// listable metadata.
const ONE_TIME_CODE_FIELD: &str = "one-time-code";

/// The record type Keeper logins map onto.
const LOGIN_TYPE: &str = "login";

// ---------------------------------------------------------------------------
// Wire format
// ---------------------------------------------------------------------------

/// Top level of a Keeper JSON file.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct KeeperFile {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared_folders: Vec<KeeperSharedFolder>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<KeeperRecord>,
}

/// A single Keeper record.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct KeeperRecord {
    pub title: String,
    /// Keeper record type (`login`, …). Absent in older exports.
    #[serde(rename = "$type", default, skip_serializing_if = "Option::is_none")]
    pub record_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Free-form user fields. Values are `Value` rather than `String`
    /// because real exports carry numbers and booleans here too; they are
    /// stringified on import (see [`stringify_custom_field`]).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_fields: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folders: Vec<KeeperFolderRef>,
}

/// One folder association on a record: either private (`folder`) or shared
/// (`shared_folder`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct KeeperFolderRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_folder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_edit: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_share: Option<bool>,
}

/// A shared-folder definition with its permission grants.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct KeeperSharedFolder {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manage_users: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manage_records: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_edit: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_share: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<KeeperPermission>,
}

/// A single grant inside a shared folder: a team `uid` or a user `name`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct KeeperPermission {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manage_users: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manage_records: Option<bool>,
}

/// Parses a Keeper JSON document.
pub fn parse_keeper_file(input: &str) -> Result<KeeperFile> {
    let file: KeeperFile = serde_json::from_str(input)
        .map_err(|e| CrosstacheError::serialization(format!("failed to parse Keeper JSON: {e}")))?;
    if file.records.is_empty() && file.shared_folders.is_empty() {
        return Err(CrosstacheError::serialization(
            "Keeper JSON contains neither a 'records' nor a 'shared_folders' array",
        ));
    }
    Ok(file)
}

// ---------------------------------------------------------------------------
// Import: Keeper -> xv
// ---------------------------------------------------------------------------

/// The result of translating a Keeper file into writes, before anything is
/// sent to a backend.
#[derive(Debug, Default)]
pub struct KeeperImportPlan {
    /// Writes to perform, in file order.
    pub requests: Vec<SecretRequest>,
    /// Non-fatal notes about fidelity loss (dropped permissions, coerced
    /// values). The import still proceeds.
    pub warnings: Vec<String>,
    /// Records that could not be translated, as `(title, reason)`. Reported
    /// as failures; the rest of the import still proceeds.
    pub rejected: Vec<(String, String)>,
}

/// Translates a Keeper file into secret writes.
///
/// Pure: performs no I/O and touches no backend. Every reason a record can be
/// refused — tag-budget overflow, an unusable folder path, a name collision,
/// no storable content — is decided here so `--dry-run` sees exactly what a
/// real run would do.
pub fn plan_import(
    file: &KeeperFile,
    types: &[RecordType],
    caps: &BackendCapabilities,
    backend_kind: BackendKind,
) -> Result<KeeperImportPlan> {
    let mut plan = KeeperImportPlan::default();

    // Shared-folder permissions have no xv equivalent. Report once for the
    // file rather than once per record, and name the principals so the user
    // can reconstruct the grants with `xv share`.
    warn_about_shared_folders(file, &mut plan.warnings);

    let login_type = find_type(types, LOGIN_TYPE).ok_or_else(|| {
        CrosstacheError::config(format!(
            "record type '{LOGIN_TYPE}' is not defined; Keeper import needs it to store \
             username/password records. Did a [types.{LOGIN_TYPE}] block in config override \
             the built-in?"
        ))
    })?;

    // Collisions are tracked on the SANITIZED name, because that (not the
    // raw title) is what the backend keys on: "My Server" and "My/Server"
    // both sanitize to "My-Server" and the second would silently overwrite
    // the first.
    let mut used_names: HashSet<String> = HashSet::new();

    for record in &file.records {
        match plan_record(
            record,
            login_type,
            caps,
            backend_kind,
            &mut used_names,
            &mut plan.warnings,
        ) {
            Ok(Some(request)) => plan.requests.push(request),
            // `Ok(None)` means "nothing storable here" — already recorded as
            // a rejection by `plan_record`.
            Ok(None) => {}
            Err(reason) => plan
                .rejected
                .push((display_title(&record.title), reason.to_string())),
        }
    }

    Ok(plan)
}

/// Translates one record. `Err` carries the human-readable refusal reason.
fn plan_record(
    record: &KeeperRecord,
    login_type: &RecordType,
    caps: &BackendCapabilities,
    backend_kind: BackendKind,
    used_names: &mut HashSet<String>,
    warnings: &mut Vec<String>,
) -> std::result::Result<Option<SecretRequest>, String> {
    let title = display_title(&record.title);

    if record.title.trim().is_empty() {
        return Err("record has no title, so it has no secret name to store under".to_string());
    }

    // Resolve the secret name up front: a collision is a refusal, not an
    // overwrite.
    let name = unique_name(&record.title, used_names)?;

    // Split custom fields: the TOTP seed is secret material, everything else
    // is a listable tag.
    let mut user_tags: BTreeMap<String, String> = BTreeMap::new();
    let mut one_time_code: Option<String> = None;
    for (key, raw) in &record.custom_fields {
        let Some(value) = stringify_custom_field(raw) else {
            warnings.push(format!(
                "{title}: custom field '{key}' is a JSON {} and cannot be stored as a tag; dropped",
                json_kind(raw)
            ));
            continue;
        };
        if key == KEEPER_ONE_TIME_CODE {
            one_time_code = Some(value);
            continue;
        }
        if let Some(reason) = reserved_tag_reason(key) {
            warnings.push(format!(
                "{title}: custom field '{key}' {reason}; dropped to avoid corrupting record \
                 bookkeeping"
            ));
            continue;
        }
        user_tags.insert(key.clone(), value);
    }

    let folder = resolve_folder(record, warnings, &title)?;
    let note = record.notes.as_ref().and_then(|s| non_empty(s));

    // A record needs *something* to store as its value. Password first; a
    // password-less record (a Keeper secure note) falls back to its notes so
    // it isn't dropped.
    let password = record.password.as_ref().and_then(|s| non_empty(s));
    let username = record.login.as_ref().and_then(|s| non_empty(s));

    // A TOTP seed can only be stored as secret material, and the only shape
    // with room for it is a typed `login` record — which needs BOTH a password
    // (the primary field) and a username (required by the type). Any other
    // shape is a plain secret whose single value slot is already spoken for,
    // so the seed could only land in a plaintext tag.
    //
    // This guard covers every such shape at once, deliberately: it previously
    // lived inside the password-without-login arm alone, which let a
    // password-less secure note carrying a `$oneTimeCode` through and dropped
    // the seed silently.
    if one_time_code.is_some() && !(password.is_some() && username.is_some()) {
        return Err(format!(
            "has a {KEEPER_ONE_TIME_CODE} TOTP seed, but without both a 'login' and a \
             'password' it cannot be stored as a typed '{LOGIN_TYPE}' record, and a plain \
             secret has nowhere to keep the seed except a plaintext tag; add the missing \
             field in Keeper, or import this record separately"
        ));
    }

    let (content_type, value, field_tags) = match (&password, &username) {
        // Full login record: typed, with the password in the envelope.
        (Some(password), Some(username)) => {
            let mut metadata: BTreeMap<String, String> = BTreeMap::new();
            metadata.insert("username".to_string(), username.clone());
            if let Some(url) = record.login_url.as_ref().and_then(|s| non_empty(s)) {
                metadata.insert("url".to_string(), url);
            }

            let mut envelope: BTreeMap<String, String> = BTreeMap::new();
            envelope.insert(login_type.primary().name.clone(), password.clone());
            if let Some(code) = &one_time_code {
                envelope.insert(ONE_TIME_CODE_FIELD.to_string(), code.clone());
            }

            let field_tags: BTreeMap<String, String> = metadata
                .iter()
                .map(|(k, v)| (format!("{FIELD_TAG_PREFIX}{k}"), v.clone()))
                .collect();

            let reserved = predicted_reserved_tag_count(
                backend_kind,
                true, // xv-type
                false,
                note.is_some(),
                folder.is_some(),
                false,
            );
            check_tag_budget(caps, reserved, &field_tags, &user_tags).map_err(|e| e.to_string())?;

            let envelope_value = encode_envelope(&envelope).map_err(|e| e.to_string())?;
            (
                Some(RECORD_CONTENT_TYPE.to_string()),
                envelope_value,
                field_tags,
            )
        }
        // No username: the `login` type's required `username` can't be
        // satisfied, so store a plain secret rather than an invalid record.
        (Some(password), None) => {
            let mut extra = user_tags.clone();
            if let Some(url) = record.login_url.as_ref().and_then(|s| non_empty(s)) {
                extra.insert("url".to_string(), url);
            }
            // A `$oneTimeCode` here was already refused by the guard above.
            let reserved = predicted_reserved_tag_count(
                backend_kind,
                false,
                false,
                note.is_some(),
                folder.is_some(),
                false,
            );
            check_tag_budget(caps, reserved, &BTreeMap::new(), &extra)
                .map_err(|e| e.to_string())?;
            user_tags = extra;
            (
                Some("text/plain".to_string()),
                password.clone(),
                BTreeMap::new(),
            )
        }
        // No password at all: keep the record only if its notes carry
        // content, which is how Keeper stores secure notes.
        (None, _) => {
            let Some(notes) = note.clone() else {
                return Err(
                    "has neither a password nor notes, so there is nothing to store".to_string(),
                );
            };
            let reserved = predicted_reserved_tag_count(
                backend_kind,
                false,
                false,
                false, // notes became the value, not the note tag
                folder.is_some(),
                false,
            );
            check_tag_budget(caps, reserved, &BTreeMap::new(), &user_tags)
                .map_err(|e| e.to_string())?;
            warnings.push(format!(
                "{title}: no password; stored its notes as the secret value"
            ));
            return Ok(Some(SecretRequest {
                name,
                value: Zeroizing::new(notes),
                content_type: Some("text/plain".to_string()),
                enabled: Some(true),
                expires_on: None,
                not_before: None,
                tags: Some(user_tags.into_iter().collect()),
                groups: None,
                note: None,
                folder,
            }));
        }
    };

    let mut tags: HashMap<String, String> = HashMap::new();
    if content_type.as_deref() == Some(RECORD_CONTENT_TYPE) {
        tags.insert(TYPE_TAG.to_string(), login_type.name.clone());
    }
    tags.extend(field_tags);
    tags.extend(user_tags);

    Ok(Some(SecretRequest {
        name,
        value: Zeroizing::new(value),
        content_type,
        enabled: Some(true),
        expires_on: None,
        not_before: None,
        tags: Some(tags),
        groups: None,
        note,
        folder,
    }))
}

/// Picks the folder for a record and translates Keeper's `\` nesting to `/`.
///
/// A record can sit in several folders in Keeper; xv's `folder` tag holds
/// one, so the first is used and the rest are reported.
fn resolve_folder(
    record: &KeeperRecord,
    warnings: &mut Vec<String>,
    title: &str,
) -> std::result::Result<Option<String>, String> {
    let paths: Vec<(String, bool)> = record
        .folders
        .iter()
        .filter_map(|f| {
            f.folder
                .as_ref()
                .and_then(|s| non_empty(s))
                .map(|p| (p, false))
                .or_else(|| {
                    f.shared_folder
                        .as_ref()
                        .and_then(|s| non_empty(s))
                        .map(|p| (p, true))
                })
        })
        .collect();

    let Some((first, _shared)) = paths.first() else {
        return Ok(None);
    };

    if paths.len() > 1 {
        let dropped: Vec<String> = paths[1..].iter().map(|(p, _)| translate(p)).collect();
        warnings.push(format!(
            "{title}: is in {} folders in Keeper but an xv secret holds one; used '{}' and \
             dropped {}",
            paths.len(),
            translate(first),
            dropped.join(", ")
        ));
    }

    let path = translate(first);
    crate::utils::helpers::validate_folder_path(&path)
        .map_err(|e| format!("folder path '{path}' (from Keeper '{first}') is not usable: {e}"))?;
    Ok(Some(path))
}

/// `Customer1\Folder2` -> `Customer1/Folder2`.
fn translate(keeper_path: &str) -> String {
    keeper_path.replace(KEEPER_PATH_SEP, "/")
}

/// `Customer1/Folder2` -> `Customer1\Folder2`.
fn to_keeper_path(xv_path: &str) -> String {
    xv_path.replace('/', "\\")
}

/// Reports the permission grants a shared folder carries that xv cannot
/// represent.
fn warn_about_shared_folders(file: &KeeperFile, warnings: &mut Vec<String>) {
    if file.shared_folders.is_empty() {
        return;
    }

    let mut principals: Vec<String> = Vec::new();
    for folder in &file.shared_folders {
        for perm in &folder.permissions {
            if let Some(name) = perm.name.as_ref().and_then(|s| non_empty(s)) {
                principals.push(name);
            } else if let Some(uid) = perm.uid.as_ref().and_then(|s| non_empty(s)) {
                principals.push(format!("team {uid}"));
            }
        }
    }
    principals.sort_unstable();
    principals.dedup();

    // An xv folder is metadata on a secret, not a standalone object, so a
    // `shared_folders` block with no records to attach creates nothing at all.
    // Saying it was "imported" there would be a false success.
    if file.records.is_empty() {
        warnings.push(format!(
            "{} shared folder definition(s) found but the file has no records; xv folders exist \
             only as metadata on a secret, so nothing was created. Their permissions were NOT \
             applied.",
            file.shared_folders.len()
        ));
    } else {
        warnings.push(format!(
            "{} shared folder(s) imported as plain folders; xv has no per-folder ACL, so their \
             permissions were NOT applied.",
            file.shared_folders.len()
        ));
    }
    if !principals.is_empty() {
        warnings.push(format!(
            "Keeper granted access to {}. Grant equivalent vault access with 'xv share grant'.",
            principals.join(", ")
        ));
    }
}

/// Returns the sanitized, collision-free secret name for a Keeper title.
fn unique_name(title: &str, used: &mut HashSet<String>) -> std::result::Result<String, String> {
    let sanitized = crate::utils::sanitizer::sanitize_secret_name(title)
        .map_err(|e| format!("title cannot be turned into a secret name: {e}"))?;

    if used.insert(sanitized.clone()) {
        // The raw title is returned, not the sanitized form: the backend
        // sanitizes on write and preserves the original in `original_name`,
        // so passing the title through keeps that round-trip intact.
        return Ok(title.to_string());
    }

    // A duplicate title is legal in Keeper (folders disambiguate) but would
    // silently overwrite here. Refuse rather than guess.
    Err(format!(
        "title collides with an earlier record in this file (both resolve to the secret name \
         '{sanitized}'); rename one in Keeper before importing"
    ))
}

/// Tags whose meaning xv owns; a Keeper custom field may not claim them.
fn reserved_tag_reason(key: &str) -> Option<&'static str> {
    if key == TYPE_TAG {
        return Some("is the reserved record type marker");
    }
    if key.starts_with(FIELD_TAG_PREFIX) {
        return Some("uses the reserved record field prefix 'f.'");
    }
    if key == crate::backend::TAG_ORIGINAL_NAME || key == crate::backend::TAG_CREATED_BY {
        return Some("is written by the backend itself");
    }
    if key == "groups" || key == "note" || key == "folder" {
        return Some("is a reserved xv metadata tag");
    }
    None
}

/// Renders a Keeper custom-field value as a tag string. Objects and arrays
/// have no faithful flat representation, so they are refused (`None`).
fn stringify_custom_field(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        serde_json::Value::Null => Some(String::new()),
        serde_json::Value::Object(_) | serde_json::Value::Array(_) => None,
    }
}

fn json_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Object(_) => "object",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Null => "null",
    }
}

/// Treats a whitespace-only Keeper field as absent: an empty `login` or
/// `password` in an export means "not set", and storing it verbatim would
/// create a record whose required field is blank.
fn non_empty(s: &str) -> Option<String> {
    let trimmed = s.trim();
    (!trimmed.is_empty()).then(|| s.to_owned())
}

fn display_title(title: &str) -> String {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        "<untitled>".to_string()
    } else {
        trimmed.to_string()
    }
}

// ---------------------------------------------------------------------------
// Export: xv -> Keeper
// ---------------------------------------------------------------------------

/// One secret's worth of input to [`build_keeper_record`].
pub struct ExportedSecret<'a> {
    /// User-facing secret name (`original_name`), used as the Keeper title.
    pub name: &'a str,
    /// The stored value: a record envelope for typed records, otherwise the
    /// raw secret value.
    pub value: &'a str,
    pub content_type: &'a str,
    pub tags: &'a HashMap<String, String>,
}

/// Builds a Keeper record for one exported secret.
///
/// Typed `login` records export with full field fidelity; anything else
/// exports as a title + password record so no secret is silently omitted
/// from the file.
pub fn build_keeper_record(
    secret: &ExportedSecret<'_>,
    types: &[RecordType],
) -> Result<KeeperRecord> {
    let mut record = KeeperRecord {
        title: secret.name.to_string(),
        ..Default::default()
    };

    // Reserved bookkeeping never becomes a Keeper custom field.
    for (key, value) in secret.tags {
        match key.as_str() {
            "note" => record.notes = Some(value.clone()),
            "folder" => record.folders.push(KeeperFolderRef {
                folder: Some(to_keeper_path(value)),
                ..Default::default()
            }),
            _ if reserved_tag_reason(key).is_some() => {}
            _ => {
                record
                    .custom_fields
                    .insert(key.clone(), serde_json::Value::String(value.clone()));
            }
        }
    }

    let type_name = secret.tags.get(TYPE_TAG).map(String::as_str);
    let record_type = type_name.and_then(|t| find_type(types, t));

    match (crate::records::is_record(secret.content_type), record_type) {
        // A typed record: unpack the envelope into Keeper's native fields.
        (true, Some(record_type)) => {
            record.record_type = Some(LOGIN_TYPE.to_string());
            let envelope = parse_envelope(secret.value)?;

            // Metadata fields live in `f.*` tags. Map the ones Keeper has a
            // home for; the rest stay as custom fields.
            record.login = secret
                .tags
                .get(&format!("{FIELD_TAG_PREFIX}username"))
                .cloned();
            record.login_url = secret.tags.get(&format!("{FIELD_TAG_PREFIX}url")).cloned();
            for (key, value) in secret.tags {
                if let Some(field) = key.strip_prefix(FIELD_TAG_PREFIX) {
                    if field != "username" && field != "url" {
                        record
                            .custom_fields
                            .insert(field.to_string(), serde_json::Value::String(value.clone()));
                    }
                }
            }

            // The primary field is Keeper's `password`; other envelope
            // fields are secret material, so they must not be downgraded
            // into plaintext custom fields — except the TOTP seed, which
            // Keeper itself carries as `$oneTimeCode`.
            let primary = record_type.primary().name.as_str();
            for (field, value) in &envelope {
                if field == primary {
                    record.password = Some(value.clone());
                } else if field == ONE_TIME_CODE_FIELD {
                    record.custom_fields.insert(
                        KEEPER_ONE_TIME_CODE.to_string(),
                        serde_json::Value::String(value.clone()),
                    );
                } else {
                    record
                        .custom_fields
                        .insert(field.clone(), serde_json::Value::String(value.clone()));
                }
            }
            // A declared-but-absent metadata field is simply omitted: every
            // custom field above came from a real tag or envelope entry, so
            // there is nothing to prune. Pruning by declared field *name*
            // here would delete a genuine user custom field that happens to
            // share a declared field's name (e.g. an `account` tag on a type
            // that declares `account` but has no `f.account` set).
        }
        // Untyped (or an unknown type): the whole value is the password.
        _ => {
            record.password = Some(secret.value.to_string());
        }
    }

    Ok(record)
}

/// Serializes a Keeper file, pretty-printed like Keeper's own examples.
pub fn serialize_keeper_file(file: &KeeperFile) -> Result<String> {
    serde_json::to_string_pretty(file).map_err(|e| {
        CrosstacheError::serialization(format!("failed to serialize Keeper JSON: {e}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::records::builtin_types;

    fn azure_caps() -> BackendCapabilities {
        BackendCapabilities {
            max_tags: Some(15),
            max_tag_value_len: Some(256),
            ..Default::default()
        }
    }

    fn local_caps() -> BackendCapabilities {
        BackendCapabilities::default()
    }

    /// The first example from the Keeper import docs, verbatim.
    const DOCS_RECORDS: &str = r#"{
      "records": [
        {
          "title": "Dev Server 1",
          "login": "root",
          "password": "123123123",
          "login_url": "https://myserver.com",
          "notes": "These are some notes.",
          "custom_fields": { "Security Group": "Private" },
          "folders": [ { "folder": "Private Folder 1" } ]
        },
        {
          "title": "Prod Server 1",
          "login": "root",
          "password": "kj949234723jhfs4jf7h",
          "login_url": "https://myprodserver.com",
          "notes": "These are some notes.",
          "custom_fields": { "Security Group": "Public", "IP Address": "12.45.67.8" },
          "folders": [
            { "folder": "Private Folder 2" },
            { "shared_folder": "My Shared Folder 1", "can_edit": true, "can_share": true }
          ]
        },
        {
          "title": "Google",
          "login": "testing",
          "password": "1234567890",
          "login_url": "https://google.com",
          "notes": "These are some notes.",
          "custom_fields": { "Favorite Food": "Cheetos" },
          "folders": [ { "folder": "My Websites\\Online" } ]
        },
        {
          "title": "Facebook",
          "$type": "login",
          "login": "me@gmail.com",
          "password": "123123123123",
          "login_url": "https://facebook.com",
          "notes": "This is our corporate shared record.",
          "custom_fields": {
            "Facebook Application ID": "ABC12345",
            "$oneTimeCode": "otpauth://totp/Amazon:me@company.com?secret=JBSWY3DPEHPK3PXP&issuer=Amazon&algorithm=SHA1&digits=6&period=30"
          },
          "folders": [
            { "folder": "Social Media" },
            { "shared_folder": "Shared Social", "can_edit": false, "can_share": false }
          ]
        }
      ]
    }"#;

    fn plan_for(json: &str, caps: BackendCapabilities, kind: BackendKind) -> KeeperImportPlan {
        let file = parse_keeper_file(json).expect("parses");
        plan_import(&file, &builtin_types(), &caps, kind).expect("plans")
    }

    #[test]
    fn parses_the_documented_records_example() {
        let file = parse_keeper_file(DOCS_RECORDS).unwrap();
        assert_eq!(file.records.len(), 4);
        assert_eq!(file.records[0].title, "Dev Server 1");
        assert_eq!(file.records[3].record_type.as_deref(), Some("login"));
    }

    #[test]
    fn maps_a_login_record_to_a_typed_record() {
        let plan = plan_for(DOCS_RECORDS, azure_caps(), BackendKind::Azure);
        assert!(plan.rejected.is_empty(), "{:?}", plan.rejected);
        assert_eq!(plan.requests.len(), 4);

        let dev = &plan.requests[0];
        assert_eq!(dev.name, "Dev Server 1");
        assert_eq!(dev.content_type.as_deref(), Some(RECORD_CONTENT_TYPE));
        assert_eq!(dev.note.as_deref(), Some("These are some notes."));
        assert_eq!(dev.folder.as_deref(), Some("Private Folder 1"));

        let tags = dev.tags.as_ref().unwrap();
        assert_eq!(tags.get(TYPE_TAG).map(String::as_str), Some("login"));
        assert_eq!(tags.get("f.username").map(String::as_str), Some("root"));
        assert_eq!(
            tags.get("f.url").map(String::as_str),
            Some("https://myserver.com")
        );
        assert_eq!(
            tags.get("Security Group").map(String::as_str),
            Some("Private")
        );

        // The password is envelope material, never a tag.
        let envelope = parse_envelope(&dev.value).unwrap();
        assert_eq!(
            envelope.get("password").map(String::as_str),
            Some("123123123")
        );
        assert!(!tags.contains_key("password"));
    }

    #[test]
    fn translates_backslash_folder_nesting_to_slashes() {
        let plan = plan_for(DOCS_RECORDS, azure_caps(), BackendKind::Azure);
        let google = plan
            .requests
            .iter()
            .find(|r| r.name == "Google")
            .expect("Google record");
        assert_eq!(google.folder.as_deref(), Some("My Websites/Online"));
    }

    #[test]
    fn stores_the_totp_seed_as_secret_material_not_a_tag() {
        let plan = plan_for(DOCS_RECORDS, azure_caps(), BackendKind::Azure);
        let fb = plan
            .requests
            .iter()
            .find(|r| r.name == "Facebook")
            .expect("Facebook record");

        let envelope = parse_envelope(&fb.value).unwrap();
        assert!(
            envelope
                .get(ONE_TIME_CODE_FIELD)
                .is_some_and(|v| v.contains("JBSWY3DPEHPK3PXP")),
            "TOTP seed must live in the encrypted envelope: {envelope:?}"
        );

        let tags = fb.tags.as_ref().unwrap();
        for (key, value) in tags {
            assert!(
                !value.contains("JBSWY3DPEHPK3PXP"),
                "TOTP seed leaked into listable tag '{key}'"
            );
        }
    }

    #[test]
    fn uses_the_first_folder_and_reports_the_dropped_ones() {
        let plan = plan_for(DOCS_RECORDS, azure_caps(), BackendKind::Azure);
        let prod = plan
            .requests
            .iter()
            .find(|r| r.name == "Prod Server 1")
            .unwrap();
        assert_eq!(prod.folder.as_deref(), Some("Private Folder 2"));
        assert!(
            plan.warnings
                .iter()
                .any(|w| w.contains("Prod Server 1") && w.contains("My Shared Folder 1")),
            "dropped folder must be reported: {:?}",
            plan.warnings
        );
    }

    #[test]
    fn reports_shared_folder_permissions_as_unapplied() {
        let json = r#"{
          "shared_folders": [
            {
              "path": "My Shared Folder 1",
              "can_edit": true,
              "permissions": [
                { "uid": "kVM96KGEoGxhskZoSTd_jw", "manage_users": true },
                { "name": "myusername@company.com", "manage_users": true }
              ]
            }
          ],
          "records": [
            { "title": "Bank", "login": "c1", "password": "p",
              "folders": [ { "shared_folder": "My Shared Folder 1" } ] }
          ]
        }"#;
        let plan = plan_for(json, azure_caps(), BackendKind::Azure);

        assert!(
            plan.warnings.iter().any(|w| w.contains("NOT applied")),
            "{:?}",
            plan.warnings
        );
        assert!(
            plan.warnings
                .iter()
                .any(|w| w.contains("myusername@company.com")
                    && w.contains("team kVM96KGEoGxhskZoSTd_jw")),
            "principals must be named so grants can be rebuilt: {:?}",
            plan.warnings
        );
        // The record still imports, into the shared folder's path.
        assert_eq!(plan.requests.len(), 1);
        assert_eq!(
            plan.requests[0].folder.as_deref(),
            Some("My Shared Folder 1")
        );
    }

    #[test]
    fn a_record_without_a_login_degrades_to_a_plain_secret() {
        let json = r#"{"records":[
          {"title":"Wifi Key","password":"hunter2","notes":"guest network"}
        ]}"#;
        let plan = plan_for(json, local_caps(), BackendKind::Local);
        assert_eq!(plan.requests.len(), 1);
        let req = &plan.requests[0];
        assert_eq!(req.content_type.as_deref(), Some("text/plain"));
        assert_eq!(req.value.as_str(), "hunter2");
        assert_eq!(req.note.as_deref(), Some("guest network"));
        assert!(!req.tags.as_ref().unwrap().contains_key(TYPE_TAG));
    }

    #[test]
    fn a_secure_note_without_a_password_keeps_its_notes_as_the_value() {
        let json = r#"{"records":[
          {"title":"Recovery Steps","notes":"call the bank first"}
        ]}"#;
        let plan = plan_for(json, local_caps(), BackendKind::Local);
        assert_eq!(plan.requests.len(), 1);
        assert_eq!(plan.requests[0].value.as_str(), "call the bank first");
        assert!(plan.warnings.iter().any(|w| w.contains("no password")));
    }

    #[test]
    fn a_record_with_nothing_storable_is_rejected_not_dropped() {
        let json = r#"{"records":[{"title":"Empty"}]}"#;
        let plan = plan_for(json, local_caps(), BackendKind::Local);
        assert!(plan.requests.is_empty());
        assert_eq!(plan.rejected.len(), 1);
        assert_eq!(plan.rejected[0].0, "Empty");
        assert!(plan.rejected[0].1.contains("nothing to store"));
    }

    /// Regression (PR #396 review): a password-less secure note carrying a
    /// `$oneTimeCode` used to import successfully with the seed silently
    /// dropped — the guard lived in the password-without-login arm only, and
    /// this shape returns from a different arm.
    #[test]
    fn refuses_a_passwordless_note_carrying_a_totp_seed() {
        let json = r#"{"records":[{"title":"Note","notes":"recovery info",
          "custom_fields":{"$oneTimeCode":"otpauth://totp/x?secret=JBSWY3DPEHPK3PXP"}}]}"#;
        let plan = plan_for(json, local_caps(), BackendKind::Local);

        assert!(
            plan.requests.is_empty(),
            "must not import while dropping the seed: {:?}",
            plan.requests
        );
        assert_eq!(plan.rejected.len(), 1);
        assert!(
            plan.rejected[0].1.contains("TOTP seed"),
            "{:?}",
            plan.rejected
        );
    }

    /// The seed must not be lost to the "nothing to store" path either: with
    /// no password and no notes, the accurate reason is the seed, not
    /// emptiness.
    #[test]
    fn a_record_carrying_only_a_totp_seed_is_refused_for_the_right_reason() {
        let json = r#"{"records":[{"title":"OnlyOtp",
          "custom_fields":{"$oneTimeCode":"otpauth://totp/x?secret=ABC"}}]}"#;
        let plan = plan_for(json, local_caps(), BackendKind::Local);

        assert!(plan.requests.is_empty());
        assert_eq!(plan.rejected.len(), 1);
        assert!(
            plan.rejected[0].1.contains("TOTP seed"),
            "expected the seed to be named as the reason, got: {:?}",
            plan.rejected
        );
    }

    /// No TOTP seed involved, so a secure note still imports normally — the
    /// guard must not over-reach.
    #[test]
    fn a_passwordless_note_without_a_seed_still_imports() {
        let json = r#"{"records":[{"title":"Plain Note","notes":"just text",
          "custom_fields":{"Category":"personal"}}]}"#;
        let plan = plan_for(json, local_caps(), BackendKind::Local);
        assert_eq!(plan.requests.len(), 1, "{:?}", plan.rejected);
        assert_eq!(plan.requests[0].value.as_str(), "just text");
    }

    /// Whatever the shape, a seed is never written into a tag.
    #[test]
    fn no_import_shape_ever_puts_a_seed_in_a_tag() {
        let seed = "JBSWY3DPEHPK3PXP";
        let shapes = [
            r#"{"title":"A","login":"u","password":"p","custom_fields":{"$oneTimeCode":"otpauth://totp/x?secret=JBSWY3DPEHPK3PXP"}}"#,
            r#"{"title":"B","password":"p","custom_fields":{"$oneTimeCode":"otpauth://totp/x?secret=JBSWY3DPEHPK3PXP"}}"#,
            r#"{"title":"C","notes":"n","custom_fields":{"$oneTimeCode":"otpauth://totp/x?secret=JBSWY3DPEHPK3PXP"}}"#,
            r#"{"title":"D","custom_fields":{"$oneTimeCode":"otpauth://totp/x?secret=JBSWY3DPEHPK3PXP"}}"#,
        ];
        for shape in shapes {
            let json = format!(r#"{{"records":[{shape}]}}"#);
            let plan = plan_for(&json, local_caps(), BackendKind::Local);
            for req in &plan.requests {
                for (key, value) in req.tags.as_ref().unwrap() {
                    assert!(
                        !value.contains(seed),
                        "seed leaked into tag '{key}' for shape {shape}"
                    );
                }
                assert!(
                    !req.value.as_str().contains(seed)
                        || req.content_type.as_deref() == Some(RECORD_CONTENT_TYPE),
                    "seed stored outside a record envelope for shape {shape}"
                );
            }
        }
    }

    #[test]
    fn refuses_a_totp_seed_that_has_nowhere_safe_to_go() {
        // No login => plain secret => the one value slot is the password,
        // so the seed would have to become a plaintext tag. Refuse.
        let json = r#"{"records":[{"title":"T","password":"p","custom_fields":{
          "$oneTimeCode":"otpauth://totp/x?secret=ABC"}}]}"#;
        let plan = plan_for(json, local_caps(), BackendKind::Local);
        assert!(plan.requests.is_empty());
        assert_eq!(plan.rejected.len(), 1);
        assert!(
            plan.rejected[0].1.contains("TOTP seed"),
            "{:?}",
            plan.rejected
        );
    }

    #[test]
    fn colliding_titles_are_rejected_rather_than_overwriting() {
        // Both sanitize to "My-Server": the second must not clobber the first.
        let json = r#"{"records":[
          {"title":"My Server","login":"a","password":"1"},
          {"title":"My/Server","login":"b","password":"2"}
        ]}"#;
        let plan = plan_for(json, local_caps(), BackendKind::Local);
        assert_eq!(plan.requests.len(), 1);
        assert_eq!(plan.requests[0].name, "My Server");
        assert_eq!(plan.rejected.len(), 1);
        assert!(
            plan.rejected[0].1.contains("collides"),
            "{:?}",
            plan.rejected
        );
    }

    #[test]
    fn azure_tag_budget_overflow_is_rejected_before_any_write() {
        // login record: xv-type + original_name + created_by = 3 reserved,
        // + f.username + f.url = 5. 11 custom fields would total 16 > 15.
        let mut fields = String::new();
        for i in 0..11 {
            fields.push_str(&format!("\"cf{i}\":\"v\","));
        }
        let json = format!(
            r#"{{"records":[{{"title":"Fat","login":"a","password":"p",
               "login_url":"https://x.com","custom_fields":{{{}}}}}]}}"#,
            fields.trim_end_matches(',')
        );

        let plan = plan_for(&json, azure_caps(), BackendKind::Azure);
        assert!(plan.requests.is_empty(), "must not attempt the write");
        assert_eq!(plan.rejected.len(), 1);
        assert!(plan.rejected[0].1.contains("16"), "{:?}", plan.rejected);

        // The same file is fine on a backend with no tag cap.
        let local = plan_for(&json, local_caps(), BackendKind::Local);
        assert_eq!(local.requests.len(), 1, "{:?}", local.rejected);
    }

    #[test]
    fn custom_fields_may_not_hijack_reserved_tags() {
        let json = r#"{"records":[{"title":"X","login":"a","password":"p",
          "custom_fields":{"xv-type":"evil","f.username":"evil","folder":"evil",
                           "original_name":"evil"}}]}"#;
        let plan = plan_for(json, local_caps(), BackendKind::Local);
        let tags = plan.requests[0].tags.as_ref().unwrap();
        assert_eq!(tags.get(TYPE_TAG).map(String::as_str), Some("login"));
        assert_eq!(tags.get("f.username").map(String::as_str), Some("a"));
        assert!(!tags.contains_key("original_name"));
        assert_eq!(
            plan.warnings
                .iter()
                .filter(|w| w.contains("dropped"))
                .count(),
            4
        );
    }

    #[test]
    fn non_string_custom_fields_are_coerced_and_containers_reported() {
        let json = r#"{"records":[{"title":"X","login":"a","password":"p",
          "custom_fields":{"port":8080,"active":true,"nested":{"a":1}}}]}"#;
        let plan = plan_for(json, local_caps(), BackendKind::Local);
        let tags = plan.requests[0].tags.as_ref().unwrap();
        assert_eq!(tags.get("port").map(String::as_str), Some("8080"));
        assert_eq!(tags.get("active").map(String::as_str), Some("true"));
        assert!(!tags.contains_key("nested"));
        assert!(plan.warnings.iter().any(|w| w.contains("JSON object")));
    }

    #[test]
    fn an_unusable_folder_path_rejects_only_that_record() {
        let long = "x".repeat(60);
        let json = format!(
            r#"{{"records":[
              {{"title":"Bad","login":"a","password":"p","folders":[{{"folder":"{long}"}}]}},
              {{"title":"Good","login":"a","password":"p"}}
            ]}}"#
        );
        let plan = plan_for(&json, local_caps(), BackendKind::Local);
        assert_eq!(plan.requests.len(), 1);
        assert_eq!(plan.requests[0].name, "Good");
        assert_eq!(plan.rejected.len(), 1);
        assert!(
            plan.rejected[0].1.contains("not usable"),
            "{:?}",
            plan.rejected
        );
    }

    #[test]
    fn a_shared_folders_only_file_does_not_claim_to_have_imported_folders() {
        // xv folders are metadata on a secret, so this file creates nothing.
        // The warning must not read as a success.
        let json = r#"{"shared_folders":[
          {"path":"Customer1\\My Shared Folder 2",
           "permissions":[{"name":"recipient@example.com"}]}
        ]}"#;
        let plan = plan_for(json, local_caps(), BackendKind::Local);
        assert!(plan.requests.is_empty());
        assert!(
            plan.warnings
                .iter()
                .any(|w| w.contains("nothing was created")),
            "{:?}",
            plan.warnings
        );
        assert!(
            !plan
                .warnings
                .iter()
                .any(|w| w.contains("imported as plain folders")),
            "must not claim folders were imported: {:?}",
            plan.warnings
        );
    }

    #[test]
    fn rejects_json_with_no_recognized_arrays() {
        assert!(parse_keeper_file(r#"{"secrets":[]}"#).is_err());
        assert!(parse_keeper_file("not json").is_err());
    }

    // ── Export ────────────────────────────────────────────────────────────

    fn exported<'a>(
        name: &'a str,
        value: &'a str,
        content_type: &'a str,
        tags: &'a HashMap<String, String>,
    ) -> ExportedSecret<'a> {
        ExportedSecret {
            name,
            value,
            content_type,
            tags,
        }
    }

    #[test]
    fn exports_a_typed_record_with_full_fidelity() {
        let tags: HashMap<String, String> = [
            (TYPE_TAG, "login"),
            ("f.username", "root"),
            ("f.url", "https://myserver.com"),
            ("note", "These are some notes."),
            ("folder", "My Websites/Online"),
            ("Security Group", "Private"),
            ("original_name", "Dev Server 1"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

        let envelope = encode_envelope(
            &[("password", "123123123")]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
        .unwrap();

        let record = build_keeper_record(
            &exported("Dev Server 1", &envelope, RECORD_CONTENT_TYPE, &tags),
            &builtin_types(),
        )
        .unwrap();

        assert_eq!(record.title, "Dev Server 1");
        assert_eq!(record.record_type.as_deref(), Some("login"));
        assert_eq!(record.login.as_deref(), Some("root"));
        assert_eq!(record.password.as_deref(), Some("123123123"));
        assert_eq!(record.login_url.as_deref(), Some("https://myserver.com"));
        assert_eq!(record.notes.as_deref(), Some("These are some notes."));
        // Folder separator goes back to Keeper's backslash.
        assert_eq!(
            record.folders[0].folder.as_deref(),
            Some("My Websites\\Online")
        );
        assert_eq!(
            record.custom_fields.get("Security Group"),
            Some(&serde_json::Value::String("Private".into()))
        );
        // Backend bookkeeping must not leak into the Keeper file.
        assert!(!record.custom_fields.contains_key("original_name"));
        assert!(!record.custom_fields.contains_key("note"));
    }

    #[test]
    fn export_keeps_a_custom_field_named_like_a_declared_field() {
        // `api-key` declares an `account` metadata field. This secret has no
        // `f.account`, but it does carry a user tag called `account` — that
        // tag is real data and must survive the export.
        let tags: HashMap<String, String> = [(TYPE_TAG, "api-key"), ("account", "acme-corp")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let envelope = encode_envelope(
            &[("key", "abc123")]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
        .unwrap();

        let record = build_keeper_record(
            &exported("token", &envelope, RECORD_CONTENT_TYPE, &tags),
            &builtin_types(),
        )
        .unwrap();

        assert_eq!(record.password.as_deref(), Some("abc123"));
        assert_eq!(
            record.custom_fields.get("account"),
            Some(&serde_json::Value::String("acme-corp".into())),
            "a user tag sharing a declared field's name must not be pruned: {:?}",
            record.custom_fields
        );
    }

    #[test]
    fn exports_a_plain_secret_as_a_password_only_record() {
        let tags = HashMap::new();
        let record = build_keeper_record(
            &exported("api-token", "s3cr3t", "text/plain", &tags),
            &builtin_types(),
        )
        .unwrap();
        assert_eq!(record.title, "api-token");
        assert_eq!(record.password.as_deref(), Some("s3cr3t"));
        assert_eq!(record.record_type, None);
        assert_eq!(record.login, None);
    }

    #[test]
    fn exports_the_totp_seed_back_into_the_keeper_custom_field() {
        let tags: HashMap<String, String> = [(TYPE_TAG, "login"), ("f.username", "me")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let envelope = encode_envelope(
            &[
                ("password", "p"),
                (ONE_TIME_CODE_FIELD, "otpauth://totp/x?secret=ABC"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        )
        .unwrap();

        let record = build_keeper_record(
            &exported("fb", &envelope, RECORD_CONTENT_TYPE, &tags),
            &builtin_types(),
        )
        .unwrap();
        assert_eq!(
            record.custom_fields.get(KEEPER_ONE_TIME_CODE),
            Some(&serde_json::Value::String(
                "otpauth://totp/x?secret=ABC".into()
            ))
        );
        assert!(!record.custom_fields.contains_key(ONE_TIME_CODE_FIELD));
    }

    #[test]
    fn round_trips_the_documented_example_through_export_and_back() {
        let plan = plan_for(DOCS_RECORDS, azure_caps(), BackendKind::Azure);
        assert_eq!(plan.requests.len(), 4);

        // Re-export each planned write, simulating what the backend would
        // hand back on read.
        let mut file = KeeperFile::default();
        for req in &plan.requests {
            let mut tags = req.tags.clone().unwrap_or_default();
            if let Some(note) = &req.note {
                tags.insert("note".to_string(), note.clone());
            }
            if let Some(folder) = &req.folder {
                tags.insert("folder".to_string(), folder.clone());
            }
            file.records.push(
                build_keeper_record(
                    &exported(
                        &req.name,
                        req.value.as_str(),
                        req.content_type.as_deref().unwrap_or("text/plain"),
                        &tags,
                    ),
                    &builtin_types(),
                )
                .unwrap(),
            );
        }

        let json = serialize_keeper_file(&file).unwrap();
        let reparsed = parse_keeper_file(&json).unwrap();
        assert_eq!(reparsed.records.len(), 4);

        let dev = &reparsed.records[0];
        assert_eq!(dev.title, "Dev Server 1");
        assert_eq!(dev.login.as_deref(), Some("root"));
        assert_eq!(dev.password.as_deref(), Some("123123123"));
        assert_eq!(dev.login_url.as_deref(), Some("https://myserver.com"));
        assert_eq!(dev.notes.as_deref(), Some("These are some notes."));
        assert_eq!(dev.folders[0].folder.as_deref(), Some("Private Folder 1"));
        assert_eq!(
            dev.custom_fields.get("Security Group"),
            Some(&serde_json::Value::String("Private".into()))
        );

        // The Google record's nested path survives the full loop.
        let google = reparsed
            .records
            .iter()
            .find(|r| r.title == "Google")
            .unwrap();
        assert_eq!(
            google.folders[0].folder.as_deref(),
            Some("My Websites\\Online")
        );

        // And the TOTP seed comes back as Keeper's own field.
        let fb = reparsed
            .records
            .iter()
            .find(|r| r.title == "Facebook")
            .unwrap();
        assert!(fb
            .custom_fields
            .get(KEEPER_ONE_TIME_CODE)
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.contains("JBSWY3DPEHPK3PXP")));
    }
}
