use std::collections::HashSet;
use std::io::{Seek, SeekFrom, Write};
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{Json, Query, State};
use axum::http::{header, StatusCode};
use axum::response::Response;
use serde::Deserialize;
use tokio::io::AsyncReadExt;
use zip::result::ZipError;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::backend::FileBackend;
use crate::error::CrosstacheError;
use crate::secret::attachments;

use super::api::{ApiError, VaultQuery};
use super::WebState;

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

fn zip_io(error: ZipError) -> std::io::Error {
    std::io::Error::other(error)
}

fn internal_archive_error(error: impl std::fmt::Display) -> ApiError {
    ApiError::App(CrosstacheError::Unknown(format!(
        "file archive creation failed: {error}",
    )))
}

fn join_error(error: tokio::task::JoinError) -> ApiError {
    internal_archive_error(error)
}

fn response_error(error: axum::http::Error) -> ApiError {
    internal_archive_error(error)
}

fn files_unsupported() -> ApiError {
    ApiError::Validation {
        status: StatusCode::NOT_IMPLEMENTED,
        message: "This backend does not provide file storage.",
        field: None,
    }
}

async fn add_entry(
    writer: ZipWriter<std::fs::File>,
    name: String,
    bytes: Vec<u8>,
) -> Result<ZipWriter<std::fs::File>, ApiError> {
    tokio::task::spawn_blocking(move || {
        let mut writer = writer;
        writer
            .start_file(
                name,
                SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
            )
            .map_err(zip_io)?;
        writer.write_all(&bytes)?;
        Ok::<_, std::io::Error>(writer)
    })
    .await
    .map_err(join_error)?
    .map_err(CrosstacheError::from)
    .map_err(ApiError::from)
}

async fn finish_archive(writer: ZipWriter<std::fs::File>) -> Result<std::fs::File, ApiError> {
    tokio::task::spawn_blocking(move || {
        let mut file = writer.finish().map_err(zip_io)?;
        file.seek(SeekFrom::Start(0))?;
        Ok::<_, std::io::Error>(file)
    })
    .await
    .map_err(join_error)?
    .map_err(CrosstacheError::from)
    .map_err(ApiError::from)
}

