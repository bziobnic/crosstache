//! General utility helper functions
//!
//! This module contains various helper functions for common operations
//! including GUID validation, connection string handling, and URI manipulation.

use crate::error::{CrosstacheError, Result};
use regex::Regex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[cfg(unix)]
type FileMode = libc::mode_t;
#[cfg(not(unix))]
type FileMode = u32;

#[derive(Clone, Copy)]
enum FileOpenBehavior {
    Replace,
    Exclusive,
    Lock,
}

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
        // If the file already exists it may be read-only from a previous
        // sensitive write; clear the read-only attribute first so the
        // overwrite does not fail with a permission error (e.g. a second
        // context `save` after `set_context` on Windows).
        if let Ok(meta) = std::fs::metadata(path.as_ref()) {
            let mut perms = meta.permissions();
            if perms.readonly() {
                perms.set_readonly(false);
                std::fs::set_permissions(path.as_ref(), perms)?;
            }
        }
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

/// Write a downloaded file without following symlinks in any path component.
///
/// On Unix, every directory is opened relative to the previously opened
/// directory handle with `O_NOFOLLOW`, and the final file is opened the same
/// way. This keeps the security check and the write on the same kernel-resolved
/// path. Other platforms perform the strongest std-only equivalent by rejecting
/// reparse/symlink metadata for every existing component before opening.
pub fn write_file_no_follow(path: &Path, content: &[u8], overwrite: bool) -> Result<std::fs::File> {
    let behavior = if overwrite {
        FileOpenBehavior::Replace
    } else {
        FileOpenBehavior::Exclusive
    };
    write_file_no_follow_with_mode(path, content, behavior, 0o666, 0o777)
}

/// Open or create an empty private lock file without following symlinks.
///
/// Missing parent directories are created owner-only (0700 on Unix), and the
/// lock file itself is created owner-only (0600 on Unix).
pub fn open_private_lock_file_no_follow(path: &Path) -> Result<std::fs::File> {
    write_file_no_follow_with_mode(path, &[], FileOpenBehavior::Lock, 0o600, 0o700)
}

