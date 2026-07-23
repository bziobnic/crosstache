# Context/workflows Task 5 report

## Outcome

Implemented scoped conversion preview/apply and rename APIs:

- `POST /api/secrets/{name}/conversion/preview`
- `POST /api/secrets/{name}/conversion`
- `POST /api/secrets/{name}/rename`

All three routes bind each request to its exact attached workspace
alias/backend/vault. Effective UI context now advertises the guarantees the
routes actually require: conditional record conversion and atomic rename.
Backends that cannot provide those guarantees fail closed before source data is
read or mutated.

## Review remediation

### Opaque revision-guarded conversion

Conversion preview reads one `SecretSnapshot` containing properties plus a
backend-issued opaque revision. Apply requires that `source_revision` and
commits with `update_secret_if_revision`; it never performs an unconditional
update after a version-label comparison.

The Local backend persists UUID revisions and publishes a new revision for
create, update, rollback, restore, delete/recreate, and rename. This prevents
ABA mistakes such as delete/recreate returning to the same display version.
Tests inject a write between the route snapshot and commit and prove that the
newer value survives.

AWS and Azure advertise conditional conversion as unsupported. The shared
conversion service requires both the advertised capability and the backend
trait primitive before it reads the source.

### Source-safe atomic rename

The unsafe trait default read/create/delete rename was removed. Both rename
entry points now return `Unsupported` unless a backend implements an atomic,
revision-guarded primitive.

Local rename holds the vault mutation lock across source-revision and
destination-absence checks. It uses a durable rename journal and backups to
activate the destination and soft-delete the source as one recoverable
transaction. On an injected partial failure or restart at each visible stage,
recovery restores the source, removes the partial destination, repairs the
opaque index, and leaves no observable partial rename.

AWS and Azure explicitly advertise atomic rename as unsupported. Their
integration contracts assert that rename fails closed, preserves the source,
and creates no destination.

The web route passes the preview revision into the atomic rename primitive.
It also lists the reserved attachment namespace before mutation and rejects
any attached secret with `xv-attachments-block-rename`, preventing orphaned
attachments.

### Strict conversion contract and safe responses

Conversion supports typed targets and a strict record-to-plain form:

```json
{"target":{"kind":"plain"}}
```

The compatibility typed form remains accepted. Request DTOs deny unknown
fields, enforce bounded bodies and supplied fields, and report actionable
field paths such as `target_type`, `source_revision`, and
`supplied_fields.account`. Content-Type parsing accepts case-insensitive
`application/json` and `application/*+json` media types with optional
parameters.

Preview and apply preserve the shared conversion service's loss and sensitivity
confirmation rules. Every conversion and rename response strips the value and
all tags, including record type, field-shape, public-value, and protected-field
markers. Stable API errors likewise contain no values or backend internals.

## TDD evidence

The remediation was implemented in red/green slices covering:

1. stale conversion after an intervening write;
2. delete/recreate with the same display version;
3. fresh revisions after rollback and restore;
4. source changes and destination creation before rename commit;
5. partial rename failure and crash recovery at all visible stages;
6. record-to-plain preview/apply and confirmation;
7. response tag redaction;
8. attached-secret rename rejection;
9. exact Effective UI context capabilities; and
10. dynamic validation fields and vendor JSON media types.

The final full-suite gate also exposed a pre-existing real-home dependency in
the native-rotation capability test. The test now supplies temporary local
store and key paths and passes without touching real Crosstache state.

## Verification

- `cargo test --features ui --lib --quiet` — 1,031 passed, 1 ignored.
- `cargo test --features ui web::secrets::tests --lib` — 26 passed.
- `cargo test --features ui records::conversion::tests --lib` — 27 passed.
- `cargo test --features ui backend::secret::tests --lib` — 4 passed.
- `cargo test --features ui --test e2e_record_types` — 71 passed.
- `cargo test --features ui --test cli_integration_tests` — 23 passed.
- `cargo test --features ui --test e2e_local_backend` — 89 passed.
- `node --test src/web/assets/*.test.js` — 105 passed.
- `cargo check --features ui --all-targets` — passed.
- `cargo clippy --features ui --all-targets -- -D warnings` — passed.
- `cargo check --features "ui aws" --all-targets` — passed.
- `cargo clippy --features "ui aws" --all-targets -- -D warnings` — passed.
- `cargo check -p xv-desktop --all-targets` — passed.
- `cargo clippy -p xv-desktop --all-targets -- -D warnings` — passed.
- `cargo fmt --all -- --check` — passed.
- `git diff --check` — passed.

Authenticated Azure and LocalStack tests remain ignored unless their documented
external services and credentials are enabled; both compile in the applicable
all-target feature gates.