pub(crate) async fn download(
    State(state): State<Arc<WebState>>,
    Query(query): Query<VaultQuery>,
    Json(request): Json<ArchiveRequest>,
) -> Result<Response, ApiError> {
    let target = query.target(&state)?;
    let files = target.backend.files().ok_or_else(files_unsupported)?;
    validate_names(files, &request.files)?;

    let mut writer = ZipWriter::new(tempfile::tempfile().map_err(CrosstacheError::from)?);
    for name in request.files {
        let bytes = attachments::download_decrypted(
            target.backend.secrets(),
            files,
            &target.context.vault,
            &name,
            None,
        )
        .await?;
        writer = add_entry(writer, name, bytes).await?;
    }

    let file = finish_archive(writer).await?;
    let stream =
        futures::stream::try_unfold(tokio::fs::File::from_std(file), |mut file| async move {
            let mut buffer = vec![0; 64 * 1024];
            let read = file.read(&mut buffer).await?;
            if read == 0 {
                Ok::<_, std::io::Error>(None)
            } else {
                buffer.truncate(read);
                Ok(Some((Bytes::from(buffer), file)))
            }
        });
    Response::builder()
        .header(header::CONTENT_TYPE, "application/zip")
        .header(
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"crosstache-files.zip\"",
        )
        .body(Body::from_stream(stream))
        .map_err(response_error)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::{Cursor, Read, Seek};
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{header, HeaderValue, Request, StatusCode};
    use http_body_util::BodyExt;
    use serde_json::{json, Value};
    use tower::ServiceExt;
    use zip::ZipArchive;

    use super::validate_names;
    use crate::backend::FileBackend;
    use crate::blob::models::FileUploadRequest;
    use crate::secret::attachments;
    use crate::web::{self, testutil};

    use testutil::stub::StubBackend;

    fn file_request(name: &str, content: &[u8]) -> FileUploadRequest {
        FileUploadRequest {
            name: name.into(),
            content: content.to_vec(),
            content_type: Some("application/octet-stream".into()),
            groups: Vec::new(),
            metadata: HashMap::new(),
            tags: HashMap::new(),
        }
    }

    fn archive_request(uri: &str, body: Value) -> Request<Body> {
        Request::post(uri)
            .header(header::HOST, "127.0.0.1:1")
            .header(header::AUTHORIZATION, "Bearer test-token")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    fn read_entry<R: Read + Seek>(archive: &mut ZipArchive<R>, name: &str) -> Vec<u8> {
        let mut entry = archive.by_name(name).unwrap();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        bytes
    }

    fn scoped_state(
        primary: Arc<testutil::stub::StubBackend>,
        secondary: Arc<testutil::stub::StubBackend>,
    ) -> Arc<web::WebState> {
        let mut context = testutil::test_context(primary.as_ref(), "default", 30);
        context
            .workspace
            .entries
            .push(super::super::context::WorkspaceEntrySummary {
                alias: "secondary-workspace".into(),
                backend: "secondary".into(),
                vault: "other".into(),
                default: false,
            });
        let registry = Arc::new(crate::backend::BackendRegistry::for_test(
            "primary",
            vec![
                ("primary", primary.clone()),
                ("secondary", secondary.clone()),
            ],
        ));
        Arc::new(web::WebState::new(
            primary,
            context,
            "test-token".into(),
            crate::records::builtin_types(),
            super::super::preferences::PreferenceStore::new(
                std::env::temp_dir()
                    .join(format!("xv-archive-scope-{}", uuid::Uuid::new_v4()))
                    .join("ui.json"),
                30,
            ),
            registry,
        ))
    }

    #[tokio::test]
    async fn archive_contains_exact_selected_plaintext_and_paths() {
        let state = testutil::test_state();
        let backend = state.base_backend();
        let files = backend.files().unwrap();
        for (name, content) in [
            ("root.txt", b"root".as_slice()),
            ("docs/report.txt", b"report"),
        ] {
            files
                .upload_file("default", file_request(name, content), None)
                .await
                .unwrap();
        }

        let response = web::build_router(state)
            .oneshot(archive_request(
                "/api/files/archive",
                json!({"files": ["docs/report.txt", "root.txt"]}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/zip");
        assert_eq!(
            response.headers()[header::CONTENT_DISPOSITION],
            "attachment; filename=\"crosstache-files.zip\"",
        );

        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        assert_eq!(read_entry(&mut archive, "docs/report.txt"), b"report");
        assert_eq!(read_entry(&mut archive, "root.txt"), b"root");
        assert_eq!(archive.len(), 2);
    }

    #[tokio::test]
    async fn archive_transparently_decrypts_crosstache_files() {
        let state = testutil::test_state();
        let backend = state.base_backend();
        attachments::upload_encrypted(
            backend.secrets(),
            backend.files().unwrap(),
            "default",
            file_request("private/secret.txt", b"plaintext"),
            None,
        )
        .await
        .unwrap();
        let response = web::build_router(state)
            .oneshot(archive_request(
                "/api/files/archive",
                json!({"files": ["private/secret.txt"]}),
            ))
            .await
            .unwrap();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        assert_eq!(read_entry(&mut archive, "private/secret.txt"), b"plaintext");
    }

    #[tokio::test]
    async fn archive_uses_the_exact_selected_workspace_backend_and_vault() {
        let capabilities = crate::backend::BackendCapabilities {
            has_file_storage: true,
            ..Default::default()
        };
        let primary = Arc::new(testutil::stub::StubBackend::with_capabilities(
            "primary",
            capabilities.clone(),
        ));
        let secondary = Arc::new(testutil::stub::StubBackend::with_capabilities(
            "secondary",
            capabilities,
        ));
        primary
            .upload_file("default", file_request("same.txt", b"primary"), None)
            .await
            .unwrap();
        secondary
            .upload_file("other", file_request("same.txt", b"secondary"), None)
            .await
            .unwrap();
        let response = web::build_router(scoped_state(primary, secondary))
            .oneshot(archive_request(
                "/api/files/archive?alias=secondary-workspace&backend=secondary&vault=other",
                json!({"files": ["same.txt"]}),
            ))
            .await
            .unwrap();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        assert_eq!(read_entry(&mut archive, "same.txt"), b"secondary");
    }

    #[tokio::test]
    async fn invalid_or_unreadable_archive_never_returns_zip() {
        for body in [
            json!({"files": []}),
            json!({"files": ["same", "same"]}),
            json!({"files": ["../outside"]}),
            json!({"files": ["missing.txt"]}),
        ] {
            let response = web::build_router(testutil::test_state())
                .oneshot(archive_request("/api/files/archive", body))
                .await
                .unwrap();
            assert_ne!(response.status(), StatusCode::OK);
            assert_ne!(
                response.headers().get(header::CONTENT_TYPE),
                Some(&HeaderValue::from_static("application/zip")),
            );
        }
    }

    #[tokio::test]
    async fn archive_json_body_is_limited_to_512_kib() {
        let names = (0..1000)
            .map(|index| format!("{index}-{}", "x".repeat(600)))
            .collect::<Vec<_>>();
        let response = web::build_router(testutil::test_state())
            .oneshot(archive_request(
                "/api/files/archive",
                json!({"files": names}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

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
