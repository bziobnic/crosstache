//! Name sanitization for Azure Key Vault compatibility
//! 
//! This module provides functionality to sanitize secret names
//! to comply with Azure Key Vault naming requirements while
//! preserving original names in tags.

use regex::Regex;
use sha2::{Digest, Sha256};
use crate::error::Result;

const MAX_NAME_LENGTH: usize = 127;

/// Azure Key Vault naming rules
pub struct AzureKeyVaultNameRules {
    pub max_length: usize,
    pub allowed_pattern: &'static str,
    pub description: &'static str,
}

impl AzureKeyVaultNameRules {
    pub fn new() -> Self {
        Self {
            max_length: MAX_NAME_LENGTH,
            allowed_pattern: r"^[a-zA-Z0-9-]+$",
            description: "Azure Key Vault secret names must be 1-127 characters, containing only 0-9, a-z, A-Z, and -",
        }
    }
}

/// Check if a name is valid for Azure Key Vault
pub fn is_valid_keyvault_name(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_NAME_LENGTH {
        return false;
    }
    
    let re = Regex::new(r"^[a-zA-Z0-9-]+$").unwrap();
    re.is_match(name)
}

/// Sanitize a secret name for Azure Key Vault compatibility
pub fn sanitize_secret_name(name: &str) -> Result<String> {
    if name.is_empty() {
        return Ok("empty-name".to_string());
    }
    
    // Replace invalid characters with hyphens
    let re = Regex::new(r"[^a-zA-Z0-9-]")?;
    let mut sanitized = re.replace_all(name, "-").to_string();
    
    // Remove consecutive hyphens
    let consecutive_re = Regex::new(r"-+")?;
    sanitized = consecutive_re.replace_all(&sanitized, "-").to_string();
    
    // Trim hyphens from start and end
    sanitized = sanitized.trim_matches('-').to_string();
    
    // If empty after sanitization, use hash
    if sanitized.is_empty() {
        return Ok(hash_secret_name(name));
    }
    
    // If too long, use hash
    if sanitized.len() > MAX_NAME_LENGTH {
        return Ok(hash_secret_name(name));
    }
    
    Ok(sanitized)
}

/// Generate a hash-based name for secrets that can't be sanitized normally
pub fn hash_secret_name(name: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(name.as_bytes());
    let hash = hasher.finalize();
    
    // Use first 16 bytes (32 hex chars) with 'h-' prefix to indicate it's hashed
    format!("h-{}", hex::encode(&hash[..16]))
}

/// Generate a unique secret name by appending a suffix if needed
pub fn generate_unique_secret_name(base_name: &str, existing_names: &[String]) -> String {
    if !existing_names.contains(&base_name.to_string()) {
        return base_name.to_string();
    }
    
    for i in 2..=100 {
        let candidate = format!("{}-{}", base_name, i);
        if !existing_names.contains(&candidate) {
            return candidate;
        }
    }
    
    // If still collision, use hash-based approach
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    hash_secret_name(&format!("{}-{}", base_name, timestamp))
}

/// Get detailed information about secret name sanitization
pub fn get_secret_name_info(name: &str) -> Result<SecretNameInfo> {
    let sanitized = sanitize_secret_name(name)?;
    
    Ok(SecretNameInfo {
        original_name: name.to_string(),
        sanitized_name: sanitized.clone(),
        original_length: name.len(),
        sanitized_length: sanitized.len(),
        was_modified: name != sanitized,
        is_hashed: sanitized.starts_with("h-"),
        is_valid_keyvault_name: is_valid_keyvault_name(&sanitized),
    })
}

#[derive(Debug, Clone)]
pub struct SecretNameInfo {
    pub original_name: String,
    pub sanitized_name: String,
    pub original_length: usize,
    pub sanitized_length: usize,
    pub was_modified: bool,
    pub is_hashed: bool,
    pub is_valid_keyvault_name: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_valid_names() {
        assert!(is_valid_keyvault_name("valid-name"));
        assert!(is_valid_keyvault_name("ValidName123"));
        assert!(is_valid_keyvault_name("123"));
        assert!(is_valid_keyvault_name("a"));
        assert!(is_valid_keyvault_name("A"));
        assert!(is_valid_keyvault_name("0"));
        assert!(is_valid_keyvault_name("a-b-c"));
        assert!(is_valid_keyvault_name("Test123-Name"));
        assert!(is_valid_keyvault_name(&"a".repeat(127))); // Max length
    }
    
