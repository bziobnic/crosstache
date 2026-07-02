//! Shared wording for list-command empty-states and count lines.
//!
//! Every list-style command routes its human empty-state and count text
//! through these helpers so the wording cannot drift per-command again.
//! Streams are the caller's job: empty-states go to stderr via
//! `output::info`, counts go to stdout on human formats only.

/// "No <nouns> found[ in <scope>]." — scope is pre-formatted, e.g. "vault 'kv'".
pub fn empty_state_message(noun_plural: &str, scope: Option<&str>) -> String {
    match scope {
        Some(scope) => format!("No {noun_plural} found in {scope}."),
        None => format!("No {noun_plural} found."),
    }
}

/// "N <noun>[ in <scope>]", or "Showing X of Y <noun>[ in <scope>]"
/// when paginated. Grammatically pluralized: callers pass both noun forms and the
/// correct one is chosen from `total` (equal to `displayed` when unpaginated).
pub fn count_label(
    displayed: usize,
    total: usize,
    noun_singular: &str,
    noun_plural: &str,
    scope: Option<&str>,
    paginated: bool,
) -> String {
    let noun = if total == 1 {
        noun_singular
    } else {
        noun_plural
    };
    let base = if paginated {
        format!("Showing {displayed} of {total} {noun}")
    } else {
        format!("{displayed} {noun}")
    };
    match scope {
        Some(scope) => format!("{base} in {scope}"),
        None => base,
    }
}

/// "N <noun>" with the grammatically correct form — for count-line suffixes
/// like ", 3 folders" that are composed outside `count_label`.
pub fn pluralize(count: usize, singular: &str, plural: &str) -> String {
    let noun = if count == 1 { singular } else { plural };
    format!("{count} {noun}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_state_without_scope() {
        assert_eq!(empty_state_message("vaults", None), "No vaults found.");
    }

    #[test]
    fn empty_state_with_scope() {
        assert_eq!(
            empty_state_message("secrets", Some("folder 'prod'")),
            "No secrets found in folder 'prod'."
        );
    }

    #[test]
    fn count_singular() {
        assert_eq!(count_label(1, 1, "vault", "vaults", None, false), "1 vault");
    }

    #[test]
    fn count_plural() {
        assert_eq!(
            count_label(3, 3, "vault", "vaults", None, false),
            "3 vaults"
        );
    }

    #[test]
    fn count_zero_is_plural() {
        assert_eq!(
            count_label(0, 0, "secret", "secrets", None, false),
            "0 secrets"
        );
    }

    #[test]
    fn count_irregular_plural() {
        assert_eq!(
            count_label(5, 5, "audit log entry", "audit log entries", None, false),
            "5 audit log entries"
        );
    }

    #[test]
    fn count_unpaginated_scoped() {
        assert_eq!(
            count_label(65, 65, "secret", "secrets", Some("vault 'kv'"), false),
            "65 secrets in vault 'kv'"
        );
    }

    #[test]
    fn count_paginated() {
        assert_eq!(
            count_label(10, 42, "secret", "secrets", Some("vault 'kv'"), true),
            "Showing 10 of 42 secrets in vault 'kv'"
        );
    }

    #[test]
    fn count_paginated_singular_total() {
        assert_eq!(
            count_label(1, 1, "secret", "secrets", None, true),
            "Showing 1 of 1 secret"
        );
    }

    #[test]
    fn pluralize_picks_form() {
        assert_eq!(pluralize(1, "folder", "folders"), "1 folder");
        assert_eq!(pluralize(2, "folder", "folders"), "2 folders");
        assert_eq!(pluralize(0, "directory", "directories"), "0 directories");
    }
}
