# Files Bulk Download Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a Files-tab bulk action that downloads the exact selected files as one backend-independent ZIP archive.

**Architecture:** An authenticated, scoped Rust endpoint validates logical file names, reads and transparently decrypts each file through `FileBackend`, writes the archive to an unnamed temporary file, and streams the finished ZIP. The existing Files selection state calls that endpoint through a small JavaScript download primitive and keeps selection intact across success and failure.

**Tech Stack:** Rust 2021, Axum 0.8, Tokio, `zip` 2, `tempfile` 3, JavaScript ES modules, Node test runner.

## Global Constraints

- The feature must work through `Backend::files()` and `FileBackend`; it must not dispatch on Local, AWS, or Azure types.
- The request accepts 1 to 1,000 names, at most 512 KiB of JSON, with each UTF-8 name at most 1,024 bytes.
- ZIP entry paths are relative forward-slash paths with no empty, `.` or `..` component, backslash, NUL, or duplicate name.
- Plain and Crosstache age-encrypted files must match existing single-file download semantics.
- The response is all-or-nothing and downloads as `crosstache-files.zip`.
- Selection remains active and unchanged after either success or failure.
- The server holds at most one downloaded file in memory while archive bytes live in an unnamed temporary file.

---

## File Structure

- Create `src/web/archive.rs`: archive request model, validation, backend-neutral ZIP construction, streaming response, and focused Rust tests.
- Modify `src/web/mod.rs`: declare the archive module and register the bounded `POST /api/files/archive` route before `/files/{name}`.
- Modify `Cargo.toml`: make the already-locked `zip = "2"` dependency available on all platforms; keep only `windows-sys` target-specific.
- Modify `Cargo.lock`: retain the resolved ZIP dependency metadata if Cargo rewrites the lockfile.
- Modify `src/web/assets/files.js`: export the browser primitive that saves a raw archive response and always revokes its object URL.
- Modify `src/web/assets/files.test.js`: test the archive browser primitive independently of selection UI.
- Modify `src/web/assets/index.html`: add the Files bulk `Download` button.
- Modify `src/web/assets/secrets.js`: bind the button to existing file selection, scoped routing, pending state, toast, and Files error UI.
- Modify `src/web/assets/secrets.routes.test.js`: test selection enablement, exact request scope/body, retained selection, and failure recovery.

---

### Task 1: Archive Request Validation

**Files:**
- Create: `src/web/archive.rs`
- Modify: `src/web/mod.rs:20-27`
- Test: `src/web/archive.rs` inline `#[cfg(test)]` module

**Interfaces:**
- Consumes: `crate::backend::FileBackend`, `super::api::ApiError`.
- Produces: `pub(crate) const MAX_ARCHIVE_BODY_BYTES: usize = 512 * 1024`, `pub(crate) ArchiveRequest { files: Vec<String> }`, and `fn validate_names(files: &dyn FileBackend, names: &[String]) -> Result<(), ApiError>`.

- [ ] **Step 1: Declare the module and write failing validation tests**

Add `#[cfg(feature = "file-ops")] mod archive;` beside `mod files;` in `src/web/mod.rs`. Create `src/web/archive.rs` with table-driven tests that use `testutil::stub::StubBackend` and require:

```rust
#[test]
fn archive_names_are_relative_unique_backend_validated_paths() {
    let backend = StubBackend::new();
    assert!(validate_names(
        &backend,
        &["root.txt".into(), "docs/report.pdf".into()],
    )
    .is_ok());
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
        assert!(validate_names(&backend, &names).is_err(), "accepted {names:?}");
    }
}

#[test]
fn archive_selection_is_bounded() {
    let backend = StubBackend::new();
    let names = (0..=1000).map(|index| format!("{index}.txt")).collect::<Vec<_>>();
    assert!(validate_names(&backend, &names).is_err());
}

#[test]
fn archive_uses_the_selected_backends_name_rules() {
    let backend = StubBackend::new().with_file_name_limit(4);
    assert!(validate_names(&backend, &["five5".into()]).is_err());
}
```

- [ ] **Step 2: Run the validation test and verify RED**

Run:

```bash
cargo test web::archive::tests::archive_names --features ui,file-ops
```

Expected: compilation fails because `validate_names` and the archive request constants do not exist.

- [ ] **Step 3: Implement the minimal validation contract**

Add the exact request bounds and a denied-path check, call `files.validate_file_name(name)` for every otherwise-valid name, and reject duplicates with a `HashSet`:

```rust
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
            || components.iter().any(|part| part.is_empty() || *part == "." || *part == "..")
            || !seen.insert(name)
        {
            return Err(archive_validation("Choose valid unique file names."));
        }
        files.validate_file_name(name)?;
    }
    Ok(())
}
```

- [ ] **Step 4: Run all archive validation tests and verify GREEN**

Run:

```bash
cargo test web::archive::tests::archive_ --features ui,file-ops
```

Expected: all three validation tests pass.

- [ ] **Step 5: Commit the validation unit**

```bash
git add src/web/archive.rs src/web/mod.rs
git commit -m "feat(web): validate bulk archive selections"
```

---

### Task 2: Backend-Neutral ZIP Endpoint

**Files:**
- Modify: `Cargo.toml:103-122`
- Modify: `Cargo.lock`
- Modify: `src/web/archive.rs`
- Modify: `src/web/mod.rs:320-346`
- Test: `src/web/archive.rs` inline `#[cfg(test)]` module

**Interfaces:**
- Consumes: Task 1 `ArchiveRequest`, `MAX_ARCHIVE_BODY_BYTES`, and `validate_names`; `VaultQuery::target`; `attachments::download_decrypted`.
- Produces: `pub(crate) async fn download(State<Arc<WebState>>, Query<VaultQuery>, Json<ArchiveRequest>) -> Result<Response, ApiError>` registered at `POST /api/files/archive`.

- [ ] **Step 1: Write all failing endpoint behavior tests**

First add concrete test helpers for stored files, authenticated JSON requests,
and ZIP entry reads:

```rust
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
```

Use `testutil::test_state()`, upload two files through its trait object, request
the archive, collect its body, and inspect it with `zip::ZipArchive`:

```rust
#[tokio::test]
async fn archive_contains_exact_selected_plaintext_and_paths() {
    let state = testutil::test_state();
    let backend = state.base_backend();
    let files = backend.files().unwrap();
    for (name, content) in [("root.txt", b"root".as_slice()), ("docs/report.txt", b"report")]
    {
        files.upload_file("default", file_request(name, content), None).await.unwrap();
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
```

Before touching endpoint production code, also add the concrete decryption,
immutable-scope, invalid-selection, unreadable-file, and body-limit tests shown
in Step 6 below. They form one RED endpoint contract and must all exist before
Step 2 runs.

- [ ] **Step 2: Run the endpoint test and verify RED**

Run:

```bash
cargo test web::archive::tests --features ui,file-ops
```

Expected: every route-level test fails with `404 Not Found` because
`/api/files/archive` is not registered. The pure Task 1 validation tests remain
green.

- [ ] **Step 3: Make ZIP available cross-platform and implement archive writing**

Move `zip = "2"` from `[target.'cfg(target_os = "windows")'.dependencies]` into normal `[dependencies]`; leave `windows-sys` in the Windows table. Run `cargo check --features ui,file-ops` once so Cargo normalizes `Cargo.lock`.

In `src/web/archive.rs`, implement one-file-at-a-time ZIP writing by moving `ZipWriter<std::fs::File>` through `spawn_blocking`:

```rust
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
```

Use these error and completion helpers; they deliberately omit selected names
and backend diagnostics from response copy:

```rust
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

async fn finish_archive(
    writer: ZipWriter<std::fs::File>,
) -> Result<std::fs::File, ApiError> {
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
```

- [ ] **Step 4: Implement the scoped handler and streamed response**

Resolve the target once, validate through its `FileBackend`, then use the same helper as single-file downloads:

```rust
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
    let stream = futures::stream::try_unfold(
        tokio::fs::File::from_std(file),
        |mut file| async move {
            let mut buffer = vec![0; 64 * 1024];
            let read = file.read(&mut buffer).await?;
            if read == 0 {
                Ok(None)
            } else {
                buffer.truncate(read);
                Ok(Some((Bytes::from(buffer), file)))
            }
        },
    );
    Response::builder()
        .header(header::CONTENT_TYPE, "application/zip")
        .header(
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"crosstache-files.zip\"",
        )
        .body(Body::from_stream(stream))
        .map_err(response_error)
}
```

`finish_archive` must call `ZipWriter::finish`, seek to `SeekFrom::Start(0)`, and return the still-open unnamed file from `spawn_blocking`. Register the route before `/files/{name}`:

```rust
.route(
    "/files/archive",
    post(archive::download).layer(
        axum::extract::DefaultBodyLimit::max(archive::MAX_ARCHIVE_BODY_BYTES),
    ),
)
```

- [ ] **Step 5: Run the exact-content test and verify GREEN**

Run:

```bash
cargo test web::archive::tests::archive_contains_exact --features ui,file-ops
```

Expected: PASS with two readable entries and the exact response headers.

- [ ] **Step 6: Verify the complete endpoint contract is GREEN**

The scoped-state helper and route tests added during Step 1 are:

```rust
fn scoped_state(
    primary: Arc<testutil::stub::StubBackend>,
    secondary: Arc<testutil::stub::StubBackend>,
) -> Arc<web::WebState> {
    let mut context = testutil::test_context(primary.as_ref(), "default", 30);
    context.workspace.entries.push(
        super::super::context::WorkspaceEntrySummary {
            alias: "secondary-workspace".into(),
            backend: "secondary".into(),
            vault: "other".into(),
            default: false,
        },
    );
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
```

- [ ] **Step 7: Correct only defects exposed by the complete endpoint suite**

Run twice:

```bash
cargo test web::archive::tests --features ui,file-ops
```

Expected: every archive test passes. If a test fails, adjust only handler error
conversion, archive construction, or the concrete fixture setup, then rerun the
same command until it exits 0.

- [ ] **Step 8: Commit the backend endpoint**

```bash
git add Cargo.toml Cargo.lock src/web/archive.rs src/web/mod.rs
git commit -m "feat(web): stream selected files as zip"
```

---

### Task 3: Browser Archive Download Primitive

**Files:**
- Modify: `src/web/assets/files.js`
- Modify: `src/web/assets/files.test.js`

**Interfaces:**
- Consumes: the existing `api(method, path, body, raw)` contract.
- Produces: `downloadFileArchive({ api, path, names, document, objectUrls }) -> Promise<void>`.

- [ ] **Step 1: Write failing save-and-cleanup tests**

Import `downloadFileArchive` in `files.test.js` and add:

```javascript
test('archive download posts exact names, clicks one zip anchor, and revokes its URL', async () => {
  const calls = [];
  const anchors = [];
  const api = async (...args) => {
    calls.push(args);
    return { blob: async () => new Blob(['zip']) };
  };
  const document = { createElement: () => {
    const anchor = { clickCount: 0, click() { this.clickCount++; } };
    anchors.push(anchor);
    return anchor;
  } };
  const revoked = [];
  const objectUrls = {
    createObjectURL: () => 'blob:archive',
    revokeObjectURL: (value) => revoked.push(value),
  };

  await downloadFileArchive({
    api,
    path: '/api/files/archive?alias=primary&backend=local&vault=one',
    names: ['docs/a.txt', 'b.txt'],
    document,
    objectUrls,
  });

  assert.deepEqual(calls, [[
    'POST',
    '/api/files/archive?alias=primary&backend=local&vault=one',
    { files: ['docs/a.txt', 'b.txt'] },
    true,
  ]]);
  assert.equal(anchors[0].download, 'crosstache-files.zip');
  assert.equal(anchors[0].href, 'blob:archive');
  assert.equal(anchors[0].clickCount, 1);
  assert.deepEqual(revoked, ['blob:archive']);
});

test('archive download does not create an anchor when the API fails', async () => {
  let created = 0;
  await assert.rejects(() => downloadFileArchive({
    api: async () => { throw new Error('failed'); },
    path: '/api/files/archive',
    names: ['a.txt'],
    document: { createElement() { created++; } },
    objectUrls: {},
  }), /failed/);
  assert.equal(created, 0);
});
```

- [ ] **Step 2: Run the focused JavaScript test and verify RED**

Run:

```bash
node --test --test-name-pattern="archive download" src/web/assets/files.test.js
```

Expected: import failure because `downloadFileArchive` is not exported.

- [ ] **Step 3: Implement the minimal browser primitive**

Add to `files.js`:

```javascript
export async function downloadFileArchive({
  api,
  path,
  names,
  document = globalThis.document,
  objectUrls = globalThis.URL,
}) {
  const response = await api('POST', path, { files: [...names] }, true);
  const blob = await response.blob();
  const href = objectUrls.createObjectURL(blob);
  try {
    const anchor = document.createElement('a');
    anchor.href = href;
    anchor.download = 'crosstache-files.zip';
    anchor.click();
  } finally {
    objectUrls.revokeObjectURL(href);
  }
}
```