    #[test]
    fn test_invalid_names() {
        // Empty string
        assert!(!is_valid_keyvault_name(""));
        
        // Invalid characters
        assert!(!is_valid_keyvault_name("invalid@name"));
        assert!(!is_valid_keyvault_name("name with spaces"));
        assert!(!is_valid_keyvault_name("name.with.dots"));
        assert!(!is_valid_keyvault_name("name_with_underscores"));
        assert!(!is_valid_keyvault_name("name/with/slashes"));
        assert!(!is_valid_keyvault_name("name\\with\\backslashes"));
        assert!(!is_valid_keyvault_name("name:with:colons"));
        assert!(!is_valid_keyvault_name("name;with;semicolons"));
        assert!(!is_valid_keyvault_name("name+with+plus"));
        assert!(!is_valid_keyvault_name("name=with=equals"));
        assert!(!is_valid_keyvault_name("name!with!exclamation"));
        assert!(!is_valid_keyvault_name("name?with?question"));
        assert!(!is_valid_keyvault_name("name*with*asterisk"));
        assert!(!is_valid_keyvault_name("name%with%percent"));
        assert!(!is_valid_keyvault_name("name#with#hash"));
        assert!(!is_valid_keyvault_name("name$with$dollar"));
        assert!(!is_valid_keyvault_name("name&with&ampersand"));
        assert!(!is_valid_keyvault_name("name(with)parentheses"));
        assert!(!is_valid_keyvault_name("name[with]brackets"));
        assert!(!is_valid_keyvault_name("name{with}braces"));
        assert!(!is_valid_keyvault_name("name<with>angles"));
        assert!(!is_valid_keyvault_name("name|with|pipes"));
        assert!(!is_valid_keyvault_name("name\"with\"quotes"));
        assert!(!is_valid_keyvault_name("name'with'apostrophes"));
        assert!(!is_valid_keyvault_name("name`with`backticks"));
        assert!(!is_valid_keyvault_name("name~with~tildes"));
        assert!(!is_valid_keyvault_name("name^with^carets"));
        
        // Unicode characters
        assert!(!is_valid_keyvault_name("naméwithaccents"));
        assert!(!is_valid_keyvault_name("名前"));
        assert!(!is_valid_keyvault_name("имя"));
        assert!(!is_valid_keyvault_name("🚀rocket"));
        
        // Too long
        assert!(!is_valid_keyvault_name(&"a".repeat(128)));
        assert!(!is_valid_keyvault_name(&"a".repeat(200)));
    }
    
    #[test]
    fn test_basic_sanitization() {
        assert_eq!(sanitize_secret_name("valid-name").unwrap(), "valid-name");
        assert_eq!(sanitize_secret_name("invalid@name").unwrap(), "invalid-name");
        assert_eq!(sanitize_secret_name("name with spaces").unwrap(), "name-with-spaces");
        assert_eq!(sanitize_secret_name("---name---").unwrap(), "name");
    }
    
    #[test]
    fn test_empty_string_sanitization() {
        assert_eq!(sanitize_secret_name("").unwrap(), "empty-name");
    }
    
    #[test]
    fn test_special_character_replacement() {
        assert_eq!(sanitize_secret_name("test@example.com").unwrap(), "test-example-com");
        assert_eq!(sanitize_secret_name("user_name").unwrap(), "user-name");
        assert_eq!(sanitize_secret_name("file/path/name").unwrap(), "file-path-name");
        assert_eq!(sanitize_secret_name("connection:string").unwrap(), "connection-string");
        assert_eq!(sanitize_secret_name("key=value").unwrap(), "key-value");
        assert_eq!(sanitize_secret_name("name with spaces").unwrap(), "name-with-spaces");
        assert_eq!(sanitize_secret_name("test.config.json").unwrap(), "test-config-json");
    }
    
    #[test]
    fn test_consecutive_hyphen_removal() {
        assert_eq!(sanitize_secret_name("name--with--double").unwrap(), "name-with-double");
        assert_eq!(sanitize_secret_name("name---with---triple").unwrap(), "name-with-triple");
        assert_eq!(sanitize_secret_name("name@@@@with@@@@multiple").unwrap(), "name-with-multiple");
        assert_eq!(sanitize_secret_name("test...dots...everywhere").unwrap(), "test-dots-everywhere");
        assert_eq!(sanitize_secret_name("mixed@@@...___special").unwrap(), "mixed-special");
    }
    
