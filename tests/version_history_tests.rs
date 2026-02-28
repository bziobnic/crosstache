//! End-to-end tests for version and history functionality
//!
//! Tests cover:
//! - `xv version` command output
//! - Version ID parsing from Azure Key Vault API responses
//! - History display formatting
//! - Edge cases in version response parsing

use std::process::Command;

// ============================================================================
// CLI Version Command Tests
// ============================================================================

#[cfg(test)]
mod version_command_tests {
    use super::*;

    /// Test that `xv version` runs successfully and outputs expected fields
    #[test]
    fn test_version_command_output() {
        let output = Command::new(env!("CARGO_BIN_EXE_xv"))
            .arg("version")
            .output()
            .expect("Failed to execute xv version");

        assert!(output.status.success(), "xv version should exit with code 0");

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("crosstache Rust CLI"), "Should contain product name");
        assert!(stdout.contains("Version:"), "Should contain Version field");
        assert!(stdout.contains("Git Hash:"), "Should contain Git Hash field");
        assert!(stdout.contains("Git Branch:"), "Should contain Git Branch field");
    }

    /// Test that version string is a valid semver
    #[test]
    fn test_version_is_valid_semver() {
        let output = Command::new(env!("CARGO_BIN_EXE_xv"))
            .arg("version")
            .output()
            .expect("Failed to execute xv version");

        let stdout = String::from_utf8_lossy(&output.stdout);
        let version_line = stdout
            .lines()
            .find(|l| l.starts_with("Version:"))
            .expect("Should have a Version line");

        let version_str = version_line
            .strip_prefix("Version:")
            .unwrap()
            .trim();

        // Should match semver pattern (e.g. 0.4.6 or 0.4.6.123)
        let parts: Vec<&str> = version_str.split('.').collect();
        assert!(
            parts.len() >= 3,
            "Version '{version_str}' should have at least 3 semver components"
        );
        for (i, part) in parts.iter().take(3).enumerate() {
            // Strip any +metadata suffix from the last part
            let clean = part.split('+').next().unwrap();
            assert!(
                clean.parse::<u32>().is_ok(),
                "Version component {i} ('{clean}') should be numeric"
            );
        }
    }

    /// Test that `xv --version` flag works
    #[test]
    fn test_version_flag() {
        let output = Command::new(env!("CARGO_BIN_EXE_xv"))
            .arg("--version")
            .output()
            .expect("Failed to execute xv --version");

        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("xv") || stdout.contains("crosstache"),
            "--version should contain binary or package name"
        );
    }

    /// Test that git hash is present and not empty
    #[test]
    fn test_version_has_git_hash() {
        let output = Command::new(env!("CARGO_BIN_EXE_xv"))
            .arg("version")
            .output()
            .expect("Failed to execute xv version");

        let stdout = String::from_utf8_lossy(&output.stdout);
        let hash_line = stdout
            .lines()
            .find(|l| l.starts_with("Git Hash:"))
            .expect("Should have a Git Hash line");

        let hash = hash_line.strip_prefix("Git Hash:").unwrap().trim();
        // Hash should be either a short hex string or "unknown" (in non-git builds)
        assert!(
            !hash.is_empty(),
            "Git hash should not be empty"
        );
    }
}

// ============================================================================
// Version ID Parsing Tests (mirrors Azure Key Vault API response format)
// ============================================================================

#[cfg(test)]
mod version_id_parsing_tests {
    /// Extract version ID from a secret URL, same logic as get_secret_versions
    fn extract_version_from_id(id: Option<&str>) -> String {
        id.and_then(|url| url.split('/').next_back())
            .unwrap_or("unknown")
            .to_string()
    }

    #[test]
    fn test_parse_version_from_standard_id() {
        let id = "https://myvault.vault.azure.net/secrets/mysecret/abc123def456";
        assert_eq!(extract_version_from_id(Some(id)), "abc123def456");
    }

    #[test]
    fn test_parse_version_from_id_with_long_version() {
        let id = "https://myvault.vault.azure.net/secrets/mysecret/6a3b7c8d9e0f1a2b3c4d5e6f7a8b9c0d";
        assert_eq!(
            extract_version_from_id(Some(id)),
            "6a3b7c8d9e0f1a2b3c4d5e6f7a8b9c0d"
        );
    }

    #[test]
    fn test_parse_version_from_none_returns_unknown() {
        assert_eq!(extract_version_from_id(None), "unknown");
    }

    #[test]
    fn test_parse_version_from_empty_string() {
        // Empty string split('/').next_back() returns Some("")
        assert_eq!(extract_version_from_id(Some("")), "");
    }

