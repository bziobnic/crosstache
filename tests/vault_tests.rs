//! Vault operations tests
//!
//! Tests for vault management functionality including validation,
//! URI generation, and basic operations.

#[cfg(test)]
mod vault_validation_tests {
    use super::*;

    #[test]
    fn test_vault_name_validation() {
        // Valid vault names
        let valid_names = vec![
            "test-vault",
            "my-vault-123",
            "vault1",
            "a",
            "a-b-c-d-e-f-g-h-i-j-k-l", // 24 chars (max)
        ];

        for name in valid_names {
            assert!(is_valid_vault_name(name), "Name '{}' should be valid", name);
        }

        // Invalid vault names
        let invalid_names = vec![
            "",                          // Empty
            "Test-Vault",                // Uppercase
            "test_vault",                // Underscore
            "test vault",                // Space
            "test-vault-",               // Ends with hyphen
            "-test-vault",               // Starts with hyphen
            "test--vault",               // Double hyphen
            "a-b-c-d-e-f-g-h-i-j-k-l-m", // 25 chars (too long)
            "test.vault",                // Period
            "test@vault",                // Special character
        ];

        for name in invalid_names {
            assert!(
                !is_valid_vault_name(name),
                "Name '{}' should be invalid",
                name
            );
        }
    }

    #[test]
    fn test_location_validation() {
        // Valid Azure regions
        let valid_locations = vec![
            "eastus",
            "westus2",
            "centralus",
            "northeurope",
            "westeurope",
            "southeastasia",
            "australiaeast",
        ];

        for location in valid_locations {
            assert!(
                is_valid_azure_location(location),
                "Location '{}' should be valid",
                location
            );
        }

        // Invalid locations
        let invalid_locations = vec![
            "",
            "invalid-region",
            "East US", // Spaces
            "EASTUS",  // Uppercase
            "east_us", // Underscore
        ];

        for location in invalid_locations {
            assert!(
                !is_valid_azure_location(location),
                "Location '{}' should be invalid",
                location
            );
        }
    }

    #[test]
    fn test_sku_validation() {
        // Valid SKUs
        let valid_skus = vec!["standard", "premium"];

        for sku in valid_skus {
            assert!(is_valid_sku(sku), "SKU '{}' should be valid", sku);
        }

        // Invalid SKUs
        let invalid_skus = vec!["", "basic", "Standard", "PREMIUM", "invalid"];

        for sku in invalid_skus {
            assert!(!is_valid_sku(sku), "SKU '{}' should be invalid", sku);
        }
    }

    #[test]
    fn test_retention_days_validation() {
        // Valid retention periods
        let valid_days = vec![7, 30, 90];

        for days in valid_days {
            assert!(
                is_valid_retention_days(days),
                "Retention days {} should be valid",
                days
            );
        }

        // Invalid retention periods
        let invalid_days = vec![0, 6, 91, 100, -1];

        for days in invalid_days {
            assert!(
                !is_valid_retention_days(days),
                "Retention days {} should be invalid",
                days
            );
        }
    }
}

#[cfg(test)]
mod vault_uri_tests {
    use super::*;

    #[test]
    fn test_vault_uri_generation() {
        let vault_name = "test-vault";
        let expected_uri = "https://test-vault.vault.azure.net/";

        assert_eq!(generate_vault_uri(vault_name), expected_uri);
    }

    #[test]
    fn test_vault_name_from_uri() {
        let test_cases = vec![
            ("https://test-vault.vault.azure.net/", "test-vault"),
            ("https://my-vault-123.vault.azure.net", "my-vault-123"),
            ("https://prod-vault.vault.azure.net/secrets/", "prod-vault"),
        ];

        for (uri, expected_name) in test_cases {
            assert_eq!(extract_vault_name_from_uri(uri).unwrap(), expected_name);
        }
    }

    #[test]
    fn test_invalid_vault_uri() {
        let invalid_uris = vec![
            "",
            "not-a-uri",
            "https://example.com",
            "https://test.vault.azure.com/", // Wrong domain
            "http://test.vault.azure.net/",  // HTTP instead of HTTPS
        ];

        for uri in invalid_uris {
            assert!(
                extract_vault_name_from_uri(uri).is_err(),
                "URI '{}' should be invalid",
                uri
            );
        }
    }
}

#[cfg(test)]
mod vault_resource_tests {
    use super::*;

    #[test]
    fn test_resource_group_validation() {
        // Valid resource group names
        let valid_names = vec![
            "test-rg",
            "my_resource_group",
            "rg123",
            "Resource-Group_1",
            "a",
        ];

        for name in valid_names {
            assert!(
                is_valid_resource_group_name(name),
                "Resource group '{}' should be valid",
                name
            );
        }

        // Test max length separately
        let max_length_name = "a".repeat(90);
        assert!(
            is_valid_resource_group_name(&max_length_name),
            "90 char name should be valid"
        );

        // Invalid resource group names
        let invalid_names = vec![
            "",         // Empty
            "rg.",      // Ends with period
            ".rg",      // Starts with period
            "rg-",      // Ends with hyphen
            "-rg",      // Starts with hyphen
            "test..rg", // Double period
            "test--rg", // Double hyphen
        ];

        for name in invalid_names {
            assert!(
                !is_valid_resource_group_name(name),
                "Resource group '{}' should be invalid",
                name
            );
        }

        // Test too long name separately
        let too_long_name = "a".repeat(91);
        assert!(
            !is_valid_resource_group_name(&too_long_name),
            "91 char name should be invalid"
        );
    }

