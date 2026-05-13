//! Tag <-> SecretProperties round-trip for AWS backend.
//!
//! AWS Secrets Manager allows up to 50 tags per secret (vs Azure's 15);
//! comfortable budget. Reserved keys live under the `xv:` prefix.

use std::collections::HashMap;

pub const TAG_ORIGINAL_NAME: &str = "xv:original_name";
pub const TAG_GROUPS: &str = "xv:groups";
pub const TAG_FOLDER: &str = "xv:folder";
pub const TAG_CREATED_BY: &str = "xv:created_by";
pub const TAG_CONTENT_TYPE: &str = "xv:content_type";
pub const TAG_EXPIRES_AT: &str = "xv:expires_at";
pub const TAG_TYPE: &str = "xv:type";
pub const TAG_VALUE_VAULT_MARKER: &str = "vault-marker";
pub const TAG_MIGRATED_FROM: &str = "xv:migrated_from";
pub const TAG_MIGRATED_AT: &str = "xv:migrated_at";

/// Subset of `SecretProperties` fields we actually round-trip.
/// Real `SecretProperties` from `crate::secret::models` is bigger; we map
/// to/from it at the call site.
#[derive(Debug, Default, Clone)]
pub struct TestProps {
    pub original_name: String,
    pub groups: Vec<String>,
    pub folder: Option<String>,
    pub created_by: Option<String>,
    pub content_type: Option<String>,
    pub note: Option<String>, // -> AWS Description, not a tag
    pub expires_at: Option<String>,
    pub user_tags: HashMap<String, String>,
}

impl TestProps {
    pub fn empty(name: &str) -> Self {
        Self {
            original_name: name.into(),
            ..Default::default()
        }
    }
}

/// Encode metadata into AWS-tag-shaped `(key, value)` pairs.
/// Note: `note` is intentionally NOT encoded — it lives in AWS Description.
pub fn encode_tags(p: &TestProps) -> Vec<(String, String)> {
    let mut tags: Vec<(String, String)> = Vec::new();
    tags.push((TAG_ORIGINAL_NAME.into(), p.original_name.clone()));
    if !p.groups.is_empty() {
        tags.push((TAG_GROUPS.into(), p.groups.join(",")));
    }
    if let Some(ref f) = p.folder {
        tags.push((TAG_FOLDER.into(), f.clone()));
    }
    if let Some(ref c) = p.created_by {
        tags.push((TAG_CREATED_BY.into(), c.clone()));
    }
    if let Some(ref ct) = p.content_type {
        tags.push((TAG_CONTENT_TYPE.into(), ct.clone()));
    }
    if let Some(ref e) = p.expires_at {
        tags.push((TAG_EXPIRES_AT.into(), e.clone()));
    }
    for (k, v) in &p.user_tags {
        if !k.starts_with("xv:") {
            tags.push((k.clone(), v.clone()));
        }
    }
    tags
}

/// Decode AWS tags back into the metadata struct.
pub fn decode_tags(tags: &[(String, String)]) -> TestProps {
    let mut p = TestProps::default();
    let mut user_tags = HashMap::new();
    for (k, v) in tags {
        match k.as_str() {
            TAG_ORIGINAL_NAME => p.original_name = v.clone(),
            TAG_GROUPS => {
                p.groups = v
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .collect()
            }
            TAG_FOLDER => p.folder = Some(v.clone()),
            TAG_CREATED_BY => p.created_by = Some(v.clone()),
            TAG_CONTENT_TYPE => p.content_type = Some(v.clone()),
            TAG_EXPIRES_AT => p.expires_at = Some(v.clone()),
            _ if !k.starts_with("xv:") => {
                user_tags.insert(k.clone(), v.clone());
            }
            _ => {} // unknown xv: tag — ignored on decode
        }
    }
    p.user_tags = user_tags;
    p
}

/// True if this tag is a vault marker tag (`xv:type=vault-marker`).
pub fn is_vault_marker_tag(key: &str, value: &str) -> bool {
    key == TAG_TYPE && value == TAG_VALUE_VAULT_MARKER
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn round_trip_full_metadata() {
        let mut user_tags = HashMap::new();
        user_tags.insert("team".to_string(), "platform".to_string());

        let props = TestProps {
            original_name: "db-password".into(),
            groups: vec!["backend".into(), "prod".into()],
            folder: Some("app/database".into()),
            created_by: Some("alice@example.com".into()),
            content_type: Some("text/plain".into()),
            note: Some("primary database admin password".into()),
            expires_at: Some("2027-01-01T00:00:00Z".into()),
            user_tags: user_tags.clone(),
        };

        let aws_tags = encode_tags(&props);
        let decoded = decode_tags(&aws_tags);

        assert_eq!(decoded.original_name, "db-password");
        assert_eq!(decoded.groups, vec!["backend", "prod"]);
        assert_eq!(decoded.folder.as_deref(), Some("app/database"));
        assert_eq!(decoded.user_tags.get("team").unwrap(), "platform");
    }

    #[test]
    fn empty_metadata_round_trips_to_empty_tags() {
        let props = TestProps::empty("name1");
        let aws_tags = encode_tags(&props);
        assert!(aws_tags.iter().any(|(k, _)| k == "xv:original_name"));
    }

    #[test]
    fn note_not_in_tags() {
        let props = TestProps {
            original_name: "x".into(),
            note: Some("a note".into()),
            ..Default::default()
        };
        let aws_tags = encode_tags(&props);
        assert!(!aws_tags
            .iter()
            .any(|(k, _)| k == "note" || k.contains("note")));
    }

    #[test]
    fn user_tags_with_xv_prefix_excluded() {
        let mut user_tags = HashMap::new();
        user_tags.insert("xv:sneaky".to_string(), "value".to_string());
        user_tags.insert("safe-key".to_string(), "value".to_string());
        let props = TestProps {
            original_name: "x".into(),
            user_tags,
            ..Default::default()
        };
        let aws_tags = encode_tags(&props);
        assert!(!aws_tags.iter().any(|(k, _)| k == "xv:sneaky"));
        assert!(aws_tags.iter().any(|(k, _)| k == "safe-key"));
    }

    #[test]
    fn is_vault_marker_tag_works() {
        assert!(is_vault_marker_tag("xv:type", "vault-marker"));
        assert!(!is_vault_marker_tag("xv:type", "other"));
        assert!(!is_vault_marker_tag("other", "vault-marker"));
    }
}