    #[test]
    fn test_parse_version_from_no_slashes() {
        // Edge case: bare string
        assert_eq!(extract_version_from_id(Some("abc123")), "abc123");
    }

    #[test]
    fn test_parse_version_from_trailing_slash() {
        // Trailing slash gives empty last segment
        let id = "https://myvault.vault.azure.net/secrets/mysecret/abc123/";
        assert_eq!(extract_version_from_id(Some(id)), "");
    }

    /// Verify that using "kid" (the old bug) would return unknown for secrets
    #[test]
    fn test_kid_field_is_null_for_secrets() {
        let secret_version_json = serde_json::json!({
            "id": "https://myvault.vault.azure.net/secrets/mysecret/abc123def456",
            "attributes": {
                "enabled": true,
                "created": 1700000000,
                "updated": 1700000001
            }
        });

        // "kid" does not exist in secret responses
        let kid = secret_version_json["kid"].as_str();
        assert!(kid.is_none(), "Secret responses should not have a 'kid' field");

        // "id" does exist
        let id = secret_version_json["id"].as_str();
        assert!(id.is_some(), "Secret responses should have an 'id' field");
        assert_eq!(extract_version_from_id(id), "abc123def456");
    }
}

// ============================================================================
// History Response Parsing Tests (simulates Azure API responses)
// ============================================================================

#[cfg(test)]
mod history_response_parsing_tests {
    use chrono::{DateTime, Utc};

    /// Simulates the version parsing logic from get_secret_versions
    struct ParsedVersion {
        version: String,
        enabled: bool,
        created_on: String,
        #[allow(dead_code)]
        updated_on: String,
    }

    fn parse_versions_response(json: &serde_json::Value) -> Vec<ParsedVersion> {
        let mut versions = Vec::new();

        if let Some(value_array) = json["value"].as_array() {
            for version_json in value_array {
                let attributes = &version_json["attributes"];

                let version = version_json["id"]
                    .as_str()
                    .and_then(|id| id.split('/').next_back())
                    .unwrap_or("unknown")
                    .to_string();

                let enabled = attributes["enabled"].as_bool().unwrap_or(true);
                let created_timestamp = attributes["created"].as_i64().unwrap_or(0);
                let updated_timestamp = attributes["updated"].as_i64().unwrap_or(0);

                let created_on = DateTime::from_timestamp(created_timestamp, 0)
                    .unwrap_or_else(Utc::now)
                    .format("%Y-%m-%d %H:%M:%S UTC")
                    .to_string();

                let updated_on = DateTime::from_timestamp(updated_timestamp, 0)
                    .unwrap_or_else(Utc::now)
                    .format("%Y-%m-%d %H:%M:%S UTC")
                    .to_string();

                versions.push(ParsedVersion {
                    version,
                    enabled,
                    created_on,
                    updated_on,
                });
            }
        }

        versions
    }

