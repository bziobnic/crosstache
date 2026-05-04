//! URL construction and OData query helpers.
//!
//! These helpers ensure that user-controlled values are properly escaped
//! before being embedded in OData filter expressions or URL path segments,
//! preventing OData injection and URL path injection attacks.

/// Build an OData `eq` filter expression, escaping single quotes in `value`.
///
/// OData string literals delimit with single quotes; a literal `'` must be
/// doubled (`''`).  Without this escaping an attacker-controlled value could
/// break out of the literal and inject arbitrary filter logic.
///
/// # Example
/// ```
/// use crosstache::utils::url_helpers::odata_eq;
/// assert_eq!(odata_eq("principalId", "abc-123"), "principalId eq 'abc-123'");
/// assert_eq!(odata_eq("id", "it's"), "id eq 'it''s'");
/// ```
pub fn odata_eq(field: &str, value: &str) -> String {
    let escaped = value.replace('\'', "''");
    format!("{field} eq '{escaped}'")
}

/// Build a URL from a `base` string and additional `segments`, percent-encoding each segment.
///
/// Each segment is encoded with [`urlencoding::encode`] so that characters
/// such as `@`, `/`, `?`, and `#` in user-supplied values cannot traverse
/// outside their intended path component.
///
/// # Example
/// ```
/// use crosstache::utils::url_helpers::graph_url;
/// let url = graph_url("https://graph.microsoft.com/v1.0/users", &["user@example.com"]);
/// assert_eq!(url, "https://graph.microsoft.com/v1.0/users/user%40example.com");
/// ```
pub fn graph_url(base: &str, segments: &[&str]) -> String {
    let mut url = base.to_string();
    for seg in segments {
        url.push('/');
        url.push_str(&urlencoding::encode(seg));
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn odata_eq_no_special_chars() {
        assert_eq!(
            odata_eq("principalId", "00000000-0000-0000-0000-000000000000"),
            "principalId eq '00000000-0000-0000-0000-000000000000'"
        );
    }

    #[test]
    fn odata_eq_escapes_single_quote() {
        assert_eq!(odata_eq("name", "it's"), "name eq 'it''s'");
    }

    #[test]
    fn odata_eq_escapes_multiple_quotes() {
        assert_eq!(odata_eq("x", "a'b'c"), "x eq 'a''b''c'");
    }

    #[test]
    fn graph_url_encodes_at_sign() {
        let url = graph_url("https://graph.microsoft.com/v1.0/users", &["user@corp.com"]);
        assert_eq!(
            url,
            "https://graph.microsoft.com/v1.0/users/user%40corp.com"
        );
    }

    #[test]
    fn graph_url_multiple_segments() {
        let url = graph_url("https://example.com", &["seg/1", "seg 2"]);
        assert_eq!(url, "https://example.com/seg%2F1/seg%202");
    }
}
