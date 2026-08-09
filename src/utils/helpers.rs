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
    #[cfg(any(feature = "ui", test))]
    Lock,
}

impl FileOpenBehavior {
    fn is_lock(self) -> bool {
        #[cfg(any(feature = "ui", test))]
        {
            matches!(self, Self::Lock)
        }
        #[cfg(not(any(feature = "ui", test)))]
        {
            false
        }
    }
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
    #[cfg(windows)]
    {
        use std::io::Write;
        use windows_sys::Win32::Storage::FileSystem::{
            CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_WRITE,
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };

        // This used to emulate 0600 with FILE_ATTRIBUTE_READONLY. That grants
        // no confidentiality whatsoever on Windows — every other user can still
        // read the file — while blocking precisely the operations the local
        // backend depends on: `fs::rename` cannot replace a read-only
        // destination, and `FlushFileBuffers` cannot run against one. Both fail
        // with ERROR_ACCESS_DENIED (os error 5), which is how secret
        // activation and file syncing broke on Windows.
        //
        // Clear the attribute off files left behind by those builds, then
        // create with the protected owner+SYSTEM DACL that
        // `atomic_write_file_no_follow` already uses — the actual 0600
        // equivalent, and one that still permits the owner to delete and
        // replace (FILE_ALL_ACCESS), as renaming over the file requires.
        if let Ok(metadata) = std::fs::metadata(path.as_ref()) {
            let mut permissions = metadata.permissions();
            if permissions.readonly() {
                permissions.set_readonly(false);
                std::fs::set_permissions(path.as_ref(), permissions)?;
            }
        }

        let descriptor = windows_private_security_descriptor()
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let attributes = windows_security_attributes(Some(&descriptor));
        // FILE_FLAG_OPEN_REPARSE_POINT is the counterpart to the Unix branch's
        // `O_NOFOLLOW`: a symlink at `path` is replaced rather than written
        // through to whatever it points at.
        let mut file = windows_create_file(
            path.as_ref(),
            FILE_GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            CREATE_ALWAYS,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            &attributes,
        )?;
        file.write_all(bytes.as_ref())?;
        Ok(())
    }
    #[cfg(all(not(unix), not(windows)))]
    {
        std::fs::write(path.as_ref(), bytes.as_ref())
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

/// Read a file without following its final symlink.
pub fn read_file_no_follow(path: &Path) -> Result<Vec<u8>> {
    use std::io::Read;

    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;

        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|error| {
                CrosstacheError::config(format!(
                    "Failed to safely open config file '{}': {error}",
                    path.display()
                ))
            })?
    };

    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;

        let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
        let inspected = unsafe { libc::fstat(file.as_raw_fd(), metadata.as_mut_ptr()) };
        if inspected < 0 {
            return Err(CrosstacheError::config(format!(
                "Failed to inspect config file '{}': {}",
                path.display(),
                std::io::Error::last_os_error()
            )));
        }
        let metadata = unsafe { metadata.assume_init() };
        if metadata.st_mode & libc::S_IFMT != libc::S_IFREG {
            return Err(CrosstacheError::config(format!(
                "Refusing non-regular config file '{}'",
                path.display()
            )));
        }
    }

    #[cfg(not(unix))]
    let mut file = {
        let metadata = std::fs::symlink_metadata(path).map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to inspect config file '{}': {error}",
                path.display()
            ))
        })?;
        if metadata.file_type().is_symlink() {
            return Err(CrosstacheError::config(format!(
                "Refusing symlinked config file '{}'",
                path.display()
            )));
        }
        std::fs::File::open(path).map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to safely open config file '{}': {error}",
                path.display()
            ))
        })?
    };

    #[cfg(not(unix))]
    {
        let metadata = file.metadata().map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to inspect opened config file '{}': {error}",
                path.display()
            ))
        })?;
        if !metadata.is_file() {
            return Err(CrosstacheError::config(format!(
                "Refusing non-regular config file '{}'",
                path.display()
            )));
        }
    }

    let mut content = Vec::new();
    file.read_to_end(&mut content).map_err(|error| {
        CrosstacheError::config(format!(
            "Failed to read config file '{}': {error}",
            path.display()
        ))
    })?;
    Ok(content)
}

/// Create a new private file without following symlinks.
#[cfg(test)]
pub fn write_private_file_no_follow_create_new(
    path: &Path,
    content: &[u8],
) -> Result<std::fs::File> {
    write_file_no_follow_with_mode(path, content, FileOpenBehavior::Exclusive, 0o600, 0o700)
}

/// Open or create an empty private lock file without following symlinks.
///
/// Missing parent directories are created owner-only (0700 on Unix), and the
/// lock file itself is created owner-only (0600 on Unix).
#[cfg(any(feature = "ui", test))]
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
            #[cfg(any(feature = "ui", test))]
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
            && behavior.is_lock()
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
            #[cfg(any(feature = "ui", test))]
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

#[cfg(unix)]
struct UnixAtomicParent {
    directory: std::fs::File,
    destination: std::ffi::CString,
    absolute: PathBuf,
}

#[cfg(unix)]
fn open_unix_atomic_parent(path: &Path, directory_mode: FileMode) -> Result<UnixAtomicParent> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Component;

    let mut absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| {
                CrosstacheError::config(format!("Cannot resolve current directory: {error}"))
            })?
            .join(path)
    };

    // Permit only root-owned, non-writable compatibility links such as
    // macOS /var -> /private/var. User-controlled links remain unresolved and
    // are rejected by the descriptor-relative traversal.
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
                        resolved = candidate.canonicalize().map_err(|error| {
                            CrosstacheError::config(format!(
                                "Failed to resolve trusted system path '{}': {error}",
                                candidate.display()
                            ))
                        })?;
                    } else {
                        tail.push(component.as_os_str().to_os_string());
                    }
                }
                Ok(_) => resolved.push(component.as_os_str()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    tail.push(component.as_os_str().to_os_string());
                }
                Err(error) => return Err(CrosstacheError::config(error.to_string())),
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
            "Atomic destination '{}' is not an absolute filesystem path",
            absolute.display()
        )));
    }
    let names: Vec<_> = components
        .map(|component| match component {
            Component::Normal(name) => Ok(name.to_os_string()),
            _ => Err(CrosstacheError::invalid_argument(format!(
                "Atomic destination '{}' contains an unsafe path component",
                absolute.display()
            ))),
        })
        .collect::<Result<Vec<_>>>()?;
    let (file_name, parent_names) = names
        .split_last()
        .ok_or_else(|| CrosstacheError::invalid_argument("Atomic destination must name a file"))?;

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
            CrosstacheError::invalid_argument("Atomic destination contains a NUL byte")
        })?;
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        let mut next_fd = unsafe { libc::openat(directory.as_raw_fd(), c_name.as_ptr(), flags) };
        if next_fd < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
            let mkdir_result =
                unsafe { libc::mkdirat(directory.as_raw_fd(), c_name.as_ptr(), directory_mode) };
            if mkdir_result < 0
                && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err(CrosstacheError::config(format!(
                    "Failed to create atomic directory '{}': {}",
                    name.to_string_lossy(),
                    std::io::Error::last_os_error()
                )));
            }
            directory.sync_all().map_err(|error| {
                CrosstacheError::config(format!(
                    "Failed to sync parent after creating atomic directory '{}': {error}",
                    name.to_string_lossy()
                ))
            })?;
            next_fd = unsafe { libc::openat(directory.as_raw_fd(), c_name.as_ptr(), flags) };
        }
        if next_fd < 0 {
            return Err(CrosstacheError::config(format!(
                "Refusing unsafe atomic path component '{}': {}",
                name.to_string_lossy(),
                std::io::Error::last_os_error()
            )));
        }
        directory = unsafe { std::fs::File::from_raw_fd(next_fd) };
    }

    let destination = CString::new(file_name.as_bytes())
        .map_err(|_| CrosstacheError::invalid_argument("Atomic destination contains a NUL byte"))?;
    Ok(UnixAtomicParent {
        directory,
        destination,
        absolute,
    })
}

#[cfg(unix)]
fn atomic_write_file_no_follow_unix(path: &Path, content: &[u8], private: bool) -> Result<()> {
    use std::ffi::CString;
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd};

    let (file_mode, directory_mode): (FileMode, FileMode) = if private {
        (0o600, 0o700)
    } else {
        (0o666, 0o777)
    };
    let parent = open_unix_atomic_parent(path, directory_mode)?;
    let temp_name = CString::new(format!(".xv-{}.tmp", Uuid::new_v4()))
        .expect("UUID temporary name contains no NUL");
    let mut temp_exists = false;

    let operation = (|| {
        let fd = unsafe {
            libc::openat(
                parent.directory.as_raw_fd(),
                temp_name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                libc::c_uint::from(file_mode),
            )
        };
        if fd < 0 {
            return Err(CrosstacheError::config(format!(
                "Failed to create atomic temporary file for '{}': {}",
                parent.absolute.display(),
                std::io::Error::last_os_error()
            )));
        }
        temp_exists = true;
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        file.write_all(content).map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to write atomic temporary file for '{}': {error}",
                parent.absolute.display()
            ))
        })?;
        file.sync_all().map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to flush atomic temporary file for '{}': {error}",
                parent.absolute.display()
            ))
        })?;

        #[cfg(test)]
        tests::run_atomic_parent_swap_hook(path)?;

        let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
        let inspected = unsafe {
            libc::fstatat(
                parent.directory.as_raw_fd(),
                parent.destination.as_ptr(),
                metadata.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if inspected == 0 {
            let metadata = unsafe { metadata.assume_init() };
            if metadata.st_mode & libc::S_IFMT == libc::S_IFLNK {
                return Err(CrosstacheError::config(format!(
                    "Refusing symlinked destination '{}'",
                    parent.absolute.display()
                )));
            }
        } else if std::io::Error::last_os_error().kind() != std::io::ErrorKind::NotFound {
            return Err(CrosstacheError::config(format!(
                "Failed to inspect atomic destination '{}': {}",
                parent.absolute.display(),
                std::io::Error::last_os_error()
            )));
        }

        // Caller-visible commit point: after renameat succeeds, the previous
        // bytes are no longer recoverable through this API. Any later
        // directory-sync failure can reduce crash durability, but must not be
        // reported as an operation failure after the replacement committed.
        let renamed = unsafe {
            libc::renameat(
                parent.directory.as_raw_fd(),
                temp_name.as_ptr(),
                parent.directory.as_raw_fd(),
                parent.destination.as_ptr(),
            )
        };
        if renamed < 0 {
            return Err(CrosstacheError::config(format!(
                "Failed to atomically replace '{}': {}",
                parent.absolute.display(),
                std::io::Error::last_os_error()
            )));
        }
        temp_exists = false;

        #[cfg(test)]
        let directory_sync = tests::run_atomic_post_rename_sync_hook(path)
            .and_then(|()| parent.directory.sync_all());
        #[cfg(not(test))]
        let directory_sync = parent.directory.sync_all();
        // Best effort only: renameat is the commit point above. Do not leak
        // destination details or claim failure after committed bytes changed.
        let _ = directory_sync;
        Ok(())
    })();

    match operation {
        Err(operation_error) if temp_exists => {
            let removed =
                unsafe { libc::unlinkat(parent.directory.as_raw_fd(), temp_name.as_ptr(), 0) };
            if removed < 0 {
                let cleanup_error = std::io::Error::last_os_error();
                if cleanup_error.kind() != std::io::ErrorKind::NotFound {
                    return Err(CrosstacheError::config(format!(
                        "{operation_error}; additionally failed to remove atomic temporary file for '{}': {cleanup_error}",
                        parent.absolute.display()
                    )));
                }
            } else if let Err(cleanup_error) = parent.directory.sync_all() {
                return Err(CrosstacheError::config(format!(
                    "{operation_error}; removed temporary file but failed to sync cleanup for '{}': {cleanup_error}",
                    parent.absolute.display()
                )));
            }
            Err(operation_error)
        }
        operation => operation,
    }
}