fn write_file_no_follow_with_mode(
    path: &Path,
    content: &[u8],
    behavior: FileOpenBehavior,
    file_mode: FileMode,
    directory_mode: FileMode,
) -> Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::io::Write;
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::ffi::OsStrExt;
        use std::path::Component;

        let mut absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|e| {
                    CrosstacheError::config(format!("Cannot resolve current directory: {e}"))
                })?
                .join(path)
        };
        // macOS and some Unix layouts expose root-owned compatibility links
        // such as /var -> /private/var. Resolve only symlinks owned by root
        // and not writable by group/other; user-controlled links remain in
        // the path and are rejected by the O_NOFOLLOW traversal below.
        {
            use std::os::unix::fs::MetadataExt;
            let mut resolved = PathBuf::from("/");
            let mut tail = Vec::new();
            let mut components = absolute.components();
            let _ = components.next();
            for component in components {
                if !tail.is_empty() {
                    tail.push(component.as_os_str().to_os_string());
                    continue;
                }
                let candidate = resolved.join(component.as_os_str());
                match std::fs::symlink_metadata(&candidate) {
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        if metadata.uid() == 0 && metadata.mode() & 0o022 == 0 {
                            resolved = candidate.canonicalize().map_err(|e| {
                                CrosstacheError::config(format!(
                                    "Failed to resolve trusted system path '{}': {e}",
                                    candidate.display()
                                ))
                            })?;
                        } else {
                            tail.push(component.as_os_str().to_os_string());
                        }
                    }
                    Ok(_) => resolved.push(component.as_os_str()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        tail.push(component.as_os_str().to_os_string());
                    }
                    Err(e) => return Err(CrosstacheError::config(e.to_string())),
                }
            }
            for component in tail {
                resolved.push(component);
            }
            absolute = resolved;
        }
        let mut components = absolute.components();
        if !matches!(components.next(), Some(Component::RootDir)) {
            return Err(CrosstacheError::invalid_argument(format!(
                "Download destination '{}' is not an absolute filesystem path",
                absolute.display()
            )));
        }
        let names: Vec<_> = components
            .map(|component| match component {
                Component::Normal(name) => Ok(name.to_os_string()),
                _ => Err(CrosstacheError::invalid_argument(format!(
                    "Download destination '{}' contains an unsafe path component",
                    absolute.display()
                ))),
            })
            .collect::<Result<Vec<_>>>()?;
        let (file_name, parent_names) = names.split_last().ok_or_else(|| {
            CrosstacheError::invalid_argument("Download destination must name a file")
        })?;

        let root_fd = unsafe {
            libc::open(
                c"/".as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if root_fd < 0 {
            return Err(CrosstacheError::config(format!(
                "Failed to open filesystem root: {}",
                std::io::Error::last_os_error()
            )));
        }
        let mut directory = unsafe { std::fs::File::from_raw_fd(root_fd) };

        for name in parent_names {
            let c_name = CString::new(name.as_bytes()).map_err(|_| {
                CrosstacheError::invalid_argument("Download destination contains a NUL byte")
            })?;
            let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
            let mut next_fd =
                unsafe { libc::openat(directory.as_raw_fd(), c_name.as_ptr(), flags) };
            if next_fd < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound
            {
                let mkdir_result = unsafe {
                    libc::mkdirat(directory.as_raw_fd(), c_name.as_ptr(), directory_mode)
                };
                if mkdir_result < 0
                    && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
                {
                    return Err(CrosstacheError::config(format!(
                        "Failed to create download directory '{}': {}",
                        name.to_string_lossy(),
                        std::io::Error::last_os_error()
                    )));
                }
                next_fd = unsafe { libc::openat(directory.as_raw_fd(), c_name.as_ptr(), flags) };
            }
            if next_fd < 0 {
                return Err(CrosstacheError::config(format!(
                    "Refusing unsafe download path component '{}': {}",
                    name.to_string_lossy(),
                    std::io::Error::last_os_error()
                )));
            }
            directory = unsafe { std::fs::File::from_raw_fd(next_fd) };
        }

        let c_name = CString::new(file_name.as_bytes()).map_err(|_| {
            CrosstacheError::invalid_argument("Download destination contains a NUL byte")
        })?;
        let (access_mode, create_mode) = match behavior {
            FileOpenBehavior::Replace => (libc::O_WRONLY, libc::O_TRUNC),
            FileOpenBehavior::Exclusive => (libc::O_WRONLY, libc::O_EXCL),
            FileOpenBehavior::Lock => (libc::O_RDWR, libc::O_EXCL),
        };
        let mut fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                c_name.as_ptr(),
                access_mode | libc::O_CREAT | create_mode | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                libc::c_uint::from(file_mode),
            )
        };
        if fd < 0
            && matches!(behavior, FileOpenBehavior::Lock)
            && std::io::Error::last_os_error().kind() == std::io::ErrorKind::AlreadyExists
        {
            fd = unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    c_name.as_ptr(),
                    libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
        }
        if fd < 0 {
            return Err(CrosstacheError::config(format!(
                "Refusing unsafe download destination '{}': {}",
                absolute.display(),
                std::io::Error::last_os_error()
            )));
        }
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        file.write_all(content).map_err(|e| {
            CrosstacheError::config(format!("Failed to write file {}: {e}", absolute.display()))
        })?;
        Ok(file)
    }

    #[cfg(not(unix))]
    {
        use std::io::Write;

        let _ = (file_mode, directory_mode);

        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|e| {
                    CrosstacheError::config(format!("Cannot resolve current directory: {e}"))
                })?
                .join(path)
        };
        if let Some(parent) = absolute.parent() {
            let mut current = PathBuf::new();
            for component in parent.components() {
                current.push(component.as_os_str());
                match std::fs::symlink_metadata(&current) {
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        return Err(CrosstacheError::config(format!(
                            "Refusing symlinked download path component '{}'",
                            current.display()
                        )));
                    }
                    Ok(metadata) if !metadata.is_dir() => {
                        return Err(CrosstacheError::config(format!(
                            "Download path component '{}' is not a directory",
                            current.display()
                        )));
                    }
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        std::fs::create_dir(&current).map_err(|e| {
                            CrosstacheError::config(format!(
                                "Failed to create download directory '{}': {e}",
                                current.display()
                            ))
                        })?;
                    }
                    Err(e) => return Err(CrosstacheError::config(e.to_string())),
                }
            }
        }
        if std::fs::symlink_metadata(&absolute)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            return Err(CrosstacheError::config(format!(
                "Refusing symlinked download destination '{}'",
                absolute.display()
            )));
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true);
        match behavior {
            FileOpenBehavior::Replace => {
                options.truncate(true);
            }
            FileOpenBehavior::Exclusive => {
                options.create_new(true);
            }
            FileOpenBehavior::Lock => {
                options.read(true);
            }
        }
        let mut file = options.open(&absolute).map_err(|e| {
            CrosstacheError::config(format!("Failed to open {}: {e}", absolute.display()))
        })?;
        file.write_all(content).map_err(|e| {
            CrosstacheError::config(format!("Failed to write {}: {e}", absolute.display()))
        })?;
        Ok(file)
    }
}

