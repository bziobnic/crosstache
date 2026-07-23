use crate::backend::{secret::split_denormalized_tags, Backend};
use crate::error::{CrosstacheError, Result};
use crate::records::{
    check_tag_budget, encode_envelope, find_type, is_record, parse_envelope,
    predicted_reserved_tag_count_for_shape, FieldKind, RecordType, FIELD_TAG_PREFIX,
    RECORD_CONTENT_TYPE, TYPE_TAG,
};
use crate::secret::manager::{FieldUpdate, SecretProperties, SecretUpdateRequest};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use zeroize::Zeroizing;

/// Shape to write after a conversion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConversionTarget {
    ToType(String),
    Plain,
}

/// User intent for a conversion.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversionRequest {
    pub target: ConversionTarget,
    #[serde(default)]
    pub supplied_fields: BTreeMap<String, String>,
    #[serde(default)]
    pub confirm_lossy: bool,
}

impl std::fmt::Debug for ConversionRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConversionRequest")
            .field("target", &self.target)
            .field(
                "supplied_fields",
                &self.supplied_fields.keys().collect::<Vec<_>>(),
            )
            .field("confirm_lossy", &self.confirm_lossy)
            .finish()
    }
}

impl ConversionRequest {
    pub fn to_type(name: impl Into<String>) -> Self {
        Self {
            target: ConversionTarget::ToType(name.into()),
            supplied_fields: BTreeMap::new(),
            confirm_lossy: false,
        }
    }

    pub fn plain() -> Self {
        Self {
            target: ConversionTarget::Plain,
            supplied_fields: BTreeMap::new(),
            confirm_lossy: false,
        }
    }
}

/// Display-safe conversion impact plus the prepared atomic write.
#[derive(Clone, Serialize)]
pub struct ConversionPreview {
    pub retained: Vec<String>,
    pub renamed: Vec<String>,
    pub copied: Vec<String>,
    pub dropped: Vec<String>,
    pub sensitivity_changes: Vec<String>,
    pub exposed: Vec<String>,
    pub target_type: Option<String>,
    pub requires_confirmation: bool,
    /// Kept out of serialized previews because these are secret values.
    #[serde(skip)]
    pub target_secret_fields: BTreeMap<String, String>,
    #[serde(skip)]
    target_metadata_fields: BTreeMap<String, String>,
    #[serde(skip)]
    prepared: PreparedConversion,
}

impl std::fmt::Debug for ConversionPreview {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConversionPreview")
            .field("retained", &self.retained)
            .field("renamed", &self.renamed)
            .field("copied", &self.copied)
            .field("dropped", &self.dropped)
            .field("sensitivity_changes", &self.sensitivity_changes)
            .field("exposed", &self.exposed)
            .field("target_type", &self.target_type)
            .field("requires_confirmation", &self.requires_confirmation)
            .field(
                "target_secret_fields",
                &self.target_secret_fields.keys().collect::<Vec<_>>(),
            )
            .field(
                "target_metadata_fields",
                &self.target_metadata_fields.keys().collect::<Vec<_>>(),
            )
            .finish()
    }
}

#[derive(Clone)]
struct PreparedConversion {
    original: SecretProperties,
    value: String,
    content_type: String,
    tags: HashMap<String, String>,
    groups: Option<Vec<String>>,
    note: Option<String>,
    folder: Option<String>,
    confirm_lossy: bool,
    no_op: bool,
}

#[derive(Default)]
struct SourceFields {
    fields: BTreeMap<String, String>,
    kinds: BTreeMap<String, FieldKind>,
    primary: Option<String>,
}

/// Build a deterministic, display-safe conversion plan without writing.
pub fn preview_conversion(
    secret: &SecretProperties,
    types: &[RecordType],
    request: ConversionRequest,
) -> Result<ConversionPreview> {
    let source_is_record = is_record(&secret.content_type);
    let source = source_fields(secret, types)?;

    match request.target.clone() {
        ConversionTarget::Plain => {
            preview_to_plain(secret, source_is_record, source, request.confirm_lossy)
        }
        ConversionTarget::ToType(target_name) => preview_to_type(
            secret,
            types,
            source_is_record,
            source,
            &target_name,
            request,
        ),
    }
}

fn source_fields(secret: &SecretProperties, types: &[RecordType]) -> Result<SourceFields> {
    if !is_record(&secret.content_type) {
        return Ok(SourceFields::default());
    }

    let type_name = secret.tags.get(TYPE_TAG).map(String::as_str).unwrap_or("");
    let source_type = find_type(types, type_name).ok_or_else(|| {
        CrosstacheError::config(format!(
            "secret '{}' has type '{type_name}', which has no resolvable type definition; conversion cannot determine its primary field",
            secret.original_name
        ))
    })?;
    let raw = secret
        .value
        .as_deref()
        .map(|value| value.as_str())
        .unwrap_or("");
    let envelope = parse_envelope(raw).map_err(|_| {
        CrosstacheError::config(format!(
            "secret '{}' has a malformed {RECORD_CONTENT_TYPE} envelope; conversion aborted",
            secret.original_name
        ))
    })?;

    let mut kinds: BTreeMap<String, FieldKind> = envelope
        .keys()
        .map(|name| (name.clone(), FieldKind::Secret))
        .collect();
    let mut fields = envelope;
    for (key, value) in &secret.tags {
        if let Some(name) = key.strip_prefix(FIELD_TAG_PREFIX) {
            if fields.insert(name.to_string(), value.clone()).is_some() {
                return Err(CrosstacheError::config(format!(
                    "secret '{}' stores field '{name}' in both metadata and its protected envelope; conversion aborted",
                    secret.original_name
                )));
            }
            kinds.insert(name.to_string(), FieldKind::Metadata);
        }
    }

    let primary = source_type.primary().name.clone();
    if !fields.contains_key(&primary) {
        return Err(CrosstacheError::config(format!(
            "secret '{}' is missing its primary field '{primary}' in the record envelope",
            secret.original_name
        )));
    }

    Ok(SourceFields {
        fields,
        kinds,
        primary: Some(primary),
    })
}