/// Replace an existing private file with one atomic visible-destination
/// operation, preserve the exact displaced file under a no-overwrite backup
/// name, and retain the validated parent for the complete transaction. The
/// returned bytes were read from the anchored destination after the commit.
pub(crate) fn atomic_replace_with_private_backup_no_follow(
    path: &Path,
    backup_name: &std::ffi::OsStr,
    expected: &[u8],
    replacement: &[u8],
) -> Result<Vec<u8>> {
    #[cfg(unix)]
    {
        atomic_replace_with_private_backup_no_follow_unix(path, backup_name, expected, replacement)
    }

    #[cfg(windows)]
    {
        atomic_replace_with_private_backup_no_follow_windows(
            path,
            backup_name,
            expected,
            replacement,
        )
    }

    #[cfg(all(not(unix), not(windows)))]
    {
        let _ = (path, backup_name, expected, replacement);
        Err(CrosstacheError::config(
            "Anchored config backup and replacement is unavailable on this platform",
        ))
    }
}

#[cfg(all(
    unix,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    )
))]
fn unix_exchange_in_parent(
    parent: &UnixAtomicParent,
    first: &std::ffi::CStr,
    second: &std::ffi::CStr,
) -> Result<()> {
    use std::os::fd::AsRawFd;

    #[cfg(any(target_os = "linux", target_os = "android"))]
    let exchanged = unsafe {
        libc::renameat2(
            parent.directory.as_raw_fd(),
            first.as_ptr(),
            parent.directory.as_raw_fd(),
            second.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    let exchanged = unsafe {
        libc::renameatx_np(
            parent.directory.as_raw_fd(),
            first.as_ptr(),
            parent.directory.as_raw_fd(),
            second.as_ptr(),
            libc::RENAME_SWAP,
        )
    };

    if exchanged < 0 {
        return Err(CrosstacheError::config(format!(
            "Failed to atomically exchange repair files for '{}': {}",
            parent.absolute.display(),
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

#[cfg(all(
    unix,
    not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    ))
))]
fn unix_exchange_in_parent(
    parent: &UnixAtomicParent,
    first: &std::ffi::CStr,
    second: &std::ffi::CStr,
) -> Result<()> {
    let _ = (parent, first, second);
    Err(CrosstacheError::config(
        "Atomic file exchange is unavailable on this Unix platform",
    ))
}

#[cfg(all(
    unix,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    )
))]
fn unix_rename_noreplace_in_parent(
    parent: &UnixAtomicParent,
    source: &std::ffi::CStr,
    destination: &std::ffi::CStr,
) -> Result<()> {
    use std::os::fd::AsRawFd;

    #[cfg(any(target_os = "linux", target_os = "android"))]
    let renamed = unsafe {
        libc::renameat2(
            parent.directory.as_raw_fd(),
            source.as_ptr(),
            parent.directory.as_raw_fd(),
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    let renamed = unsafe {
        libc::renameatx_np(
            parent.directory.as_raw_fd(),
            source.as_ptr(),
            parent.directory.as_raw_fd(),
            destination.as_ptr(),
            libc::RENAME_EXCL,
        )
    };

    if renamed < 0 {
        return Err(CrosstacheError::config(format!(
            "Failed to promote displaced config to backup for '{}': {}",
            parent.absolute.display(),
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

#[cfg(all(
    unix,
    not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    ))
))]
fn unix_rename_noreplace_in_parent(
    parent: &UnixAtomicParent,
    source: &std::ffi::CStr,
    destination: &std::ffi::CStr,
) -> Result<()> {
    let _ = (parent, source, destination);
    Err(CrosstacheError::config(
        "Atomic no-replace rename is unavailable on this Unix platform",
    ))
}

#[cfg(all(
    unix,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    )
))]
fn ensure_unix_repair_primitives_available() -> Result<()> {
    Ok(())
}

#[cfg(all(
    unix,
    not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    ))
))]
fn ensure_unix_repair_primitives_available() -> Result<()> {
    Err(CrosstacheError::config(
        "Anchored config backup and replacement is unavailable on this Unix platform",
    ))
}

#[cfg(unix)]
fn atomic_replace_with_private_backup_no_follow_unix(
    path: &Path,
    backup_name: &std::ffi::OsStr,
    expected: &[u8],
    replacement: &[u8],
) -> Result<Vec<u8>> {
    use std::ffi::CString;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    // Unsupported Unix targets must reject the operation before opening a
    // possibly missing parent (which could create directories) or a temporary.
    ensure_unix_repair_primitives_available()?;

    let mut backup_components = Path::new(backup_name).components();
    if !matches!(
        backup_components.next(),
        Some(std::path::Component::Normal(_))
    ) || backup_components.next().is_some()
    {
        return Err(CrosstacheError::invalid_argument(
            "Config backup must be a single file name",
        ));
    }
    let backup_name = CString::new(backup_name.as_bytes())
        .map_err(|_| CrosstacheError::invalid_argument("Config backup name contains a NUL byte"))?;
    let parent = open_unix_atomic_parent(path, 0o700)?;
    let temp_name = CString::new(format!(".xv-repair-{}.tmp", Uuid::new_v4()))
        .expect("UUID temporary name contains no NUL");

    let open_anchored_name =
        |parent: &UnixAtomicParent, name: &std::ffi::CStr, label: &str| -> Result<std::fs::File> {
            let fd = unsafe {
                libc::openat(
                    parent.directory.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(CrosstacheError::config(format!(
                    "Failed to open anchored {label} '{}': {}",
                    parent.absolute.display(),
                    std::io::Error::last_os_error()
                )));
            }
            let file = unsafe { std::fs::File::from_raw_fd(fd) };
            let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
            let inspected = unsafe { libc::fstat(file.as_raw_fd(), metadata.as_mut_ptr()) };
            if inspected < 0 {
                return Err(CrosstacheError::config(format!(
                    "Failed to inspect anchored {label} '{}': {}",
                    parent.absolute.display(),
                    std::io::Error::last_os_error()
                )));
            }
            let metadata = unsafe { metadata.assume_init() };
            if metadata.st_mode & libc::S_IFMT != libc::S_IFREG {
                return Err(CrosstacheError::config(format!(
                    "Refusing non-regular anchored {label} '{}'",
                    parent.absolute.display()
                )));
            }
            Ok(file)
        };
    let open_destination =
        || open_anchored_name(&parent, parent.destination.as_c_str(), "config destination");
    let read_file = |mut file: std::fs::File| -> Result<(Vec<u8>, (u64, u64))> {
        let metadata = file.metadata().map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to inspect anchored config destination '{}': {error}",
                parent.absolute.display()
            ))
        })?;
        let identity = (metadata.dev(), metadata.ino());
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to read anchored config destination '{}': {error}",
                parent.absolute.display()
            ))
        })?;
        Ok((bytes, identity))
    };

    let (initial_bytes, initial_identity) = read_file(open_destination()?)?;
    if initial_bytes != expected {
        return Err(CrosstacheError::config(format!(
            "Configuration file '{}' changed after diagnosis; refusing repair",
            parent.absolute.display()
        )));
    }

    let mut backup_metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    let backup_probe = unsafe {
        libc::fstatat(
            parent.directory.as_raw_fd(),
            backup_name.as_ptr(),
            backup_metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if backup_probe == 0 {
        return Err(CrosstacheError::config(format!(
            "Refusing to overwrite existing config backup for '{}'",
            parent.absolute.display()
        )));
    }
    let backup_probe_error = std::io::Error::last_os_error();
    if backup_probe_error.kind() != std::io::ErrorKind::NotFound {
        return Err(CrosstacheError::config(format!(
            "Failed to verify exclusive config backup path for '{}': {}",
            parent.absolute.display(),
            backup_probe_error
        )));
    }

    let operation = (|| {
        let temp_fd = unsafe {
            libc::openat(
                parent.directory.as_raw_fd(),
                temp_name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if temp_fd < 0 {
            return Err(CrosstacheError::config(format!(
                "Failed to create repair temporary file for '{}': {}",
                parent.absolute.display(),
                std::io::Error::last_os_error()
            )));
        }
        let mut temporary = unsafe { std::fs::File::from_raw_fd(temp_fd) };
        temporary.write_all(replacement).map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to write repair temporary file for '{}': {error}",
                parent.absolute.display()
            ))
        })?;
        temporary.sync_all().map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to flush repair temporary file for '{}': {error}",
                parent.absolute.display()
            ))
        })?;

        #[cfg(test)]
        tests::run_anchored_repair_content_hook(path)?;
        #[cfg(test)]
        tests::run_atomic_parent_swap_hook(path)?;

        let (current_bytes, current_identity) = read_file(open_destination()?)?;
        if current_identity != initial_identity || current_bytes != expected {
            return Err(CrosstacheError::config(format!(
                "Configuration file '{}' changed after diagnosis; refusing repair",
                parent.absolute.display()
            )));
        }

        #[cfg(test)]
        tests::run_anchored_repair_commit_hook(path)?;

        // This is the only operation that changes the visible destination.
        // The exact file displaced by it moves to the private temporary name.
        unix_exchange_in_parent(&parent, parent.destination.as_c_str(), temp_name.as_c_str())?;
        #[cfg(test)]
        tests::run_anchored_repair_post_publish_hooks(path)?;

        let displaced_file = open_anchored_name(&parent, temp_name.as_c_str(), "displaced config")?;
        displaced_file
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|error| {
                CrosstacheError::config(format!(
                    "Failed to restrict displaced config for '{}': {error}; both versions were preserved",
                    parent.absolute.display()
                ))
            })?;
        displaced_file.sync_all().map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to flush displaced config for '{}': {error}; both versions were preserved",
                parent.absolute.display()
            ))
        })?;
        drop(displaced_file);

        #[cfg(test)]
        tests::run_anchored_repair_displaced_artifact_hook(
            path,
            &parent
                .absolute
                .with_file_name(std::ffi::OsStr::from_bytes(temp_name.as_bytes())),
        )?;

        // Promotion consumes the displaced temporary name and refuses a
        // concurrently created backup. No cleanup unlink is ever needed.
        unix_rename_noreplace_in_parent(&parent, temp_name.as_c_str(), backup_name.as_c_str())
            .map_err(|error| {
                CrosstacheError::config(format!(
                    "{error}; repaired and displaced versions were preserved under separate names"
                ))
            })?;
        let _ = parent.directory.sync_all();

        #[cfg(test)]
        tests::run_anchored_repair_backup_artifact_hook(
            path,
            &parent
                .absolute
                .with_file_name(std::ffi::OsStr::from_bytes(backup_name.as_bytes())),
        )?;

        let displaced = read_file(open_anchored_name(
            &parent,
            backup_name.as_c_str(),
            "captured config backup",
        )?)?;
        let (verified, _) = read_file(open_destination()?)?;
        if displaced.1 != initial_identity || displaced.0 != expected || verified != replacement {
            return Err(CrosstacheError::config(format!(
                "Configuration file '{}' changed during repair; repaired, displaced, and concurrent versions were preserved",
                parent.absolute.display()
            )));
        }
        Ok(verified)
    })();
    operation
}