- [ ] **Step 4: Run the focused and complete files tests**

Run:

```bash
node --test --test-name-pattern="archive download" src/web/assets/files.test.js
node --test src/web/assets/files.test.js
```

Expected: both commands pass with zero failures.

- [ ] **Step 5: Commit the browser primitive**

```bash
git add src/web/assets/files.js src/web/assets/files.test.js
git commit -m "feat(web): add zip download primitive"
```

---

### Task 4: Files Selection Toolbar Integration

**Files:**
- Modify: `src/web/assets/index.html:148-153`
- Modify: `src/web/assets/secrets.js:920-1025,3470-3820`
- Modify: `src/web/assets/secrets.routes.test.js:15-115,730-820`

**Interfaces:**
- Consumes: Task 3 `downloadFileArchive`, existing `fileSelection.ids`, `vaultQS`, `canStartScopedAction`, `showListError`, and `toast`.
- Produces: `#bulk-download-files` and private `bulkDownloadFiles() -> Promise<void>`.

- [ ] **Step 1: Write failing success and recovery route tests**

Extend the route-test `Element` with a real no-navigation click recorder:

```javascript
click() {
  this.clickCount = (this.clickCount || 0) + 1;
  return this.onclick?.({ preventDefault() {}, stopPropagation() {}, currentTarget: this });
}
```

Set `this.tagName = id.startsWith('#') ? '' : id.toUpperCase()` in the test
element constructor. In `createDocument`, retain created elements so the test
can inspect the generated anchor:

```javascript
const createdElements = [];
document.createdElements = createdElements;
document.createElement = (name) => {
  const element = new Element(name, document);
  createdElements.push(element);
  return element;
};
```

Then mount a files-capable context with two listed files and a raw archive response. Enter Files selection mode, select the folder or both item checkboxes, and assert:

```javascript
assert.equal(ui.elements.get('#bulk-download-files').disabled, false);
await ui.elements.get('#bulk-download-files').onclick();
assert.deepEqual(archiveCalls, [{
  method: 'POST',
  path: '/api/files/archive?alias=primary&backend=local&vault=one',
  body: { files: ['docs/a.txt', 'docs/b.txt'] },
  raw: true,
}]);
assert.equal(ui.elements.get('#file-selection-count').textContent, '2 selected');
assert.equal(ui.elements.get('#file-bulk-bar').hidden, false);
assert.equal(ui.elements.get('#bulk-download-files').textContent, 'Download');
assert.equal(ui.elements.get('#bulk-download-files').disabled, false);
```

Capture dynamically created anchors in `createDocument()` and assert exactly one has `download === 'crosstache-files.zip'` and `clickCount === 1`.

Before changing `index.html` or `secrets.js`, also add the concrete rejected-API
recovery test and assertions shown in Step 6 below. Both UI behaviors must be
RED together.

- [ ] **Step 2: Run the route test and verify RED**

Run:

```bash
node --test --test-name-pattern="selected files as one zip|zip download failure retains file selection" src/web/assets/secrets.routes.test.js
```

Expected: both tests fail because `#bulk-download-files` has no behavior and no
request is sent.

- [ ] **Step 3: Add the toolbar button and selection-state integration**

Add this button before Delete in the Files bulk bar:

```html
<button id="bulk-download-files" class="button secondary" type="button">
  <svg class="icon" aria-hidden="true"><use href="#icon-download"></use></svg>Download
</button>
```

Import `downloadFileArchive` from `files.js`. Add `downloadButton` to `selectionElements('files')`, disable it in `updateSelectionControls` when pending or empty, and reset it to `Download` when file selection mode clears.

Implement pending state without reusing the destructive confirmation state:

```javascript
function setFileDownloadPending(pending) {
  fileSelection.pending = pending;
  $('#cancel-file-selection').disabled = pending;
  const button = $('#bulk-download-files');
  if (pending) beginPendingAction(button, 'Downloading…');
  else resetConfirmation(button, 'Download');
  updateSelectionControls('files');
  renderFiles();
}
```

Because `updateSelectionControls` now knows `downloadButton`, an in-progress bulk delete also disables Download automatically.

- [ ] **Step 4: Implement the exact scoped bulk action**

Add and bind:

```javascript
async function bulkDownloadFiles() {
  const state = fileSelection;
  const names = [...state.ids];
  if (!names.length || state.pending) return;
  const scope = captureOperationScope();
  if (!canStartScopedAction(scope)) return;
  const generation = state.generation;
  setFileDownloadPending(true);
  try {
    await downloadFileArchive({
      api,
      path: `/api/files/archive${vaultQS(scope.vault, scope)}`,
      names,
    });
    if (generation === state.generation && scopeMatchesCurrent(scope)) {
      toast(`Downloaded ${names.length} file${names.length === 1 ? '' : 's'} in ${formatContextLine(scope)}`);
    }
  } catch (error) {
    if (generation === state.generation && scopeMatchesCurrent(scope)) {
      showListError('files', error);
    }
  } finally {
    if (generation === state.generation) setFileDownloadPending(false);
  }
}

$('#bulk-download-files').onclick = () => bulkDownloadFiles();
```

Do not clear `fileSelection.ids` in any path.

- [ ] **Step 5: Run the success route test and verify GREEN**

Run:

```bash
node --test --test-name-pattern="selected files as one zip" src/web/assets/secrets.routes.test.js
```

Expected: PASS with one scoped POST, one clicked archive anchor, and two selected files retained.

- [ ] **Step 6: Verify failure recovery is GREEN**

The second route test added during Step 1 makes the archive API call reject.
Hold the promise long enough to assert that Download, Delete, checkboxes, and
Cancel are disabled and the button reads `Downloading…`; reject it and then
assert:

```javascript
assert.equal(createdArchiveAnchors.length, 0);
assert.equal(ui.elements.get('#file-error').hidden, false);
assert.equal(ui.elements.get('#file-selection-count').textContent, '2 selected');
assert.equal(ui.elements.get('#bulk-download-files').textContent, 'Download');
assert.equal(ui.elements.get('#bulk-download-files').disabled, false);
assert.equal(ui.elements.get('#bulk-delete-files').disabled, false);
assert.equal(ui.elements.get('#cancel-file-selection').disabled, false);
```

Run:

```bash
node --test --test-name-pattern="zip download failure retains file selection" src/web/assets/secrets.routes.test.js
```

Expected: PASS. If it fails, correct only `setFileDownloadPending`, generation
checks, or the explicit Files error routing, then rerun the same command until
it exits 0.

- [ ] **Step 7: Run all affected JavaScript tests**

Run:

```bash
node --test src/web/assets/files.test.js src/web/assets/secrets.routes.test.js src/web/assets/module-contracts.test.js
```

Expected: all tests pass with zero failures and no warnings.

- [ ] **Step 8: Commit the UI integration**

```bash
git add src/web/assets/index.html src/web/assets/secrets.js src/web/assets/secrets.routes.test.js
git commit -m "feat(web): download selected files as zip"
```

---

### Task 5: Full Verification and Branch Handoff

**Files:**
- Verify only; modify a file only if a command exposes a defect in the feature.

**Interfaces:**
- Consumes: Tasks 1-4 complete implementation.
- Produces: a clean, rebased, pushed feature branch with fresh verification evidence.

- [ ] **Step 1: Run formatting checks**

```bash
cargo fmt --all -- --check
git diff --check
```

Expected: both commands exit 0 with no formatting or whitespace errors.

- [ ] **Step 2: Run the complete JavaScript unit suite**

```bash
npm run test:unit
```

Expected: every `src/web/assets/*.test.js` test passes with zero failures.

- [ ] **Step 3: Run Rust web and default test coverage**

```bash
cargo test web:: --features ui,file-ops
cargo test
```

Expected: both commands exit 0 with zero failed tests.

- [ ] **Step 4: Run lint and compile checks**

```bash
cargo clippy --all-targets --features ui,file-ops -- -D warnings
cargo check --all-targets --features ui,file-ops
```

Expected: both commands exit 0 with no warnings or errors.

- [ ] **Step 5: Inspect the exact change set against the approved spec**

```bash
git status --short
git diff origin/main...HEAD --stat
git diff origin/main...HEAD -- Cargo.toml src/web/archive.rs src/web/mod.rs src/web/assets/files.js src/web/assets/index.html src/web/assets/secrets.js
```

Confirm that the diff contains one ZIP endpoint, one bulk toolbar action, test coverage, and no backend-specific branch or unrelated refactor.

- [ ] **Step 6: Rebase and push**

```bash
git pull --rebase
git push origin codex/files-bulk-download
```

Expected: the branch tracks `origin/codex/files-bulk-download`, is up to date, and the worktree is clean.
