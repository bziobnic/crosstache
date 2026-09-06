//! Shared anchored filesystem operations used by local files and transfer namespaces.
#![cfg_attr(not(feature = "file-ops"), allow(dead_code))]
use crate::backend::error::BackendError;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
pub(super) struct AnchoredDir {
    /// The retained directory handle.
    ///
    /// Read on Unix, where every operation is performed relative to it via
    /// `openat`. Windows has no `openat`, so the operations there address
    /// their targets by path — but the handle is still held for the lifetime
    /// of the chain, so the directory validated on the way in cannot be
    /// swapped out from under them.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(super) file: fs::File,
    pub(super) display: PathBuf,
}

#[cfg(unix)]
pub(super) type AnchoredFileMode = libc::mode_t;
#[cfg(not(unix))]
pub(super) type AnchoredFileMode = u32;

#[cfg(unix)]
#[derive(Debug)]
pub(super) struct StableSymlink {
    target: std::ffi::OsString,
    uid: libc::uid_t,
    mode: u32,
    parent_uid: libc::uid_t,
    parent_mode: u32,
}

#[cfg(unix)]
#[allow(clippy::useless_conversion)]
fn stable_symlink_mode(mode: libc::mode_t) -> u32 {
    // `mode_t` is narrower on macOS but already `u32` on Linux.
    u32::from(mode)
}

impl AnchoredDir {
    pub(super) fn open_path(path: &Path) -> Result<Self, BackendError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let file = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(path)
                .map_err(|error| {
                    BackendError::Internal(format!(
                        "open anchored directory {}: {error}",
                        path.display()
                    ))
                })?;
            Ok(Self {
                file,
                display: path.to_path_buf(),
            })
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
            use windows_sys::Win32::Storage::FileSystem::{
                FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
                FILE_FLAG_OPEN_REPARSE_POINT,
            };

            // Windows refuses to open a directory at all without
            // FILE_FLAG_BACKUP_SEMANTICS — a plain `File::open` on one always
            // fails with ERROR_ACCESS_DENIED (os error 5), which took the whole
            // local file backend down on Windows.
            //
            // FILE_FLAG_OPEN_REPARSE_POINT stands in for the Unix branch's
            // `O_NOFOLLOW`: it opens a reparse point itself instead of
            // redirecting to its target, so the attribute checks below can
            // refuse it rather than silently anchoring somewhere else.
            let file = fs::OpenOptions::new()
                .read(true)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(path)
                .map_err(|error| {
                    BackendError::Internal(format!(
                        "open anchored directory {}: {error}",
                        path.display()
                    ))
                })?;