pub(crate) async fn atomic_replace_with_private_backup_no_follow_async(
    path: &Path,
    backup_name: &std::ffi::OsStr,
    expected: &[u8],
    replacement: &[u8],
) -> Result<Vec<u8>> {
    let path = path.to_path_buf();
    let backup_name = backup_name.to_os_string();
    let expected = expected.to_vec();
    let replacement = replacement.to_vec();
    tokio::task::spawn_blocking(move || {
        atomic_replace_with_private_backup_no_follow(&path, &backup_name, &expected, &replacement)
    })
    .await
    .map_err(|error| CrosstacheError::config(format!("Anchored repair task failed: {error}")))?
}

#[cfg(windows)]
struct WindowsSecurityDescriptor(windows_sys::Win32::Security::PSECURITY_DESCRIPTOR);

#[cfg(windows)]
impl Drop for WindowsSecurityDescriptor {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::LocalFree(self.0.cast());
        }
    }
}

#[cfg(windows)]
fn windows_private_security_descriptor() -> Result<WindowsSecurityDescriptor> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };

    // Protected DACL: full control belongs only to the object owner and the
    // Windows SYSTEM account. The token's default owner is the current user.
    let sddl: Vec<u16> = std::ffi::OsStr::new("D:P(A;;FA;;;OW)(A;;FA;;;SY)")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut descriptor = std::ptr::null_mut();
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    };
    if converted == 0 {
        return Err(CrosstacheError::config(format!(
            "Failed to create private Windows security descriptor: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(WindowsSecurityDescriptor(descriptor))
}

#[cfg(windows)]
fn windows_security_attributes(
    descriptor: Option<&WindowsSecurityDescriptor>,
) -> windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
    windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<windows_sys::Win32::Security::SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor
            .map(|descriptor| descriptor.0)
            .unwrap_or(std::ptr::null_mut()),
        bInheritHandle: 0,
    }
}

#[cfg(windows)]
fn windows_wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(windows)]
fn windows_create_file(
    path: &Path,
    access: u32,
    share: u32,
    creation: u32,
    flags: u32,
    security: *const windows_sys::Win32::Security::SECURITY_ATTRIBUTES,
) -> std::io::Result<std::fs::File> {
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::CreateFileW;

    let wide = windows_wide(path);
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            access,
            share,
            security,
            creation,
            flags,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { std::fs::File::from_raw_handle(handle.cast()) })
}

#[cfg(windows)]
fn windows_file_attributes(file: &std::fs::File) -> std::io::Result<u32> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    let inspected =
        unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut information) };
    if inspected == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(information.dwFileAttributes)
}

#[cfg(windows)]
fn windows_file_identity(file: &std::fs::File) -> std::io::Result<(u32, u32, u32)> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    let inspected =
        unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut information) };
    if inspected == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((
        information.dwVolumeSerialNumber,
        information.nFileIndexHigh,
        information.nFileIndexLow,
    ))
}

#[cfg(windows)]
fn windows_security_descriptor_dacl(
    descriptor: windows_sys::Win32::Security::PSECURITY_DESCRIPTOR,
    label: &str,
) -> Result<*mut windows_sys::Win32::Security::ACL> {
    use windows_sys::Win32::Security::GetSecurityDescriptorDacl;

    let mut present = 0;
    let mut defaulted = 0;
    let mut dacl = std::ptr::null_mut();
    let inspected =
        unsafe { GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted) };
    if inspected == 0 {
        return Err(CrosstacheError::config(format!(
            "Failed to inspect {label} Windows DACL: {}",
            std::io::Error::last_os_error()
        )));
    }
    if present == 0 || dacl.is_null() {
        return Err(CrosstacheError::config(format!(
            "Refusing missing or null {label} Windows DACL"
        )));
    }
    Ok(dacl)
}

#[cfg(windows)]
fn windows_acl_bytes(
    dacl: *const windows_sys::Win32::Security::ACL,
    label: &str,
) -> Result<Vec<u8>> {
    use windows_sys::Win32::Security::{
        AclSizeInformation, GetAclInformation, ACL_SIZE_INFORMATION,
    };

    let mut information = ACL_SIZE_INFORMATION::default();
    let inspected = unsafe {
        GetAclInformation(
            dacl,
            (&mut information as *mut ACL_SIZE_INFORMATION).cast(),
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    };
    if inspected == 0 {
        return Err(CrosstacheError::config(format!(
            "Failed to inspect {label} Windows ACL: {}",
            std::io::Error::last_os_error()
        )));
    }
    if information.AclBytesInUse < std::mem::size_of::<windows_sys::Win32::Security::ACL>() as u32 {
        return Err(CrosstacheError::config(format!(
            "Refusing malformed {label} Windows ACL"
        )));
    }
    Ok(unsafe {
        std::slice::from_raw_parts(dacl.cast::<u8>(), information.AclBytesInUse as usize).to_vec()
    })
}

#[cfg(windows)]
fn windows_apply_and_verify_private_dacl(
    file: &std::fs::File,
    private_descriptor: &WindowsSecurityDescriptor,
    path: &Path,
    label: &str,
) -> Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::Security::Authorization::{
        GetSecurityInfo, SetSecurityInfo, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{
        GetSecurityDescriptorControl, DACL_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, SE_DACL_PROTECTED,
    };

    let expected_dacl = windows_security_descriptor_dacl(private_descriptor.0, "expected private")?;
    let applied = unsafe {
        SetSecurityInfo(
            file.as_raw_handle().cast(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            expected_dacl,
            std::ptr::null_mut(),
        )
    };
    if applied != ERROR_SUCCESS {
        return Err(CrosstacheError::config(format!(
            "Failed to apply a private DACL to the Windows {label} '{}': {}",
            path.display(),
            std::io::Error::from_raw_os_error(applied as i32)
        )));
    }

    let mut actual_dacl = std::ptr::null_mut();
    let mut actual_descriptor = std::ptr::null_mut();
    let queried = unsafe {
        GetSecurityInfo(
            file.as_raw_handle().cast(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut actual_dacl,
            std::ptr::null_mut(),
            &mut actual_descriptor,
        )
    };
    if queried != ERROR_SUCCESS {
        return Err(CrosstacheError::config(format!(
            "Failed to verify the Windows {label} DACL for '{}': {}",
            path.display(),
            std::io::Error::from_raw_os_error(queried as i32)
        )));
    }
    if actual_descriptor.is_null() || actual_dacl.is_null() {
        if !actual_descriptor.is_null() {
            unsafe {
                windows_sys::Win32::Foundation::LocalFree(actual_descriptor.cast());
            }
        }
        return Err(CrosstacheError::config(format!(
            "Refusing an unverifiable Windows {label} DACL for '{}'",
            path.display()
        )));
    }
    let actual_descriptor = WindowsSecurityDescriptor(actual_descriptor);
    let mut control = 0;
    let mut revision = 0;
    let inspected =
        unsafe { GetSecurityDescriptorControl(actual_descriptor.0, &mut control, &mut revision) };
    if inspected == 0 {
        return Err(CrosstacheError::config(format!(
            "Failed to inspect Windows {label} DACL protection for '{}': {}",
            path.display(),
            std::io::Error::last_os_error()
        )));
    }
    if control & SE_DACL_PROTECTED == 0 {
        return Err(CrosstacheError::config(format!(
            "Windows {label} DACL for '{}' is not protected",
            path.display()
        )));
    }

    let expected = windows_acl_bytes(expected_dacl, "expected private")?;
    let actual = windows_acl_bytes(actual_dacl, label)?;
    if actual != expected {
        return Err(CrosstacheError::config(format!(
            "Windows {label} DACL for '{}' does not match the required owner/SYSTEM ACL",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(windows)]
struct WindowsAtomicParent {
    // Holding every traversed component without write/delete sharing prevents
    // an ancestor from becoming a reparse point or being renamed/replaced
    // after validation.
    _chain: Vec<std::fs::File>,
    // Retention-only, like `_chain`: pins the immediate parent for the lifetime
    // of the write so the validated directory cannot be swapped out from under
    // the rename. Never read — the rename addresses its target by full path
    // because Win32 has no handle-relative rename (see
    // `windows_rename_into_parent`).
    _directory: std::fs::File,
    absolute: PathBuf,
}

#[cfg(windows)]
fn open_windows_atomic_parent(
    path: &Path,
    private_security: Option<&WindowsSecurityDescriptor>,
) -> Result<WindowsAtomicParent> {
    use std::path::Component;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateDirectoryW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY,
        FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE,
        OPEN_EXISTING,
    };

    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| {
                CrosstacheError::config(format!("Cannot resolve current directory: {error}"))
            })?
            .join(path)
    };
    // Validated for its own sake — the rename uses the full path, but a
    // destination that names no file is still a caller error.
    absolute
        .file_name()
        .ok_or_else(|| CrosstacheError::invalid_argument("Atomic destination must name a file"))?;
    let parent_path = absolute.parent().ok_or_else(|| {
        CrosstacheError::invalid_argument("Atomic destination must have a parent directory")
    })?;
    let security_attributes = windows_security_attributes(private_security);
    let security_pointer = if private_security.is_some() {
        &security_attributes
    } else {
        std::ptr::null()
    };
    let mut current = PathBuf::new();
    let mut chain = Vec::new();

    // The immediate parent needs write/delete sharing; every ancestor above it
    // stays locked down. Replacing an entry inside a directory makes the kernel
    // open that directory for write, so a `FILE_SHARE_READ`-only handle on it
    // makes our *own* rename fail with ERROR_SHARING_VIOLATION (os error 32) —
    // confirmed against both `SetFileInformationByHandle` and the NT-level
    // `NtSetInformationFile` (STATUS_SHARING_VIOLATION, 0xC0000043), so it is
    // inherent to the operation rather than an artifact of either entry point.
    // Ancestors are unaffected: renaming this directory would require opening
    // *it* for delete, which its own retained parent still refuses.
    let components: Vec<Component> = parent_path.components().collect();
    let last_component = components.len().saturating_sub(1);

    for (index, component) in components.into_iter().enumerate() {
        let share = if index == last_component {
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
        } else {
            FILE_SHARE_READ
        };
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                current.push(component.as_os_str());
            }
            Component::CurDir | Component::ParentDir => {
                return Err(CrosstacheError::invalid_argument(format!(
                    "Atomic destination '{}' contains an unsafe path component",
                    absolute.display()
                )));
            }
        }
        if matches!(component, Component::Prefix(_)) {
            continue;
        }

        let access = FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | FILE_TRAVERSE;
        let flags = FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT;
        let file = match windows_create_file(
            &current,
            access,
            share,
            OPEN_EXISTING,
            flags,
            std::ptr::null(),
        ) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let wide = windows_wide(&current);
                let created = unsafe { CreateDirectoryW(wide.as_ptr(), security_pointer) };
                if created == 0
                    && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
                {
                    return Err(CrosstacheError::config(format!(
                        "Failed to create private Windows directory '{}': {}",
                        current.display(),
                        std::io::Error::last_os_error()
                    )));
                }
                windows_create_file(
                    &current,
                    access,
                    share,
                    OPEN_EXISTING,
                    flags,
                    std::ptr::null(),
                )
                .map_err(|error| {
                    CrosstacheError::config(format!(
                        "Failed to retain Windows directory '{}': {error}",
                        current.display()
                    ))
                })?
            }
            Err(error) => {
                return Err(CrosstacheError::config(format!(
                    "Failed to retain Windows directory '{}': {error}",
                    current.display()
                )));
            }
        };
        let attributes = windows_file_attributes(&file).map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to inspect Windows directory '{}': {error}",
                current.display()
            ))
        })?;
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(CrosstacheError::config(format!(
                "Refusing Windows reparse-point path component '{}'",
                current.display()
            )));
        }
        if attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
            return Err(CrosstacheError::config(format!(
                "Atomic path component '{}' is not a directory",
                current.display()
            )));
        }
        chain.push(file);
    }

    let directory = chain.pop().ok_or_else(|| {
        CrosstacheError::invalid_argument("Atomic destination must have a retained parent")
    })?;
    Ok(WindowsAtomicParent {
        _chain: chain,
        _directory: directory,
        absolute,
    })
}

