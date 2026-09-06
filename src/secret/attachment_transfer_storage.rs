//! Private bounded recovery storage. Unix operations stay anchored to the opened root.
use super::{invalid, Result};
use age::secrecy::ExposeSecret;
#[cfg(unix)]
use fs2::FileExt;
use std::path::Path;
#[cfg(unix)]
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::PathBuf,
};
use zeroize::Zeroizing;

pub(super) struct Session {
    #[cfg(unix)]
    root: PathBuf,
    #[cfg(unix)]
    directory: File,
    #[cfg(unix)]
    _lock: File,
    #[cfg(windows)]
    windows: crate::utils::helpers::WindowsRecoveryDirectory,
    pub identity: age::x25519::Identity,
}
#[cfg(unix)]
fn private(meta: &std::fs::Metadata, directory: bool) -> Result<()> {
    if if directory {
        !meta.is_dir()
    } else {
        !meta.is_file()
    } {
        return Err(invalid());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != unsafe { libc::geteuid() }
            || meta.mode() & 0o077 != 0
            || (!directory && meta.nlink() != 1)
        {
            return Err(invalid());
        }
    }
    Ok(())
}
#[cfg(unix)]
fn open_root(root: &Path) -> Result<File> {
    use std::{
        ffi::CString,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::{
                ffi::OsStrExt,
                fs::{MetadataExt, OpenOptionsExt},
            },
        },
        path::Component,
    };
    let absolute = if root.is_absolute() {
        root.to_owned()
    } else {
        std::env::current_dir().map_err(|_| invalid())?.join(root)
    };
    // Resolve only immutable system compatibility links (e.g. macOS /var),
    // then retain each directory with no-follow descriptor-relative traversal.
    let mut resolved = PathBuf::from("/");
    for component in absolute.components().skip(1) {
        let Component::Normal(name) = component else {
            return Err(invalid());
        };
        resolved.push(name);
        let metadata = std::fs::symlink_metadata(&resolved).map_err(|_| invalid())?;
        if metadata.file_type().is_symlink() {
            let parent =
                std::fs::metadata(resolved.parent().ok_or_else(invalid)?).map_err(|_| invalid())?;
            if metadata.uid() != 0
                || metadata.mode() & 0o022 != 0
                || parent.uid() != 0
                || parent.mode() & 0o022 != 0
            {
                return Err(invalid());
            }
            resolved = resolved.canonicalize().map_err(|_| invalid())?;
        }
    }
    let mut directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/")
        .map_err(|_| invalid())?;
    for component in resolved.components().skip(1) {
        let Component::Normal(name) = component else {
            return Err(invalid());
        };
        let name = CString::new(name.as_bytes()).map_err(|_| invalid())?;
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(invalid());
        }
        directory = unsafe { File::from_raw_fd(fd) };
    }
    Ok(directory)
}
impl Session {
    pub fn open(root: &Path, create: bool) -> Result<Self> {
        // The helper performs descriptor-relative no-follow traversal, including
        // private directory creation. Listing never calls this on an absent root.
        if !create && !root.exists() {
            return Err(invalid());
        }
        #[cfg(unix)]
        let mut session = {
            use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
            let lock = if create {
                crate::utils::helpers::open_private_lock_file_no_follow(&root.join("lock"))?
            } else {
                OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                    .open(root.join("lock"))
                    .map_err(|_| invalid())?
            };
            private(&lock.metadata().map_err(|_| invalid())?, false)?;
            lock.try_lock_exclusive().map_err(|_| {
                crate::error::CrosstacheError::conflict(
                    "Another attachment transfer recovery session is active",
                )
            })?;
            let directory = open_root(root)?;
            private(&directory.metadata().map_err(|_| invalid())?, true)?;
            let session = Self {
                root: root.to_owned(),
                directory,
                _lock: lock,
                identity: age::x25519::Identity::generate(),
            };
            let anchored_lock = session
                .open_child("lock", false)?
                .metadata()
                .map_err(|_| invalid())?;
            let held_lock = session._lock.metadata().map_err(|_| invalid())?;
            if anchored_lock.dev() != held_lock.dev() || anchored_lock.ino() != held_lock.ino() {
                return Err(invalid());
            }
            session
        };
        #[cfg(windows)]
        let mut session = Self {
            windows: crate::utils::helpers::WindowsRecoveryDirectory::open(root, create)?,
            identity: age::x25519::Identity::generate(),
        };
        match session.read("identity", 256) {
            Ok(bytes) => {
                let text = Zeroizing::new(String::from_utf8(bytes).map_err(|_| invalid())?);
                session.identity = text.parse().map_err(|_| invalid())?;
            }
            Err(_) if create && !session.exists("identity")? && session.names()?.is_empty() => {
                let identity = session.identity.to_string();
                session.write("identity", identity.expose_secret().as_bytes())?;
            }
            Err(e) => return Err(e),
        }
        Ok(session)
    }
    #[cfg(unix)]
    fn open_child(&self, name: &str, write: bool) -> Result<File> {
        #[cfg(unix)]
        {
            use std::{
                ffi::CString,
                os::fd::{AsRawFd, FromRawFd},
            };
            let name = CString::new(name).map_err(|_| invalid())?;
            let flags = libc::O_NOFOLLOW
                | libc::O_NONBLOCK
                | libc::O_CLOEXEC
                | if write {
                    libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL
                } else {
                    libc::O_RDONLY
                };
            let fd =
                unsafe { libc::openat(self.directory.as_raw_fd(), name.as_ptr(), flags, 0o600) };
            if fd < 0 {
                return Err(invalid());
            }
            let file = unsafe { File::from_raw_fd(fd) };
            private(&file.metadata().map_err(|_| invalid())?, false)?;
            Ok(file)
        }
    }
    pub fn check_location(&self) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let current = open_root(&self.root)?.metadata().map_err(|_| invalid())?;
            let anchored = self.directory.metadata().map_err(|_| invalid())?;
            private(&current, true)?;
            if current.dev() != anchored.dev() || current.ino() != anchored.ino() {
                return Err(invalid());
            }
        }
        Ok(())
    }
    fn exists(&self, name: &str) -> Result<bool> {
        #[cfg(unix)]
        {
            use std::{ffi::CString, os::fd::AsRawFd};
            let name = CString::new(name).map_err(|_| invalid())?;
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            if unsafe {
                libc::fstatat(
                    self.directory.as_raw_fd(),
                    name.as_ptr(),
                    stat.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } == 0
            {
                return Ok(true);
            }
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
                Ok(false)
            } else {
                Err(invalid())
            }
        }
        #[cfg(windows)]
        {
            self.windows.exists(name)
        }
    }
    pub fn read(&self, name: &str, max: usize) -> Result<Vec<u8>> {
        self.check_location()?;
        #[cfg(unix)]
        {
            let file = self.open_child(name, false)?;
            if file.metadata().map_err(|_| invalid())?.len() > max as u64 {
                return Err(invalid());
            }
            let mut bytes = Vec::new();
            file.take(max as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| invalid())?;
            if bytes.len() > max {
                return Err(invalid());
            }
            Ok(bytes)
        }
        #[cfg(windows)]
        {
            self.windows.read(name, max)
        }
    }
    pub fn write(&self, name: &str, bytes: &[u8]) -> Result<()> {
        self.check_location()?;
        #[cfg(unix)]
        {
            if self.exists(name)? {
                let _ = self.open_child(name, false)?;
            }
            use std::{ffi::CString, os::fd::AsRawFd};
            let temp = format!(".pending-{}", uuid::Uuid::new_v4());
            let mut file = self.open_child(&temp, true)?;
            file.write_all(bytes)
                .and_then(|_| file.sync_all())
                .map_err(|_| invalid())?;
            let from = CString::new(temp).map_err(|_| invalid())?;
            let to = CString::new(name).map_err(|_| invalid())?;
            if unsafe {
                libc::renameat(
                    self.directory.as_raw_fd(),
                    from.as_ptr(),
                    self.directory.as_raw_fd(),
                    to.as_ptr(),
                )
            } != 0
            {
                return Err(invalid());
            }
            self.directory.sync_all().map_err(|_| invalid())?;
            Ok(())
        }
        #[cfg(windows)]
        {
            self.windows.write(name, bytes)
        }
    }
    pub fn names(&self) -> Result<Vec<String>> {
        self.check_location()?;
        #[cfg(unix)]
        {
            // File opening is always anchored; directory replacement cannot redirect
            // authentication or journal reads. Entries are treated as untrusted names.
            let mut names = Vec::new();
            for entry in std::fs::read_dir(&self.root).map_err(|_| invalid())? {
                let name = entry
                    .map_err(|_| invalid())?
                    .file_name()
                    .into_string()
                    .map_err(|_| invalid())?;
                if name.ends_with(".age") {
                    names.push(name);
                }
                if names.len() > 10_000 {
                    return Err(invalid());
                }
            }
            Ok(names)
        }
        #[cfg(windows)]
        {
            self.windows.names()
        }
    }
}
