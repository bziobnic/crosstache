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

/// "N <noun>(s)[ in <scope>]", or "Showing X of Y <noun>(s)[ in <scope>]"
/// when paginated. Matches the pre-existing "(s)" pluralization style.
pub fn count_label(
    displayed: usize,
    total: usize,
    noun: &str,
    scope: Option<&str>,
    paginated: bool,
) -> String {
    let base = if paginated {
        format!("Showing {displayed} of {total} {noun}(s)")
    } else {
        format!("{displayed} {noun}(s)")
    };
    match scope {
        Some(scope) => format!("{base} in {scope}"),
        None => base,
    }
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
    fn count_unpaginated_unscoped() {
        assert_eq!(count_label(3, 3, "vault", None, false), "3 vault(s)");
    }

    #[test]
    fn count_unpaginated_scoped() {
        assert_eq!(
            count_label(65, 65, "secret", Some("vault 'kv'"), false),
            "65 secret(s) in vault 'kv'"
        );
    }

    #[test]
    fn count_paginated() {
        assert_eq!(
            count_label(10, 42, "secret", Some("vault 'kv'"), true),
            "Showing 10 of 42 secret(s) in vault 'kv'"
        );
    }
}
