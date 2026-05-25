//! General utility helper functions
//!
//! This module contains various helper functions for common operations
//! including GUID validation, connection string handling, and URI manipulation.

use crate::error::{CrosstacheError, Result};
use regex::Regex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Write bytes to a file with mode 0o600 (owner read/write only).
/// Refuses to follow symlinks on Unix (O_NOFOLLOW).
pub fn write_private(
    path: impl AsRef<std::path::Path>,
    bytes: impl AsRef<[u8]>,
) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path.as_ref())?;
        file.write_all(bytes.as_ref())?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path.as_ref(), bytes.as_ref())?;
        let mut perms = std::fs::metadata(path.as_ref())?.permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(path.as_ref(), perms)?;
        Ok(())
    }
}

/// Create a directory (and parents) with mode 0o700 (owner only).
pub fn create_private_dir(path: impl AsRef<std::path::Path>) -> std::io::Result<()> {
    std::fs::create_dir_all(path.as_ref())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path.as_ref(), std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Write content to a file with restricted permissions (0600 on Unix).
/// Use for any file that may contain secrets, tokens, or sensitive config.
pub fn write_sensitive_file(path: &Path, content: &[u8]) -> std::io::Result<()> {
    write_private(path, content)
}

/// Async version of write_sensitive_file.
///
/// Delegates to the synchronous `write_private` on a blocking thread so that
/// the atomic `OpenOptions::mode(0o600)` path is used (no TOCTOU window).
pub async fn write_sensitive_file_async(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let path = path.to_path_buf();
    let content = content.to_vec();
    tokio::task::spawn_blocking(move || write_private(&path, &content))
        .await
        .map_err(std::io::Error::other)?
}

/// Check if a string is a valid GUID/UUID
#[allow(dead_code)]
pub fn is_guid(s: &str) -> bool {
    Uuid::parse_str(s).is_ok()
}

/// Build a connection string from key-value pairs
#[allow(dead_code)]
pub fn build_connection_string(params: &HashMap<String, String>) -> String {
    if params.is_empty() {
        return String::new();
    }

    params
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
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
#[allow(dead_code)]
pub fn get_vault_uri(vault_name: &str) -> String {
    format!("https://{vault_name}.vault.azure.net/")
}

/// Extract vault name from vault URI
#[allow(dead_code)]
pub fn extract_vault_name_from_uri(vault_uri: &str) -> Result<String> {
    let re = Regex::new(r"^https://([^.]+)\.vault\.azure\.net/?$")?;

    if let Some(captures) = re.captures(vault_uri) {
        if let Some(name) = captures.get(1) {
            return Ok(name.as_str().to_string());
        }
    }

    Err(CrosstacheError::invalid_argument(format!(
        "Invalid vault URI format: {vault_uri}"
    )))
}

/// Generate a new UUID
#[allow(dead_code)]
pub fn generate_uuid() -> String {
    Uuid::new_v4().to_string()
}

/// Convert a name to environment variable format (UPPER_SNAKE_CASE)
pub fn to_env_var_name(name: &str) -> String {
    let re = Regex::new(r"[^a-zA-Z0-9]").unwrap();
    re.replace_all(name, "_").to_uppercase()
}

/// Normalize a name for matching (lowercase, replace non-alphanumeric with underscore)
#[allow(dead_code)]
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
        return Err(CrosstacheError::invalid_argument(
            "Folder path cannot be empty",
        ));
    }

    // Check for invalid characters at start/end
    if folder_path.starts_with('/') {
        return Err(CrosstacheError::invalid_argument(
            "Folder path cannot start with '/'",
        ));
    }

    if folder_path.ends_with('/') {
        return Err(CrosstacheError::invalid_argument(
            "Folder path cannot end with '/'",
        ));
    }

    // Split by '/' and validate each folder name
    let folders: Vec<&str> = folder_path.split('/').collect();

    for folder in &folders {
        if folder.is_empty() {
            return Err(CrosstacheError::invalid_argument(
                "Folder path cannot contain empty folder names (consecutive '/')",
            ));
        }

        // Folder names can contain alphanumeric characters, hyphens, underscores, spaces, and dots
        // but cannot contain '/' (which is the separator)
        if folder.contains('/') {
            return Err(CrosstacheError::invalid_argument(
                "Folder names cannot contain '/' character",
            ));
        }

        // Additional validation for reasonable folder names
        if folder.len() > 50 {
            return Err(CrosstacheError::invalid_argument(
                "Folder names cannot exceed 50 characters",
            ));
        }

        // Ensure folder name is not just whitespace
        if folder.trim().is_empty() {
            return Err(CrosstacheError::invalid_argument(
                "Folder names cannot be only whitespace",
            ));
        }
    }

    // Limit the depth of folder structure
    if folders.len() > 10 {
        return Err(CrosstacheError::invalid_argument(
            "Folder path depth cannot exceed 10 levels",
        ));
    }

    Ok(())
}

/// Safely join an untrusted path component onto a base directory.
///
/// Rejects absolute paths and `..` components in `untrusted` to prevent
/// path traversal from malicious blob names.
pub fn safe_join(base: &Path, untrusted: &str) -> Result<PathBuf> {
    let untrusted_path = Path::new(untrusted);

    if untrusted_path.is_absolute() {
        return Err(CrosstacheError::invalid_argument(format!(
            "Blob name '{untrusted}' is an absolute path, which is not allowed"
        )));
    }

    for component in untrusted_path.components() {
        if component == std::path::Component::ParentDir {
            return Err(CrosstacheError::invalid_argument(format!(
                "Blob name '{untrusted}' contains '..', which is not allowed"
            )));
        }
    }

    Ok(base.join(untrusted_path))
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
        let deep_path = (0..11)
            .map(|i| format!("folder{i}"))
            .collect::<Vec<_>>()
            .join("/");
        assert!(validate_folder_path(&deep_path).is_err()); // Too deep
    }

    #[test]
    fn test_safe_join_rejects_traversal() {
        let base = std::path::Path::new("/tmp/base");
        assert!(safe_join(base, "../escape.txt").is_err());
        assert!(safe_join(base, "subdir/../../escape.txt").is_err());
        assert!(safe_join(base, "a/../../../etc/passwd").is_err());
    }

    #[test]
    fn test_safe_join_rejects_absolute() {
        let base = std::path::Path::new("/tmp/base");
        assert!(safe_join(base, "/etc/passwd").is_err());
        assert!(safe_join(base, "/absolute/path").is_err());
    }

    #[test]
    fn test_safe_join_allows_normal_names() {
        let base = std::path::Path::new("/tmp/base");

        let result = safe_join(base, "readme.txt").unwrap();
        assert_eq!(result, std::path::Path::new("/tmp/base/readme.txt"));

        let result = safe_join(base, "docs/readme.md").unwrap();
        assert_eq!(result, std::path::Path::new("/tmp/base/docs/readme.md"));
    }

    #[test]
    #[cfg(unix)]
    fn test_write_private_rejects_symlinks() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        let link = dir.path().join("symlink.txt");

        // Create a symlink
        symlink(&target, &link).unwrap();

        // write_private should refuse to follow the symlink (O_NOFOLLOW)
        let result = write_private(&link, b"secret data");
        assert!(result.is_err());
        assert!(result.unwrap_err().raw_os_error() == Some(libc::ELOOP));
    }
}
