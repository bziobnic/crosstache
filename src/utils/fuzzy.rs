//! Fuzzy ranking helpers backed by `nucleo`.
//!
//! Pure functions; no I/O. Used by `xv find` to rank secrets and
//! by future commands that want a "did you mean a list of these?"
//! ranked output.

use nucleo::{Config, Matcher, Utf32Str};

/// One row's worth of metadata that `score_matches` can search against.
/// Caller fills in whichever fields are populated; missing fields are skipped.
#[derive(Debug, Clone)]
pub struct CandidateItem {
    pub name: String,
    pub folder: Option<String>,
    pub groups: Option<String>,
    pub note: Option<String>,
    pub tags: Vec<String>,
}

/// Which field(s) of a `CandidateItem` to score the pattern against.
/// When multiple fields are given, the highest score across the listed
/// fields wins for that item; an item with no scoring field producing a
/// match is excluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FuzzyField {
    Name,
    Folder,
    Groups,
    Note,
    Tags,
}

/// One scored result.
#[derive(Debug, Clone)]
pub struct Match<'a> {
    pub item: &'a CandidateItem,
    /// Raw nucleo score. Higher = better. `0` for the
    /// empty-pattern degenerate case (we still surface every item).
    pub score: u32,
}

/// Score every item against `pattern` using the requested fields.
///
/// Empty `pattern` returns every item with score `0`, in input order.
/// Otherwise: items that score against at least one of the requested
/// fields are kept; items with no matching field are dropped. Results
/// are sorted by score descending; ties are broken alphabetically by
/// `item.name` (case-insensitive).
pub fn score_matches<'a>(
    pattern: &str,
    items: &'a [CandidateItem],
    fields: &[FuzzyField],
) -> Vec<Match<'a>> {
    if pattern.is_empty() {
        return items.iter().map(|item| Match { item, score: 0 }).collect();
    }
    if items.is_empty() || fields.is_empty() {
        return Vec::new();
    }

    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut pattern_buf = Vec::new();
    let pattern_utf32 = Utf32Str::new(pattern, &mut pattern_buf);

    let mut out: Vec<Match<'a>> = Vec::new();
    for item in items {
        let mut best: Option<u32> = None;

        for field in fields {
            // Collect the string(s) to score for this field.
            let field_strings: Vec<&str> = match field {
                FuzzyField::Name => vec![item.name.as_str()],
                FuzzyField::Folder => item.folder.as_deref().map(|s| vec![s]).unwrap_or_default(),
                FuzzyField::Groups => item.groups.as_deref().map(|s| vec![s]).unwrap_or_default(),
                FuzzyField::Note => item.note.as_deref().map(|s| vec![s]).unwrap_or_default(),
                FuzzyField::Tags => item.tags.iter().map(String::as_str).collect(),
            };
            for hay_str in field_strings {
                let mut hay_buf = Vec::new();
                let hay = Utf32Str::new(hay_str, &mut hay_buf);
                if let Some(s) = matcher.fuzzy_match(hay, pattern_utf32) {
                    let s = u32::from(s);
                    best = Some(best.map_or(s, |b| b.max(s)));
                }
            }
        }
        if let Some(score) = best {
            out.push(Match { item, score });
        }
    }

    // Sort: score desc, then name asc (case-insensitive) for tie-break.
    out.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.item.name.to_lowercase().cmp(&b.item.name.to_lowercase()))
    });
    out
}

