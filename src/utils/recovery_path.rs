//! Resolve recovery paths before validation or creation, preserving no-follow custody.
use crate::error::{CrosstacheError, Result};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

pub(crate) fn resolve(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(CrosstacheError::invalid_argument(
            "Recovery directory must not be empty",
        ));
    }
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|e| CrosstacheError::config(e.to_string()))?
            .join(path)
    };
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => continue,
            Component::Prefix(_) => {
                resolved.push(component.as_os_str());
                continue;
            }
            Component::ParentDir => {
                resolved.pop();
                continue;
            }
            _ => resolved.push(component.as_os_str()),
        }
        match fs::symlink_metadata(&resolved) {
            Ok(metadata) => {
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if metadata.file_attributes() & 0x400 != 0 {
                        return Err(CrosstacheError::invalid_argument(
                            "Recovery path contains a reparse point",
                        ));
                    }
                }
                if metadata.file_type().is_symlink() {
                    // macOS system aliases are immutable compatibility paths. User
                    // aliases must not turn a rejected no-follow path into an accepted one.
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        let parent = fs::metadata(resolved.parent().ok_or_else(|| {
                            CrosstacheError::invalid_argument("Invalid recovery path")
                        })?)
                        .map_err(|e| CrosstacheError::config(e.to_string()))?;
                        if metadata.uid() != 0
                            || metadata.mode() & 0o022 != 0
                            || parent.uid() != 0
                            || parent.mode() & 0o022 != 0
                        {
                            return Err(CrosstacheError::invalid_argument(
                                "Recovery path contains an unsafe symlink",
                            ));
                        }
                        resolved = fs::canonicalize(&resolved)
                            .map_err(|e| CrosstacheError::config(e.to_string()))?;
                    }
                    #[cfg(not(unix))]
                    return Err(CrosstacheError::invalid_argument(
                        "Recovery path contains a symlink",
                    ));
                } else if !metadata.is_dir() {
                    return Err(CrosstacheError::invalid_argument(
                        "Recovery path contains a non-directory",
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(CrosstacheError::config(format!(
                    "Inspect recovery path: {error}"
                )))
            }
        }
    }
    Ok(resolved)
}

pub(crate) fn in_git(path: &Path) -> Result<bool> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor.join(".git")) {
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(CrosstacheError::config(format!(
                    "Inspect recovery Git boundary: {error}"
                )))
            }
        }
    }
    Ok(false)
}
