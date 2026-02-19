//! Resource type detection for the smart info command
//!
//! This module provides intelligent detection of resource types (vault, secret, file)
//! based on naming patterns, context clues, and discovery fallback.

use crate::cli::commands::ResourceType;

/// Resource detector for smart type detection
pub struct ResourceDetector;

impl ResourceDetector {
    /// Detect resource type based on various heuristics
    /// 
    /// Priority order:
    /// 1. Explicit type hint (if provided)
    /// 2. Context clues (resource group implies vault)
    /// 3. Pattern matching (file extensions, naming conventions)
    /// 4. Default to secret (most common use case)
    pub fn detect_resource_type(
        resource: &str,
        hint: Option<ResourceType>,
        has_resource_group: bool,
    ) -> ResourceType {
        // Priority 1: Use explicit hint if provided
        if let Some(resource_type) = hint {
            return resource_type;
        }
        
        // Priority 2: If resource group is provided, it's likely a vault
        if has_resource_group {
            return ResourceType::Vault;
        }
        
        // Priority 3: Pattern matching
        
        // Check for file patterns (has extension)
        #[cfg(feature = "file-ops")]
        if Self::looks_like_file(resource) {
            return ResourceType::File;
        }
        
        // Check for vault naming patterns
        if Self::looks_like_vault(resource) {
            return ResourceType::Vault;
        }
        
        // Default: Assume it's a secret (most common operation)
        ResourceType::Secret
    }
    
    /// Check if the resource name looks like a file
    fn looks_like_file(name: &str) -> bool {
        // Files typically have extensions
        if name.contains('.') {
            // Check if it's a common file extension
            let common_extensions = [
                "txt", "json", "xml", "csv", "pdf", "doc", "docx",
                "xls", "xlsx", "zip", "tar", "gz", "jpg", "jpeg",
                "png", "gif", "mp4", "avi", "mov", "log", "conf",
                "yaml", "yml", "toml", "ini", "cfg", "env", "pem",
                "key", "crt", "cer", "pfx", "p12", "jks", "keystore",
            ];
            
            if let Some(extension) = name.split('.').next_back() {
                let ext_lower = extension.to_lowercase();
                if common_extensions.contains(&ext_lower.as_str()) {
                    return true;
                }
            }
        }
        
        // Check for path-like patterns
        if name.contains('/') || name.contains('\\') {
            return true;
        }
        
        false
    }
    
    /// Check if the resource name looks like a vault
    fn looks_like_vault(name: &str) -> bool {
        // Azure Key Vault naming rules:
        // - 3-24 characters
        // - Alphanumeric and hyphens only
        // - Must start with a letter
        // - Must end with a letter or digit
        // - No consecutive hyphens
        
        let len = name.len();
        if !(3..=24).contains(&len) {
            return false;
        }

        // Check if it starts with a letter
        if !name.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
            return false;
        }

        // Check if it ends with a letter or digit
        if !name.chars().last().is_some_and(|c| c.is_ascii_alphanumeric()) {
            return false;
        }
        
        // Check for valid characters (alphanumeric and hyphens)
        let mut prev_hyphen = false;
        for c in name.chars() {
            if !c.is_ascii_alphanumeric() && c != '-' {
                return false;
            }
            
            // Check for consecutive hyphens
            if c == '-' {
                if prev_hyphen {
                    return false;
                }
                prev_hyphen = true;
            } else {
                prev_hyphen = false;
            }
        }
        
        // Common vault naming patterns
        let vault_patterns = [
            "vault", "kv", "keyvault", "akv", "-kv-", "-vault-",
        ];
        
        let name_lower = name.to_lowercase();
        for pattern in &vault_patterns {
            if name_lower.contains(pattern) {
                return true;
            }
        }
        