/// Byte size to allocate and report for a `FILE_RENAME_INFO` carrying a name
/// of `name_bytes` bytes.
///
/// `sizeof(FILE_RENAME_INFO) + FileNameLength` is the size the API documents.
/// Because the struct's trailing `FileName: [u16; 1]` and its padding are
/// already counted by `size_of`, the result always leaves at least one zero
/// `u16` past the copied name, which supplies the terminator for free given a
/// zeroed buffer.
#[cfg(windows)]
fn windows_rename_info_size(name_bytes: usize) -> usize {
    use windows_sys::Win32::Storage::FileSystem::FILE_RENAME_INFO;

    std::mem::size_of::<FILE_RENAME_INFO>() + name_bytes
}

/// Atomically replace `destination` with `file`.
///
/// `destination` must be the **full** path. `RootDirectory` is deliberately
/// left NULL: Win32's `SetFileInformationByHandle` does not implement the
/// handle-relative form of `FileRenameInfo`, and rejects every non-NULL
/// `RootDirectory` with ERROR_INVALID_PARAMETER (os error 87) — independent of
/// buffer sizing, of the share mode the directory handle was opened with, and
/// of whether the destination already exists. Only the NT-level
/// `NtSetInformationFile` honours that field. This is why the config write,
/// `xv context use`, and the web UI's `ui.json` all failed on Windows.
///
/// The retained parent handle in [`WindowsAtomicParent`] still pins the
/// directory for the lifetime of the operation, so re-resolving the path here
/// does not reopen a window an attacker can swap the parent out of.
#[cfg(windows)]
fn windows_rename_into_parent(
    file: &std::fs::File,
    destination: &std::ffi::OsStr,
) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FileRenameInfo, SetFileInformationByHandle, FILE_RENAME_INFO,
    };

    // Buffer sizing follows the documented contract for FileRenameInfo:
    // `sizeof(FILE_RENAME_INFO) + FileNameLength`, with `FileNameLength`
    // counting the name only (no terminator) while the buffer itself leaves
    // room for one.
    let name: Vec<u16> = destination.encode_wide().collect();
    let name_bytes = name.len() * std::mem::size_of::<u16>();
    let name_offset = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
    let byte_length = windows_rename_info_size(name_bytes);
    let word_length = byte_length.div_ceil(std::mem::size_of::<usize>());
    let mut buffer = vec![0usize; word_length];
    let information = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    unsafe {
        // Write only the 1-byte `ReplaceIfExists` arm of the union rather than
        // assigning the whole `FILE_RENAME_INFO_0`: constructing that union
        // from a single `bool` field leaves its other three bytes
        // uninitialized, and assigning it copies all four into the buffer.
        (*information).Anonymous.ReplaceIfExists = true;
        (*information).RootDirectory = std::ptr::null_mut();
        (*information).FileNameLength = name_bytes as u32;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            information.cast::<u8>().add(name_offset).cast::<u16>(),
            name.len(),
        );
    }
    let renamed = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle().cast(),
            FileRenameInfo,
            information.cast(),
            byte_length as u32,
        )
    };
    if renamed == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
fn windows_delete_by_handle(file: &std::fs::File) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FileDispositionInfo, SetFileInformationByHandle, FILE_DISPOSITION_INFO,
    };

    let information = FILE_DISPOSITION_INFO { DeleteFile: true };
    let deleted = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle().cast(),
            FileDispositionInfo,
            (&information as *const FILE_DISPOSITION_INFO).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    };
    if deleted == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
fn atomic_write_file_no_follow_windows(path: &Path, content: &[u8], private: bool) -> Result<()> {
    use std::io::Write;
    use windows_sys::Win32::Storage::FileSystem::{
        CREATE_NEW, DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_FLAG_WRITE_THROUGH, FILE_GENERIC_WRITE, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    let private_descriptor = if private {
        Some(windows_private_security_descriptor()?)
    } else {
        None
    };
    let parent = open_windows_atomic_parent(path, private_descriptor.as_ref())?;
    let security_attributes = windows_security_attributes(private_descriptor.as_ref());
    let security_pointer = if private_descriptor.is_some() {
        &security_attributes
    } else {
        std::ptr::null()
    };
    let temp_path = parent
        .absolute
        .parent()
        .expect("retained parent path exists")
        .join(format!(".xv-{}.tmp", Uuid::new_v4()));
    let mut temporary = windows_create_file(
        &temp_path,
        FILE_GENERIC_WRITE | FILE_READ_ATTRIBUTES | DELETE,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        CREATE_NEW,
        FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_WRITE_THROUGH,
        security_pointer,
    )
    .map_err(|error| {
        CrosstacheError::config(format!(
            "Failed to create private Windows temporary file for '{}': {error}",
            parent.absolute.display()
        ))
    })?;

    let operation = (|| {
        temporary.write_all(content).map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to write Windows temporary file for '{}': {error}",
                parent.absolute.display()
            ))
        })?;
        temporary.sync_all().map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to flush Windows temporary file for '{}': {error}",
                parent.absolute.display()
            ))
        })?;

        match windows_create_file(
            &parent.absolute,
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null(),
        ) {
            Ok(destination) => {
                let attributes = windows_file_attributes(&destination).map_err(|error| {
                    CrosstacheError::config(format!(
                        "Failed to inspect Windows destination '{}': {error}",
                        parent.absolute.display()
                    ))
                })?;
                if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                    return Err(CrosstacheError::config(format!(
                        "Refusing Windows reparse-point destination '{}'",
                        parent.absolute.display()
                    )));
                }
                if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
                    return Err(CrosstacheError::config(format!(
                        "Refusing Windows directory destination '{}'",
                        parent.absolute.display()
                    )));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(CrosstacheError::config(format!(
                    "Failed to inspect Windows destination '{}': {error}",
                    parent.absolute.display()
                )));
            }
        }

        // Caller-visible Windows commit point. No fallible operation is
        // performed after the handle-relative rename succeeds.
        windows_rename_into_parent(&temporary, parent.absolute.as_os_str()).map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to atomically replace Windows destination '{}': {error}",
                parent.absolute.display()
            ))
        })
    })();

    if let Err(operation_error) = operation {
        if let Err(cleanup_error) = windows_delete_by_handle(&temporary) {
            return Err(CrosstacheError::config(format!(
                "{operation_error}; additionally failed to delete Windows temporary file by handle for '{}': {cleanup_error}",
                parent.absolute.display()
            )));
        }
        return Err(operation_error);
    }
    Ok(())
}