    #[test]
    fn test_parse_single_version() {
        let response = serde_json::json!({
            "value": [{
                "id": "https://myvault.vault.azure.net/secrets/db-password/v1abc",
                "attributes": {
                    "enabled": true,
                    "created": 1700000000,
                    "updated": 1700000001
                }
            }]
        });

        let versions = parse_versions_response(&response);
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].version, "v1abc");
        assert!(versions[0].enabled);
        assert_eq!(versions[0].created_on, "2023-11-14 22:13:20 UTC");
    }

    #[test]
    fn test_parse_multiple_versions() {
        let response = serde_json::json!({
            "value": [
                {
                    "id": "https://vault.vault.azure.net/secrets/key/version1",
                    "attributes": { "enabled": true, "created": 1700000000, "updated": 1700000000 }
                },
                {
                    "id": "https://vault.vault.azure.net/secrets/key/version2",
                    "attributes": { "enabled": false, "created": 1700100000, "updated": 1700100000 }
                },
                {
                    "id": "https://vault.vault.azure.net/secrets/key/version3",
                    "attributes": { "enabled": true, "created": 1700200000, "updated": 1700200000 }
                }
            ]
        });

        let versions = parse_versions_response(&response);
        assert_eq!(versions.len(), 3);
        assert_eq!(versions[0].version, "version1");
        assert_eq!(versions[1].version, "version2");
        assert_eq!(versions[2].version, "version3");
        assert!(!versions[1].enabled);
    }

    #[test]
    fn test_parse_empty_response() {
        let response = serde_json::json!({ "value": [] });
        let versions = parse_versions_response(&response);
        assert!(versions.is_empty());
    }

    #[test]
    fn test_parse_missing_value_array() {
        let response = serde_json::json!({});
        let versions = parse_versions_response(&response);
        assert!(versions.is_empty());
    }

    #[test]
    fn test_parse_missing_id_falls_back_to_unknown() {
        let response = serde_json::json!({
            "value": [{
                "attributes": {
                    "enabled": true,
                    "created": 1700000000,
                    "updated": 1700000000
                }
            }]
        });

        let versions = parse_versions_response(&response);
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].version, "unknown");
    }

    #[test]
    fn test_parse_missing_attributes_uses_defaults() {
        let response = serde_json::json!({
            "value": [{
                "id": "https://vault.vault.azure.net/secrets/key/ver1"
            }]
        });

        let versions = parse_versions_response(&response);
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].version, "ver1");
        assert!(versions[0].enabled); // defaults to true
        assert_eq!(versions[0].created_on, "1970-01-01 00:00:00 UTC"); // timestamp 0
    }

    #[test]
    fn test_parse_disabled_version() {
        let response = serde_json::json!({
            "value": [{
                "id": "https://vault.vault.azure.net/secrets/key/disabled-ver",
                "attributes": {
                    "enabled": false,
                    "created": 1700000000,
                    "updated": 1700000000
                }
            }]
        });

        let versions = parse_versions_response(&response);
        assert!(!versions[0].enabled);
    }

    #[test]
    fn test_parse_azure_style_version_ids() {
        // Real Azure version IDs are 32-char hex strings
        let response = serde_json::json!({
            "value": [
                {
                    "id": "https://prod-vault.vault.azure.net/secrets/api-key/a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6",
                    "attributes": { "enabled": true, "created": 1700000000, "updated": 1700000000 }
                },
                {
                    "id": "https://prod-vault.vault.azure.net/secrets/api-key/f6e5d4c3b2a1f0e9d8c7b6a5f4e3d2c1",
                    "attributes": { "enabled": true, "created": 1699900000, "updated": 1699900000 }
                }
            ]
        });

        let versions = parse_versions_response(&response);
        assert_eq!(versions[0].version, "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6");
        assert_eq!(versions[1].version, "f6e5d4c3b2a1f0e9d8c7b6a5f4e3d2c1");
    }

    /// Regression test: ensure "id" is used, not "kid"
    /// This was the root cause of the "unknown" versions bug
    #[test]
    fn test_regression_kid_vs_id_field() {
        // A response that has "kid" (like key responses) but NOT "id"
        // should produce "unknown" — this is expected and correct
        let key_style_response = serde_json::json!({
            "value": [{
                "kid": "https://vault.vault.azure.net/keys/mykey/abc123",
                "attributes": { "enabled": true, "created": 1700000000, "updated": 1700000000 }
            }]
        });

        let versions = parse_versions_response(&key_style_response);
        assert_eq!(
            versions[0].version, "unknown",
            "Without 'id' field, version should be 'unknown'"
        );

        // A proper secret response with "id" should parse correctly
        let secret_response = serde_json::json!({
            "value": [{
                "id": "https://vault.vault.azure.net/secrets/mysecret/abc123",
                "attributes": { "enabled": true, "created": 1700000000, "updated": 1700000000 }
            }]
        });

        let versions = parse_versions_response(&secret_response);
        assert_eq!(
            versions[0].version, "abc123",
            "With 'id' field, version should be parsed correctly"
        );
    }

    /// Test paginated response (nextLink field)
    #[test]
    fn test_response_has_next_link() {
        let response = serde_json::json!({
            "value": [{
                "id": "https://vault.vault.azure.net/secrets/key/ver1",
                "attributes": { "enabled": true, "created": 1700000000, "updated": 1700000000 }
            }],
            "nextLink": "https://vault.vault.azure.net/secrets/key/versions?api-version=7.4&$skiptoken=abc"
        });

        let next_link = response
            .get("nextLink")
            .and_then(|v| v.as_str());

        assert!(next_link.is_some(), "Should have a nextLink for pagination");
        assert!(next_link.unwrap().contains("skiptoken"));
    }

    #[test]
    fn test_response_without_next_link() {
        let response = serde_json::json!({
            "value": [{
                "id": "https://vault.vault.azure.net/secrets/key/ver1",
                "attributes": { "enabled": true, "created": 1700000000, "updated": 1700000000 }
            }]
        });

        let next_link = response.get("nextLink").and_then(|v| v.as_str());
        assert!(next_link.is_none());
    }
}
