//! Read-only filesystem case semantics for potentially aliasing legacy stems.
use super::anchored::{open_configured_store_with_mode, AnchoredDir};
use crate::backend::error::BackendError;
use std::path::Path;

pub(super) fn case_insensitive(store: &Path, vault: &str) -> Result<bool, BackendError> {
    case_insensitive_for_child(store, vault, "secrets")
}

pub(super) fn case_insensitive_for_child(
    store: &Path,
    vault: &str,
    child: &str,
) -> Result<bool, BackendError> {
    let mut directory =
        open_configured_store_with_mode(store, false, false)?.ok_or_else(unknown)?;
    // A not-yet-created descendant inherits the closest existing parent's
    // filesystem/directory semantics. Never create a probe or target directory.
    for component in ["vaults", vault, child] {
        match directory.open_dir(component)? {
            Some(child) => directory = child,
            None => break,
        }
    }
    directory_case_insensitive(&directory)
}

fn unknown() -> BackendError {
    BackendError::Unsupported(
        "cannot establish destination filesystem case semantics for potentially aliasing names"
            .into(),
    )
}

#[cfg(target_os = "macos")]
fn directory_case_insensitive(directory: &AnchoredDir) -> Result<bool, BackendError> {
    use std::os::fd::AsRawFd;
    // fpathconf reports the actual volume semantics, including case-sensitive APFS.
    match unsafe { libc::fpathconf(directory.file.as_raw_fd(), libc::_PC_CASE_SENSITIVE) } {
        0 => Ok(true),
        1 => Ok(false),
        _ => Err(unknown()),
    }
}

#[cfg(target_os = "linux")]
fn directory_case_insensitive(directory: &AnchoredDir) -> Result<bool, BackendError> {
    use std::os::fd::AsRawFd;
    let fd = directory.file.as_raw_fd();
    let mut info = std::mem::MaybeUninit::<libc::statfs>::uninit();
    if unsafe { libc::fstatfs(fd, info.as_mut_ptr()) } != 0 {
        return Err(unknown());
    }
    let kind = unsafe { info.assume_init() }.f_type;
    if kind == libc::EXT4_SUPER_MAGIC
        || kind == libc::F2FS_SUPER_MAGIC
        || kind == libc::BTRFS_SUPER_MAGIC
    {
        // The ioctl ABI writes an int, despite the request encoding using long.
        let mut flags: libc::c_int = 0;
        if unsafe { libc::ioctl(fd, libc::FS_IOC_GETFLAGS, &mut flags) } != 0 {
            return Err(unknown());
        }
        return Ok(flags & 0x4000_0000 != 0); // FS_CASEFOLD_FL
    }
    if kind == libc::TMPFS_MAGIC {
        return Ok(false);
    }
    // XFS may use its filesystem-wide ASCII-ci mode. Network/FUSE/overlay
    // mounts may expose another filesystem's semantics. Do not guess.
    Err(unknown())
}

#[cfg(windows)]
fn directory_case_insensitive(directory: &AnchoredDir) -> Result<bool, BackendError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FileCaseSensitiveInfo, GetFileInformationByHandleEx, FILE_CASE_SENSITIVE_INFO,
    };
    let mut info = FILE_CASE_SENSITIVE_INFO { Flags: 0 };
    let success = unsafe {
        GetFileInformationByHandleEx(
            directory.file.as_raw_handle(),
            FileCaseSensitiveInfo,
            (&mut info as *mut FILE_CASE_SENSITIVE_INFO).cast(),
            std::mem::size_of::<FILE_CASE_SENSITIVE_INFO>() as u32,
        )
    };
    if success == 0 {
        return Err(unknown());
    }
    Ok(info.Flags & 1 == 0) // FILE_CS_FLAG_CASE_SENSITIVE_DIR
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn directory_case_insensitive(_directory: &AnchoredDir) -> Result<bool, BackendError> {
    Err(unknown())
}