#[cfg(windows)]
fn atomic_replace_with_private_backup_no_follow_windows(
    path: &Path,
    backup_name: &std::ffi::OsStr,
    expected: &[u8],
    replacement: &[u8],
) -> Result<Vec<u8>> {
    use std::io::{Read, Seek, Write};
    use std::path::Component;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, ReplaceFileW, CREATE_NEW, DELETE, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_FLAG_WRITE_THROUGH, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_READ_ATTRIBUTES,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, MOVEFILE_WRITE_THROUGH,
        OPEN_EXISTING, READ_CONTROL, WRITE_DAC,
    };

    let mut components = Path::new(backup_name).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err(CrosstacheError::invalid_argument(
            "Config backup must be a single file name",
        ));
    }
    let private_descriptor = windows_private_security_descriptor()?;
    let parent = open_windows_atomic_parent(path, Some(&private_descriptor))?;
    let directory_path = parent
        .absolute
        .parent()
        .expect("retained Windows parent path exists");
    let backup_path = directory_path.join(backup_name);
    let temporary_path = directory_path.join(format!(".xv-repair-{}.tmp", Uuid::new_v4()));
    let displaced_path = directory_path.join(format!(".xv-displaced-{}.backup", Uuid::new_v4()));
    let security_attributes = windows_security_attributes(Some(&private_descriptor));

    let read_regular = |target: &Path, label: &str| -> Result<(Vec<u8>, (u32, u32, u32))> {
        let mut file = windows_create_file(
            target,
            FILE_GENERIC_READ | FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null(),
        )
        .map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to open Windows {label} '{}': {error}",
                parent.absolute.display()
            ))
        })?;
        let attributes = windows_file_attributes(&file).map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to inspect Windows {label} '{}': {error}",
                parent.absolute.display()
            ))
        })?;
        if attributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DIRECTORY) != 0 {
            return Err(CrosstacheError::config(format!(
                "Refusing unsafe Windows {label} '{}'",
                parent.absolute.display()
            )));
        }
        let identity = windows_file_identity(&file).map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to identify Windows {label} '{}': {error}",
                parent.absolute.display()
            ))
        })?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to read Windows {label} '{}': {error}",
                parent.absolute.display()
            ))
        })?;
        Ok((bytes, identity))
    };

    let initial = read_regular(&parent.absolute, "config destination")?;
    if initial.0 != expected {
        return Err(CrosstacheError::config(format!(
            "Configuration file '{}' changed after diagnosis; refusing repair",
            parent.absolute.display()
        )));
    }

    match std::fs::symlink_metadata(&backup_path) {
        Ok(_) => {
            return Err(CrosstacheError::config(format!(
                "Refusing to overwrite existing Windows config backup for '{}'",
                parent.absolute.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(CrosstacheError::config(format!(
                "Failed to verify Windows config backup path for '{}': {error}",
                parent.absolute.display()
            )));
        }
    }

    let prepare = (|| -> Result<(u32, u32, u32)> {
        let mut temporary = windows_create_file(
            &temporary_path,
            FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_READ_ATTRIBUTES | DELETE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_WRITE_THROUGH,
            &security_attributes,
        )
        .map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to create Windows repair temporary file for '{}': {error}",
                parent.absolute.display()
            ))
        })?;
        temporary.write_all(replacement).map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to write Windows repair temporary file for '{}': {error}",
                parent.absolute.display()
            ))
        })?;
        temporary.sync_all().map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to flush Windows repair temporary file for '{}': {error}",
                parent.absolute.display()
            ))
        })?;
        let replacement_identity = windows_file_identity(&temporary).map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to identify Windows repair temporary for '{}': {error}",
                parent.absolute.display()
            ))
        })?;

        let current = read_regular(&parent.absolute, "config destination")?;
        if current.1 != initial.1 || current.0 != expected {
            return Err(CrosstacheError::config(format!(
                "Configuration file '{}' changed after diagnosis; refusing repair",
                parent.absolute.display()
            )));
        }
        Ok(replacement_identity)
    })();
    let replacement_identity = prepare.map_err(|error| {
        CrosstacheError::config(format!(
            "{error}; the private Windows repair temporary was preserved"
        ))
    })?;

    let destination_wide = windows_wide(&parent.absolute);
    let temporary_wide = windows_wide(&temporary_path);
    let displaced_wide = windows_wide(&displaced_path);
    let backup_wide = windows_wide(&backup_path);

    #[cfg(test)]
    tests::run_anchored_repair_commit_hook(path)?;
    #[cfg(test)]
    if tests::take_windows_replace_failure(path) {
        return Err(CrosstacheError::config(format!(
            "Injected Windows replacement failure for '{}'; the destination and repair temporary were preserved",
            parent.absolute.display()
        )));
    }

    let replaced = unsafe {
        ReplaceFileW(
            destination_wide.as_ptr(),
            temporary_wide.as_ptr(),
            displaced_wide.as_ptr(),
            0,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if replaced == 0 {
        let error = std::io::Error::last_os_error();
        return Err(CrosstacheError::config(format!(
            "Failed to atomically replace Windows config '{}': {error}; all ambiguous transaction artifacts were preserved",
            parent.absolute.display(),
        )));
    }

    let mut published = (|| -> Result<std::fs::File> {
        let published = windows_create_file(
            &parent.absolute,
            FILE_GENERIC_READ | FILE_READ_ATTRIBUTES | READ_CONTROL | WRITE_DAC,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null(),
        )
        .map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to reopen repaired Windows config destination '{}': {error}",
                parent.absolute.display()
            ))
        })?;
        let attributes = windows_file_attributes(&published).map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to inspect repaired Windows config destination '{}': {error}",
                parent.absolute.display()
            ))
        })?;
        if attributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DIRECTORY) != 0 {
            return Err(CrosstacheError::config(format!(
                "Refusing unsafe repaired Windows config destination '{}'",
                parent.absolute.display()
            )));
        }
        let identity = windows_file_identity(&published).map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to identify repaired Windows config destination '{}': {error}",
                parent.absolute.display()
            ))
        })?;
        if identity != replacement_identity {
            return Err(CrosstacheError::config(format!(
                "Repaired Windows config destination '{}' changed before its DACL could be secured",
                parent.absolute.display()
            )));
        }
        windows_apply_and_verify_private_dacl(
            &published,
            &private_descriptor,
            &parent.absolute,
            "repaired config destination",
        )?;
        Ok(published)
    })()
    .map_err(|error| {
        CrosstacheError::config(format!(
            "{error}; the repaired destination and displaced Windows config were preserved"
        ))
    })?;

    let secure_backup = (|| -> Result<()> {
        let backup = windows_create_file(
            &displaced_path,
            FILE_GENERIC_READ
                | FILE_GENERIC_WRITE
                | FILE_READ_ATTRIBUTES
                | READ_CONTROL
                | WRITE_DAC,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_WRITE_THROUGH,
            std::ptr::null(),
        )
        .map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to reopen Windows config backup for '{}': {error}",
                parent.absolute.display()
            ))
        })?;
        let attributes = windows_file_attributes(&backup).map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to inspect captured Windows config backup for '{}': {error}",
                parent.absolute.display()
            ))
        })?;
        if attributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DIRECTORY) != 0 {
            return Err(CrosstacheError::config(format!(
                "Refusing unsafe captured Windows config backup for '{}'",
                parent.absolute.display()
            )));
        }
        let identity = windows_file_identity(&backup).map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to identify captured Windows config backup for '{}': {error}",
                parent.absolute.display()
            ))
        })?;
        if identity != initial.1 {
            return Err(CrosstacheError::config(format!(
                "Captured Windows config backup for '{}' changed before its DACL could be secured",
                parent.absolute.display()
            )));
        }
        windows_apply_and_verify_private_dacl(
            &backup,
            &private_descriptor,
            &displaced_path,
            "captured config backup",
        )?;
        backup.sync_all().map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to flush captured Windows config backup for '{}': {error}",
                parent.absolute.display()
            ))
        })?;
        Ok(())
    })();
    if let Err(error) = secure_backup {
        return Err(CrosstacheError::config(format!(
            "{error}; the repaired destination and displaced Windows config were preserved"
        )));
    }

    #[cfg(test)]
    tests::run_windows_backup_conflict_hook(path, &backup_path)?;

    let promoted = unsafe {
        MoveFileExW(
            displaced_wide.as_ptr(),
            backup_wide.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    };
    if promoted == 0 {
        return Err(CrosstacheError::config(format!(
            "Failed to promote displaced Windows config to backup for '{}': {}; repaired, displaced, and conflicting versions were preserved",
            parent.absolute.display(),
            std::io::Error::last_os_error()
        )));
    }

    let captured = read_regular(&backup_path, "captured config backup")?;
    let final_identity = windows_file_identity(&published).map_err(|error| {
        CrosstacheError::config(format!(
            "Failed to re-identify repaired Windows config destination '{}': {error}",
            parent.absolute.display()
        ))
    })?;
    published
        .seek(std::io::SeekFrom::Start(0))
        .map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to rewind repaired Windows config destination '{}': {error}",
                parent.absolute.display()
            ))
        })?;
    let mut verified = Vec::new();
    published.read_to_end(&mut verified).map_err(|error| {
        CrosstacheError::config(format!(
            "Failed to verify repaired Windows config destination '{}': {error}",
            parent.absolute.display()
        ))
    })?;
    if captured.1 != initial.1
        || captured.0 != expected
        || final_identity != replacement_identity
        || verified != replacement
    {
        return Err(CrosstacheError::config(format!(
            "Configuration file '{}' changed during Windows repair; repaired, displaced, and concurrent versions were preserved",
            parent.absolute.display()
        )));
    }
    Ok(verified)
}

/// Atomically replace a file while refusing unsafe path components and final
/// links. The temporary file, replacement, directory sync, and error cleanup
/// remain anchored to one retained destination-parent handle.
pub fn atomic_write_file_no_follow(path: &Path, content: &[u8], private: bool) -> Result<()> {
    #[cfg(unix)]
    {
        atomic_write_file_no_follow_unix(path, content, private)
    }

    #[cfg(windows)]
    {
        atomic_write_file_no_follow_windows(path, content, private)
    }

    #[cfg(all(not(unix), not(windows)))]
    {
        let parent = path.parent().ok_or_else(|| {
            CrosstacheError::invalid_argument("Atomic destination must have a parent directory")
        })?;
        let file_name = path.file_name().ok_or_else(|| {
            CrosstacheError::invalid_argument("Atomic destination must name a file")
        })?;
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
            file.sync_all().map_err(|error| {
                CrosstacheError::config(format!(
                    "Failed to flush temporary file '{}': {error}",
                    temp_path.display()
                ))
            })?;
            std::fs::rename(&temp_path, path).map_err(|error| {
                CrosstacheError::config(format!(
                    "Failed to atomically replace '{}': {error}",
                    path.display()
                ))
            })
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp_path);
        }
        result
    }
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

#[cfg(all(test, windows))]
mod windows_rename_tests {
    use super::*;
    use windows_sys::Win32::Storage::FileSystem::FILE_RENAME_INFO;

    /// Keeps the buffer at the documented `sizeof(FILE_RENAME_INFO) +
    /// FileNameLength`, which always leaves room for a terminator past the
    /// copied name.
    ///
    /// Note this sizing was NOT what broke atomic replace on Windows, despite
    /// what an earlier fix concluded: `SetFileInformationByHandle` returns
    /// ERROR_INVALID_PARAMETER (os error 87) for a non-NULL `RootDirectory`
    /// whatever the buffer length, and succeeds at either length once
    /// `RootDirectory` is NULL. Sizing from `offset_of(FileName)` is merely
    /// off-contract, not the fault. See `windows_rename_into_parent`.
    #[test]
    fn rename_info_buffer_has_room_for_the_name_and_a_terminator() {
        let offset = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
        for name_bytes in [2usize, 14, 24, 512] {
            let size = windows_rename_info_size(name_bytes);
            assert_eq!(
                size,
                std::mem::size_of::<FILE_RENAME_INFO>() + name_bytes,
                "must match the documented sizeof + FileNameLength contract"
            );
            assert!(
                size >= offset + name_bytes + std::mem::size_of::<u16>(),
                "buffer of {size} leaves no terminator after a {name_bytes}-byte name \
                 at offset {offset}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_file_no_follow_reads_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("xv.conf");
        std::fs::write(&path, b"debug = false\n").unwrap();
        assert_eq!(read_file_no_follow(&path).unwrap(), b"debug = false\n");
    }

    #[cfg(unix)]
    #[test]
    fn read_file_no_follow_rejects_final_symlink() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let link = dir.path().join("xv.conf");
        std::fs::write(&target, b"secret").unwrap();
        symlink(&target, &link).unwrap();
        assert!(read_file_no_follow(&link).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn read_file_no_follow_rejects_fifo_without_blocking() {
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("xv.conf");
        let fifo = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);

        let worker_path = path.clone();
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            sender.send(read_file_no_follow(&worker_path)).unwrap();
        });

        let result = receive_bounded_fifo_probe(receiver, worker, &path, "initial config read");
        let error = result.unwrap_err();
        assert!(error.to_string().contains("non-regular"), "{error}");
    }