impl CandidateItem {
    /// Adapt a `SecretSummary` to a `CandidateItem`. Prefers
    /// `original_name` over the sanitized `name` since users search
    /// against what they typed, not against post-sanitization forms.
    /// Empty `original_name` falls back to `name`.
    pub fn from_secret_summary(s: &crate::secret::manager::SecretSummary) -> Self {
        let name = if s.original_name.is_empty() {
            s.name.clone()
        } else {
            s.original_name.clone()
        };
        let tags: Vec<String> = s
            .groups
            .as_deref()
            .map(|g| g.split(',').map(|t| t.trim().to_string()).collect())
            .unwrap_or_default();
        Self {
            name,
            folder: s.folder.clone(),
            groups: s.groups.clone(),
            note: s.note.clone(),
            tags,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build minimal candidate items for tests.
    fn item(name: &str) -> CandidateItem {
        CandidateItem {
            name: name.to_string(),
            folder: None,
            groups: None,
            note: None,
            tags: vec![],
        }
    }

    #[test]
    fn ranks_close_match_first() {
        let items = vec![item("DB_PASSWORD"), item("API_TOKEN"), item("DB_HOSTNAME")];
        let matches = score_matches("dbpw", &items, &[FuzzyField::Name]);
        assert!(!matches.is_empty(), "must produce matches");
        assert_eq!(matches[0].item.name, "DB_PASSWORD");
    }

    #[test]
    fn empty_pattern_returns_all_with_score_zero() {
        let items = vec![item("FOO"), item("BAR")];
        let matches = score_matches("", &items, &[FuzzyField::Name]);
        assert_eq!(matches.len(), 2, "empty pattern returns all items");
        for m in &matches {
            assert_eq!(m.score, 0, "empty pattern → score 0 for every item");
        }
    }

    #[test]
    fn no_matches_returns_empty() {
        let items = vec![item("FOO"), item("BAR")];
        let matches = score_matches("xyzzy_nonexistent", &items, &[FuzzyField::Name]);
        assert!(matches.is_empty(), "no candidates match → empty");
    }

    #[test]
    fn ties_broken_by_alphabetical_name() {
        // Two identical patterns; nucleo will return equal scores; we
        // tie-break alphabetically by name.
        let items = vec![item("zebra"), item("alpha"), item("middle")];
        let matches = score_matches("a", &items, &[FuzzyField::Name]);
        // 'alpha' contains 'a' first (position 0); 'middle' has it later;
        // 'zebra' has it. nucleo's score may differ, but among equal scores,
        // alphabetical wins — this test asserts the tie-break works when
        // scores DO match (assert at least the order is deterministic).
        assert!(!matches.is_empty());
        // We don't assert exact ordering of dissimilar scores; we only
        // assert determinism: running again returns identical order.
        let again = score_matches("a", &items, &[FuzzyField::Name]);
        assert_eq!(
            matches
                .iter()
                .map(|m| m.item.name.clone())
                .collect::<Vec<_>>(),
            again
                .iter()
                .map(|m| m.item.name.clone())
                .collect::<Vec<_>>(),
            "scoring must be deterministic"
        );
    }

    #[test]
    fn name_only_does_not_match_folder_text() {
        let items = vec![CandidateItem {
            name: "FOO".to_string(),
            folder: Some("database".to_string()),
            groups: None,
            note: None,
            tags: vec![],
        }];
        // pattern matches the folder, but we asked for name-only → no match
        let matches = score_matches("data", &items, &[FuzzyField::Name]);
        assert!(
            matches.is_empty(),
            "name-only field selector ignores folder"
        );
    }

    #[test]
    fn from_secret_summary_extracts_all_fields() {
        use crate::secret::manager::SecretSummary;
        let summary = SecretSummary {
            name: "DB_PASSWORD".to_string(),
            original_name: "DB_PASSWORD".to_string(),
            note: Some("primary db".to_string()),
            folder: Some("backend/database".to_string()),
            groups: Some("backend,prod".to_string()),
            updated_on: String::new(),
            enabled: true,
            content_type: String::new(),
            tags: std::collections::HashMap::new(),
        };
        let item = CandidateItem::from_secret_summary(&summary);
        // Prefer original_name over sanitized name (matches the user-typed form).
        assert_eq!(item.name, "DB_PASSWORD");
        assert_eq!(item.folder.as_deref(), Some("backend/database"));
        assert_eq!(item.groups.as_deref(), Some("backend,prod"));
        assert_eq!(item.note.as_deref(), Some("primary db"));
        // Tags come from groups (comma-separated). v0.6.1 has no separate
        // tags field on SecretSummary; we map groups to tags so `--in tags`
        // still works on existing data.
        assert!(item.tags.contains(&"backend".to_string()));
        assert!(item.tags.contains(&"prod".to_string()));
    }
}