/// Atomically replace a file while refusing symlink components and final
/// symlinks. The temporary file is created exclusively in the destination
/// directory, flushed, and then renamed over the destination.
pub fn atomic_write_file_no_follow(path: &Path, content: &[u8], private: bool) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        CrosstacheError::invalid_argument("Atomic destination must have a parent directory")
    })?;
    let file_name = path
        .file_name()
        .ok_or_else(|| CrosstacheError::invalid_argument("Atomic destination must name a file"))?;
    let temp_name = format!(".{}.{}.tmp", file_name.to_string_lossy(), Uuid::new_v4());
    let temp_path = parent.join(temp_name);
    let (file_mode, directory_mode) = if private {
        (0o600, 0o700)
    } else {
        (0o666, 0o777)
    };

    let result = (|| {
        let file = write_file_no_follow_with_mode(
            &temp_path,
            content,
            FileOpenBehavior::Exclusive,
            file_mode,
            directory_mode,
        )?;
        file.sync_all().map_err(|e| {
            CrosstacheError::config(format!(
                "Failed to flush temporary file '{}': {e}",
                temp_path.display()
            ))
        })?;

        if std::fs::symlink_metadata(path)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            return Err(CrosstacheError::config(format!(
                "Refusing symlinked destination '{}'",
                path.display()
            )));
        }
        #[cfg(not(target_os = "windows"))]
        std::fs::rename(&temp_path, path).map_err(|e| {
            CrosstacheError::config(format!(
                "Failed to atomically replace '{}': {e}",
                path.display()
            ))
        })?;

        // Unlike Unix rename(2), std::fs::rename does not replace an existing
        // file on Windows. MoveFileExW preserves the atomic-replacement
        // contract used by context and project saves on every supported OS.
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::ffi::OsStrExt;
            use windows_sys::Win32::Storage::FileSystem::{
                MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
            };

            let source: Vec<u16> = temp_path
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let destination: Vec<u16> = path
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let moved = unsafe {
                MoveFileExW(
                    source.as_ptr(),
                    destination.as_ptr(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            };
            if moved == 0 {
                let error = std::io::Error::last_os_error();
                return Err(CrosstacheError::config(format!(
                    "Failed to atomically replace '{}': {error}",
                    path.display()
                )));
            }
        }
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

pub async fn atomic_write_file_no_follow_async(
    path: &Path,
    content: &[u8],
    private: bool,
) -> Result<()> {
    let path = path.to_path_buf();
    let content = content.to_vec();
    tokio::task::spawn_blocking(move || atomic_write_file_no_follow(&path, &content, private))
        .await
        .map_err(|e| CrosstacheError::config(format!("Atomic file write task failed: {e}")))?
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

    let bytes = untrusted.as_bytes();
    let has_windows_drive_prefix =
        bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if untrusted_path.is_absolute()
        || untrusted.starts_with('/')
        || untrusted.starts_with('\\')
        || untrusted.contains('\\')
        || has_windows_drive_prefix
    {
        return Err(CrosstacheError::invalid_argument(format!(
            "Blob name '{untrusted}' is an absolute path, which is not allowed"
        )));
    }

    for component in untrusted_path.components() {
        if matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        ) {
            return Err(CrosstacheError::invalid_argument(format!(
                "Blob name '{untrusted}' contains '..', which is not allowed"
            )));
        }
    }

    Ok(base.join(untrusted_path))
}

/// Compile `pattern` into a whole-name, case-sensitive glob matcher, exactly
/// as `xv migrate --filter` does. Used by `xv ls --filter` and `xv find
/// --filter` (shared helper). Returns `invalid_argument` on a bad pattern —
/// callers must invoke this before any backend call so a typo'd glob fails
/// fast.
pub fn compile_name_glob(pattern: &str) -> Result<globset::GlobMatcher> {
    Ok(globset::Glob::new(pattern)
        .map_err(|e| CrosstacheError::invalid_argument(format!("Invalid glob pattern: {e}")))?
        .compile_matcher())
}

/// True when `matcher` matches either `name` (the backend/sanitized name) or
/// `original_name` (the user-facing display name, when set) — the
/// either-name convention shared with `xv mv` and `xv run --include`/
/// `--exclude`.
pub fn glob_matches_either_name(
    matcher: &globset::GlobMatcher,
    name: &str,
    original_name: &str,
) -> bool {
    matcher.is_match(name) || (!original_name.is_empty() && matcher.is_match(original_name))
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
    fn test_safe_join_rejects_windows_drive_and_unc_paths_on_every_platform() {
        let base = std::path::Path::new("/safe/base");
        assert!(safe_join(base, r"C:\Windows\system32\payload.dll").is_err());
        assert!(safe_join(base, r"\\server\share\payload.dll").is_err());
        assert!(safe_join(base, r"nested\..\payload.dll").is_err());
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
    fn test_compile_name_glob_rejects_invalid_pattern() {
        let err = compile_name_glob("test-[").unwrap_err();
        assert!(err.to_string().contains("Invalid glob pattern"));
    }

    #[test]
    fn test_compile_name_glob_prefix_anchoring() {
        let matcher = compile_name_glob("test-*").unwrap();
        assert!(matcher.is_match("test-db"));
        assert!(!matcher.is_match("latest-db"));
    }

    #[test]
    fn test_compile_name_glob_specials() {
        let q = compile_name_glob("ab?").unwrap();
        assert!(q.is_match("abc"));
        assert!(!q.is_match("ab"));
        assert!(!q.is_match("abcd"));

        let bracket = compile_name_glob("f[ab]o").unwrap();
        assert!(bracket.is_match("fao"));
        assert!(bracket.is_match("fbo"));
        assert!(!bracket.is_match("fco"));
    }

    #[test]
    fn test_glob_matches_either_name() {
        let matcher = compile_name_glob("display-*").unwrap();
        // Matches on original_name (display), not on backend name.
        assert!(glob_matches_either_name(
            &matcher,
            "sanitized-name",
            "display-thing"
        ));
        // Matches on backend name when original_name is empty.
        let matcher2 = compile_name_glob("backend-*").unwrap();
        assert!(glob_matches_either_name(&matcher2, "backend-thing", ""));
        // Neither matches.
        assert!(!glob_matches_either_name(
            &matcher2,
            "other",
            "other-display"
        ));
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