        // If it passes all vault naming rules, it might be a vault
        // but we'll still default to secret since that's more common
        false
    }
    
    /// Check if a name is a valid vault name
    #[allow(dead_code)]
    pub fn is_valid_vault_name(name: &str) -> bool {
        // More strict validation for vault names
        let len = name.len();
        if !(3..=24).contains(&len) {
            return false;
        }
        
        // Must start with a letter
        let first_char = name.chars().next().unwrap();
        if !first_char.is_ascii_alphabetic() {
            return false;
        }
        
        // Must end with a letter or digit
        let last_char = name.chars().last().unwrap();
        if !last_char.is_ascii_alphanumeric() {
            return false;
        }
        
        // Check all characters and no consecutive hyphens
        let mut prev_hyphen = false;
        for c in name.chars() {
            if !c.is_ascii_alphanumeric() && c != '-' {
                return false;
            }
            
            if c == '-' {
                if prev_hyphen {
                    return false;
                }
                prev_hyphen = true;
            } else {
                prev_hyphen = false;
            }
        }
        
        true
    }
    
    /// Check if a name is a valid secret name
    #[allow(dead_code)]
    pub fn is_valid_secret_name(name: &str) -> bool {
        // Azure Key Vault secret naming rules:
        // - 1-127 characters
        // - Alphanumeric and hyphens
        
        let len = name.len();
        if len == 0 || len > 127 {
            return false;
        }
        
        // Check for valid characters
        for c in name.chars() {
            if !c.is_ascii_alphanumeric() && c != '-' {
                return false;
            }
        }
        
        true
    }
    
    /// Check if a name is a valid file name
    #[allow(dead_code)]
    pub fn is_valid_file_name(name: &str) -> bool {
        // Basic file name validation
        // Azure Blob Storage naming rules are quite permissive
        
        if name.is_empty() || name.len() > 1024 {
            return false;
        }
        
        // Check for invalid characters in blob names
        // Blob names can contain any URL-safe characters
        true
    }
    
    /// Get a user-friendly description of why a resource was detected as a certain type
    pub fn get_detection_reason(
        resource: &str,
        detected_type: ResourceType,
        has_resource_group: bool,
    ) -> String {
        match detected_type {
            ResourceType::Vault => {
                if has_resource_group {
                    "Resource group was provided, indicating a vault operation".to_string()
                } else if Self::looks_like_vault(resource) {
                    "Name matches vault naming patterns".to_string()
                } else {
                    "Detected as vault based on context".to_string()
                }
            }
            ResourceType::Secret => {
                if Self::looks_like_file(resource) {
                    "Defaulting to secret (use --type file if this is a file)".to_string()
                } else {
                    "Name matches secret patterns or is the default type".to_string()
                }
            }
            #[cfg(feature = "file-ops")]
            ResourceType::File => {
                if resource.contains('.') {
                    format!("Has file extension: .{}", resource.split('.').next_back().unwrap_or(""))
                } else if resource.contains('/') || resource.contains('\\') {
                    "Contains path separators".to_string()
                } else {
                    "Detected as file based on naming patterns".to_string()
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_file_detection() {
        assert!(ResourceDetector::looks_like_file("document.pdf"));
        assert!(ResourceDetector::looks_like_file("config.json"));
        assert!(ResourceDetector::looks_like_file("data.csv"));
        assert!(ResourceDetector::looks_like_file("path/to/file.txt"));
        assert!(!ResourceDetector::looks_like_file("my-secret"));
        assert!(!ResourceDetector::looks_like_file("my-vault"));
    }
    
    #[test]
    fn test_vault_detection() {
        assert!(ResourceDetector::looks_like_vault("my-key-vault"));
        assert!(ResourceDetector::looks_like_vault("prod-kv-001"));
        assert!(!ResourceDetector::looks_like_vault("my--vault")); // consecutive hyphens
        assert!(!ResourceDetector::looks_like_vault("-vault")); // starts with hyphen
        assert!(!ResourceDetector::looks_like_vault("vault-")); // ends with hyphen
        assert!(!ResourceDetector::looks_like_vault("v")); // too short
        assert!(!ResourceDetector::looks_like_vault("this-is-a-very-long-vault-name-that-exceeds-limit")); // too long
    }
    
    #[test]
    fn test_resource_type_detection() {
        // Explicit hint takes priority
        assert_eq!(
            ResourceDetector::detect_resource_type("anything", Some(ResourceType::Vault), false),
            ResourceType::Vault
        );
        
        // Resource group implies vault
        assert_eq!(
            ResourceDetector::detect_resource_type("my-resource", None, true),
            ResourceType::Vault
        );
        
        // File detection by extension
        #[cfg(feature = "file-ops")]
        assert_eq!(
            ResourceDetector::detect_resource_type("data.json", None, false),
            ResourceType::File
        );
        
        // Default to secret
        assert_eq!(
            ResourceDetector::detect_resource_type("my-secret-name", None, false),
            ResourceType::Secret
        );
    }
    
    #[test]
    fn test_valid_names() {
        // Valid vault names
        assert!(ResourceDetector::is_valid_vault_name("myvault"));
        assert!(ResourceDetector::is_valid_vault_name("my-vault-123"));
        assert!(!ResourceDetector::is_valid_vault_name("my_vault")); // underscore not allowed
        assert!(!ResourceDetector::is_valid_vault_name("123vault")); // must start with letter
        
        // Valid secret names
        assert!(ResourceDetector::is_valid_secret_name("my-secret"));
        assert!(ResourceDetector::is_valid_secret_name("secret123"));
        assert!(!ResourceDetector::is_valid_secret_name("my_secret")); // underscore not allowed
        assert!(!ResourceDetector::is_valid_secret_name("")); // empty not allowed
        
        // File names are generally valid
        assert!(ResourceDetector::is_valid_file_name("file.txt"));
        assert!(ResourceDetector::is_valid_file_name("my-file"));
    }
}