    #[test]
    fn test_hyphen_trimming() {
        assert_eq!(sanitize_secret_name("-name").unwrap(), "name");
        assert_eq!(sanitize_secret_name("name-").unwrap(), "name");
        assert_eq!(sanitize_secret_name("-name-").unwrap(), "name");
        assert_eq!(sanitize_secret_name("--name--").unwrap(), "name");
        assert_eq!(sanitize_secret_name("---name---").unwrap(), "name");
        assert_eq!(sanitize_secret_name("@name@").unwrap(), "name");
        assert_eq!(sanitize_secret_name("@@name@@").unwrap(), "name");
    }
    
    #[test]
    fn test_unicode_character_handling() {
        assert_eq!(sanitize_secret_name("naméwithaccents").unwrap(), "nam-withaccents");
        assert_eq!(sanitize_secret_name("🚀rocket").unwrap(), "rocket");
        assert_eq!(sanitize_secret_name("test🔥fire").unwrap(), "test-fire");
        
        // When only invalid characters remain, should use hash
        let result = sanitize_secret_name("名前").unwrap();
        assert!(result.starts_with("h-"));
        assert_eq!(result.len(), 34); // "h-" + 32 hex chars
        
        let result = sanitize_secret_name("🚀🔥💯").unwrap();
        assert!(result.starts_with("h-"));
        assert_eq!(result.len(), 34); // "h-" + 32 hex chars
    }
    
    #[test]
    fn test_only_invalid_characters() {
        // Should use hash when only invalid characters
        let result = sanitize_secret_name("@@@").unwrap();
        assert!(result.starts_with("h-"));
        assert_eq!(result.len(), 34);
        
        let result = sanitize_secret_name("...").unwrap();
        assert!(result.starts_with("h-"));
        assert_eq!(result.len(), 34);
        
        let result = sanitize_secret_name("___").unwrap();
        assert!(result.starts_with("h-"));
        assert_eq!(result.len(), 34);
        
        let result = sanitize_secret_name("   ").unwrap();
        assert!(result.starts_with("h-"));
        assert_eq!(result.len(), 34);
    }
    
    #[test]
    fn test_length_limit_handling() {
        // Test exactly at limit
        let long_name = "a".repeat(127);
        assert_eq!(sanitize_secret_name(&long_name).unwrap(), long_name);
        
        // Test over limit - should use hash
        let too_long = "a".repeat(128);
        let result = sanitize_secret_name(&too_long).unwrap();
        assert!(result.starts_with("h-"));
        assert_eq!(result.len(), 34);
        
        // Test way over limit
        let way_too_long = "a".repeat(500);
        let result = sanitize_secret_name(&way_too_long).unwrap();
        assert!(result.starts_with("h-"));
        assert_eq!(result.len(), 34);
        
        // Test long name with special characters that results in over 127 chars
        let long_with_special = format!("{}@{}", "a".repeat(70), "b".repeat(70));
        let result = sanitize_secret_name(&long_with_special).unwrap();
        assert!(result.starts_with("h-"));
        assert_eq!(result.len(), 34);
        
        // Test long name with special characters that stays under limit
        let medium_with_special = format!("{}@{}", "a".repeat(60), "b".repeat(60));
        let result = sanitize_secret_name(&medium_with_special).unwrap();
        assert!(!result.starts_with("h-"));
        assert_eq!(result.len(), 121); // 60 + 1 + 60 = 121 chars
        assert_eq!(result, format!("{}-{}", "a".repeat(60), "b".repeat(60)));
    }
    
    #[test]
    fn test_hash_generation() {
        let name1 = "test@example.com";
        let name2 = "test@example.org";
        
        let hash1 = hash_secret_name(name1);
        let hash2 = hash_secret_name(name2);
        
        // Hashes should be different
        assert_ne!(hash1, hash2);
        
        // Hashes should be consistent
        assert_eq!(hash1, hash_secret_name(name1));
        assert_eq!(hash2, hash_secret_name(name2));
        
        // Hash format should be correct
        assert!(hash1.starts_with("h-"));
        assert_eq!(hash1.len(), 34); // "h-" + 32 hex chars
        assert!(hash2.starts_with("h-"));
        assert_eq!(hash2.len(), 34);
        
        // Hash should only contain valid characters
        assert!(is_valid_keyvault_name(&hash1));
        assert!(is_valid_keyvault_name(&hash2));
    }
    
