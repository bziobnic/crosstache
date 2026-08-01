use std::collections::HashSet;

use axum::http::StatusCode;
use serde::Deserialize;

use crate::backend::FileBackend;

use super::api::ApiError;

pub(crate) const MAX_ARCHIVE_BODY_BYTES: usize = 512 * 1024;
const MAX_ARCHIVE_FILES: usize = 1000;
const MAX_ARCHIVE_NAME_BYTES: usize = 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArchiveRequest {
    files: Vec<String>,
}

fn archive_validation(message: &'static str) -> ApiError {
    ApiError::Validation {
        status: StatusCode::BAD_REQUEST,
        message,
        field: Some("files"),
    }
}

fn validate_names(files: &dyn FileBackend, names: &[String]) -> Result<(), ApiError> {
    if names.is_empty() || names.len() > MAX_ARCHIVE_FILES {
        return Err(archive_validation("Choose between 1 and 1000 files."));
    }

    let mut seen = HashSet::with_capacity(names.len());
    for name in names {
        let components = name.split('/').collect::<Vec<_>>();
        if name.len() > MAX_ARCHIVE_NAME_BYTES
            || name.starts_with('/')
            || name.contains('\\')
            || name.contains('\0')
            || components
                .iter()
                .any(|part| part.is_empty() || *part == "." || *part == "..")
            || !seen.insert(name)
        {
            return Err(archive_validation("Choose valid unique file names."));
        }
        files.validate_file_name(name)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_names;
    use crate::web::testutil::stub::StubBackend;

    #[test]
    fn archive_names_are_relative_unique_backend_validated_paths() {
        let backend = StubBackend::new();
        assert!(validate_names(&backend, &["root.txt".into(), "docs/report.pdf".into()],).is_ok());
        assert_eq!(backend.file_name_validation_calls(), 2);

        for names in [
            vec![],
            vec!["same.txt".into(), "same.txt".into()],
            vec!["/absolute.txt".into()],
            vec!["../outside.txt".into()],
            vec!["docs//empty.txt".into()],
            vec!["docs/./dot.txt".into()],
            vec!["docs\\windows.txt".into()],
            vec!["nul\0name.txt".into()],
            vec!["x".repeat(1025)],
        ] {
            assert!(
                validate_names(&backend, &names).is_err(),
                "accepted {names:?}"
            );
        }
    }

    #[test]
    fn archive_selection_is_bounded() {
        let backend = StubBackend::new();
        let names = (0..=1000)
            .map(|index| format!("{index}.txt"))
            .collect::<Vec<_>>();
        assert!(validate_names(&backend, &names).is_err());
    }

    #[test]
    fn archive_uses_the_selected_backends_name_rules() {
        let backend = StubBackend::new().with_file_name_limit(4);
        assert!(validate_names(&backend, &["five5".into()]).is_err());
    }
}
