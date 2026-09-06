//! Read-only local transfer identities and anchored destination preparation.
use super::{
    anchored::{open_configured_store_with_mode, AnchoredDir},
    paths,
};
use crate::backend::error::BackendError;
use sha2::{Digest, Sha256};
use std::path::Path;
/// Resolve through the same anchored directory chain as file operations.
pub(super) fn transfer_location(
    store: &Path,
    vault: &str,
) -> Result<crate::backend::TransferLocation, BackendError> {
    discover(store, vault, true)
}

pub(super) fn secret_namespace(store: &Path, vault: &str) -> Result<String, BackendError> {
    Ok(discover(store, vault, false)?.secrets)
}

fn discover(
    store: &Path,
    vault: &str,
    include_files: bool,
) -> Result<crate::backend::TransferLocation, BackendError> {
    paths::validate_vault_name(vault)?;
    let missing = || BackendError::Unsupported("local transfer namespace does not exist".into());
    let store = open_configured_store_with_mode(store, false, false)?.ok_or_else(missing)?;
    let vaults = store.open_dir("vaults")?.ok_or_else(missing)?;
    let vault_dir = vaults.open_dir(vault)?.ok_or_else(missing)?;
    let files = if include_files {
        vault_dir.open_dir("files")?
    } else {
        None
    };
    let secrets = vault_dir.open_dir("secrets")?.ok_or_else(missing)?;
    location_from_dirs(&store, &vaults, &vault_dir, &secrets, files.as_ref())
}

fn location_from_dirs(
    store: &AnchoredDir,
    vaults: &AnchoredDir,
    vault_dir: &AnchoredDir,
    secrets: &AnchoredDir,
    files: Option<&AnchoredDir>,
) -> Result<crate::backend::TransferLocation, BackendError> {
    let identity = |dir: &AnchoredDir| -> Result<String, BackendError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let meta = dir
                .file
                .metadata()
                .map_err(|e| BackendError::Internal(format!("inspect transfer directory: {e}")))?;
            Ok(format!("{}:{}", meta.dev(), meta.ino()))
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Storage::FileSystem::{
                GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
            };
            let mut info = BY_HANDLE_FILE_INFORMATION::default();
            if unsafe { GetFileInformationByHandle(dir.file.as_raw_handle().cast(), &mut info) }
                == 0
            {
                return Err(BackendError::Internal(format!(
                    "inspect transfer directory: {}",
                    std::io::Error::last_os_error()
                )));
            }
            Ok(format!(
                "{}:{}:{}",
                info.dwVolumeSerialNumber, info.nFileIndexHigh, info.nFileIndexLow
            ))
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = dir;
            Err(BackendError::Unsupported(
                "stable directory identities".into(),
            ))
        }
    };
    let base = format!(
        "{}:{}:{}",
        identity(store)?,
        identity(vaults)?,
        identity(vault_dir)?
    );
    let namespace = |kind: &str, id: String| {
        format!(
            "local:{:x}",
            Sha256::digest(format!("{kind}:{base}:{id}").as_bytes())
        )
    };
    let secret_id = identity(secrets)?;
    Ok(crate::backend::TransferLocation {
        secrets: namespace("secrets", secret_id.clone()),
        files: match files {
            Some(files) => namespace("files", identity(files)?),
            None => format!(
                "local-pending-files:{:x}",
                Sha256::digest(format!("pending-files:{base}:{secret_id}").as_bytes())
            ),
        },
        keys: namespace("keys", secret_id),
    })
}