            // Windows has no open flag equivalent to `O_DIRECTORY`/`O_NOFOLLOW`
            // that fails the open outright, so enforce both after the fact from
            // the handle — not from the path, which would reintroduce the
            // race the anchored handle exists to close.
            let attributes = file
                .metadata()
                .map_err(|error| {
                    BackendError::Internal(format!(
                        "inspect anchored directory {}: {error}",
                        path.display()
                    ))
                })?
                .file_attributes();
            if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err(BackendError::Internal(format!(
                    "refusing reparse-point anchored directory {}",
                    path.display()
                )));
            }
            if attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
                return Err(BackendError::Internal(format!(
                    "anchored path {} is not a directory",
                    path.display()
                )));
            }

            Ok(Self {
                file,
                display: path.to_path_buf(),
            })
        }
        #[cfg(all(not(unix), not(windows)))]
        {
            let file = fs::File::open(path).map_err(|error| {
                BackendError::Internal(format!(
                    "open anchored directory {}: {error}",
                    path.display()
                ))
            })?;
            Ok(Self {
                file,
                display: path.to_path_buf(),
            })
        }
    }

    pub(super) fn open_dir(&self, name: &str) -> Result<Option<Self>, BackendError> {
        validate_relative_component(name)?;
        #[cfg(unix)]
        {
            use std::ffi::CString;
            use std::os::fd::{AsRawFd, FromRawFd};
            let name = CString::new(name)
                .map_err(|_| BackendError::Internal("directory name contains NUL".into()))?;
            let fd = unsafe {
                libc::openat(
                    self.file.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::NotFound {
                    return Ok(None);
                }
                return Err(BackendError::Internal(format!(
                    "refusing unsafe anchored directory {}/{}: {error}",
                    self.display.display(),
                    name.to_string_lossy()
                )));
            }
            Ok(Some(Self {
                file: unsafe { fs::File::from_raw_fd(fd) },
                display: self.display.join(name.to_string_lossy().as_ref()),
            }))
        }
        #[cfg(not(unix))]
        {
            let path = self.display.join(name);
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                    Self::open_path(&path).map(Some)
                }
                Ok(_) => Err(BackendError::Internal(
                    "refusing unsafe anchored directory".into(),
                )),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(BackendError::Internal(format!(
                    "inspect anchored directory: {error}"
                ))),
            }
        }
    }

    #[cfg(unix)]
    pub(super) fn read_stable_symlink(
        &self,
        name: &str,
    ) -> Result<Option<StableSymlink>, BackendError> {
        use std::ffi::{CString, OsString};
        use std::mem::MaybeUninit;
        use std::os::fd::AsRawFd;
        use std::os::unix::ffi::OsStringExt;
        use std::os::unix::fs::MetadataExt;

        validate_relative_component(name)?;
        let name = CString::new(name)
            .map_err(|_| BackendError::Internal("symlink name contains NUL".into()))?;
        let inspect = || -> Result<Option<libc::stat>, BackendError> {
            let mut stat = MaybeUninit::<libc::stat>::uninit();
            let result = unsafe {
                libc::fstatat(
                    self.file.as_raw_fd(),
                    name.as_ptr(),
                    stat.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if result < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::NotFound {
                    return Ok(None);
                }
                return Err(BackendError::Internal(format!(
                    "inspect anchored symlink: {error}"
                )));
            }
            Ok(Some(unsafe { stat.assume_init() }))
        };

        let Some(before) = inspect()? else {
            return Ok(None);
        };
        if before.st_mode & libc::S_IFMT != libc::S_IFLNK {
            return Ok(None);
        }
        let parent = self.file.metadata().map_err(|error| {
            BackendError::Internal(format!("inspect symlink parent handle: {error}"))
        })?;
        let read_target = || -> Result<Vec<u8>, BackendError> {
            let mut bytes = vec![0_u8; 4096];
            let length = unsafe {
                libc::readlinkat(
                    self.file.as_raw_fd(),
                    name.as_ptr(),
                    bytes.as_mut_ptr().cast(),
                    bytes.len(),
                )
            };
            if length < 0 {
                return Err(BackendError::Internal(format!(
                    "read anchored symlink: {}",
                    std::io::Error::last_os_error()
                )));
            }
            let length = usize::try_from(length)
                .map_err(|_| BackendError::Internal("invalid symlink length".into()))?;
            if length == bytes.len() {
                return Err(BackendError::Internal(
                    "configured-store symlink target is too long".into(),
                ));
            }
            bytes.truncate(length);
            Ok(bytes)
        };
        let bytes = read_target()?;
        #[cfg(all(test, feature = "file-ops"))]
        super::files::tests::run_configured_link_swap_hook(
            &self.display.join(name.to_string_lossy().as_ref()),
        )?;
        let after = inspect()?.ok_or_else(|| {
            BackendError::Internal("configured-store symlink changed during inspection".into())
        })?;
        if before.st_dev != after.st_dev
            || before.st_ino != after.st_ino
            || before.st_uid != after.st_uid
            || before.st_mode != after.st_mode
            || bytes != read_target()?
        {
            return Err(BackendError::Internal(
                "configured-store symlink changed during inspection".into(),
            ));
        }
        Ok(Some(StableSymlink {
            target: OsString::from_vec(bytes),
            uid: before.st_uid,
            mode: stable_symlink_mode(before.st_mode),
            parent_uid: parent.uid(),
            parent_mode: parent.mode(),
        }))
    }

    pub(super) fn open_or_create_private_dir(&self, name: &str) -> Result<Self, BackendError> {
        self.open_or_create_private_dir_with_mode(name, true)
    }

    pub(super) fn open_or_create_checked_private_dir(
        &self,
        name: &str,
    ) -> Result<Self, BackendError> {
        self.open_or_create_private_dir_with_mode(name, false)
    }

    fn open_or_create_private_dir_with_mode(
        &self,
        name: &str,
        repair: bool,
    ) -> Result<Self, BackendError> {
        if let Some(directory) = self.open_dir(name)? {
            if repair {
                directory.repair_private_mode()?;
            } else {
                directory.check_private_mode()?;
            }
            return Ok(directory);
        }
        validate_relative_component(name)?;
        #[cfg(unix)]
        {
            use std::ffi::CString;
            use std::os::fd::AsRawFd;
            let c_name = CString::new(name)
                .map_err(|_| BackendError::Internal("directory name contains NUL".into()))?;
            let result = unsafe { libc::mkdirat(self.file.as_raw_fd(), c_name.as_ptr(), 0o700) };
            if result < 0
                && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err(BackendError::Internal(format!(
                    "create anchored directory: {}",
                    std::io::Error::last_os_error()
                )));
            }
        }
        #[cfg(not(unix))]
        fs::create_dir(self.display.join(name)).map_err(|error| {
            BackendError::Internal(format!("create anchored directory: {error}"))
        })?;
        let directory = self.open_dir(name)?.ok_or_else(|| {
            BackendError::Internal("anchored directory disappeared after creation".into())
        })?;
        if repair {
            directory.repair_private_mode()?;
        } else {
            directory.check_private_mode()?;
        }
        directory.sync()?;
        self.sync()?;
        Ok(directory)
    }

    fn check_private_mode(&self) -> Result<(), BackendError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = self.file.metadata().map_err(|error| {
                BackendError::Internal(format!("inspect private directory: {error}"))
            })?;
            if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
                return Err(BackendError::PermissionDenied(
                    "transfer directory must be private and owned by current user".into(),
                ));
            }
        }
        Ok(())
    }

    pub(super) fn repair_private_mode(&self) -> Result<(), BackendError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let metadata = self.file.metadata().map_err(|error| {
                BackendError::Internal(format!("inspect anchored directory: {error}"))
            })?;
            // SAFETY: `geteuid` takes no arguments and has no preconditions.
            if metadata.uid() != unsafe { libc::geteuid() } {
                return Err(BackendError::Internal(
                    "anchored directory is not owned by the current user".into(),
                ));
            }
            if metadata.permissions().mode() & 0o777 != 0o700 {
                self.file
                    .set_permissions(fs::Permissions::from_mode(0o700))
                    .map_err(|error| {
                        BackendError::Internal(format!(
                            "repair anchored directory permissions: {error}"
                        ))
                    })?;
                self.sync()?;
            }
        }
        Ok(())
    }

    pub(super) fn open_file(&self, name: &str) -> Result<Option<fs::File>, BackendError> {
        self.open_file_with_flags(name, libc::O_RDONLY, 0)
    }

    pub(super) fn create_private_file(&self, name: &str, bytes: &[u8]) -> Result<(), BackendError> {
        let mut file = self.create_private_file_handle(name)?;
        file.write_all(bytes)
            .map_err(|error| BackendError::Internal(format!("write anchored file: {error}")))?;
        file.sync_all()
            .map_err(|error| BackendError::Internal(format!("sync anchored file: {error}")))
    }

    pub(super) fn create_private_file_handle(&self, name: &str) -> Result<fs::File, BackendError> {
        self.open_file_with_flags(name, libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC, 0o600)?
            .ok_or_else(|| BackendError::Internal("create anchored file failed".into()))
    }

    pub(super) fn copy_file_to(
        &self,
        source: &str,
        destination_dir: &Self,
        destination: &str,
    ) -> Result<(), BackendError> {
        let mut input = self
            .open_file(source)?
            .ok_or_else(|| BackendError::Internal("missing anchored source file".into()))?;
        let mut output = destination_dir.create_private_file_handle(destination)?;
        let mut buffer = zeroize::Zeroizing::new([0_u8; 64 * 1024]);
        loop {
            let count = input
                .read(&mut buffer[..])
                .map_err(|error| BackendError::Internal(format!("read anchored file: {error}")))?;
            if count == 0 {
                break;
            }
            #[cfg(all(test, feature = "file-ops"))]
            super::files::tests::record_transfer_chunk(count);
            output
                .write_all(&buffer[..count])
                .map_err(|error| BackendError::Internal(format!("write anchored file: {error}")))?;
        }
        output
            .sync_all()
            .map_err(|error| BackendError::Internal(format!("sync anchored file: {error}")))
    }

    pub(super) fn open_file_with_flags(
        &self,
        name: &str,
        flags: libc::c_int,
        mode: AnchoredFileMode,
    ) -> Result<Option<fs::File>, BackendError> {
        validate_relative_component(name)?;
        #[cfg(unix)]
        {
            use std::ffi::CString;
            use std::os::fd::{AsRawFd, FromRawFd};
            let c_name = CString::new(name)
                .map_err(|_| BackendError::Internal("file name contains NUL".into()))?;
            let fd = unsafe {
                libc::openat(
                    self.file.as_raw_fd(),
                    c_name.as_ptr(),
                    flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    libc::c_uint::from(mode),
                )
            };
            if fd < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::NotFound {
                    return Ok(None);
                }
                return Err(BackendError::Internal(format!(
                    "open anchored file {}: {error}",
                    name
                )));
            }
            let file = unsafe { fs::File::from_raw_fd(fd) };
            if !file
                .metadata()
                .map_err(|error| BackendError::Internal(format!("inspect anchored file: {error}")))?
                .is_file()
            {
                return Err(BackendError::Internal(
                    "refusing unsafe non-file anchored entry".into(),
                ));
            }
            Ok(Some(file))
        }
        #[cfg(not(unix))]
        {
            let mut options = fs::OpenOptions::new();

            // The access mode is an enum packed into the low bits, not a set of
            // independent flags: `O_RDONLY` is 0 (so `flags & O_RDONLY ==
            // O_RDONLY` is vacuously true for every input) and `O_RDWR` (2)
            // shares no bit with `O_WRONLY` (1). Testing them as bitmasks
            // therefore left `write` unset for `O_RDWR`, and `OpenOptions`
            // rejected the accompanying `create` with "creating or truncating a
            // file requires write or append access" — which is exactly how the
            // vault lock (`O_RDWR | O_CREAT`) failed on Windows. Decode the
            // mode instead.
            const ACCESS_MODE: libc::c_int = libc::O_RDONLY | libc::O_WRONLY | libc::O_RDWR;
            let access = flags & ACCESS_MODE;
            options.read(access == libc::O_RDONLY || access == libc::O_RDWR);
            options.write(access == libc::O_WRONLY || access == libc::O_RDWR);
            options.create(flags & libc::O_CREAT != 0);
            options.truncate(flags & libc::O_TRUNC != 0);
            let _ = mode;

            // Counterpart to the Unix branch's `O_NOFOLLOW`. Without it Windows
            // silently follows a symlink placed at `name`, anchoring the
            // operation outside the directory this handle exists to pin. With
            // it the open yields the reparse point itself, whose `is_file()` is
            // false, so the match below rejects it as a non-file entry.
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                options.custom_flags(
                    windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT,
                );
            }

            match options.open(self.display.join(name)) {
                Ok(file) if file.metadata().is_ok_and(|metadata| metadata.is_file()) => {
                    Ok(Some(file))
                }
                Ok(_) => Err(BackendError::Internal(
                    "refusing unsafe non-file anchored entry".into(),
                )),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(BackendError::Internal(format!(
                    "open anchored file: {error}"
                ))),
            }
        }
    }

    pub(super) fn read_file(&self, name: &str) -> Result<Option<Vec<u8>>, BackendError> {
        #[cfg(all(test, feature = "file-ops"))]
        super::files::tests::record_full_file_read(name);
        let Some(mut file) = self.open_file(name)? else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|error| BackendError::Internal(format!("read anchored file: {error}")))?;
        Ok(Some(bytes))
    }

    pub(super) fn file_exists(&self, name: &str) -> Result<bool, BackendError> {
        Ok(self.open_file(name)?.is_some())
    }

    pub(super) fn rename_to(
        &self,
        source: &str,
        destination_dir: &Self,
        destination: &str,
    ) -> Result<(), BackendError> {
        validate_relative_component(source)?;
        validate_relative_component(destination)?;
        #[cfg(unix)]
        {
            use std::ffi::CString;
            use std::os::fd::AsRawFd;
            let source = CString::new(source)
                .map_err(|_| BackendError::Internal("source name contains NUL".into()))?;
            let destination = CString::new(destination)
                .map_err(|_| BackendError::Internal("destination name contains NUL".into()))?;
            let result = unsafe {
                libc::renameat(
                    self.file.as_raw_fd(),
                    source.as_ptr(),
                    destination_dir.file.as_raw_fd(),
                    destination.as_ptr(),
                )
            };
            if result < 0 {
                return Err(BackendError::Internal(format!(
                    "rename anchored file: {}",
                    std::io::Error::last_os_error()
                )));
            }
            Ok(())
        }
        #[cfg(not(unix))]
        fs::rename(
            self.display.join(source),
            destination_dir.display.join(destination),
        )
        .map_err(|error| BackendError::Internal(format!("rename anchored file: {error}")))
    }

    pub(super) fn remove_file(&self, name: &str) -> Result<(), BackendError> {
        validate_relative_component(name)?;
        #[cfg(unix)]
        {
            self.unlink(name, 0)
        }
        #[cfg(not(unix))]
        {
            fs::remove_file(self.display.join(name))
                .map_err(|error| BackendError::Internal(format!("remove anchored file: {error}")))
        }
    }

    pub(super) fn remove_dir(&self, name: &str) -> Result<(), BackendError> {
        validate_relative_component(name)?;
        #[cfg(unix)]
        {
            self.unlink(name, libc::AT_REMOVEDIR)
        }
        #[cfg(not(unix))]
        {
            fs::remove_dir(self.display.join(name)).map_err(|error| {
                BackendError::Internal(format!("remove anchored directory: {error}"))
            })
        }
    }

    #[cfg(unix)]
    pub(super) fn unlink(&self, name: &str, flags: libc::c_int) -> Result<(), BackendError> {
        validate_relative_component(name)?;
        use std::ffi::CString;
        use std::os::fd::AsRawFd;
        let name = CString::new(name)
            .map_err(|_| BackendError::Internal("unlink name contains NUL".into()))?;
        let result = unsafe { libc::unlinkat(self.file.as_raw_fd(), name.as_ptr(), flags) };
        if result < 0 {
            return Err(BackendError::Internal(format!(
                "remove anchored entry: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(())
    }

    pub(super) fn entry_names(&self) -> Result<Vec<String>, BackendError> {
        #[cfg(unix)]
        {
            use std::ffi::CStr;
            use std::os::fd::AsRawFd;
            let duplicate = unsafe { libc::dup(self.file.as_raw_fd()) };
            if duplicate < 0 {
                return Err(BackendError::Internal(format!(
                    "duplicate directory handle: {}",
                    std::io::Error::last_os_error()
                )));
            }
            let directory = unsafe { libc::fdopendir(duplicate) };
            if directory.is_null() {
                unsafe { libc::close(duplicate) };
                return Err(BackendError::Internal(format!(
                    "open directory stream: {}",
                    std::io::Error::last_os_error()
                )));
            }
            let mut names = Vec::new();
            loop {
                let entry = unsafe { libc::readdir(directory) };
                if entry.is_null() {
                    break;
                }
                let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }
                    .to_string_lossy()
                    .into_owned();
                if name != "." && name != ".." {
                    names.push(name);
                }
            }
            unsafe { libc::closedir(directory) };
            names.sort();
            Ok(names)
        }
        #[cfg(not(unix))]
        {
            let mut names = fs::read_dir(&self.display)
                .map_err(|error| BackendError::Internal(format!("read directory: {error}")))?
                .map(|entry| {
                    entry
                        .map(|entry| entry.file_name().to_string_lossy().into_owned())
                        .map_err(|error| {
                            BackendError::Internal(format!("read directory entry: {error}"))
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            names.sort();
            Ok(names)
        }
    }

    pub(super) fn sync(&self) -> Result<(), BackendError> {
        // Directory fsync is a Unix durability primitive. Windows has no
        // analogue: `FlushFileBuffers` on a directory handle fails with
        // ERROR_ACCESS_DENIED (os error 5) — and the handle is opened read-only
        // by design, so there is nothing to flush. Metadata durability there
        // comes from the filesystem's own ordering, not an explicit call.
        // Skip the sync off Unix but keep recording the event so the ordering
        // assertions in the tests stay meaningful on every platform. Mirrors
        // `sync_directory` in `super::secrets`.
        #[cfg(unix)]
        self.file
            .sync_all()
            .map_err(|error| BackendError::Internal(format!("sync anchored directory: {error}")))?;
        #[cfg(all(test, feature = "file-ops"))]
        super::files::tests::record_file_event("sync-dir", &self.display);
        Ok(())
    }
}

fn validate_relative_component(name: &str) -> Result<(), BackendError> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\\') {
        return Err(BackendError::Internal(
            "invalid anchored filesystem component".into(),
        ));
    }
    Ok(())
}

pub(super) fn open_configured_store_with_mode(
    store_path: &Path,
    create: bool,
    repair: bool,
) -> Result<Option<AnchoredDir>, BackendError> {
    #[cfg(not(unix))]
    let _ = repair;
    #[cfg(unix)]
    {
        use std::path::Component;

        let logical = if store_path.is_absolute() {
            store_path.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|error| {
                    BackendError::Internal(format!("resolve current directory: {error}"))
                })?
                .join(store_path)
        };
        let mut components = logical.components();
        if !matches!(components.next(), Some(Component::RootDir)) {
            return Err(BackendError::Internal(
                "configured store did not resolve to an absolute path".into(),
            ));
        }
        let mut names = components
            .map(|component| match component {
                Component::Normal(name) => name.to_str().map(str::to_string).ok_or_else(|| {
                    BackendError::Internal("configured store component is not UTF-8".into())
                }),
                _ => Err(BackendError::Internal(
                    "configured store contains unsafe path components".into(),
                )),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut directory = AnchoredDir::open_path(Path::new("/"))?;
        if let Some(alias) = names.first() {
            if let Some(link) = directory.read_stable_symlink(alias)? {
                let expected = match alias.as_str() {
                    "var" => Some("private/var"),
                    "tmp" => Some("private/tmp"),
                    _ => None,
                };
                let target = link.target.to_str();
                let trusted = cfg!(target_os = "macos")
                    && expected == target
                    && link.uid == 0
                    && link.mode & 0o022 == 0
                    && link.parent_uid == 0
                    && link.parent_mode & 0o022 == 0;
                if !trusted {
                    return Err(BackendError::Internal(format!(
                        "refusing unsafe configured-store symlink /{alias}"
                    )));
                }
                let mut expanded = target
                    .expect("trusted target is present")
                    .split('/')
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                expanded.extend(names.into_iter().skip(1));
                names = expanded;
            }
        }
        for (index, name) in names.iter().enumerate() {
            let final_component = index + 1 == names.len();
            if directory.read_stable_symlink(name)?.is_some() {
                return Err(BackendError::Internal(format!(
                    "refusing unsafe configured-store symlink {}/{}",
                    directory.display.display(),
                    name
                )));
            }
            directory = match directory.open_dir(name)? {
                Some(next) => next,
                None if create && final_component => {
                    // `display` is diagnostic/test-hook state only; the handle
                    // remains anchored to the already-validated resolved
                    // parent. Preserve the configured spelling (for example
                    // macOS `/var` rather than `/private/var`).
                    if let Some(parent) = logical.parent() {
                        directory.display = parent.to_path_buf();
                    }
                    directory.open_or_create_private_dir(name)?
                }
                None => return Ok(None),
            };
        }
        directory.display = logical;
        if repair {
            directory.repair_private_mode()?;
        }
        Ok(Some(directory))
    }
    #[cfg(not(unix))]
    {
        match AnchoredDir::open_path(store_path) {
            Ok(directory) => Ok(Some(directory)),
            Err(_) if create => {
                let parent = store_path.parent().ok_or_else(|| {
                    BackendError::Internal("configured store has no parent".into())
                })?;
                let parent = AnchoredDir::open_path(parent)?;
                let name = store_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| {
                        BackendError::Internal("invalid configured store name".into())
                    })?;
                parent.open_or_create_private_dir(name).map(Some)
            }
            Err(_) => Ok(None),
        }
    }
}