fn preview_to_plain(
    secret: &SecretProperties,
    source_is_record: bool,
    source: SourceFields,
    confirm_lossy: bool,
) -> Result<ConversionPreview> {
    if !source_is_record {
        return Err(CrosstacheError::config(format!(
            "secret '{}' is not a typed record; nothing to untype",
            secret.original_name
        )));
    }

    let primary = source.primary.as_ref().expect("record source has primary");
    let primary_value = source.fields.get(primary).expect("primary was validated");
    let mut dropped: Vec<String> = source
        .fields
        .keys()
        .filter(|name| *name != primary)
        .cloned()
        .collect();
    dropped.sort();
    let requires_confirmation = !dropped.is_empty() && !confirm_lossy;
    let mut target_secret_fields = BTreeMap::new();
    target_secret_fields.insert(primary.clone(), primary_value.clone());
    let prepared = prepare_common(
        secret,
        primary_value.clone(),
        String::new(),
        HashMap::new(),
        confirm_lossy,
        false,
    );

    Ok(ConversionPreview {
        retained: vec![primary.clone()],
        renamed: Vec::new(),
        copied: Vec::new(),
        dropped,
        sensitivity_changes: Vec::new(),
        exposed: Vec::new(),
        target_type: None,
        requires_confirmation,
        target_secret_fields,
        target_metadata_fields: BTreeMap::new(),
        prepared,
    })
}

fn preview_to_type(
    secret: &SecretProperties,
    types: &[RecordType],
    source_is_record: bool,
    source: SourceFields,
    target_name: &str,
    request: ConversionRequest,
) -> Result<ConversionPreview> {
    let target = find_type(types, target_name).ok_or_else(|| {
        let mut known: Vec<&str> = types
            .iter()
            .map(|record_type| record_type.name.as_str())
            .collect();
        known.sort_unstable();
        CrosstacheError::config(format!(
            "unknown type '{target_name}'. Known types: {}",
            known.join(", ")
        ))
    })?;

    for (name, value) in &request.supplied_fields {
        let Some(field) = target.field(name) else {
            return Err(CrosstacheError::config(format!(
                "field '{name}' is not declared by target type '{}'",
                target.name
            )));
        };
        if field.required && value.trim().is_empty() {
            return Err(CrosstacheError::config(format!(
                "required field '{name}' for type '{}' cannot be empty",
                target.name
            )));
        }
    }

    if !source_is_record {
        let value = secret
            .value
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                CrosstacheError::config(format!(
                    "secret '{}' has no value to convert",
                    secret.original_name
                ))
            })?;
        let mut all_target_fields = request.supplied_fields;
        all_target_fields
            .entry(target.primary().name.clone())
            .or_insert_with(|| value.as_str().to_string());
        return finish_typed_preview(
            secret,
            target,
            all_target_fields,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            request.confirm_lossy,
            false,
            true,
        );
    }

    let current_type = secret.tags.get(TYPE_TAG).map(String::as_str).unwrap_or("");
    let same_type = current_type == target.name;
    let mut all_target_fields = BTreeMap::new();
    let mut retained = Vec::new();
    let mut used_source = std::collections::BTreeSet::new();

    for target_field in &target.fields {
        if request.supplied_fields.contains_key(&target_field.name) {
            // An explicit value has new provenance. It intentionally replaces
            // the same-named source field, so the old value is neither
            // retained nor interpreted as crossing a sensitivity boundary.
            used_source.insert(target_field.name.clone());
            continue;
        }
        if let Some(value) = source.fields.get(&target_field.name) {
            all_target_fields.insert(target_field.name.clone(), value.clone());
            retained.push(target_field.name.clone());
            used_source.insert(target_field.name.clone());
        }
    }

    let target_primary = target.primary().name.clone();
    let mut renamed = Vec::new();
    let mut copied = Vec::new();
    if !all_target_fields.contains_key(&target_primary)
        && !request.supplied_fields.contains_key(&target_primary)
    {
        let source_primary = source.primary.as_ref().expect("record source has primary");
        if let Some(value) = source.fields.get(source_primary) {
            all_target_fields.insert(target_primary.clone(), value.clone());
            used_source.insert(source_primary.clone());
            if source_primary != &target_primary {
                let impact = format!("{source_primary} -> {target_primary}");
                if retained.iter().any(|name| name == source_primary) {
                    copied.push(impact);
                } else {
                    renamed.push(impact);
                }
            }
        }
    }

    for (name, value) in request.supplied_fields {
        all_target_fields.insert(name, value);
    }

    if same_type {
        for (name, value) in &source.fields {
            all_target_fields
                .entry(name.clone())
                .or_insert_with(|| value.clone());
            used_source.insert(name.clone());
        }
    }

    let mut dropped: Vec<String> = source
        .fields
        .keys()
        .filter(|name| !used_source.contains(*name))
        .cloned()
        .collect();
    retained.sort();
    renamed.sort();
    copied.sort();
    dropped.sort();
    let mut sensitivity_changes = Vec::new();
    let mut exposed = Vec::new();
    for field in &target.fields {
        if !retained.iter().any(|name| name == &field.name) {
            continue;
        }
        let Some(source_kind) = source.kinds.get(&field.name) else {
            continue;
        };
        if *source_kind != field.kind {
            sensitivity_changes.push(format!(
                "{}: {} -> {}",
                field.name,
                kind_name(*source_kind),
                kind_name(field.kind)
            ));
            if *source_kind == FieldKind::Secret && field.kind == FieldKind::Metadata {
                exposed.push(field.name.clone());
            }
        }
    }
    sensitivity_changes.sort();
    exposed.sort();

    finish_typed_preview(
        secret,
        target,
        all_target_fields,
        retained,
        renamed,
        copied,
        dropped,
        sensitivity_changes,
        exposed,
        request.confirm_lossy,
        same_type,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
fn finish_typed_preview(
    secret: &SecretProperties,
    target: &RecordType,
    all_target_fields: BTreeMap<String, String>,
    retained: Vec<String>,
    renamed: Vec<String>,
    copied: Vec<String>,
    dropped: Vec<String>,
    sensitivity_changes: Vec<String>,
    exposed: Vec<String>,
    confirm_lossy: bool,
    same_type_candidate: bool,
    allow_missing_required: bool,
) -> Result<ConversionPreview> {
    let primary = target.primary();
    if all_target_fields
        .get(&primary.name)
        .is_none_or(|value| value.trim().is_empty())
    {
        return Err(CrosstacheError::config(format!(
            "conversion to type '{}' requires primary field '{}'",
            target.name, primary.name
        )));
    }
    for field in &target.fields {
        if field.required
            && ((!allow_missing_required && !all_target_fields.contains_key(&field.name))
                || all_target_fields
                    .get(&field.name)
                    .is_some_and(|value| value.trim().is_empty()))
        {
            return Err(CrosstacheError::config(format!(
                "required field '{}' for type '{}' cannot be empty",
                field.name, target.name
            )));
        }
    }

    let mut target_secret_fields = BTreeMap::new();
    let mut target_metadata_fields = BTreeMap::new();
    for (name, value) in all_target_fields {
        match target.field(&name).map(|field| field.kind) {
            Some(FieldKind::Metadata) => {
                target_metadata_fields.insert(name, value);
            }
            Some(FieldKind::Secret) | None => {
                target_secret_fields.insert(name, value);
            }
        }
    }

    let value = encode_envelope(&target_secret_fields)?;
    let mut tags = HashMap::new();
    tags.insert(TYPE_TAG.to_string(), target.name.clone());
    tags.extend(
        target_metadata_fields
            .iter()
            .map(|(name, value)| (format!("{FIELD_TAG_PREFIX}{name}"), value.clone())),
    );
    let prepared = prepare_common(
        secret,
        value,
        RECORD_CONTENT_TYPE.to_string(),
        tags,
        confirm_lossy,
        false,
    );
    let mut current_tags = secret.tags.clone();
    let (current_groups, current_note, current_folder) = split_denormalized_tags(&mut current_tags);
    let no_op = same_type_candidate
        && secret.content_type == RECORD_CONTENT_TYPE
        && secret.value.as_deref().map(|value| value.as_str()) == Some(prepared.value.as_str())
        && current_tags == prepared.tags
        && current_groups == prepared.groups
        && current_note == prepared.note
        && current_folder == prepared.folder;
    let mut prepared = prepared;
    prepared.no_op = no_op;
    let requires_confirmation = (!dropped.is_empty() || !exposed.is_empty()) && !confirm_lossy;

    Ok(ConversionPreview {
        retained,
        renamed,
        copied,
        dropped,
        sensitivity_changes,
        exposed,
        target_type: Some(target.name.clone()),
        requires_confirmation,
        target_secret_fields,
        target_metadata_fields,
        prepared,
    })
}

fn kind_name(kind: FieldKind) -> &'static str {
    match kind {
        FieldKind::Metadata => "metadata",
        FieldKind::Secret => "secret",
    }
}

