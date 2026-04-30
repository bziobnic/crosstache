//! Levenshtein-based "did you mean...?" matcher. Pure functions; no I/O.

/// Maximum edit distance for a candidate to be considered a "did you mean"
/// match. Empirically tuned for short identifier-style names.
const MAX_DISTANCE: usize = 2;

/// Return the closest candidate to `target` if any are within
/// `MAX_DISTANCE` edits. Ties broken by first-seen order in `candidates`.
///
/// Returns `None` when:
///   - `candidates` is empty
///   - no candidate scores ≤ `MAX_DISTANCE`
///   - `target` is empty (avoids degenerate matches)
pub fn closest_match<'a>(target: &str, candidates: &'a [String]) -> Option<&'a str> {
    if target.is_empty() || candidates.is_empty() {
        return None;
    }
    let mut best: Option<(usize, &str)> = None;
    for c in candidates {
        let d = strsim::levenshtein(target, c);
        if d > MAX_DISTANCE {
            continue;
        }
        match best {
            None => best = Some((d, c.as_str())),
            Some((bd, _)) if d < bd => best = Some((d, c.as_str())),
            _ => {}
        }
    }
    best.map(|(_, name)| name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_close_match() {
        let candidates = vec![
            "myproj-prod".to_string(),
            "myproj-dev".to_string(),
            "completely-different".to_string(),
        ];
        let result = closest_match("myproj-prood", &candidates);
        assert_eq!(result, Some("myproj-prod"));
    }

    #[test]
    fn returns_none_when_too_far() {
        let candidates = vec!["banana".to_string(), "apple".to_string()];
        let result = closest_match("xyzzy", &candidates);
        assert_eq!(result, None);
    }

    #[test]
    fn returns_none_for_empty_candidates() {
        let candidates: Vec<String> = vec![];
        let result = closest_match("anything", &candidates);
        assert_eq!(result, None);
    }

    #[test]
    fn exact_match_wins_over_close_match() {
        let candidates = vec!["foo".to_string(), "fop".to_string()];
        let result = closest_match("foo", &candidates);
        assert_eq!(result, Some("foo"));
    }

    #[test]
    fn distance_threshold_is_two() {
        // distance 2: one edit away in two places
        let candidates = vec!["abcde".to_string()];
        assert_eq!(closest_match("axcye", &candidates), Some("abcde"));
        // distance 3: too far
        let candidates = vec!["abcde".to_string()];
        assert_eq!(closest_match("axcyf", &candidates), None);
    }
}