    #[test]
    fn test_unique_name_generation() {
        let existing = vec!["test".to_string(), "test-2".to_string(), "test-3".to_string()];
        
        // Should return original if not in existing
        assert_eq!(generate_unique_secret_name("new-name", &existing), "new-name");
        
        // Should append -2 if original exists
        assert_eq!(generate_unique_secret_name("test", &existing), "test-4");
        
        // Should find first available number
        let existing2 = vec!["test".to_string(), "test-3".to_string()];
        assert_eq!(generate_unique_secret_name("test", &existing2), "test-2");
        
        // Test with many existing names
        let many_existing: Vec<String> = (1..=100).map(|i| {
            if i == 1 { "test".to_string() } else { format!("test-{}", i) }
        }).collect();
        
        let result = generate_unique_secret_name("test", &many_existing);
        assert!(result.starts_with("h-")); // Should use hash when all numbers exhausted
    }
    
    #[test]
    fn test_secret_name_info() {
        // Test unmodified name
        let info = get_secret_name_info("valid-name").unwrap();
        assert_eq!(info.original_name, "valid-name");
        assert_eq!(info.sanitized_name, "valid-name");
        assert_eq!(info.original_length, 10);
        assert_eq!(info.sanitized_length, 10);
        assert!(!info.was_modified);
        assert!(!info.is_hashed);
        assert!(info.is_valid_keyvault_name);
        
        // Test modified name
        let info = get_secret_name_info("invalid@name").unwrap();
        assert_eq!(info.original_name, "invalid@name");
        assert_eq!(info.sanitized_name, "invalid-name");
        assert_eq!(info.original_length, 12);
        assert_eq!(info.sanitized_length, 12);
        assert!(info.was_modified);
        assert!(!info.is_hashed);
        assert!(info.is_valid_keyvault_name);
        
        // Test hashed name
        let long_name = "a".repeat(200);
        let info = get_secret_name_info(&long_name).unwrap();
        assert_eq!(info.original_name, long_name);
        assert!(info.sanitized_name.starts_with("h-"));
        assert_eq!(info.original_length, 200);
        assert_eq!(info.sanitized_length, 34);
        assert!(info.was_modified);
        assert!(info.is_hashed);
        assert!(info.is_valid_keyvault_name);
    }
    
    #[test]
    fn test_azure_keyvault_name_rules() {
        let rules = AzureKeyVaultNameRules::new();
        assert_eq!(rules.max_length, 127);
        assert_eq!(rules.allowed_pattern, r"^[a-zA-Z0-9-]+$");
        assert!(!rules.description.is_empty());
    }
    
    #[test]
    fn test_edge_cases() {
        // Single character
        assert_eq!(sanitize_secret_name("a").unwrap(), "a");
        
        // Single invalid character should use hash
        let result = sanitize_secret_name("@").unwrap();
        assert!(result.starts_with("h-"));
        assert_eq!(result.len(), 34);
        
        // Only hyphens should use hash
        let result = sanitize_secret_name("-").unwrap();
        assert!(result.starts_with("h-"));
        assert_eq!(result.len(), 34);
        
        let result = sanitize_secret_name("--").unwrap();
        assert!(result.starts_with("h-"));
        assert_eq!(result.len(), 34);
        
        let result = sanitize_secret_name("---").unwrap();
        assert!(result.starts_with("h-"));
        assert_eq!(result.len(), 34);
        
        // Mixed valid and invalid at boundaries
        assert_eq!(sanitize_secret_name("@a@").unwrap(), "a");
        assert_eq!(sanitize_secret_name("@a-b@").unwrap(), "a-b");
        
        // Numbers only
        assert_eq!(sanitize_secret_name("123").unwrap(), "123");
        assert_eq!(sanitize_secret_name("1@2@3").unwrap(), "1-2-3");
        
        // Whitespace variations
        assert_eq!(sanitize_secret_name(" name ").unwrap(), "name");
        assert_eq!(sanitize_secret_name("\tname\t").unwrap(), "name");
        assert_eq!(sanitize_secret_name("\nname\n").unwrap(), "name");
        assert_eq!(sanitize_secret_name("name\r\nwith\r\nnewlines").unwrap(), "name-with-newlines");
    }
    
    #[test]
    fn test_consistency() {
        // Same input should always produce same output
        let long_string = "a".repeat(200);
        let test_cases = vec![
            "test@example.com",
            "user_name_123",
            "file/path/to/secret",
            "connection:string:value",
            &long_string,
            "🚀🔥💯",
            "",
            "---",
            "valid-name",
        ];
        
        for case in test_cases {
            let result1 = sanitize_secret_name(case).unwrap();
            let result2 = sanitize_secret_name(case).unwrap();
            assert_eq!(result1, result2, "Inconsistent result for input: {}", case);
            
            // Result should always be valid
            assert!(is_valid_keyvault_name(&result1), "Invalid result for input: {}", case);
        }
    }
}