fn prepare_common(
    secret: &SecretProperties,
    value: String,
    content_type: String,
    record_tags: HashMap<String, String>,
    confirm_lossy: bool,
    no_op: bool,
) -> PreparedConversion {
    let mut tags = secret.tags.clone();
    tags.remove(TYPE_TAG);
    tags.retain(|key, _| !key.starts_with(FIELD_TAG_PREFIX));
    let (groups, note, folder) = split_denormalized_tags(&mut tags);
    tags.extend(record_tags);
    PreparedConversion {
        original: secret.clone(),
        value,
        content_type,
        tags,
        groups,
        note,
        folder,
        confirm_lossy,
        no_op,
    }
}

/// Reject conversion before reading values, printing impact, or prompting
/// when the backend cannot commit the complete value+metadata shape atomically.
pub fn validate_conversion_backend(backend: &dyn Backend) -> Result<()> {
    if !backend.capabilities().has_atomic_record_conversion {
        return Err(CrosstacheError::config(
            "backend does not support atomic record conversion; no changes were written",
        ));
    }
    Ok(())
}

/// Web preview/apply additionally requires a provider CAS primitive.
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub fn validate_conditional_conversion_backend(backend: &dyn Backend) -> Result<()> {
    validate_conversion_backend(backend)?;
    if !backend.capabilities().has_conditional_record_conversion
        || !backend.secrets().supports_conditional_update()
    {
        return Err(CrosstacheError::config(
            "backend does not support conditional record conversion; no changes were written",
        ));
    }
    Ok(())
}

enum ConversionCommit {
    NoOp(SecretProperties),
    Update(SecretUpdateRequest),
}

