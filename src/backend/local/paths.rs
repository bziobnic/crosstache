//! Shared path safety helpers for the local backend.

use std::path::{Component, Path, PathBuf};

use crate::backend::error::BackendError;

/// Validate that a local vault name is a single safe filesystem component.
///
/// Local vault names are used as directory names under `<store>/vaults/`.
/// Reject separators, parent/current-directory components, Windows prefixes,
/// control characters, and names that are awkward or unsafe on common filesystems.
pub(crate) fn validate_vault_name(name: &str) -> Result<(), BackendError> {
    if name.is_empty() {
        return Err(invalid_vault_name(name, "vault name cannot be empty"));
    }

    if name == "." || name == ".." {
        return Err(invalid_vault_name(
            name,
            "vault name must not be '.' or '..'",
        ));
    }

    if name.contains('/') || name.contains('\\') {
        return Err(invalid_vault_name(
            name,
            "vault name must not contain path separators",
        ));
    }

    if name.starts_with('-') {
        return Err(invalid_vault_name(
            name,
            "vault name must not start with '-'",
        ));
    }

    if name.chars().any(|ch| ch.is_control()) {
        return Err(invalid_vault_name(
            name,
            "vault name must not contain control characters",
        ));
    }

    let path = Path::new(name);
    let mut components = path.components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(component)), None) if component == name => Ok(()),
        _ => Err(invalid_vault_name(
            name,
            "vault name must be a single path component without separators or prefixes",
        )),
    }
}

pub(crate) fn vaults_dir(store_path: &Path) -> PathBuf {
    store_path.join("vaults")
}

pub(crate) fn vault_dir(store_path: &Path, name: &str) -> Result<PathBuf, BackendError> {
    validate_vault_name(name)?;
    let vaults_dir = vaults_dir(store_path);
    let candidate = vaults_dir.join(name);
    ensure_child_path(&vaults_dir, &candidate, name)?;
    Ok(candidate)
}

pub(crate) fn secrets_dir(store_path: &Path, vault: &str) -> Result<PathBuf, BackendError> {
    Ok(vault_dir(store_path, vault)?.join("secrets"))
}

pub(crate) fn trash_base_dir(store_path: &Path, vault: &str) -> Result<PathBuf, BackendError> {
    Ok(vault_dir(store_path, vault)?.join(".trash"))
}

pub(crate) fn files_dir(store_path: &Path, vault: &str) -> Result<PathBuf, BackendError> {
    Ok(vault_dir(store_path, vault)?.join("files"))
}

fn invalid_vault_name(name: &str, reason: &str) -> BackendError {
    BackendError::InvalidArgument(format!(
        "invalid local vault name '{name}': {reason}. Use a single directory name such as 'default' or 'work-secrets'."
    ))
}

fn ensure_child_path(base: &Path, candidate: &Path, name: &str) -> Result<(), BackendError> {
    if candidate.starts_with(base) {
        Ok(())
    } else {
        Err(invalid_vault_name(
            name,
            "resolved vault path escapes the local vaults directory",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_single_component_vault_names() {
        for name in ["default", "work-secrets", "team_1", "Vault123"] {
            validate_vault_name(name).expect(name);
        }
    }

    #[test]
    fn rejects_path_traversal_and_separators() {
        for name in [
            "",
            ".",
            "..",
            "../outside",
            "../../outside",
            "outside/child",
            "/absolute",
            "\\absolute",
            "parent\\child",
            "bad\nname",
        ] {
            assert!(validate_vault_name(name).is_err(), "{name:?}");
        }
    }

    #[test]
    fn vault_dir_never_escapes_vaults_root() {
        let store = PathBuf::from("/tmp/store");
        let path = vault_dir(&store, "default").unwrap();
        assert_eq!(path, store.join("vaults").join("default"));
        assert!(vault_dir(&store, "../../outside").is_err());
    }
}