    #[test]
    fn test_subscription_id_validation() {
        // Valid subscription IDs (GUID format)
        let valid_ids = vec![
            "12345678-1234-1234-1234-123456789012",
            "abcdef12-3456-7890-abcd-ef1234567890",
            "00000000-0000-0000-0000-000000000000",
        ];

        for id in valid_ids {
            assert!(
                is_valid_subscription_id(id),
                "Subscription ID '{}' should be valid",
                id
            );
        }

        // Invalid subscription IDs
        let invalid_ids = vec![
            "",
            "not-a-guid",
            "12345678-1234-1234-1234-12345678901", // Too short
            "12345678-1234-1234-1234-1234567890123", // Too long
            "12345678-1234-1234-1234-123456789012-extra", // Extra characters
            "12345678_1234_1234_1234_123456789012", // Wrong separators
        ];

        for id in invalid_ids {
            assert!(
                !is_valid_subscription_id(id),
                "Subscription ID '{}' should be invalid",
                id
            );
        }
    }
}

// Helper functions for validation (these would typically be in the main codebase)
fn is_valid_vault_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 24 {
        return false;
    }

    // Must start and end with alphanumeric
    if !name.chars().next().unwrap_or(' ').is_ascii_alphanumeric()
        || !name.chars().last().unwrap_or(' ').is_ascii_alphanumeric()
    {
        return false;
    }

    // Only lowercase letters, numbers, and hyphens
    // No consecutive hyphens
    let mut prev_char = ' ';
    for ch in name.chars() {
        if !ch.is_ascii_lowercase() && !ch.is_ascii_digit() && ch != '-' {
            return false;
        }
        if ch == '-' && prev_char == '-' {
            return false;
        }
        prev_char = ch;
    }

    true
}

fn is_valid_azure_location(location: &str) -> bool {
    let valid_locations = vec![
        "eastus",
        "eastus2",
        "westus",
        "westus2",
        "westus3",
        "centralus",
        "northcentralus",
        "southcentralus",
        "northeurope",
        "westeurope",
        "francecentral",
        "germanywestcentral",
        "norwayeast",
        "switzerlandnorth",
        "uksouth",
        "ukwest",
        "southeastasia",
        "eastasia",
        "australiaeast",
        "australiasoutheast",
        "brazilsouth",
        "canadacentral",
        "canadaeast",
        "centralindia",
        "southindia",
        "westindia",
        "japaneast",
        "japanwest",
        "koreacentral",
        "koreasouth",
        "southafricanorth",
        "uaenorth",
    ];

    valid_locations.contains(&location)
}

fn is_valid_sku(sku: &str) -> bool {
    matches!(sku, "standard" | "premium")
}

fn is_valid_retention_days(days: i32) -> bool {
    days >= 7 && days <= 90
}

fn generate_vault_uri(vault_name: &str) -> String {
    format!("https://{}.vault.azure.net/", vault_name)
}

fn extract_vault_name_from_uri(uri: &str) -> Result<String, String> {
    if !uri.starts_with("https://") || !uri.contains(".vault.azure.net") {
        return Err("Invalid vault URI format".to_string());
    }

    let start = "https://".len();
    let end = uri
        .find(".vault.azure.net")
        .ok_or("Invalid vault URI format")?;

    if start >= end {
        return Err("Invalid vault URI format".to_string());
    }

    Ok(uri[start..end].to_string())
}

fn is_valid_resource_group_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 90 {
        return false;
    }

    // Cannot start or end with period or hyphen
    if name.starts_with('.') || name.starts_with('-') || name.ends_with('.') || name.ends_with('-')
    {
        return false;
    }

    // No consecutive periods or hyphens
    let mut prev_char = ' ';
    for ch in name.chars() {
        if !ch.is_ascii_alphanumeric() && ch != '.' && ch != '-' && ch != '_' {
            return false;
        }
        if (ch == '.' && prev_char == '.') || (ch == '-' && prev_char == '-') {
            return false;
        }
        prev_char = ch;
    }

    true
}

fn is_valid_subscription_id(id: &str) -> bool {
    // GUID format: 8-4-4-4-12 characters
    if id.len() != 36 {
        return false;
    }

    let parts: Vec<&str> = id.split('-').collect();
    if parts.len() != 5 {
        return false;
    }

    // Check each part length and that all characters are hex
    let expected_lengths = [8, 4, 4, 4, 12];
    for (i, part) in parts.iter().enumerate() {
        if part.len() != expected_lengths[i] {
            return false;
        }
        if !part.chars().all(|c| c.is_ascii_hexdigit()) {
            return false;
        }
    }

    true
}
