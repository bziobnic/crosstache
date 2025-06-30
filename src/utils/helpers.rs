//! General utility helper functions
//! 
//! This module contains various helper functions for common operations
//! including GUID validation, connection string handling, and URI manipulation.

use regex::Regex;
use uuid::Uuid;
use std::collections::HashMap;
use crate::error::{crosstacheError, Result};

/// Check if a string is a valid GUID/UUID
pub fn is_guid(s: &str) -> bool {
    Uuid::parse_str(s).is_ok()
}

/// Build a connection string from key-value pairs
pub fn build_connection_string(params: &HashMap<String, String>) -> String {
    if params.is_empty() {
        return String::new();
    }
    
    params
        .iter()
        .map(|(key, value)| format!("{}={}", key, value))
        .collect::<Vec<_>>()
        .join(";")
}

/// Parse a connection string into key-value pairs
pub fn parse_connection_string(connection_string: &str) -> HashMap<String, String> {
    let mut params = HashMap::new();
    
    for pair in connection_string.split(';') {
        if let Some((key, value)) = pair.split_once('=') {
            params.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    
    params
}

/// Get vault URI from vault name
pub fn get_vault_uri(vault_name: &str) -> String {
    format!("https://{}.vault.azure.net/", vault_name)
}

/// Extract vault name from vault URI
pub fn extract_vault_name_from_uri(vault_uri: &str) -> Result<String> {
    let re = Regex::new(r"^https://([^.]+)\.vault\.azure\.net/?$")?;
    
    if let Some(captures) = re.captures(vault_uri) {
        if let Some(name) = captures.get(1) {
            return Ok(name.as_str().to_string());
        }
    }
    
    Err(crosstacheError::invalid_argument(format!(
        "Invalid vault URI format: {}",
        vault_uri
    )))
}

/// Generate a new UUID
pub fn generate_uuid() -> String {
    Uuid::new_v4().to_string()
}

/// Convert a name to environment variable format (UPPER_SNAKE_CASE)
pub fn to_env_var_name(name: &str) -> String {
    let re = Regex::new(r"[^a-zA-Z0-9]").unwrap();
    re.replace_all(name, "_").to_uppercase()
}

/// Normalize a name for matching (lowercase, replace non-alphanumeric with underscore)
pub fn normalize_name_for_matching(name: &str) -> String {
    let re = Regex::new(r"[^a-zA-Z0-9]").unwrap();
    re.replace_all(&name.to_lowercase(), "_").to_string()
}

/// Validate folder path format
/// Valid formats: 'folder1', 'folder1/folder2', 'folder1/folder2/folder3'
/// Folder names cannot contain the '/' character (except as separator)
/// Empty folder names (consecutive slashes) are not allowed
pub fn validate_folder_path(folder_path: &str) -> Result<()> {
    if folder_path.is_empty() {
        return Err(crosstacheError::invalid_argument("Folder path cannot be empty"));
    }
    
    // Check for invalid characters at start/end
    if folder_path.starts_with('/') {
        return Err(crosstacheError::invalid_argument("Folder path cannot start with '/'"));
    }
    
    if folder_path.ends_with('/') {
        return Err(crosstacheError::invalid_argument("Folder path cannot end with '/'"));
    }
    
    // Split by '/' and validate each folder name
    let folders: Vec<&str> = folder_path.split('/').collect();
    
    for folder in &folders {
        if folder.is_empty() {
            return Err(crosstacheError::invalid_argument("Folder path cannot contain empty folder names (consecutive '/')"));
        }
        
        // Folder names can contain alphanumeric characters, hyphens, underscores, spaces, and dots
        // but cannot contain '/' (which is the separator)
        if folder.contains('/') {
            return Err(crosstacheError::invalid_argument("Folder names cannot contain '/' character"));
        }
        
        // Additional validation for reasonable folder names
        if folder.len() > 50 {
            return Err(crosstacheError::invalid_argument("Folder names cannot exceed 50 characters"));
        }
        
        // Ensure folder name is not just whitespace
        if folder.trim().is_empty() {
            return Err(crosstacheError::invalid_argument("Folder names cannot be only whitespace"));
        }
    }
    
    // Limit the depth of folder structure
    if folders.len() > 10 {
        return Err(crosstacheError::invalid_argument("Folder path depth cannot exceed 10 levels"));
    }
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_is_guid() {
        assert!(is_guid("550e8400-e29b-41d4-a716-446655440000"));
        assert!(!is_guid("invalid-guid"));
        assert!(!is_guid(""));
    }
    
    #[test]
    fn test_connection_string() {
        let mut params = HashMap::new();
        params.insert("Server".to_string(), "localhost".to_string());
        params.insert("Database".to_string(), "test".to_string());
        
        let conn_str = build_connection_string(&params);
        let parsed = parse_connection_string(&conn_str);
        
        assert_eq!(parsed.get("Server"), Some(&"localhost".to_string()));
        assert_eq!(parsed.get("Database"), Some(&"test".to_string()));
    }
    
    #[test]
    fn test_vault_uri() {
        let vault_name = "test-vault";
        let uri = get_vault_uri(vault_name);
        assert_eq!(uri, "https://test-vault.vault.azure.net/");
        
        let extracted = extract_vault_name_from_uri(&uri).unwrap();
        assert_eq!(extracted, vault_name);
    }
    
    #[test]
    fn test_env_var_name() {
        assert_eq!(to_env_var_name("my-secret"), "MY_SECRET");
        assert_eq!(to_env_var_name("secret@name"), "SECRET_NAME");
        assert_eq!(to_env_var_name("secret with spaces"), "SECRET_WITH_SPACES");
    }
    
    #[test]
    fn test_validate_folder_path() {
        // Valid folder paths
        assert!(validate_folder_path("folder1").is_ok());
        assert!(validate_folder_path("folder1/folder2").is_ok());
        assert!(validate_folder_path("folder1/folder2/folder3").is_ok());
        assert!(validate_folder_path("app-configs").is_ok());
        assert!(validate_folder_path("app configs").is_ok());
        assert!(validate_folder_path("app.configs").is_ok());
        assert!(validate_folder_path("app_configs").is_ok());
        
        // Invalid folder paths
        assert!(validate_folder_path("").is_err()); // Empty
        assert!(validate_folder_path("/folder1").is_err()); // Starts with /
        assert!(validate_folder_path("folder1/").is_err()); // Ends with /
        assert!(validate_folder_path("folder1//folder2").is_err()); // Consecutive slashes
        assert!(validate_folder_path("folder1/ /folder2").is_err()); // Whitespace-only folder name
        assert!(validate_folder_path(&"a".repeat(51)).is_err()); // Folder name too long
        
        // Test depth limit
        let deep_path = (0..11).map(|i| format!("folder{}", i)).collect::<Vec<_>>().join("/");
        assert!(validate_folder_path(&deep_path).is_err()); // Too deep
    }
}