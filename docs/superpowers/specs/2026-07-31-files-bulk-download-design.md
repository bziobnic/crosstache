# Files Bulk Download Design

## Goal

Allow a user in the web UI Files tab to select files and download the exact
selection as one ZIP archive. The behavior must work through every backend that
implements Crosstache's file-storage capability.

## User Experience

The existing Files selection toolbar gains a `Download` button beside the
existing bulk actions. The button is disabled when the selection is empty or a
bulk file operation is pending.

Activating `Download` captures the current workspace, backend, vault, and the
selected logical file names. While the request runs, the button reads
`Downloading…` and conflicting selection actions are disabled. A successful
request downloads `crosstache-files.zip`, reports the number of downloaded
files, and leaves selection mode and the selected file set intact.

If the archive cannot be created, no partial ZIP is downloaded. The existing
Files error panel reports the failure, and the selection remains available so
the user can retry.

## API and Backend Architecture

Add an authenticated `POST /api/files/archive` route. The request body contains
the selected logical file names, while the existing scoped query parameters
identify the exact workspace, backend, and vault. The handler resolves that
scope through the same `WebState` mechanism used by existing file routes.

The handler operates only through `Backend::files()` and the shared
`FileBackend` trait. It must not dispatch on Local, AWS, or Azure backend types.
Consequently, the feature works for every current or future backend that exposes
the file-storage capability. Backends without that capability continue to make
the Files surface unavailable.

For each requested logical name, the handler uses the same transparent download
and decryption helper as the existing single-file route. Plain files pass
through unchanged; files marked with Crosstache's age-encryption metadata are
decrypted using the selected vault's attachment key before being placed in the
archive.

## Archive Construction

The server validates the request before returning archive bytes:

- The selection must contain between 1 and 1,000 names, the JSON body must be no
  larger than 512 KiB, and each UTF-8 name must be no larger than 1,024 bytes.
- Every name must pass the selected backend's file-name validation.
- Archive entries must be relative, use forward-slash separators, and contain
  no empty, `.` or `..` path components.
- Duplicate logical names are rejected.

Logical folder paths are preserved as ZIP entry paths. Because stored file names
are unique, valid requests cannot create colliding archive entries.

The server downloads and decrypts one file at a time and writes entries to an
unnamed temporary file with the existing ZIP library. After every entry is
written successfully, the finished archive is rewound and streamed as
`application/zip` with an attachment filename of `crosstache-files.zip`. The
temporary file is released when the response finishes or is abandoned. This
keeps server memory bounded by the largest individual downloaded file instead
of the total archive size.

If any backend read, decryption, validation, or ZIP operation fails, archive
creation stops and the temporary file is discarded. The handler returns the
normal structured API error response before any ZIP response begins.

## UI Data Flow

The bulk action reads from the existing `fileSelection.ids` set, so item,
folder, select-all, filtering, and tree-grid selection semantics remain
unchanged. The client sends one request containing exactly those identifiers and
the immutable scope captured when the action starts.

The authenticated API client receives the archive as a raw response, converts
it to a browser Blob, and triggers one download through a temporary anchor. The
object URL is always revoked. Scope changes and pending mutations use the same
guards as other scoped bulk actions; the request always targets its captured
scope rather than whatever context may later be displayed.

## Error Handling

Archive download is all-or-nothing. An error does not clear selection or claim
that a partial set succeeded. The Files error panel uses its existing safe error
copy and diagnostic behavior; backend details are not exposed directly. The
Download button and the rest of the bulk toolbar return to their normal state in
a `finally` path.

## Testing

Rust API tests will verify:

- A ZIP response contains the exact requested entries and bytes, including
  preserved folder paths.
- Plain and age-encrypted stored files have the same downloaded content as the
  existing single-file endpoint.
- The captured workspace/backend/vault scope selects the correct backend.
- The handler depends only on `FileBackend` behavior by exercising the shared
  web test backend.
- Empty, duplicate, excessive, unsafe, unknown, and unreadable selections fail
  without returning a ZIP.
- Response content type and content disposition are correct.

UI route/DOM tests will verify:

- The Download button appears in Files selection mode and follows selection and
  pending-state enablement.
- The request contains exactly the selected logical names and captured scope.
- A successful response triggers one ZIP download, retains selection, and
  restores controls.
- A failed response triggers no download, retains selection, restores controls,
  and uses the Files error surface.

The focused Rust and JavaScript tests will be run first, followed by the
repository's applicable unit, formatting, linting, and build checks.

## Non-Goals

- Multiple independent browser downloads.
- Client-side ZIP construction.
- Download progress reporting or cancellation beyond the browser/network's
  existing behavior.
- Changing file selection, filtering, upload, deletion, or single-file download
  semantics.
