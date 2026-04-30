//! Static "code -> hint" map for TTY error display.
//! Hints are short (one line) and actionable.

/// Return a one-line user hint for the given error code, or `None` if
/// no hint is registered. Hints are TTY-only — print them after the
/// main error message.
pub fn hint_for(code: &str) -> Option<&'static str> {
    Some(match code {
        "xv-vault-not-found" => "Run 'xv vault list' to see available vaults.",
        "xv-secret-not-found" => "Run 'xv list' to see secrets in the active vault.",
        "xv-invalid-secret-name" => "Names must be alphanumeric + hyphens; see 'xv help set'.",
        "xv-permission-denied" => "Check your role with 'xv whoami'; see 'xv vault share list'.",
        "xv-auth-failed" => "Try 'az login' or set AZURE_CLIENT_ID / AZURE_CLIENT_SECRET / AZURE_TENANT_ID.",
        "xv-network-dns" => "Check the vault name and your DNS settings.",
        "xv-network-timeout" => "Check your network connection or proxy settings.",
        "xv-network-refused" => "Verify the vault exists and is reachable from this network.",
        "xv-network-ssl" => "Check TLS configuration and any corporate proxy with TLS interception.",
        "xv-config-invalid" => "Run 'xv config show' to inspect, or 'xv init' to reinitialize.",
        "xv-azure-api" => "Check Azure service status and your subscription quotas.",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_codes_have_hints() {
        assert!(hint_for("xv-vault-not-found").is_some());
        assert!(hint_for("xv-secret-not-found").is_some());
        assert!(hint_for("xv-permission-denied").is_some());
        assert!(hint_for("xv-network-dns").is_some());
        assert!(hint_for("xv-config-invalid").is_some());
    }

    #[test]
    fn unknown_codes_return_none() {
        assert_eq!(hint_for("xv-this-code-does-not-exist"), None);
    }

    #[test]
    fn hints_are_one_line() {
        for code in [
            "xv-vault-not-found",
            "xv-secret-not-found",
            "xv-permission-denied",
            "xv-network-dns",
            "xv-network-timeout",
            "xv-config-invalid",
        ] {
            let hint = hint_for(code).unwrap();
            assert!(!hint.contains('\n'), "hint for {code} contains newline: {hint:?}");
            assert!(!hint.is_empty(), "hint for {code} is empty");
        }
    }
}