    #[cfg(unix)]
    fn receive_bounded_fifo_probe<T: std::fmt::Debug>(
        receiver: std::sync::mpsc::Receiver<T>,
        worker: std::thread::JoinHandle<()>,
        fifo_path: &Path,
        label: &str,
    ) -> T {
        use std::os::unix::fs::OpenOptionsExt;

        match receiver.recv_timeout(std::time::Duration::from_secs(1)) {
            Ok(result) => {
                worker.join().unwrap();
                result
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let unblock = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(fifo_path)
                    .unwrap();
                drop(unblock);
                let delayed = receiver
                    .recv_timeout(std::time::Duration::from_secs(1))
                    .expect("blocked FIFO probe did not finish after it was unblocked");
                worker.join().unwrap();
                panic!("{label} blocked on FIFO: {delayed:?}");
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                worker.join().unwrap();
                panic!("{label} worker disconnected");
            }
        }
    }

    #[test]
    fn private_create_new_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("xv.conf.backup-fixed");
        write_private_file_no_follow_create_new(&path, b"original").unwrap();
        assert!(write_private_file_no_follow_create_new(&path, b"replacement").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
    }

    #[cfg(unix)]
    #[test]
    fn private_create_new_uses_mode_0600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("backup");
        write_private_file_no_follow_create_new(&path, b"original").unwrap();
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    fn atomic_parent_swap_hooks(
    ) -> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, PathBuf>> {
        static HOOKS: std::sync::OnceLock<
            std::sync::Mutex<std::collections::HashMap<PathBuf, PathBuf>>,
        > = std::sync::OnceLock::new();
        HOOKS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
    }