fn prepare_conversion_commit(
    backend: &dyn Backend,
    name: &str,
    preview: ConversionPreview,
) -> Result<ConversionCommit> {
    if preview.requires_confirmation
        || ((!preview.dropped.is_empty() || !preview.exposed.is_empty())
            && !preview.prepared.confirm_lossy)
    {
        let mut impacts = Vec::new();
        if !preview.dropped.is_empty() {
            impacts.push(format!("drop field(s): {}", preview.dropped.join(", ")));
        }
        if !preview.exposed.is_empty() {
            impacts.push(format!(
                "expose secret field(s) as metadata: {}",
                preview.exposed.join(", ")
            ));
        }
        return Err(CrosstacheError::config(format!(
            "conversion would {}. Confirm the lossy conversion before applying it",
            impacts.join(" and ")
        )));
    }
    if preview.prepared.no_op {
        return Ok(ConversionCommit::NoOp(preview.prepared.original));
    }

    let caps = backend.capabilities();
    if preview.prepared.groups.is_some() && !caps.has_groups {
        return Err(CrosstacheError::config(
            "backend does not support preserving secret groups during conversion",
        ));
    }
    if preview.prepared.note.is_some() && !caps.has_notes {
        return Err(CrosstacheError::config(
            "backend does not support preserving the secret note during conversion",
        ));
    }
    if preview.prepared.folder.is_some() && !caps.has_folders {
        return Err(CrosstacheError::config(
            "backend does not support preserving the secret folder during conversion",
        ));
    }
    if preview.prepared.original.expires_on.is_some() && !caps.has_expiry {
        return Err(CrosstacheError::config(
            "backend does not support preserving secret expiry during conversion",
        ));
    }
    if let Some(max_size) = caps.max_secret_size {
        if preview.prepared.value.len() > max_size {
            return Err(CrosstacheError::config(format!(
                "converted secret value is {} bytes, exceeding the backend's {max_size}-byte limit",
                preview.prepared.value.len()
            )));
        }
    }

    let reserved_count = predicted_reserved_tag_count_for_shape(
        backend.kind(),
        preview.target_type.is_some(),
        preview.prepared.groups.is_some(),
        preview.prepared.note.is_some(),
        preview.prepared.folder.is_some(),
        preview.prepared.original.expires_on.is_some(),
        !preview.prepared.content_type.is_empty(),
    );
    let user_tags: BTreeMap<String, String> = preview
        .prepared
        .tags
        .iter()
        .filter(|(key, _)| *key != TYPE_TAG && !key.starts_with(FIELD_TAG_PREFIX))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    check_tag_budget(
        &caps,
        reserved_count,
        &preview.target_metadata_fields,
        &user_tags,
    )?;

    let expires_on = preview
        .prepared
        .original
        .expires_on
        .map(FieldUpdate::Set)
        .unwrap_or(FieldUpdate::Clear);
    let not_before = preview
        .prepared
        .original
        .not_before
        .map(FieldUpdate::Set)
        .unwrap_or(FieldUpdate::Clear);
    let note = preview
        .prepared
        .note
        .clone()
        .map(FieldUpdate::Set)
        .unwrap_or(FieldUpdate::Clear);
    let folder = preview
        .prepared
        .folder
        .clone()
        .map(FieldUpdate::Set)
        .unwrap_or(FieldUpdate::Clear);
    Ok(ConversionCommit::Update(SecretUpdateRequest {
        name: name.to_string(),
        expected_revision: None,
        value: Some(Zeroizing::new(preview.prepared.value)),
        content_type: Some(preview.prepared.content_type),
        enabled: caps
            .has_enable_disable
            .then_some(preview.prepared.original.enabled),
        expires_on,
        not_before,
        tags: Some(preview.prepared.tags),
        groups: preview.prepared.groups,
        note,
        folder,
        replace_tags: true,
        replace_groups: true,
    }))
}

/// Apply a prepared conversion with an opaque provider revision guard.
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub async fn apply_conversion(
    backend: &dyn Backend,
    vault: &str,
    name: &str,
    expected_revision: &str,
    preview: ConversionPreview,
) -> Result<SecretProperties> {
    validate_conditional_conversion_backend(backend)?;
    match prepare_conversion_commit(backend, name, preview)? {
        ConversionCommit::NoOp(_) => backend
            .secrets()
            .validate_secret_revision(vault, name, expected_revision)
            .await
            .map_err(Into::into),
        ConversionCommit::Update(request) => backend
            .secrets()
            .update_secret_if_revision(vault, name, expected_revision, request)
            .await
            .map_err(Into::into),
    }
}