pub(super) fn prepare(
    store: &Path,
    vault: &str,
    expected: &crate::backend::TransferLocation,
) -> Result<crate::backend::TransferLocation, BackendError> {
    paths::validate_vault_name(vault)?;
    let changed = || BackendError::Conflict("local transfer destination namespace changed".into());
    let store_dir = open_configured_store_with_mode(store, false, false)?.ok_or_else(changed)?;
    let vaults = store_dir.open_dir("vaults")?.ok_or_else(changed)?;
    let vault_dir = vaults.open_dir(vault)?.ok_or_else(changed)?;
    let secrets = vault_dir.open_dir("secrets")?.ok_or_else(changed)?;
    let pending = location_from_dirs(&store_dir, &vaults, &vault_dir, &secrets, None)?;
    if expected.files.starts_with("local-pending-files:") {
        if &pending != expected {
            return Err(changed());
        }
        // The same retained parent handles are used for comparison and mkdir.
        // Before durable pinning only an entirely empty private child may be adopted.
        let files = vault_dir.open_or_create_checked_private_dir("files")?;
        if !files.entry_names()?.is_empty() {
            return Err(BackendError::Conflict(
                "uninitialized transfer files directory is not empty".into(),
            ));
        }
        files.sync()?;
        vault_dir.sync()?;
        let actual = location_from_dirs(&store_dir, &vaults, &vault_dir, &secrets, Some(&files))?;
        if transfer_location(store, vault)? != actual {
            return Err(changed());
        }
        Ok(actual)
    } else {
        let files = vault_dir.open_dir("files")?.ok_or_else(changed)?;
        let actual = location_from_dirs(&store_dir, &vaults, &vault_dir, &secrets, Some(&files))?;
        if &actual != expected {
            return Err(changed());
        }
        Ok(actual)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    fn store() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("vaults/default/secrets")).unwrap();
        temp
    }
    #[test]
    fn transfer_prepare_pins_fresh_files_and_rejects_contents_and_replaced_parent() {
        let temp = store();
        let pending = transfer_location(temp.path(), "default").unwrap();
        let real = prepare(temp.path(), "default", &pending).unwrap();
        assert!(real.files.starts_with("local:"));
        assert_eq!(real.secrets, pending.secrets);
        assert_eq!(prepare(temp.path(), "default", &pending).unwrap(), real);
        fs::write(
            temp.path().join("vaults/default/files/.hidden"),
            b"unexpected",
        )
        .unwrap();
        assert!(prepare(temp.path(), "default", &pending).is_err());
        assert_eq!(prepare(temp.path(), "default", &real).unwrap(), real);
        fs::rename(
            temp.path().join("vaults/default/secrets"),
            temp.path().join("vaults/default/secrets-old"),
        )
        .unwrap();
        fs::create_dir(temp.path().join("vaults/default/secrets")).unwrap();
        assert!(prepare(temp.path(), "default", &real).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn transfer_prepare_rejects_nonprivate_empty_child_without_repair() {
        use std::os::unix::fs::PermissionsExt;
        let temp = store();
        let pending = transfer_location(temp.path(), "default").unwrap();
        let files = temp.path().join("vaults/default/files");
        fs::create_dir(&files).unwrap();
        fs::set_permissions(&files, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(prepare(temp.path(), "default", &pending).is_err());
        assert_eq!(
            fs::metadata(files).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
    #[cfg(unix)]
    #[test]
    fn secret_identity_never_depends_on_files_and_v2_hashes_are_unchanged() {
        use std::os::unix::fs::MetadataExt;
        let temp = store();
        let before = secret_namespace(temp.path(), "default").unwrap();
        let id = |p: &Path| {
            let m = fs::metadata(p).unwrap();
            format!("{}:{}", m.dev(), m.ino())
        };
        let base = format!(
            "{}:{}:{}",
            id(temp.path()),
            id(&temp.path().join("vaults")),
            id(&temp.path().join("vaults/default"))
        );
        let secret_id = id(&temp.path().join("vaults/default/secrets"));
        assert_eq!(
            before,
            format!(
                "local:{:x}",
                Sha256::digest(format!("secrets:{base}:{secret_id}").as_bytes())
            )
        );
        let files = temp.path().join("vaults/default/files");
        fs::create_dir(&files).unwrap();
        let existing = transfer_location(temp.path(), "default").unwrap();
        assert_eq!(
            existing.keys,
            format!(
                "local:{:x}",
                Sha256::digest(format!("keys:{base}:{secret_id}").as_bytes())
            )
        );
        assert_eq!(
            existing.files,
            format!(
                "local:{:x}",
                Sha256::digest(format!("files:{base}:{}", id(&files)).as_bytes())
            )
        );
        fs::remove_dir(files).unwrap();
        std::os::unix::fs::symlink("secrets", temp.path().join("vaults/default/files")).unwrap();
        assert_eq!(secret_namespace(temp.path(), "default").unwrap(), before);
        assert!(transfer_location(temp.path(), "default").is_err());
    }
}