    #[cfg(unix)]
    fn install_atomic_parent_swap(path: &Path, parked_parent: &Path) {
        atomic_parent_swap_hooks()
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), parked_parent.to_path_buf());
    }

    #[cfg(unix)]
    enum AnchoredRepairDestinationChange {
        Content(Vec<u8>),
        Fifo { saved_path: PathBuf },
    }

    #[cfg(unix)]
    fn anchored_repair_content_hooks() -> &'static std::sync::Mutex<
        std::collections::HashMap<PathBuf, AnchoredRepairDestinationChange>,
    > {
        static HOOKS: std::sync::OnceLock<
            std::sync::Mutex<std::collections::HashMap<PathBuf, AnchoredRepairDestinationChange>>,
        > = std::sync::OnceLock::new();
        HOOKS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
    }

    #[cfg(unix)]
    fn install_anchored_repair_content_change(path: &Path, content: &[u8]) {
        anchored_repair_content_hooks().lock().unwrap().insert(
            path.to_path_buf(),
            AnchoredRepairDestinationChange::Content(content.to_vec()),
        );
    }

    #[cfg(unix)]
    fn install_anchored_repair_fifo_change(path: &Path, saved_path: &Path) {
        anchored_repair_content_hooks().lock().unwrap().insert(
            path.to_path_buf(),
            AnchoredRepairDestinationChange::Fifo {
                saved_path: saved_path.to_path_buf(),
            },
        );
    }

    #[cfg(any(unix, windows))]
    fn anchored_repair_commit_hooks(
    ) -> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, Vec<u8>>> {
        static HOOKS: std::sync::OnceLock<
            std::sync::Mutex<std::collections::HashMap<PathBuf, Vec<u8>>>,
        > = std::sync::OnceLock::new();
        HOOKS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
    }

    #[cfg(any(unix, windows))]
    fn install_anchored_repair_commit_change(path: &Path, content: &[u8]) {
        anchored_repair_commit_hooks()
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), content.to_vec());
    }

    #[cfg(unix)]
    pub(super) fn run_anchored_repair_content_hook(path: &Path) -> Result<()> {
        use std::os::unix::ffi::OsStrExt;

        let Some(change) = anchored_repair_content_hooks().lock().unwrap().remove(path) else {
            return Ok(());
        };
        match change {
            AnchoredRepairDestinationChange::Content(content) => std::fs::write(path, content)
                .map_err(|error| {
                    CrosstacheError::config(format!("test mutate repair destination: {error}"))
                }),
            AnchoredRepairDestinationChange::Fifo { saved_path } => {
                std::fs::rename(path, saved_path).map_err(|error| {
                    CrosstacheError::config(format!("test preserve repair destination: {error}"))
                })?;
                let fifo = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
                    CrosstacheError::config("test repair FIFO path contains a NUL byte")
                })?;
                if unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) } < 0 {
                    return Err(CrosstacheError::config(format!(
                        "test replace repair destination with FIFO: {}",
                        std::io::Error::last_os_error()
                    )));
                }
                Ok(())
            }
        }
    }

    #[cfg(any(unix, windows))]
    pub(super) fn run_anchored_repair_commit_hook(path: &Path) -> Result<()> {
        let Some(content) = anchored_repair_commit_hooks().lock().unwrap().remove(path) else {
            return Ok(());
        };
        std::fs::write(path, content).map_err(|error| {
            CrosstacheError::config(format!("test mutate repair commit window: {error}"))
        })
    }

    #[cfg(windows)]
    fn windows_replace_failures() -> &'static std::sync::Mutex<std::collections::HashSet<PathBuf>> {
        static HOOKS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
            std::sync::OnceLock::new();
        HOOKS.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
    }

    #[cfg(windows)]
    fn install_windows_replace_failure(path: &Path) {
        windows_replace_failures()
            .lock()
            .unwrap()
            .insert(path.to_path_buf());
    }

    #[cfg(windows)]
    pub(super) fn take_windows_replace_failure(path: &Path) -> bool {
        windows_replace_failures().lock().unwrap().remove(path)
    }

    #[cfg(windows)]
    fn windows_backup_conflicts(
    ) -> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, Vec<u8>>> {
        static HOOKS: std::sync::OnceLock<
            std::sync::Mutex<std::collections::HashMap<PathBuf, Vec<u8>>>,
        > = std::sync::OnceLock::new();
        HOOKS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
    }

    #[cfg(windows)]
    fn install_windows_backup_conflict(path: &Path, content: &[u8]) {
        windows_backup_conflicts()
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), content.to_vec());
    }

    #[cfg(windows)]
    pub(super) fn run_windows_backup_conflict_hook(path: &Path, backup_path: &Path) -> Result<()> {
        let Some(content) = windows_backup_conflicts().lock().unwrap().remove(path) else {
            return Ok(());
        };
        std::fs::write(backup_path, content).map_err(|error| {
            CrosstacheError::config(format!("test create Windows backup conflict: {error}"))
        })
    }

    #[cfg(unix)]
    type AnchoredRepairObserver = std::sync::Arc<std::sync::Mutex<Option<Vec<u8>>>>;

    #[cfg(unix)]
    type AnchoredRepairObservers =
        std::sync::Mutex<std::collections::HashMap<PathBuf, AnchoredRepairObserver>>;

    #[cfg(unix)]
    fn anchored_repair_observers() -> &'static AnchoredRepairObservers {
        static HOOKS: std::sync::OnceLock<AnchoredRepairObservers> = std::sync::OnceLock::new();
        HOOKS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
    }

    #[cfg(unix)]
    fn anchored_repair_post_publish_writers(
    ) -> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, Vec<u8>>> {
        static HOOKS: std::sync::OnceLock<
            std::sync::Mutex<std::collections::HashMap<PathBuf, Vec<u8>>>,
        > = std::sync::OnceLock::new();
        HOOKS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
    }

    #[cfg(unix)]
    fn install_anchored_repair_observer(
        path: &Path,
        observed: std::sync::Arc<std::sync::Mutex<Option<Vec<u8>>>>,
    ) {
        anchored_repair_observers()
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), observed);
    }

    #[cfg(unix)]
    fn install_anchored_repair_post_publish_writer(path: &Path, content: &[u8]) {
        anchored_repair_post_publish_writers()
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), content.to_vec());
    }

    #[cfg(unix)]
    pub(super) fn run_anchored_repair_post_publish_hooks(path: &Path) -> Result<()> {
        if let Some(observed) = anchored_repair_observers().lock().unwrap().remove(path) {
            *observed.lock().unwrap() = Some(std::fs::read(path).map_err(|error| {
                CrosstacheError::config(format!("test observe published repair: {error}"))
            })?);
        }
        if let Some(content) = anchored_repair_post_publish_writers()
            .lock()
            .unwrap()
            .remove(path)
        {
            std::fs::write(path, content).map_err(|error| {
                CrosstacheError::config(format!("test write published repair: {error}"))
            })?;
        }
        Ok(())
    }

    #[cfg(unix)]
    #[derive(Clone)]
    struct ArtifactWriter {
        saved_path: PathBuf,
        content: Vec<u8>,
    }

    #[cfg(unix)]
    fn anchored_repair_displaced_artifact_writers(
    ) -> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, ArtifactWriter>> {
        static HOOKS: std::sync::OnceLock<
            std::sync::Mutex<std::collections::HashMap<PathBuf, ArtifactWriter>>,
        > = std::sync::OnceLock::new();
        HOOKS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
    }

    #[cfg(unix)]
    fn anchored_repair_backup_artifact_writers(
    ) -> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, ArtifactWriter>> {
        static HOOKS: std::sync::OnceLock<
            std::sync::Mutex<std::collections::HashMap<PathBuf, ArtifactWriter>>,
        > = std::sync::OnceLock::new();
        HOOKS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
    }

    #[cfg(unix)]
    fn install_anchored_repair_displaced_artifact_writer(
        path: &Path,
        saved_path: &Path,
        content: &[u8],
    ) {
        anchored_repair_displaced_artifact_writers()
            .lock()
            .unwrap()
            .insert(
                path.to_path_buf(),
                ArtifactWriter {
                    saved_path: saved_path.to_path_buf(),
                    content: content.to_vec(),
                },
            );
    }

    #[cfg(unix)]
    fn install_anchored_repair_backup_artifact_writer(
        path: &Path,
        saved_path: &Path,
        content: &[u8],
    ) {
        anchored_repair_backup_artifact_writers()
            .lock()
            .unwrap()
            .insert(
                path.to_path_buf(),
                ArtifactWriter {
                    saved_path: saved_path.to_path_buf(),
                    content: content.to_vec(),
                },
            );
    }

    #[cfg(unix)]
    fn run_artifact_writer(target: &Path, writer: ArtifactWriter) -> Result<()> {
        std::fs::rename(target, &writer.saved_path).map_err(|error| {
            CrosstacheError::config(format!("test preserve raced artifact: {error}"))
        })?;
        std::fs::write(target, writer.content).map_err(|error| {
            CrosstacheError::config(format!("test replace raced artifact: {error}"))
        })
    }

    #[cfg(unix)]
    pub(super) fn run_anchored_repair_displaced_artifact_hook(
        path: &Path,
        target: &Path,
    ) -> Result<()> {
        let Some(writer) = anchored_repair_displaced_artifact_writers()
            .lock()
            .unwrap()
            .remove(path)
        else {
            return Ok(());
        };
        run_artifact_writer(target, writer)
    }

    #[cfg(unix)]
    pub(super) fn run_anchored_repair_backup_artifact_hook(
        path: &Path,
        target: &Path,
    ) -> Result<()> {
        let Some(writer) = anchored_repair_backup_artifact_writers()
            .lock()
            .unwrap()
            .remove(path)
        else {
            return Ok(());
        };
        run_artifact_writer(target, writer)
    }

    #[cfg(unix)]
    fn atomic_post_rename_sync_failures(
    ) -> &'static std::sync::Mutex<std::collections::HashSet<PathBuf>> {
        static FAILURES: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
            std::sync::OnceLock::new();
        FAILURES.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
    }

    #[cfg(unix)]
    fn install_atomic_post_rename_sync_failure(path: &Path) {
        atomic_post_rename_sync_failures()
            .lock()
            .unwrap()
            .insert(path.to_path_buf());
    }

    #[cfg(unix)]
    pub(super) fn run_atomic_post_rename_sync_hook(path: &Path) -> std::io::Result<()> {
        if atomic_post_rename_sync_failures()
            .lock()
            .unwrap()
            .remove(path)
        {
            return Err(std::io::Error::other(
                "injected post-rename directory sync failure",
            ));
        }
        Ok(())
    }

    #[cfg(unix)]
    fn atomic_post_rename_sync_failure_pending(path: &Path) -> bool {
        atomic_post_rename_sync_failures()
            .lock()
            .unwrap()
            .contains(path)
    }

    #[cfg(unix)]
    pub(super) fn run_atomic_parent_swap_hook(path: &Path) -> Result<()> {
        let Some(parked_parent) = atomic_parent_swap_hooks().lock().unwrap().remove(path) else {
            return Ok(());
        };
        let parent = path.parent().unwrap();
        std::fs::rename(parent, &parked_parent).map_err(|error| {
            CrosstacheError::config(format!("test park atomic parent: {error}"))
        })?;
        std::fs::create_dir(parent).map_err(|error| {
            CrosstacheError::config(format!("test replace atomic parent: {error}"))
        })
    }

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

    #[test]
    #[cfg(unix)]
    fn atomic_write_remains_in_retained_parent_after_path_swap() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("live");
        let parked = root.path().join("parked");
        std::fs::create_dir(&parent).unwrap();
        let path = parent.join("xv.conf");
        std::fs::write(&path, b"old").unwrap();
        install_atomic_parent_swap(&path, &parked);

        atomic_write_file_no_follow(&path, b"new", true).unwrap();

        assert_eq!(std::fs::read(parked.join("xv.conf")).unwrap(), b"new");
        assert!(std::fs::read_dir(&parent).unwrap().next().is_none());
        assert_eq!(std::fs::read_dir(&parked).unwrap().count(), 1);
    }

    #[test]
    #[cfg(unix)]
    fn atomic_write_cleans_retained_parent_after_swapped_replacement_failure() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("live");
        let parked = root.path().join("parked");
        std::fs::create_dir(&parent).unwrap();
        let path = parent.join("xv.conf");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("sentinel"), b"keep").unwrap();
        install_atomic_parent_swap(&path, &parked);

        assert!(atomic_write_file_no_follow(&path, b"serialized-secret", true).is_err());

        assert_eq!(
            std::fs::read(parked.join("xv.conf/sentinel")).unwrap(),
            b"keep"
        );
        assert!(std::fs::read_dir(&parent).unwrap().next().is_none());
        assert_eq!(std::fs::read_dir(&parked).unwrap().count(), 1);
    }

    #[test]
    #[cfg(unix)]
    fn atomic_write_reports_success_after_post_commit_directory_sync_failure() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("xv.conf");
        std::fs::write(&path, b"old").unwrap();
        install_atomic_post_rename_sync_failure(&path);

        let result = atomic_write_file_no_follow(&path, b"new", true);

        assert!(result.is_ok(), "{result:?}");
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert!(!atomic_post_rename_sync_failure_pending(&path));
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    #[cfg(unix)]
    fn anchored_backup_replace_rejects_concurrent_destination_change() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("xv.conf");
        let backup_name = std::ffi::OsStr::new("xv.conf.backup-fixed");
        std::fs::write(&path, b"diagnosed").unwrap();
        install_anchored_repair_content_change(&path, b"concurrent");

        let error = atomic_replace_with_private_backup_no_follow(
            &path,
            backup_name,
            b"diagnosed",
            b"repaired",
        )
        .unwrap_err();

        assert!(error.to_string().contains("changed"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), b"concurrent");
        assert!(!root.path().join(backup_name).exists());
        let preserved = std::fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|entry| entry != &path)
            .unwrap();
        assert_eq!(std::fs::read(preserved).unwrap(), b"repaired");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2);
    }

    #[test]
    #[cfg(unix)]
    fn anchored_backup_replace_rejects_fifo_destination_race_without_blocking() {
        use std::os::unix::fs::FileTypeExt;

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("xv.conf");
        let saved = root.path().join("xv.conf.before-fifo-race");
        let backup_name = std::ffi::OsStr::new("xv.conf.backup-fixed");
        std::fs::write(&path, b"diagnosed").unwrap();
        install_anchored_repair_fifo_change(&path, &saved);

        let worker_path = path.clone();
        let worker_backup_name = backup_name.to_os_string();
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            sender
                .send(atomic_replace_with_private_backup_no_follow(
                    &worker_path,
                    &worker_backup_name,
                    b"diagnosed",
                    b"repaired",
                ))
                .unwrap();
        });

        let result = receive_bounded_fifo_probe(receiver, worker, &path, "anchored repair read");
        let error = result.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("non-regular anchored config destination"),
            "{error}"
        );
        assert!(std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_fifo());
        assert_eq!(std::fs::read(&saved).unwrap(), b"diagnosed");
        assert!(!root.path().join(backup_name).exists());
        let repair_artifact = std::fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|entry| entry != &path && entry != &saved)
            .unwrap();
        assert_eq!(std::fs::read(repair_artifact).unwrap(), b"repaired");
    }

    #[test]
    #[cfg(unix)]
    fn anchored_backup_replace_rejects_fifo_backup_collision_without_blocking() {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("xv.conf");
        let backup_name = std::ffi::OsStr::new("xv.conf.backup-fixed");
        let backup_path = root.path().join(backup_name);
        std::fs::write(&path, b"diagnosed").unwrap();
        let fifo_path = std::ffi::CString::new(backup_path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600) }, 0);
        let fifo_before = std::fs::symlink_metadata(&backup_path).unwrap();

        let worker_path = path.clone();
        let worker_backup_name = backup_name.to_os_string();
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            sender
                .send(atomic_replace_with_private_backup_no_follow(
                    &worker_path,
                    &worker_backup_name,
                    b"diagnosed",
                    b"repaired",
                ))
                .unwrap();
        });

        let result = match receiver.recv_timeout(std::time::Duration::from_secs(1)) {
            Ok(result) => result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let _unblock = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&backup_path)
                    .unwrap();
                let delayed = receiver
                    .recv_timeout(std::time::Duration::from_secs(1))
                    .expect("blocked repair probe did not finish after FIFO was opened");
                worker.join().unwrap();
                panic!("backup collision probe blocked on FIFO: {delayed:?}");
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("backup collision probe worker disconnected")
            }
        };
        worker.join().unwrap();

        let error = result.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Refusing to overwrite existing config backup"),
            "{error}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"diagnosed");
        let fifo_after = std::fs::symlink_metadata(&backup_path).unwrap();
        assert!(fifo_after.file_type().is_fifo());
        assert_eq!(fifo_after.dev(), fifo_before.dev());
        assert_eq!(fifo_after.ino(), fifo_before.ino());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2);
    }

    #[test]
    #[cfg(unix)]
    fn anchored_backup_replace_keeps_backup_write_and_verification_in_retained_parent() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("live");
        let parked = root.path().join("parked");
        std::fs::create_dir(&parent).unwrap();
        let path = parent.join("xv.conf");
        let backup_name = std::ffi::OsStr::new("xv.conf.backup-fixed");
        std::fs::write(&path, b"diagnosed").unwrap();
        install_atomic_parent_swap(&path, &parked);

        let verified = atomic_replace_with_private_backup_no_follow(
            &path,
            backup_name,
            b"diagnosed",
            b"repaired",
        )
        .unwrap();

        assert_eq!(verified, b"repaired");
        assert_eq!(std::fs::read(parked.join("xv.conf")).unwrap(), b"repaired");
        assert_eq!(
            std::fs::read(parked.join(backup_name)).unwrap(),
            b"diagnosed"
        );
        assert!(std::fs::read_dir(&parent).unwrap().next().is_none());
        assert_eq!(std::fs::read_dir(&parked).unwrap().count(), 2);
    }

    #[test]
    #[cfg(unix)]
    fn anchored_backup_replace_never_publishes_empty_destination() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("xv.conf");
        let backup_name = std::ffi::OsStr::new("xv.conf.backup-fixed");
        std::fs::write(&path, b"diagnosed").unwrap();
        let observed = std::sync::Arc::new(std::sync::Mutex::new(None));
        install_anchored_repair_observer(&path, observed.clone());

        atomic_replace_with_private_backup_no_follow(&path, backup_name, b"diagnosed", b"repaired")
            .unwrap();

        assert_eq!(
            observed.lock().unwrap().as_deref(),
            Some(b"repaired".as_slice())
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"repaired");
        assert_eq!(
            std::fs::read(root.path().join(backup_name)).unwrap(),
            b"diagnosed"
        );
    }

    #[test]
    #[cfg(unix)]
    fn anchored_backup_replace_preserves_exact_commit_window_displacement() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("xv.conf");
        let backup_name = std::ffi::OsStr::new("xv.conf.backup-fixed");
        std::fs::write(&path, b"diagnosed").unwrap();
        install_anchored_repair_commit_change(&path, b"commit-window");

        let error = atomic_replace_with_private_backup_no_follow(
            &path,
            backup_name,
            b"diagnosed",
            b"repaired",
        )
        .unwrap_err();

        assert!(error.to_string().contains("changed"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), b"repaired");
        assert_eq!(
            std::fs::read(root.path().join(backup_name)).unwrap(),
            b"commit-window"
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2);
    }

    #[test]
    #[cfg(unix)]
    fn anchored_backup_replace_preserves_writer_replacing_displaced_artifact() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("xv.conf");
        let backup_name = std::ffi::OsStr::new("xv.conf.backup-fixed");
        let saved = root.path().join("writer-saved-displaced");
        std::fs::write(&path, b"diagnosed").unwrap();
        install_anchored_repair_commit_change(&path, b"commit-window");
        install_anchored_repair_displaced_artifact_writer(&path, &saved, b"writer-displaced");

        let error = atomic_replace_with_private_backup_no_follow(
            &path,
            backup_name,
            b"diagnosed",
            b"repaired",
        )
        .unwrap_err();

        assert!(error.to_string().contains("changed"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), b"repaired");
        assert_eq!(std::fs::read(&saved).unwrap(), b"commit-window");
        assert_eq!(
            std::fs::read(root.path().join(backup_name)).unwrap(),
            b"writer-displaced"
        );
    }

    #[test]
    #[cfg(unix)]
    fn anchored_backup_replace_preserves_writer_replacing_promoted_backup() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("xv.conf");
        let backup_name = std::ffi::OsStr::new("xv.conf.backup-fixed");
        let saved = root.path().join("writer-saved-backup");
        std::fs::write(&path, b"diagnosed").unwrap();
        install_anchored_repair_backup_artifact_writer(&path, &saved, b"writer-backup");

        let error = atomic_replace_with_private_backup_no_follow(
            &path,
            backup_name,
            b"diagnosed",
            b"repaired",
        )
        .unwrap_err();

        assert!(error.to_string().contains("changed"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), b"repaired");
        assert_eq!(std::fs::read(&saved).unwrap(), b"diagnosed");
        assert_eq!(
            std::fs::read(root.path().join(backup_name)).unwrap(),
            b"writer-backup"
        );
    }

    #[test]
    #[cfg(unix)]
    fn anchored_backup_replace_preserves_writer_after_atomic_publish() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("xv.conf");
        let backup_name = std::ffi::OsStr::new("xv.conf.backup-fixed");
        std::fs::write(&path, b"diagnosed").unwrap();
        install_anchored_repair_post_publish_writer(&path, b"writer-visible");

        let error = atomic_replace_with_private_backup_no_follow(
            &path,
            backup_name,
            b"diagnosed",
            b"repaired",
        )
        .unwrap_err();

        assert!(error.to_string().contains("changed"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), b"writer-visible");
        assert_eq!(
            std::fs::read(root.path().join(backup_name)).unwrap(),
            b"diagnosed"
        );
    }

    #[cfg(windows)]
    fn windows_dacl_sddl(path: &Path) -> String {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Authorization::{
            ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW,
            SDDL_REVISION_1, SE_FILE_OBJECT,
        };
        use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;

        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut descriptor = std::ptr::null_mut();
        let status = unsafe {
            GetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        assert_eq!(status, 0);
        let mut sddl = std::ptr::null_mut();
        let converted = unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut sddl,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(converted, 0);
        let length = unsafe {
            let mut length = 0;
            while *sddl.add(length) != 0 {
                length += 1;
            }
            length
        };
        let rendered =
            String::from_utf16(unsafe { std::slice::from_raw_parts(sddl, length) }).unwrap();
        unsafe {
            LocalFree(sddl.cast());
            LocalFree(descriptor.cast());
        }
        rendered
    }

    #[cfg(windows)]
    fn set_windows_dacl(path: &Path, sddl: &str) {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
        };
        use windows_sys::Win32::Security::{SetFileSecurityW, DACL_SECURITY_INFORMATION};

        let encoded_sddl: Vec<u16> = std::ffi::OsStr::new(sddl)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut descriptor = std::ptr::null_mut();
        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                encoded_sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(converted, 0);
        let descriptor = WindowsSecurityDescriptor(descriptor);
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let applied =
            unsafe { SetFileSecurityW(wide.as_ptr(), DACL_SECURITY_INFORMATION, descriptor.0) };
        assert_ne!(applied, 0, "{}", std::io::Error::last_os_error());
    }

    #[cfg(windows)]
    fn assert_private_windows_dacl(path: &Path) {
        let rendered = windows_dacl_sddl(path);
        assert!(rendered.starts_with("D:P"), "{rendered}");
        assert!(rendered.contains(";;;OW)"), "{rendered}");
        assert!(rendered.contains(";;;SY)"), "{rendered}");
        for forbidden in [";;;WD)", ";;;AU)", ";;;BU)"] {
            assert!(!rendered.contains(forbidden), "{rendered}");
        }
    }

    #[test]
    #[cfg(windows)]
    fn anchored_backup_replace_windows_restricts_permissive_destination_and_backup_dacls() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("xv.conf");
        let backup_name = std::ffi::OsStr::new("xv.conf.backup-fixed");
        let backup_path = root.path().join(backup_name);
        std::fs::write(&path, b"diagnosed").unwrap();
        set_windows_dacl(&path, "D:(A;;FA;;;WD)");
        assert!(windows_dacl_sddl(&path).contains(";;;WD)"));

        let verified = atomic_replace_with_private_backup_no_follow(
            &path,
            backup_name,
            b"diagnosed",
            b"repaired",
        )
        .unwrap();

        assert_eq!(verified, b"repaired");
        assert_private_windows_dacl(&path);
        assert_private_windows_dacl(&backup_path);
    }

    #[test]
    #[cfg(windows)]
    fn anchored_backup_replace_windows_captures_exact_displaced_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("xv.conf");
        let backup_name = std::ffi::OsStr::new("xv.conf.backup-fixed");
        std::fs::write(&path, b"diagnosed").unwrap();

        let verified = atomic_replace_with_private_backup_no_follow(
            &path,
            backup_name,
            b"diagnosed",
            b"repaired",
        )
        .unwrap();

        assert_eq!(verified, b"repaired");
        assert_eq!(std::fs::read(&path).unwrap(), b"repaired");
        assert_eq!(
            std::fs::read(root.path().join(backup_name)).unwrap(),
            b"diagnosed"
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2);
    }

    #[test]
    #[cfg(windows)]
    fn anchored_backup_replace_windows_preserves_commit_window_displacement() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("xv.conf");
        let backup_name = std::ffi::OsStr::new("xv.conf.backup-fixed");
        std::fs::write(&path, b"diagnosed").unwrap();
        install_anchored_repair_commit_change(&path, b"commit-window");

        let error = atomic_replace_with_private_backup_no_follow(
            &path,
            backup_name,
            b"diagnosed",
            b"repaired",
        )
        .unwrap_err();

        assert!(error.to_string().contains("changed"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), b"repaired");
        assert_eq!(
            std::fs::read(root.path().join(backup_name)).unwrap(),
            b"commit-window"
        );
    }

    #[test]
    #[cfg(windows)]
    fn anchored_backup_replace_windows_preserves_failed_replace_artifacts() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("xv.conf");
        let backup_name = std::ffi::OsStr::new("xv.conf.backup-fixed");
        std::fs::write(&path, b"diagnosed").unwrap();
        install_windows_replace_failure(&path);

        let error = atomic_replace_with_private_backup_no_follow(
            &path,
            backup_name,
            b"diagnosed",
            b"repaired",
        )
        .unwrap_err();

        assert!(error.to_string().contains("preserved"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), b"diagnosed");
        assert!(!root.path().join(backup_name).exists());
        let artifact = std::fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|entry| entry != &path)
            .unwrap();
        assert_eq!(std::fs::read(artifact).unwrap(), b"repaired");
    }

    #[test]
    #[cfg(windows)]
    fn anchored_backup_replace_windows_preserves_backup_promotion_conflict() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("xv.conf");
        let backup_name = std::ffi::OsStr::new("xv.conf.backup-fixed");
        std::fs::write(&path, b"diagnosed").unwrap();
        install_windows_backup_conflict(&path, b"writer-backup");

        let error = atomic_replace_with_private_backup_no_follow(
            &path,
            backup_name,
            b"diagnosed",
            b"repaired",
        )
        .unwrap_err();

        assert!(error.to_string().contains("preserved"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), b"repaired");
        assert_eq!(
            std::fs::read(root.path().join(backup_name)).unwrap(),
            b"writer-backup"
        );
        let displaced = std::fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|entry| entry != &path && entry != &root.path().join(backup_name))
            .unwrap();
        assert_eq!(std::fs::read(displaced).unwrap(), b"diagnosed");
    }

    #[test]
    #[cfg(windows)]
    fn atomic_private_write_applies_protected_owner_system_dacl() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("private.conf");
        atomic_write_file_no_follow(&path, b"secret", true).unwrap();
        assert_private_windows_dacl(&path);
    }

    /// Creating a symlink needs `SeCreateSymbolicLinkPrivilege`, which an
    /// elevated process or one running under Developer Mode holds and an
    /// ordinary user shell does not. Windows reports the lack of it as
    /// ERROR_PRIVILEGE_NOT_HELD (1314), which Rust does *not* map to
    /// `ErrorKind::PermissionDenied` — so the kind-only check these tests
    /// originally used never matched and they panicked instead of skipping.
    #[cfg(windows)]
    fn windows_symlink_unavailable(error: &std::io::Error) -> bool {
        const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;
        error.kind() == std::io::ErrorKind::PermissionDenied
            || error.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD)
    }

    #[test]
    #[cfg(windows)]
    fn atomic_write_refuses_windows_reparse_parent() {
        use std::os::windows::fs::symlink_dir;

        let root = tempfile::tempdir().unwrap();
        let external = root.path().join("external");
        let linked = root.path().join("linked");
        std::fs::create_dir(&external).unwrap();
        if let Err(error) = symlink_dir(&external, &linked) {
            if windows_symlink_unavailable(&error) {
                eprintln!("skipping: cannot create symlinks without privilege ({error})");
                return;
            }
            panic!("create directory reparse point: {error}");
        }

        assert!(atomic_write_file_no_follow(&linked.join("xv.conf"), b"secret", true).is_err());
        assert!(!external.join("xv.conf").exists());
    }

    #[test]
    #[cfg(windows)]
    fn atomic_write_refuses_windows_reparse_destination() {
        use std::os::windows::fs::symlink_file;

        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target.conf");
        let linked = root.path().join("linked.conf");
        std::fs::write(&target, b"old").unwrap();
        if let Err(error) = symlink_file(&target, &linked) {
            if windows_symlink_unavailable(&error) {
                eprintln!("skipping: cannot create symlinks without privilege ({error})");
                return;
            }
            panic!("create file reparse point: {error}");
        }

        assert!(atomic_write_file_no_follow(&linked, b"secret", true).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
    }
}