/// Apply a prepared conversion through one complete atomic backend update.
/// This is the CLI Task 4 contract and does not require compare-and-swap.
pub async fn apply_atomic_conversion(
    backend: &dyn Backend,
    vault: &str,
    name: &str,
    preview: ConversionPreview,
) -> Result<SecretProperties> {
    validate_conversion_backend(backend)?;
    match prepare_conversion_commit(backend, name, preview)? {
        ConversionCommit::NoOp(properties) => Ok(properties),
        ConversionCommit::Update(request) => backend
            .secrets()
            .update_secret(vault, name, request)
            .await
            .map_err(Into::into),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{
        error::BackendError, BackendCapabilities, BackendKind, NameCharset, SecretBackend,
    };
    use crate::records::{
        builtin_types, FieldDef, RecordType, TypeSource, RECORD_CONTENT_TYPE, TYPE_TAG,
    };
    use crate::secret::manager::{
        SecretProperties, SecretRequest, SecretSummary, SecretUpdateRequest,
    };
    use async_trait::async_trait;
    use chrono::{TimeZone, Utc};
    use std::collections::{BTreeMap, HashMap};
    use std::sync::{Arc, Mutex};
    use zeroize::Zeroizing;

    fn properties(
        value: &str,
        content_type: &str,
        tags: HashMap<String, String>,
    ) -> SecretProperties {
        SecretProperties {
            name: "secret".into(),
            original_name: "secret".into(),
            value: Some(Zeroizing::new(value.into())),
            version: "1".into(),
            version_number: Some(1),
            created_timestamp: 0,
            created_on: String::new(),
            updated_on: String::new(),
            enabled: false,
            expires_on: None,
            not_before: None,
            tags,
            content_type: content_type.into(),
            recovery_level: None,
        }
    }

    fn plain(value: &str) -> SecretProperties {
        properties(value, "", HashMap::new())
    }

    fn login_record() -> SecretProperties {
        properties(
            r#"{"password":"hunter2"}"#,
            RECORD_CONTENT_TYPE,
            HashMap::from([
                (TYPE_TAG.into(), "login".into()),
                ("f.username".into(), "alice".into()),
            ]),
        )
    }

    fn custom_type(name: &str, token_kind: FieldKind) -> RecordType {
        RecordType {
            name: name.into(),
            source: TypeSource::Project,
            fields: vec![
                FieldDef {
                    name: "password".into(),
                    kind: FieldKind::Secret,
                    required: true,
                    primary: true,
                },
                FieldDef {
                    name: "token".into(),
                    kind: token_kind,
                    required: false,
                    primary: false,
                },
            ],
        }
    }

    fn custom_record(type_name: &str, token_kind: FieldKind) -> SecretProperties {
        let mut tags = HashMap::from([(TYPE_TAG.into(), type_name.into())]);
        let envelope = match token_kind {
            FieldKind::Secret => r#"{"password":"hunter2","token":"private-token"}"#,
            FieldKind::Metadata => {
                tags.insert("f.token".into(), "metadata-token".into());
                r#"{"password":"hunter2"}"#
            }
        };
        properties(envelope, RECORD_CONTENT_TYPE, tags)
    }

    #[derive(Clone)]
    struct RecordingBackend {
        updates: Arc<Mutex<Vec<SecretUpdateRequest>>>,
        conditional_revisions: Arc<Mutex<Vec<String>>>,
        fail_update: bool,
        caps: BackendCapabilities,
        kind: BackendKind,
    }

    impl RecordingBackend {
        fn supported() -> Self {
            Self {
                updates: Arc::new(Mutex::new(Vec::new())),
                conditional_revisions: Arc::new(Mutex::new(Vec::new())),
                fail_update: false,
                kind: BackendKind::Local,
                caps: BackendCapabilities {
                    has_atomic_record_conversion: true,
                    has_conditional_record_conversion: true,
                    has_enable_disable: true,
                    has_groups: true,
                    has_folders: true,
                    has_notes: true,
                    has_expiry: true,
                    name_charset: NameCharset::Unrestricted,
                    ..BackendCapabilities::default()
                },
            }
        }

        fn update_count(&self) -> usize {
            self.updates.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl SecretBackend for RecordingBackend {
        fn supports_conditional_update(&self) -> bool {
            self.caps.has_conditional_record_conversion
        }

        async fn set_secret(
            &self,
            _vault: &str,
            _request: SecretRequest,
        ) -> std::result::Result<SecretProperties, BackendError> {
            unreachable!("conversion must not create an intermediate secret")
        }

        async fn get_secret(
            &self,
            _vault: &str,
            _name: &str,
            _include_value: bool,
        ) -> std::result::Result<SecretProperties, BackendError> {
            unreachable!("preview already contains the source snapshot")
        }

        async fn get_secret_version(
            &self,
            _vault: &str,
            _name: &str,
            _version: &str,
            _include_value: bool,
        ) -> std::result::Result<SecretProperties, BackendError> {
            unreachable!()
        }

        async fn list_secrets(
            &self,
            _vault: &str,
            _group_filter: Option<&str>,
        ) -> std::result::Result<Vec<SecretSummary>, BackendError> {
            unreachable!()
        }

        async fn delete_secret(
            &self,
            _vault: &str,
            _name: &str,
        ) -> std::result::Result<(), BackendError> {
            unreachable!("conversion must not delete or untype first")
        }

        async fn update_secret(
            &self,
            _vault: &str,
            _name: &str,
            request: SecretUpdateRequest,
        ) -> std::result::Result<SecretProperties, BackendError> {
            self.updates.lock().unwrap().push(request);
            if self.fail_update {
                Err(BackendError::Network("simulated update failure".into()))
            } else {
                Ok(plain("updated"))
            }
        }

        async fn update_secret_if_revision(
            &self,
            vault: &str,
            name: &str,
            expected_revision: &str,
            request: SecretUpdateRequest,
        ) -> std::result::Result<SecretProperties, BackendError> {
            self.conditional_revisions
                .lock()
                .unwrap()
                .push(expected_revision.to_string());
            self.update_secret(vault, name, request).await
        }

        async fn validate_secret_revision(
            &self,
            _vault: &str,
            _name: &str,
            expected_revision: &str,
        ) -> std::result::Result<SecretProperties, BackendError> {
            self.conditional_revisions
                .lock()
                .unwrap()
                .push(expected_revision.to_string());
            Ok(login_record())
        }
    }

    #[async_trait]
    impl Backend for RecordingBackend {
        fn name(&self) -> &'static str {
            "recording"
        }

        fn kind(&self) -> BackendKind {
            self.kind
        }

        fn capabilities(&self) -> BackendCapabilities {
            self.caps.clone()
        }

        fn secrets(&self) -> &dyn SecretBackend {
            self
        }

        async fn health_check(&self) -> std::result::Result<(), BackendError> {
            Ok(())
        }
    }

    #[test]
    fn record_to_record_preview_names_retained_and_dropped_fields() {
        let types: Vec<_> = builtin_types()
            .into_iter()
            .map(|mut record_type| {
                if record_type.name == "database" {
                    record_type.fields.retain(|field| field.name == "password");
                }
                record_type
            })
            .collect();
        let preview = preview_conversion(
            &login_record(),
            &types,
            ConversionRequest::to_type("database"),
        )
        .unwrap();

        assert_eq!(preview.retained, vec!["password"]);
        assert!(preview.renamed.is_empty());
        assert_eq!(preview.dropped, vec!["username"]);
        assert!(preview.requires_confirmation);
    }

    #[test]
    fn plain_to_record_maps_value_to_target_primary() {
        let preview = preview_conversion(
            &plain("token"),
            &builtin_types(),
            ConversionRequest::to_type("api-key"),
        )
        .unwrap();

        assert_eq!(preview.target_secret_fields["key"], "token");
        assert!(!preview.requires_confirmation);
    }

    #[test]
    fn record_primary_is_renamed_when_target_primary_differs() {
        let preview = preview_conversion(
            &login_record(),
            &builtin_types(),
            ConversionRequest::to_type("api-key"),
        )
        .unwrap();

        assert_eq!(preview.renamed, vec!["password -> key"]);
        assert_eq!(preview.target_secret_fields["key"], "hunter2");
        assert_eq!(preview.dropped, vec!["username"]);
    }

    #[test]
    fn supplied_fields_override_exact_matches_without_exposing_values_in_debug() {
        let mut request = ConversionRequest::to_type("database");
        request
            .supplied_fields
            .insert("password".into(), "replacement".into());
        let preview = preview_conversion(&login_record(), &builtin_types(), request).unwrap();

        assert_eq!(preview.target_secret_fields["password"], "replacement");
        assert!(!format!("{preview:?}").contains("replacement"));
    }

    #[test]
    fn untype_keeps_primary_and_reports_every_removed_field() {
        let preview = preview_conversion(
            &login_record(),
            &builtin_types(),
            ConversionRequest {
                target: ConversionTarget::Plain,
                supplied_fields: BTreeMap::new(),
                confirm_lossy: false,
            },
        )
        .unwrap();

        assert_eq!(preview.retained, vec!["password"]);
        assert_eq!(preview.dropped, vec!["username"]);
        assert_eq!(preview.target_type, None);
        assert_eq!(preview.target_secret_fields["password"], "hunter2");
        assert!(preview.requires_confirmation);
    }

    #[test]
    fn malformed_envelope_error_is_safe_and_does_not_echo_secret_material() {
        let secret = properties(
            r#"{"password":"do-not-echo",broken"#,
            RECORD_CONTENT_TYPE,
            HashMap::from([(TYPE_TAG.into(), "login".into())]),
        );

        let error = preview_conversion(
            &secret,
            &builtin_types(),
            ConversionRequest::to_type("database"),
        )
        .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("malformed"), "{message}");
        assert!(!message.contains("do-not-echo"), "{message}");
    }

    #[test]
    fn unknown_source_type_and_unknown_target_are_distinct_safe_errors() {
        let unknown_source = properties(
            r#"{"password":"do-not-echo"}"#,
            RECORD_CONTENT_TYPE,
            HashMap::from([(TYPE_TAG.into(), "shadow".into())]),
        );
        let source_error = preview_conversion(
            &unknown_source,
            &builtin_types(),
            ConversionRequest::to_type("database"),
        )
        .unwrap_err()
        .to_string();
        assert!(source_error.contains("shadow"), "{source_error}");
        assert!(!source_error.contains("do-not-echo"), "{source_error}");

        let target_error = preview_conversion(
            &plain("do-not-echo"),
            &builtin_types(),
            ConversionRequest::to_type("missing"),
        )
        .unwrap_err()
        .to_string();
        assert!(
            target_error.contains("unknown type 'missing'"),
            "{target_error}"
        );
        assert!(!target_error.contains("do-not-echo"), "{target_error}");
    }

    #[test]
    fn record_markers_on_plain_secret_do_not_shadow_its_plain_value() {
        let mut secret = plain("token");
        secret.tags.insert(TYPE_TAG.into(), "unknown".into());
        secret.tags.insert("f.shadow".into(), "metadata".into());

        let preview = preview_conversion(
            &secret,
            &builtin_types(),
            ConversionRequest::to_type("api-key"),
        )
        .unwrap();

        assert_eq!(preview.target_secret_fields["key"], "token");
        assert!(preview.dropped.is_empty());
    }

    #[test]
    fn explicit_required_field_cannot_be_blank_but_missing_non_primary_keeps_legacy_default() {
        let legacy = preview_conversion(
            &plain("password"),
            &builtin_types(),
            ConversionRequest::to_type("login"),
        )
        .unwrap();
        assert_eq!(legacy.target_secret_fields["password"], "password");

        let mut blank = ConversionRequest::to_type("login");
        blank.supplied_fields.insert("username".into(), " ".into());
        let error = preview_conversion(&plain("password"), &builtin_types(), blank)
            .unwrap_err()
            .to_string();
        assert!(error.contains("required field 'username'"), "{error}");
    }

    #[test]
    fn confirmed_loss_is_still_reported_but_no_longer_requires_confirmation() {
        let mut request = ConversionRequest::to_type("api-key");
        request.confirm_lossy = true;
        let preview = preview_conversion(&login_record(), &builtin_types(), request).unwrap();

        assert_eq!(preview.dropped, vec!["username"]);
        assert!(!preview.requires_confirmation);
    }

    #[test]
    fn secret_to_metadata_is_reported_as_exposure_and_requires_confirmation() {
        let source = custom_type("secure", FieldKind::Secret);
        let target = custom_type("visible", FieldKind::Metadata);
        let preview = preview_conversion(
            &custom_record("secure", FieldKind::Secret),
            &[source, target],
            ConversionRequest::to_type("visible"),
        )
        .unwrap();
        assert_eq!(preview.exposed, vec!["token"]);
        assert_eq!(
            preview.sensitivity_changes,
            vec!["token: secret -> metadata"]
        );
        assert!(preview.requires_confirmation);

        let json = serde_json::to_string(&preview).unwrap();
        assert!(json.contains("token: secret -> metadata"), "{json}");
        assert!(!json.contains("private-token"), "{json}");
        assert!(!format!("{preview:?}").contains("private-token"));
    }

    #[test]
    fn metadata_to_secret_is_reported_without_requiring_confirmation() {
        let source = custom_type("visible", FieldKind::Metadata);
        let target = custom_type("secure", FieldKind::Secret);
        let preview = preview_conversion(
            &custom_record("visible", FieldKind::Metadata),
            &[source, target],
            ConversionRequest::to_type("secure"),
        )
        .unwrap();

        assert!(preview.exposed.is_empty());
        assert_eq!(
            preview.sensitivity_changes,
            vec!["token: metadata -> secret"]
        );
        assert!(!preview.requires_confirmation);
        assert_eq!(preview.target_secret_fields["token"], "metadata-token");

        let json = serde_json::to_string(&preview).unwrap();
        assert!(json.contains("token: metadata -> secret"), "{json}");
        assert!(!json.contains("metadata-token"), "{json}");
    }

    #[tokio::test]
    async fn same_type_schema_downgrade_is_not_a_no_op_and_rewrites_storage() {
        let target = custom_type("evolving", FieldKind::Metadata);
        let source = custom_record("evolving", FieldKind::Secret);
        let preview = preview_conversion(
            &source,
            std::slice::from_ref(&target),
            ConversionRequest::to_type("evolving"),
        )
        .unwrap();
        assert!(preview.requires_confirmation);
        assert_eq!(preview.exposed, vec!["token"]);

        let mut confirmed = ConversionRequest::to_type("evolving");
        confirmed.confirm_lossy = true;
        let preview =
            preview_conversion(&source, std::slice::from_ref(&target), confirmed).unwrap();
        let backend = RecordingBackend::supported();
        apply_conversion(&backend, "vault", "secret", "revision", preview)
            .await
            .unwrap();

        assert_eq!(backend.update_count(), 1);
        let updates = backend.updates.lock().unwrap();
        let request = &updates[0];
        let envelope = parse_envelope(request.value.as_deref().unwrap()).unwrap();
        assert!(!envelope.contains_key("token"));
        assert_eq!(request.tags.as_ref().unwrap()["f.token"], "private-token");
    }

    #[tokio::test]
    async fn same_type_schema_upgrade_is_not_a_no_op_and_rewrites_storage() {
        let target = custom_type("evolving", FieldKind::Secret);
        let source = custom_record("evolving", FieldKind::Metadata);
        let preview = preview_conversion(
            &source,
            std::slice::from_ref(&target),
            ConversionRequest::to_type("evolving"),
        )
        .unwrap();
        assert!(!preview.requires_confirmation);
        let backend = RecordingBackend::supported();
        apply_conversion(&backend, "vault", "secret", "revision", preview)
            .await
            .unwrap();

        assert_eq!(backend.update_count(), 1);
        let updates = backend.updates.lock().unwrap();
        let request = &updates[0];
        let envelope = parse_envelope(request.value.as_deref().unwrap()).unwrap();
        assert_eq!(envelope["token"], "metadata-token");
        assert!(!request.tags.as_ref().unwrap().contains_key("f.token"));
    }

    #[tokio::test]
    async fn apply_uses_the_preview_revision_as_the_atomic_commit_guard() {
        let target = custom_type("evolving", FieldKind::Secret);
        let source = custom_record("evolving", FieldKind::Metadata);
        let preview = preview_conversion(
            &source,
            std::slice::from_ref(&target),
            ConversionRequest::to_type("evolving"),
        )
        .unwrap();
        let backend = RecordingBackend::supported();

        apply_conversion(&backend, "vault", "secret", "opaque-revision", preview)
            .await
            .unwrap();

        assert_eq!(
            backend.conditional_revisions.lock().unwrap().as_slice(),
            ["opaque-revision"]
        );
    }

    #[test]
    fn supplied_metadata_override_has_new_provenance_not_source_exposure() {
        let source_type = custom_type("secure", FieldKind::Secret);
        let target = custom_type("visible", FieldKind::Metadata);
        let mut request = ConversionRequest::to_type("visible");
        request
            .supplied_fields
            .insert("token".into(), "replacement-metadata".into());
        let preview = preview_conversion(
            &custom_record("secure", FieldKind::Secret),
            &[source_type, target],
            request,
        )
        .unwrap();

        assert!(!preview.retained.contains(&"token".to_string()));
        assert!(!preview.dropped.contains(&"token".to_string()));
        assert!(preview.sensitivity_changes.is_empty());
        assert!(preview.exposed.is_empty());
        assert!(!preview.requires_confirmation);
        assert_eq!(
            preview.target_metadata_fields["token"],
            "replacement-metadata"
        );
        let serialized = serde_json::to_string(&preview).unwrap();
        assert!(!serialized.contains("private-token"), "{serialized}");
        assert!(!serialized.contains("replacement-metadata"), "{serialized}");
    }

    #[test]
    fn supplied_secret_override_has_new_provenance_not_source_upgrade() {
        let source_type = custom_type("visible", FieldKind::Metadata);
        let target = custom_type("secure", FieldKind::Secret);
        let mut request = ConversionRequest::to_type("secure");
        request
            .supplied_fields
            .insert("token".into(), "replacement-secret".into());
        let preview = preview_conversion(
            &custom_record("visible", FieldKind::Metadata),
            &[source_type, target],
            request,
        )
        .unwrap();

        assert!(!preview.retained.contains(&"token".to_string()));
        assert!(!preview.dropped.contains(&"token".to_string()));
        assert!(preview.sensitivity_changes.is_empty());
        assert!(preview.exposed.is_empty());
        assert!(!preview.requires_confirmation);
        assert_eq!(preview.target_secret_fields["token"], "replacement-secret");
        let serialized = serde_json::to_string(&preview).unwrap();
        assert!(!serialized.contains("metadata-token"), "{serialized}");
        assert!(!serialized.contains("replacement-secret"), "{serialized}");
    }

    #[test]
    fn missing_required_non_primary_target_field_fails_for_record_conversion() {
        let mut target = builtin_types()
            .into_iter()
            .find(|t| t.name == "api-key")
            .unwrap();
        target.name = "required-account".into();
        target
            .fields
            .iter_mut()
            .find(|field| field.name == "account")
            .unwrap()
            .required = true;
        let error = preview_conversion(
            &login_record(),
            &[
                builtin_types()
                    .into_iter()
                    .find(|t| t.name == "login")
                    .unwrap(),
                target,
            ],
            ConversionRequest::to_type("required-account"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("account"));
    }

    #[test]
    fn retained_source_primary_copied_to_new_primary_is_not_called_renamed() {
        let mut target = builtin_types()
            .into_iter()
            .find(|t| t.name == "api-key")
            .unwrap();
        target.fields.push(crate::records::FieldDef {
            name: "password".into(),
            kind: FieldKind::Secret,
            required: false,
            primary: false,
        });
        let preview = preview_conversion(
            &login_record(),
            &[
                builtin_types()
                    .into_iter()
                    .find(|t| t.name == "login")
                    .unwrap(),
                target,
            ],
            ConversionRequest::to_type("api-key"),
        )
        .unwrap();
        assert!(preview.renamed.is_empty());
        assert_eq!(preview.copied, vec!["password -> key"]);
    }

    #[test]
    fn serialized_preview_contains_impact_only_not_secret_values() {
        let preview = preview_conversion(
            &plain("do-not-serialize"),
            &builtin_types(),
            ConversionRequest::to_type("api-key"),
        )
        .unwrap();

        let json = serde_json::to_string(&preview).unwrap();
        assert!(json.contains("\"target_type\":\"api-key\""), "{json}");
        assert!(!json.contains("do-not-serialize"), "{json}");
        assert!(!json.contains("target_secret_fields"), "{json}");
    }

    #[tokio::test]
    async fn apply_conversion_sends_exactly_one_complete_atomic_update() {
        let mut source = plain("token");
        source.enabled = false;
        source.expires_on = Some(Utc.with_ymd_and_hms(2030, 1, 2, 0, 0, 0).unwrap());
        source.not_before = Some(Utc.with_ymd_and_hms(2029, 1, 2, 0, 0, 0).unwrap());
        source.tags = HashMap::from([
            ("custom".into(), "kept".into()),
            ("groups".into(), "ops, prod".into()),
            ("note".into(), "kept note".into()),
            ("folder".into(), "apps/prod".into()),
        ]);
        let preview = preview_conversion(
            &source,
            &builtin_types(),
            ConversionRequest::to_type("api-key"),
        )
        .unwrap();
        let backend = RecordingBackend::supported();

        apply_conversion(&backend, "vault", "secret", "revision", preview)
            .await
            .unwrap();

        assert_eq!(backend.update_count(), 1);
        let updates = backend.updates.lock().unwrap();
        let request = &updates[0];
        assert_eq!(request.content_type.as_deref(), Some(RECORD_CONTENT_TYPE));
        assert_eq!(request.enabled, Some(false));
        assert_eq!(request.groups.as_ref().unwrap(), &["ops", "prod"]);
        assert_eq!(request.tags.as_ref().unwrap()["custom"], "kept");
        assert!(!request.tags.as_ref().unwrap().contains_key("groups"));
        assert_eq!(
            request.note,
            crate::secret::manager::FieldUpdate::Set("kept note".into())
        );
        assert_eq!(
            request.folder,
            crate::secret::manager::FieldUpdate::Set("apps/prod".into())
        );
        assert!(request.replace_tags);
        assert!(request.replace_groups);
    }

    #[tokio::test]
    async fn failed_atomic_update_never_attempts_an_intermediate_write() {
        let preview = preview_conversion(
            &plain("token"),
            &builtin_types(),
            ConversionRequest::to_type("api-key"),
        )
        .unwrap();
        let mut backend = RecordingBackend::supported();
        backend.fail_update = true;

        let error = apply_conversion(&backend, "vault", "secret", "revision", preview)
            .await
            .unwrap_err();

        assert_eq!(backend.update_count(), 1);
        assert!(error.to_string().contains("simulated update failure"));
        assert!(!error.to_string().contains("token"));
    }

    #[tokio::test]
    async fn tag_budget_failure_happens_before_backend_update() {
        let mut source = plain("token");
        source.tags.insert("custom".into(), "kept".into());
        let preview = preview_conversion(
            &source,
            &builtin_types(),
            ConversionRequest::to_type("api-key"),
        )
        .unwrap();
        let mut backend = RecordingBackend::supported();
        backend.caps.max_tags = Some(1);

        let error = apply_conversion(&backend, "vault", "secret", "revision", preview)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("tag limit"));
        assert_eq!(backend.update_count(), 0);
    }

    #[tokio::test]
    async fn unsupported_metadata_capability_fails_before_backend_update() {
        let mut source = plain("token");
        source.tags.insert("folder".into(), "apps/prod".into());
        let preview = preview_conversion(
            &source,
            &builtin_types(),
            ConversionRequest::to_type("api-key"),
        )
        .unwrap();
        let mut backend = RecordingBackend::supported();
        backend.caps.has_folders = false;

        let error = apply_conversion(&backend, "vault", "secret", "revision", preview)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("folder"));
        assert_eq!(backend.update_count(), 0);
    }

    #[tokio::test]
    async fn aws_non_atomic_capability_is_rejected_before_any_sdk_mutation() {
        let preview = preview_conversion(
            &plain("token"),
            &builtin_types(),
            ConversionRequest::to_type("api-key"),
        )
        .unwrap();
        let mut backend = RecordingBackend::supported();
        backend.kind = BackendKind::Aws;
        backend.caps.has_atomic_record_conversion = false;
        backend.caps.has_conditional_record_conversion = false;
        backend.caps.has_enable_disable = false;
        let error = apply_conversion(&backend, "vault", "secret", "revision", preview)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("atomic"));
        assert_eq!(backend.update_count(), 0);
    }

    #[test]
    fn complete_atomic_conversion_does_not_require_conditional_cas() {
        let mut backend = RecordingBackend::supported();
        backend.kind = BackendKind::Azure;
        backend.caps.has_conditional_record_conversion = false;

        validate_conversion_backend(&backend)
            .expect("CLI conversion only requires one complete atomic update");
    }

    #[tokio::test]
    async fn azure_complete_atomic_conversion_uses_one_unconditional_update() {
        let mut backend = RecordingBackend::supported();
        backend.kind = BackendKind::Azure;
        backend.caps.has_conditional_record_conversion = false;
        let preview = preview_conversion(
            &plain("azure-token"),
            &builtin_types(),
            ConversionRequest::to_type("api-key"),
        )
        .unwrap();

        apply_atomic_conversion(&backend, "vault", "secret", preview)
            .await
            .unwrap();

        assert_eq!(backend.update_count(), 1);
        assert!(backend.conditional_revisions.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn backend_without_enable_disable_omits_enabled_for_every_conversion_shape() {
        let cases = [
            (
                plain("token"),
                ConversionRequest::to_type("api-key"),
                "plain to record",
            ),
            (
                login_record(),
                ConversionRequest::to_type("database"),
                "record to record",
            ),
            (
                login_record(),
                ConversionRequest::plain(),
                "record to plain",
            ),
        ];

        for (source, mut conversion, label) in cases {
            conversion.confirm_lossy = true;
            let preview = preview_conversion(&source, &builtin_types(), conversion).unwrap();
            let mut backend = RecordingBackend::supported();
            backend.caps.has_enable_disable = false;

            apply_conversion(&backend, "vault", "secret", "revision", preview)
                .await
                .unwrap();

            let updates = backend.updates.lock().unwrap();
            assert_eq!(updates.len(), 1, "{label}");
            assert_eq!(updates[0].enabled, None, "{label}");
        }
    }

    #[tokio::test]
    async fn same_type_without_supplied_fields_is_a_backend_no_op() {
        let preview = preview_conversion(
            &login_record(),
            &builtin_types(),
            ConversionRequest::to_type("login"),
        )
        .unwrap();
        let backend = RecordingBackend::supported();

        let result = apply_conversion(&backend, "vault", "secret", "revision", preview)
            .await
            .unwrap();

        assert_eq!(result.content_type, RECORD_CONTENT_TYPE);
        assert_eq!(backend.update_count(), 0);
        assert_eq!(
            backend.conditional_revisions.lock().unwrap().as_slice(),
            ["revision"]
        );
    }
}
