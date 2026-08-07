//! Keeper Security JSON import/export format.
//!
//! Implements the format documented at
//! <https://docs.keeper.io/user-guides/import-records-1/import-json>, used by
//! `xv vault import --fmt keeper` and `xv vault export --fmt keeper`.
//!
//! # Mapping
//!
//! | Keeper                       | xv                                         |
//! |------------------------------|--------------------------------------------|
//! | `title`                      | secret name (sanitized; original in a tag) |
//! | `login`                      | `f.username` tag                           |
//! | `password`                   | envelope primary field                     |
//! | `login_url`                  | `f.url` tag                                |
//! | `notes`, `$note` fields      | `note` tag (merged)                        |
//! | `folders[].folder`           | `folder` tag (`\` → `/`)                   |
//! | scalar `custom_fields`       | user tags                                  |
//! | `$secret:*`, `$oneTimeCode`  | envelope fields                            |
//! | object `custom_fields`       | envelope fields, one per sub-key           |
//!
//! ## Why object fields are always envelope material
//!
//! Keeper keeps its most sensitive values inside *object-valued* custom
//! fields: `$keyPair` holds `privateKey`/`publicKey`, `$paymentCard` holds
//! `cardNumber`/`cardSecurityCode`, `$passkey` holds a `privateKey`. Every
//! sub-key is therefore flattened into the encrypted envelope, never a tag —
//! tags are unencrypted metadata and capped at 256 characters, so a private
//! key there would be both exposed and rejected. Over-protecting a
//! `publicKey` costs nothing; under-protecting a `privateKey` is a leak.
//!
//! ## Choosing a record type
//!
//! A record must have exactly one primary field, and most Keeper credentials
//! have no password at all (in a 434-record production export, none of the 27
//! SSH keypairs had a login and only 5 had a password). Type selection is, in
//! order: `ssh-key` when a private key is present, `payment-card` when a card
//! number is, `login` for a username/password pair, and `secure-note`
//! otherwise — with the primary taken from password, then the first secret
//! field, then the notes.
//!
//! Only a record with genuinely nothing in it is refused. That happens when
//! its content was a Keeper *file attachment*, which the JSON export omits.
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

    // Every type an import can select must exist before any record is
    // translated, so a config block shadowing one fails the whole import with
    // a clear message rather than a run of per-record refusals.
    for required in [LOGIN_TYPE, "ssh-key", "payment-card", "secure-note"] {
        if find_type(types, required).is_none() {
            return Err(CrosstacheError::config(format!(
                "record type '{required}' is not defined; Keeper import needs it. Did a \
                 [types.{required}] block in config override the built-in?"
            )));
        }
    }

    let mut used_names = NameAllocator::default();

    for record in &file.records {
        match plan_record(
            record,
            types,
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

/// Keeper encodes a custom-field key as `$<type>:<label>:<n>`, `$<type>::<n>`,
/// or a bare `$<type>`; a user-named field carries no `$` at all. Returns the
/// Keeper base type (when typed) and the label to store the value under.
fn parse_field_key(key: &str) -> (Option<&str>, String) {
    let Some(body) = key.strip_prefix('$') else {
        return (None, key.to_string());
    };
    let mut parts = body.split(':');
    let base = parts.next().unwrap_or("");
    let label = parts.next().unwrap_or("").trim();
    let display = if label.is_empty() { base } else { label };
    (Some(base), display.to_string())
}

/// Keeper base types whose scalar value is secret material. Anything not
/// listed (text, host, email, url, note, …) is ordinary listable metadata.
const SECRET_BASE_TYPES: &[&str] = &[
    "secret",
    "password",
    "oneTimeCode",
    "trafficEncryptionSeed",
    "pinCode",
    "privateKey",
    "keyPair",
    "paymentCard",
    "passkey",
    "bankAccount",
];

/// camelCase / spaced / underscored -> kebab-case, matching the field-name
/// charset `RecordType::validate` enforces.
fn kebab(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 4);
    for (i, &ch) in chars.iter().enumerate() {
        if ch.is_ascii_uppercase() {
            let prev = i.checked_sub(1).map(|p| chars[p]);
            let next = chars.get(i + 1).copied();
            // Split on a lower->upper boundary ("hostName"), and also at the
            // end of an acronym run ("privatePEMKey" -> private-pem-key);
            // without the second rule the acronym swallows the next word.
            let after_word = prev.is_some_and(|p| p.is_ascii_lowercase() || p.is_ascii_digit());
            let acronym_end = prev.is_some_and(|p| p.is_ascii_uppercase())
                && next.is_some_and(|n| n.is_ascii_lowercase());
            if after_word || acronym_end {
                out.push('-');
            }
            out.push(ch.to_ascii_lowercase());
        } else if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    // Collapse and trim separators so the result is a legal field name.
    let mut collapsed = String::with_capacity(out.len());
    for ch in out.chars() {
        if ch == '-' && collapsed.ends_with('-') {
            continue;
        }
        collapsed.push(ch);
    }
    collapsed.trim_matches('-').to_string()
}

/// Envelope field name for one sub-key of a Keeper object field.
///
/// The special cases are exactly the sub-keys the `ssh-key` and
/// `payment-card` types declare, so a flattened object lands on its type's
/// real fields rather than shadowing them with a near-duplicate name.
fn object_field_name(base: &str, sub: &str) -> String {
    match (base, sub) {
        ("keyPair", "privateKey") => "private-key".to_string(),
        ("keyPair", "publicKey") => "public-key".to_string(),
        ("paymentCard", "cardNumber") => "card-number".to_string(),
        ("paymentCard", "cardSecurityCode") => "card-security-code".to_string(),
        ("paymentCard", "cardExpirationDate") => "card-expiration-date".to_string(),
        _ => {
            let b = kebab(base);
            let s = kebab(sub);
            if b.is_empty() {
                s
            } else if s.is_empty() {
                b
            } else {
                format!("{b}-{s}")
            }
        }
    }
}

/// A record's custom fields, split by where they can legally be stored.
#[derive(Default)]
struct Classified {
    /// Listable metadata, destined for plain tags.
    tags: BTreeMap<String, String>,
    /// Encrypted envelope fields.
    secrets: BTreeMap<String, String>,
    /// Which Keeper object types were present, for type selection.
    objects: BTreeMap<String, BTreeMap<String, String>>,
    /// Text from Keeper `$note` fields, merged into the record's note.
    notes: Vec<String>,
    /// Keeper base type -> the tag label its scalar value was stored under.
    /// Lets a record type claim a value by Keeper's *type* (`$host`) rather
    /// than the user-chosen label ("Host", "Server", …), which varies.
    by_base: BTreeMap<String, String>,
}

/// Splits `custom_fields` into tags and envelope material.
///
/// Every sub-key of an object-valued field becomes an envelope field: those
/// objects are where Keeper keeps private keys, card numbers and passkeys, and
/// a tag is both size-limited and unencrypted. Over-protecting a `publicKey`
/// or a `city` costs nothing; under-protecting a `privateKey` is a leak.
fn classify_custom_fields(
    record: &KeeperRecord,
    title: &str,
    warnings: &mut Vec<String>,
) -> Classified {
    let mut out = Classified::default();

    for (key, raw) in &record.custom_fields {
        let (base, label) = parse_field_key(key);

        if let Some(obj) = raw.as_object() {
            // A user-named object field has no Keeper base type, so its own
            // label is the parent name — otherwise its sub-keys would collide
            // with any other object's identically-named sub-key.
            let base_name = base.unwrap_or(label.as_str());
            let mut flat = BTreeMap::new();
            for (sub, val) in obj {
                let text = match val {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Null => continue,
                    // A nested object/array has no flat rendering; keep it as
                    // JSON rather than drop it.
                    other => other.to_string(),
                };
                if text.trim().is_empty() {
                    continue;
                }
                flat.insert(sub.clone(), text.clone());
                out.secrets.insert(object_field_name(base_name, sub), text);
            }
            if !flat.is_empty() {
                out.objects.insert(base_name.to_string(), flat);
            }
            continue;
        }

        let Some(value) = stringify_custom_field(raw) else {
            // Arrays remain unrepresentable as a single field.
            warnings.push(format!(
                "{title}: custom field '{key}' is a JSON {} and was dropped",
                json_kind(raw)
            ));
            continue;
        };
        if value.trim().is_empty() {
            continue;
        }

        if key == KEEPER_ONE_TIME_CODE {
            out.secrets.insert(ONE_TIME_CODE_FIELD.to_string(), value);
            continue;
        }

        // `$note` is Keeper's note *field type*, not a user field that happens
        // to be called "note". It is frequently a record's only content, so
        // treating it as a reserved-tag collision would discard the record
        // entirely.
        if base == Some("note") {
            out.notes.push(value);
            continue;
        }

        if base.is_some_and(|b| SECRET_BASE_TYPES.contains(&b)) {
            out.secrets.insert(kebab(&label), value);
            continue;
        }

        if let Some(reason) = reserved_tag_reason(&label) {
            // Rename rather than drop: the name is unusable, the value is
            // still the user's data.
            let renamed = format!("keeper-{label}");
            warnings.push(format!(
                "{title}: custom field '{key}' {reason}; stored as '{renamed}' instead"
            ));
            out.tags.insert(renamed, value);
            continue;
        }
        if let Some(b) = base {
            out.by_base.entry(b.to_string()).or_insert(label.clone());
        }
        out.tags.insert(label, value);
    }

    out
}

impl Classified {
    /// Removes and returns the scalar value Keeper stored under base type
    /// `base`, whatever label the user gave it.
    fn take_by_base(&mut self, base: &str) -> Option<String> {
        let label = self.by_base.remove(base)?;
        self.tags.remove(&label)
    }
}

/// The record type a Keeper record maps onto, plus the field its primary
/// value came from.
struct TypeChoice {
    type_name: &'static str,
    primary_field: &'static str,
    primary_value: String,
}

/// Chooses the record type. A record must have exactly one primary field, so
/// this is also what decides whether a password-less record is storable at
/// all.
fn choose_type(
    password: Option<&str>,
    username: Option<&str>,
    notes: Option<&str>,
    class: &mut Classified,
) -> Option<TypeChoice> {
    // An SSH keypair: the private key is the record's reason to exist.
    if let Some(pk) = class.secrets.get("private-key").cloned() {
        if class.objects.contains_key("keyPair") {
            class.secrets.remove("private-key");
            return Some(TypeChoice {
                type_name: "ssh-key",
                primary_field: "private-key",
                primary_value: pk,
            });
        }
    }

    // A payment card.
    if let Some(num) = class.secrets.get("card-number").cloned() {
        if class.objects.contains_key("paymentCard") {
            class.secrets.remove("card-number");
            return Some(TypeChoice {
                type_name: "payment-card",
                primary_field: "card-number",
                primary_value: num,
            });
        }
    }

    // The classic case: a username/password login.
    if let (Some(pw), Some(_)) = (password, username) {
        return Some(TypeChoice {
            type_name: LOGIN_TYPE,
            primary_field: "password",
            primary_value: pw.to_string(),
        });
    }

    // Everything else that still carries secret material. Precedence puts the
    // most credential-like value in the primary slot so plain `xv get` returns
    // what the user most likely wants.
    let content = password
        .map(str::to_string)
        .or_else(|| {
            // The first remaining envelope field, deterministic by name.
            class.secrets.keys().next().cloned().map(|k| {
                let v = class.secrets.get(&k).cloned().unwrap_or_default();
                class.secrets.remove(&k);
                v
            })
        })
        .or_else(|| notes.map(str::to_string))?;

    Some(TypeChoice {
        type_name: "secure-note",
        primary_field: "content",
        primary_value: content,
    })
}

/// Translates one record. `Err` carries the human-readable refusal reason.
fn plan_record(
    record: &KeeperRecord,
    types: &[RecordType],
    caps: &BackendCapabilities,
    backend_kind: BackendKind,
    used_names: &mut NameAllocator,
    warnings: &mut Vec<String>,
) -> std::result::Result<Option<SecretRequest>, String> {
    let title = display_title(&record.title);

    if record.title.trim().is_empty() {
        return Err("record has no title, so it has no secret name to store under".to_string());
    }

    let folder = resolve_folder(record, warnings, &title)?;
    let mut class = classify_custom_fields(record, &title, warnings);

    // Keeper's own `notes` plus any `$note` custom fields, which are often a
    // record's only content.
    let note = {
        let mut parts: Vec<String> = Vec::new();
        if let Some(n) = record.notes.as_ref().and_then(|s| non_empty(s)) {
            parts.push(n);
        }
        parts.extend(class.notes.iter().cloned());
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n\n"))
        }
    };
    let password = record.password.as_ref().and_then(|s| non_empty(s));
    let username = record.login.as_ref().and_then(|s| non_empty(s));

    let Some(choice) = choose_type(
        password.as_deref(),
        username.as_deref(),
        note.as_deref(),
        &mut class,
    ) else {
        // Typically a Keeper record whose content is a file attachment:
        // Keeper's JSON export carries record fields only, so such a record
        // arrives genuinely empty and no import can recover it.
        return Err(
            "has no password, notes, or custom fields, so there is nothing to store (a Keeper \
             record whose content is a file attachment exports empty — download the attachment \
             from Keeper and add it with 'xv attach')"
                .to_string(),
        );
    };

    let record_type = find_type(types, choice.type_name).ok_or_else(|| {
        format!(
            "record type '{}' is not defined; a config [types.{}] block may be overriding the \
             built-in",
            choice.type_name, choice.type_name
        )
    })?;

    // The name is allocated only once the record is known to be storable, so
    // a refused record never consumes a name its successor could have used.
    let name = used_names.allocate(&record.title, folder.as_deref(), warnings);

    // Metadata fields declared by the chosen type.
    let mut metadata: BTreeMap<String, String> = BTreeMap::new();
    if let Some(u) = username.clone() {
        if record_type.field("username").is_some() {
            metadata.insert("username".to_string(), u);
        }
    }
    if let Some(url) = record.login_url.as_ref().and_then(|s| non_empty(s)) {
        if record_type.field("url").is_some() {
            metadata.insert("url".to_string(), url);
        } else {
            class.tags.insert("url".to_string(), url);
        }
    }
    // Promote Keeper-typed scalars onto the declared fields of the chosen
    // type, so `$host:Server:1` lands on `f.host` rather than a tag named
    // after whatever label the user happened to pick.
    for (base, declared) in [("host", "host"), ("text", "cardholder-name")] {
        if record_type.field(declared).is_none() {
            continue;
        }
        if base == "text" && declared == "cardholder-name" {
            // Only the cardholder-name text field, not every $text field.
            if class.by_base.get("text").map(String::as_str) != Some("cardholderName") {
                continue;
            }
        }
        if let Some(v) = class.take_by_base(base) {
            metadata.insert(declared.to_string(), v);
        }
    }

    // Envelope: the primary plus every remaining secret field.
    let mut envelope: BTreeMap<String, String> = class.secrets.clone();
    envelope.insert(
        choice.primary_field.to_string(),
        choice.primary_value.clone(),
    );

    // A note that became the primary value must not also be duplicated into a
    // tag.
    let note_is_primary = note
        .as_deref()
        .is_some_and(|n| n == choice.primary_value.as_str());
    let mut note_tag = if note_is_primary { None } else { note.clone() };

    // Anything too long for a tag moves into the envelope rather than failing
    // at the backend. Only when it would actually be rejected, so a backend
    // without a cap (local) keeps the value listable.
    if let Some(max) = caps.max_tag_value_len {
        if let Some(n) = note_tag.clone() {
            if n.len() > max {
                warnings.push(format!(
                    "{title}: note is {} characters, over the backend's {max}-character tag \
                     limit; stored as an encrypted 'note' field instead of listable metadata",
                    n.len()
                ));
                envelope.insert("note".to_string(), n);
                note_tag = None;
            }
        }
        let oversized_meta: Vec<String> = metadata
            .iter()
            .filter(|(_, v)| v.len() > max)
            .map(|(k, _)| k.clone())
            .collect();
        for key in oversized_meta {
            let value = metadata.remove(&key).unwrap_or_default();
            warnings.push(format!(
                "{title}: field '{key}' is {} characters, over the backend's {max}-character \
                 tag limit; stored as an encrypted field instead of listable metadata",
                value.len()
            ));
            envelope.insert(key, value);
        }
        let oversized_tags: Vec<String> = class
            .tags
            .iter()
            .filter(|(_, v)| v.len() > max)
            .map(|(k, _)| k.clone())
            .collect();
        for key in oversized_tags {
            let value = class.tags.remove(&key).unwrap_or_default();
            warnings.push(format!(
                "{title}: custom field '{key}' is {} characters, over the backend's \
                 {max}-character tag limit; stored as an encrypted field instead of a tag",
                value.len()
            ));
            envelope.insert(kebab(&key), value);
        }
    }

    let field_tags: BTreeMap<String, String> = metadata
        .iter()
        .map(|(k, v)| (format!("{FIELD_TAG_PREFIX}{k}"), v.clone()))
        .collect();

    let reserved = predicted_reserved_tag_count(
        backend_kind,
        true, // xv-type
        false,
        note_tag.is_some(),
        folder.is_some(),
        false,
    );
    check_tag_budget(caps, reserved, &field_tags, &class.tags).map_err(|e| e.to_string())?;

    let envelope_value = encode_envelope(&envelope).map_err(|e| e.to_string())?;

    let mut tags: HashMap<String, String> = HashMap::new();
    tags.insert(TYPE_TAG.to_string(), record_type.name.clone());
    tags.extend(field_tags);
    tags.extend(class.tags.clone());

    Ok(Some(SecretRequest {
        name,
        value: Zeroizing::new(envelope_value),
        content_type: Some(RECORD_CONTENT_TYPE.to_string()),
        enabled: Some(true),
        expires_on: None,
        not_before: None,
        tags: Some(tags),
        groups: None,
        note: note_tag,
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
/// Hands out a unique secret name per record.
///
/// Duplicate titles are ordinary in Keeper — folders disambiguate them there,
/// and a real export has many ("American Express" ×3, "github.com" ×2). xv has
/// one flat namespace per vault, so a duplicate would silently overwrite its
/// predecessor. Collisions are tracked on the *sanitized* name because that is
/// what the backend keys on: "My Server" and "My/Server" both sanitize to
/// "My-Server".
#[derive(Default)]
pub struct NameAllocator {
    used: HashSet<String>,
}

impl NameAllocator {
    /// Returns a title guaranteed not to collide with one already handed out.
    ///
    /// Prefers qualifying with the record's folder, which keeps the name
    /// meaningful ("Finance/American Express" -> "Finance-American-Express").
    /// When the folder does not disambiguate either — common, since duplicates
    /// often share a folder — falls back to a numeric suffix.
    fn allocate(
        &mut self,
        title: &str,
        folder: Option<&str>,
        warnings: &mut Vec<String>,
    ) -> String {
        if self.try_take(title) {
            return title.to_string();
        }

        if let Some(folder) = folder {
            // Use the last path segment: the full nested path makes for very
            // long names and the leaf is what distinguishes siblings.
            let leaf = folder.rsplit('/').next().unwrap_or(folder);
            let qualified = format!("{leaf} {title}");
            if self.try_take(&qualified) {
                warnings.push(format!(
                    "{title}: title already used by an earlier record; imported as '{qualified}' \
                     (original title kept in the original_name tag)"
                ));
                return qualified;
            }
        }

        for n in 2..=1000 {
            let candidate = format!("{title} {n}");
            if self.try_take(&candidate) {
                warnings.push(format!(
                    "{title}: title already used by an earlier record; imported as '{candidate}' \
                     (original title kept in the original_name tag)"
                ));
                return candidate;
            }
        }

        // Unreachable in practice; fall back to the raw title rather than
        // panicking, and let the backend's own conflict handling decide.
        title.to_string()
    }

    /// Claims `title`'s sanitized form if free.
    fn try_take(&mut self, title: &str) -> bool {
        match crate::utils::sanitizer::sanitize_secret_name(title) {
            Ok(sanitized) => self.used.insert(sanitized),
            // An unsanitizable title can't be deduplicated; let it through and
            // let the backend reject it with a definitive error.
            Err(_) => true,
        }
    }
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
    fn a_record_without_a_login_becomes_a_secure_note() {
        // No username means the `login` type's required field can't be met,
        // but the password is still secret material and must stay encrypted.
        let json = r#"{"records":[
          {"title":"Wifi Key","password":"hunter2","notes":"guest network"}
        ]}"#;
        let plan = plan_for(json, local_caps(), BackendKind::Local);
        assert_eq!(plan.requests.len(), 1, "{:?}", plan.rejected);
        let req = &plan.requests[0];
        assert_eq!(req.content_type.as_deref(), Some(RECORD_CONTENT_TYPE));
        assert_eq!(
            req.tags.as_ref().unwrap().get(TYPE_TAG).map(String::as_str),
            Some("secure-note")
        );
        let env = parse_envelope(&req.value).unwrap();
        assert_eq!(env.get("content").map(String::as_str), Some("hunter2"));
        assert_eq!(req.note.as_deref(), Some("guest network"));
    }

    #[test]
    fn a_secure_note_without_a_password_keeps_its_notes_as_the_value() {
        let json = r#"{"records":[
          {"title":"Recovery Steps","notes":"call the bank first"}
        ]}"#;
        let plan = plan_for(json, local_caps(), BackendKind::Local);
        assert_eq!(plan.requests.len(), 1, "{:?}", plan.rejected);
        let env = parse_envelope(&plan.requests[0].value).unwrap();
        assert_eq!(
            env.get("content").map(String::as_str),
            Some("call the bank first")
        );
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

    /// Regression (PR #396 review): a password-less note carrying a
    /// `$oneTimeCode` once imported with the seed silently dropped. It is no
    /// longer refused either — `secure-note` gives the seed a real encrypted
    /// home, which is strictly better than rejecting the record.
    #[test]
    fn a_passwordless_note_carrying_a_totp_seed_keeps_the_seed_encrypted() {
        let json = r#"{"records":[{"title":"Note","notes":"recovery info",
          "custom_fields":{"$oneTimeCode":"otpauth://totp/x?secret=JBSWY3DPEHPK3PXP"}}]}"#;
        let plan = plan_for(json, local_caps(), BackendKind::Local);

        assert_eq!(plan.requests.len(), 1, "{:?}", plan.rejected);
        let req = &plan.requests[0];
        let env = parse_envelope(&req.value).unwrap();
        assert!(
            env.values().any(|v| v.contains("JBSWY3DPEHPK3PXP")),
            "seed must survive in the envelope: {env:?}"
        );
        for (k, v) in req.tags.as_ref().unwrap() {
            assert!(
                !v.contains("JBSWY3DPEHPK3PXP"),
                "seed leaked into tag '{k}'"
            );
        }
    }

    /// A record whose only content is a seed is still worth importing.
    #[test]
    fn a_record_carrying_only_a_totp_seed_is_stored_not_refused() {
        let json = r#"{"records":[{"title":"OnlyOtp",
          "custom_fields":{"$oneTimeCode":"otpauth://totp/x?secret=ABC"}}]}"#;
        let plan = plan_for(json, local_caps(), BackendKind::Local);

        assert_eq!(plan.requests.len(), 1, "{:?}", plan.rejected);
        let env = parse_envelope(&plan.requests[0].value).unwrap();
        assert!(env.values().any(|v| v.contains("secret=ABC")), "{env:?}");
    }

    #[test]
    fn a_passwordless_note_without_a_seed_still_imports() {
        let json = r#"{"records":[{"title":"Plain Note","notes":"just text",
          "custom_fields":{"Category":"personal"}}]}"#;
        let plan = plan_for(json, local_caps(), BackendKind::Local);
        assert_eq!(plan.requests.len(), 1, "{:?}", plan.rejected);
        let env = parse_envelope(&plan.requests[0].value).unwrap();
        assert_eq!(env.get("content").map(String::as_str), Some("just text"));
        // A plain user field stays listable metadata.
        assert_eq!(
            plan.requests[0]
                .tags
                .as_ref()
                .unwrap()
                .get("Category")
                .map(String::as_str),
            Some("personal")
        );
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
    fn a_password_without_a_login_still_keeps_its_totp_seed() {
        // Previously refused: with only `login` available as a typed shape,
        // there was nowhere to put the seed. `secure-note` removes that
        // constraint, so the record imports with both values encrypted.
        let json = r#"{"records":[{"title":"T","password":"p","custom_fields":{
          "$oneTimeCode":"otpauth://totp/x?secret=ABC"}}]}"#;
        let plan = plan_for(json, local_caps(), BackendKind::Local);
        assert_eq!(plan.requests.len(), 1, "{:?}", plan.rejected);
        let env = parse_envelope(&plan.requests[0].value).unwrap();
        assert_eq!(env.get("content").map(String::as_str), Some("p"));
        assert!(
            env.get(ONE_TIME_CODE_FIELD)
                .is_some_and(|v| v.contains("secret=ABC")),
            "{env:?}"
        );
    }

    #[test]
    fn colliding_titles_are_disambiguated_rather_than_dropped() {
        // Both sanitize to "My-Server". Neither may be lost, and the second
        // must not clobber the first.
        let json = r#"{"records":[
          {"title":"My Server","login":"a","password":"1"},
          {"title":"My/Server","login":"b","password":"2"}
        ]}"#;
        let plan = plan_for(json, local_caps(), BackendKind::Local);
        assert_eq!(plan.requests.len(), 2, "{:?}", plan.rejected);
        assert!(plan.rejected.is_empty());

        let sanitized: Vec<String> = plan
            .requests
            .iter()
            .map(|r| crate::utils::sanitizer::sanitize_secret_name(&r.name).unwrap())
            .collect();
        assert_eq!(
            sanitized.iter().collect::<HashSet<_>>().len(),
            2,
            "names must be distinct after sanitization: {sanitized:?}"
        );
        assert!(plan.warnings.iter().any(|w| w.contains("already used")));
    }

    #[test]
    fn a_colliding_title_is_qualified_by_its_folder_when_that_disambiguates() {
        let json = r#"{"records":[
          {"title":"American Express","login":"a","password":"1",
           "folders":[{"folder":"Finance"}]},
          {"title":"American Express","login":"b","password":"2",
           "folders":[{"folder":"Cards"}]}
        ]}"#;
        let plan = plan_for(json, local_caps(), BackendKind::Local);
        assert_eq!(plan.requests.len(), 2, "{:?}", plan.rejected);
        let names: Vec<&str> = plan.requests.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names[0], "American Express");
        assert_eq!(
            names[1], "Cards American Express",
            "the folder should carry the disambiguation, not a bare counter"
        );
    }

    #[test]
    fn same_folder_collisions_fall_back_to_a_numeric_suffix() {
        let json = r#"{"records":[
          {"title":"github.com","login":"a","password":"1","folders":[{"folder":"Web"}]},
          {"title":"github.com","login":"b","password":"2","folders":[{"folder":"Web"}]},
          {"title":"github.com","login":"c","password":"3","folders":[{"folder":"Web"}]}
        ]}"#;
        let plan = plan_for(json, local_caps(), BackendKind::Local);
        assert_eq!(plan.requests.len(), 3, "{:?}", plan.rejected);
        let names: Vec<&str> = plan.requests.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names[0], "github.com");
        assert_eq!(names[1], "Web github.com");
        assert_eq!(names[2], "github.com 2");
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
        // The reserved names keep their real meaning...
        assert!(!tags.contains_key("original_name"));
        // ...but the user's values are renamed, not discarded.
        assert_eq!(
            tags.get("keeper-original_name").map(String::as_str),
            Some("evil")
        );
        assert_eq!(tags.get("keeper-folder").map(String::as_str), Some("evil"));
        assert_eq!(
            plan.warnings
                .iter()
                .filter(|w| w.contains("stored as 'keeper-"))
                .count(),
            4
        );
    }

    #[test]
    fn non_string_custom_fields_are_coerced_and_objects_flattened() {
        let json = r#"{"records":[{"title":"X","login":"a","password":"p",
          "custom_fields":{"port":8080,"active":true,"nested":{"a":"1"}}}]}"#;
        let plan = plan_for(json, local_caps(), BackendKind::Local);
        let req = &plan.requests[0];
        let tags = req.tags.as_ref().unwrap();
        assert_eq!(tags.get("port").map(String::as_str), Some("8080"));
        assert_eq!(tags.get("active").map(String::as_str), Some("true"));
        // An object is no longer dropped: its sub-keys become envelope fields.
        assert!(!tags.contains_key("nested"));
        let env = parse_envelope(&req.value).unwrap();
        assert_eq!(env.get("nested-a").map(String::as_str), Some("1"));
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

    // ── Real-world Keeper shapes (from a 434-record production export) ────

    /// The single most important case: Keeper keeps SSH private keys inside a
    /// `$keyPair` object, and none of the 27 in the source export had a login
    /// or (mostly) a password. Dropping the object dropped the private key.
    #[test]
    fn a_keypair_record_becomes_an_ssh_key_with_the_private_key_encrypted() {
        let json = r#"{"records":[{"title":"BMO SFTP key",
          "custom_fields":{"$keyPair::1":{
            "privateKey":"-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n",
            "publicKey":"ssh-rsa AAAAB3Nza"},
            "$host:Host:1":"sftp.example.com"}}]}"#;
        let plan = plan_for(json, azure_caps(), BackendKind::Azure);
        assert_eq!(plan.requests.len(), 1, "{:?}", plan.rejected);
        let req = &plan.requests[0];

        let tags = req.tags.as_ref().unwrap();
        assert_eq!(tags.get(TYPE_TAG).map(String::as_str), Some("ssh-key"));

        let env = parse_envelope(&req.value).unwrap();
        assert!(env
            .get("private-key")
            .is_some_and(|v| v.contains("BEGIN OPENSSH")));
        assert!(env
            .get("public-key")
            .is_some_and(|v| v.starts_with("ssh-rsa")));

        // The key material must never appear in listable metadata.
        for (k, v) in tags {
            assert!(
                !v.contains("BEGIN OPENSSH"),
                "private key leaked into '{k}'"
            );
        }
    }

    /// A keypair with only a public key has no private key to be primary, so
    /// it cannot be an `ssh-key`; it must still import rather than vanish.
    #[test]
    fn a_keypair_with_only_a_public_key_still_imports() {
        let json = r#"{"records":[{"title":"pub only",
          "custom_fields":{"$keyPair::1":{"publicKey":"ssh-rsa AAAA"}}}]}"#;
        let plan = plan_for(json, local_caps(), BackendKind::Local);
        assert_eq!(plan.requests.len(), 1, "{:?}", plan.rejected);
        let env = parse_envelope(&plan.requests[0].value).unwrap();
        assert!(env.values().any(|v| v.contains("ssh-rsa")), "{env:?}");
    }

    #[test]
    fn a_payment_card_keeps_number_and_cvv_out_of_tags() {
        let json = r#"{"records":[{"title":"Amex",
          "custom_fields":{"$paymentCard::1":{
             "cardNumber":"4111111111111111",
             "cardSecurityCode":"123",
             "cardExpirationDate":"01/2030"},
             "$text:cardholderName:1":"A Person"}}]}"#;
        let plan = plan_for(json, azure_caps(), BackendKind::Azure);
        assert_eq!(plan.requests.len(), 1, "{:?}", plan.rejected);
        let req = &plan.requests[0];
        let tags = req.tags.as_ref().unwrap();
        assert_eq!(tags.get(TYPE_TAG).map(String::as_str), Some("payment-card"));

        let env = parse_envelope(&req.value).unwrap();
        assert_eq!(
            env.get("card-number").map(String::as_str),
            Some("4111111111111111")
        );
        assert_eq!(
            env.get("card-security-code").map(String::as_str),
            Some("123")
        );
        for (k, v) in tags {
            assert!(!v.contains("4111111111111111"), "PAN leaked into '{k}'");
            assert!(
                !v.contains("123") || k.starts_with('f'),
                "CVV leaked into '{k}'"
            );
        }
        // The cardholder name is not sensitive and stays listable.
        assert_eq!(
            tags.get("f.cardholder-name").map(String::as_str),
            Some("A Person")
        );
    }

    /// Keeper's `$type:Label:N` key encoding must be understood, or every
    /// field name ends up mangled.
    #[test]
    fn parses_keeper_typed_field_key_encodings() {
        assert_eq!(
            parse_field_key("$keyPair::1"),
            (Some("keyPair"), "keyPair".into())
        );
        assert_eq!(
            parse_field_key("$text:Bit Strength:1"),
            (Some("text"), "Bit Strength".into())
        );
        assert_eq!(parse_field_key("$note"), (Some("note"), "note".into()));
        assert_eq!(
            parse_field_key("Security Group"),
            (None, "Security Group".into())
        );
    }

    #[test]
    fn kebab_produces_legal_field_names() {
        assert_eq!(kebab("privateKey"), "private-key");
        assert_eq!(kebab("privatePEMKey"), "private-pem-key");
        assert_eq!(kebab("hostName"), "host-name");
        assert_eq!(kebab("Bit Strength"), "bit-strength");
        assert_eq!(kebab("card_expiration__date"), "card-expiration-date");
        assert_eq!(kebab("  spaced  "), "spaced");
    }

    /// `$secret:` fields are secret material by Keeper's own declaration.
    #[test]
    fn keeper_secret_typed_scalars_go_to_the_envelope() {
        let json = r#"{"records":[{"title":"S","login":"u","password":"p",
          "custom_fields":{"$secret:privatePEMKey:1":"-----BEGIN RSA-----",
                           "$trafficEncryptionSeed":"seed-value",
                           "$host:Server:1":"host.example.com"}}]}"#;
        let plan = plan_for(json, local_caps(), BackendKind::Local);
        let req = &plan.requests[0];
        let env = parse_envelope(&req.value).unwrap();
        assert!(env.contains_key("private-pem-key"), "{env:?}");
        assert!(env.contains_key("traffic-encryption-seed"), "{env:?}");
        // A host is ordinary metadata.
        let tags = req.tags.as_ref().unwrap();
        assert!(
            tags.values().any(|v| v == "host.example.com"),
            "host should stay listable: {tags:?}"
        );
    }

    /// The Azure failure mode from the production import: a note longer than
    /// the 256-char tag cap reached the backend and came back as an opaque
    /// HTTP 400.
    #[test]
    fn an_oversized_note_moves_into_the_envelope_instead_of_failing() {
        let long = "x".repeat(1495);
        let json = format!(
            r#"{{"records":[{{"title":"Sendgrid","login":"u","password":"p","notes":"{long}"}}]}}"#
        );
        let plan = plan_for(&json, azure_caps(), BackendKind::Azure);
        assert_eq!(plan.requests.len(), 1, "{:?}", plan.rejected);
        let req = &plan.requests[0];
        assert!(
            req.note.is_none(),
            "oversized note must not be sent as a tag"
        );
        let env = parse_envelope(&req.value).unwrap();
        assert_eq!(env.get("note").map(String::len), Some(1495));
        assert!(plan
            .warnings
            .iter()
            .any(|w| w.contains("over the backend's")));
    }

    /// Same value on a backend with no tag cap keeps its listability.
    #[test]
    fn an_oversized_note_stays_a_tag_where_the_backend_allows_it() {
        let long = "x".repeat(1495);
        let json = format!(
            r#"{{"records":[{{"title":"Sendgrid","login":"u","password":"p","notes":"{long}"}}]}}"#
        );
        let plan = plan_for(&json, local_caps(), BackendKind::Local);
        assert_eq!(plan.requests[0].note.as_ref().map(String::len), Some(1495));
    }

    #[test]
    fn an_oversized_url_moves_into_the_envelope() {
        let long = format!("https://example.com/{}", "a".repeat(4000));
        let json = format!(
            r#"{{"records":[{{"title":"adobe.com","login":"u","password":"p","login_url":"{long}"}}]}}"#
        );
        let plan = plan_for(&json, azure_caps(), BackendKind::Azure);
        assert_eq!(plan.requests.len(), 1, "{:?}", plan.rejected);
        let req = &plan.requests[0];
        assert!(!req.tags.as_ref().unwrap().contains_key("f.url"));
        let env = parse_envelope(&req.value).unwrap();
        assert!(env.get("url").is_some_and(|v| v.len() > 4000));
    }

    /// Keeper's `$note` is a field *type*, not a user field named "note".
    /// In the source export it was 33 records' only content, and treating it
    /// as a reserved-tag collision discarded every one of them.
    #[test]
    fn a_keeper_note_field_is_kept_not_dropped_as_a_reserved_tag() {
        let json = r#"{"records":[{"title":"Adaxes License",
          "custom_fields":{"$note::1":"license key: ABC-123"}}]}"#;
        let plan = plan_for(json, azure_caps(), BackendKind::Azure);

        assert_eq!(plan.requests.len(), 1, "{:?}", plan.rejected);
        let req = &plan.requests[0];
        let env = parse_envelope(&req.value).unwrap();
        assert_eq!(
            env.get("content").map(String::as_str),
            Some("license key: ABC-123"),
            "the $note text is the record's only content and must survive"
        );
        assert!(
            !plan.warnings.iter().any(|w| w.contains("reserved")),
            "a $note field is not a reserved-tag collision: {:?}",
            plan.warnings
        );
    }

    #[test]
    fn a_keeper_note_field_merges_with_the_records_own_notes() {
        let json = r#"{"records":[{"title":"N","login":"u","password":"p",
          "notes":"first","custom_fields":{"$note::1":"second"}}]}"#;
        let plan = plan_for(json, local_caps(), BackendKind::Local);
        assert_eq!(plan.requests[0].note.as_deref(), Some("first\n\nsecond"));
    }

    /// A record whose content is a Keeper file attachment exports with no
    /// fields at all. It cannot be recovered, so the reason must say why
    /// rather than leaving the user hunting.
    #[test]
    fn an_attachment_only_record_is_refused_with_an_actionable_reason() {
        let json = r#"{"records":[{"title":"salesforce-private-key"}]}"#;
        let plan = plan_for(json, local_caps(), BackendKind::Local);
        assert_eq!(plan.rejected.len(), 1);
        assert!(
            plan.rejected[0].1.contains("attachment"),
            "{:?}",
            plan.rejected
        );
    }

    /// The invariant that matters most, asserted across every shape at once:
    /// no import path may put key material, a TOTP seed, or an oversized
    /// value into a listable tag, and no record may exceed Azure's tag cap.
    #[test]
    fn no_shape_leaks_secret_material_or_busts_the_tag_cap() {
        let mut fields = String::new();
        for i in 0..6 {
            fields.push_str(&format!(r#""extra{i}":"v","#));
        }
        let shapes = [
            r#"{"title":"A","login":"u","password":"p","custom_fields":{"$keyPair::1":{"privateKey":"-----BEGIN OPENSSH PRIVATE KEY-----x","publicKey":"ssh-rsa AAAA"}}}"#.to_string(),
            r#"{"title":"B","custom_fields":{"$keyPair::1":{"privateKey":"-----BEGIN RSA PRIVATE KEY-----y"}}}"#.to_string(),
            r#"{"title":"C","custom_fields":{"$paymentCard::1":{"cardNumber":"4111111111111111","cardSecurityCode":"999"}}}"#.to_string(),
            r#"{"title":"D","login":"u","password":"p","custom_fields":{"$oneTimeCode":"otpauth://totp/x?secret=SEED"}}"#.to_string(),
            format!(r#"{{"title":"E","login":"u","password":"p","notes":"{}","custom_fields":{{{}}}}}"#,
                    "n".repeat(900), fields.trim_end_matches(',')),
        ];
        for shape in shapes {
            let json = format!(r#"{{"records":[{shape}]}}"#);
            let plan = plan_for(&json, azure_caps(), BackendKind::Azure);
            assert_eq!(plan.requests.len(), 1, "{shape}\n{:?}", plan.rejected);
            let req = &plan.requests[0];
            let tags = req.tags.as_ref().unwrap();

            assert!(tags.len() <= 15, "{} tags for {shape}", tags.len());
            for (k, v) in tags {
                assert!(v.len() <= 256, "tag '{k}' is {} chars", v.len());
                for marker in [
                    "PRIVATE KEY",
                    "BEGIN OPENSSH",
                    "BEGIN RSA",
                    "otpauth://",
                    "4111111111111111",
                ] {
                    assert!(!v.contains(marker), "tag '{k}' leaked {marker}");
                }
            }
            if let Some(n) = &req.note {
                assert!(n.len() <= 256, "note is {} chars", n.len());
            }
        }
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
