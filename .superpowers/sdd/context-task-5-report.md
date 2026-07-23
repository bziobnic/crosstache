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

## Second review remediation

The follow-up review identified seven remaining atomicity and privacy gaps.
Each was reproduced with a failing regression before the implementation was
changed.

### Durable, opaque rename transactions

Rename journals now use the required publication protocol: write a private
`journal.tmp`, fsync it, atomically rename it to `journal.json`, then fsync the
transaction directory. Crash injection before and after publication proves
that an unpublished temp is ignored and removed on restart while a published
journal is recovered.

The journal contains only storage stems. In opaque-filename mode those stems
are keyed HMAC references, and all staged source and destination metadata is
age-encrypted even when normal metadata encryption is disabled. A recursive
artifact test checks both filenames and contents for raw and legacy-encoded
source or destination names.

### Separate conversion guarantees

Complete atomic conversion and conditional conversion are now separate
contracts:

- CLI `type` and `untype` require one complete backend update, so Azure can use
  its single complete PUT without pretending to provide compare-and-swap.
- Web preview/apply additionally requires the provider revision guard and
  commits through `update_secret_if_revision`.
- A same-type no-op web apply still validates the opaque source revision under
  the backend lock, so an intervening edit returns a conflict instead of a
  false success.

Azure advertises complete atomic conversion but not conditional conversion;
AWS advertises neither. Tests assert that Azure conversion performs exactly
one unconditional complete update.

### Preflighted CLI rename and synchronized attachments

Both `mv` and `update --rename` validate the resolved backend's atomic rename
capability before any folder or metadata mutation. The regression backend
records calls and proves an unsupported rename performs zero updates.

Local file mutations and secret rename now share the same vault lock.
Attachment upload validates that its source secret is still active while
holding the exclusive lock; rename rechecks for attachments under that same
lock. A two-thread barrier test proves upload and rename cannot both succeed:
upload-first blocks rename, while rename-first makes upload fail without
leaving an orphaned old-name namespace.

### Distinct conflicts and workspace reads

Backend errors now distinguish source revision conflict, destination
collision, and attachment blocking. Rename API responses map them to separate
stable codes and fields:

- `xv-rename-source-changed` / `source_revision`
- `xv-rename-destination-exists` / `name`
- `xv-attachments-block-rename` / `name`

The full browser gate additionally exposed that a configured but not yet
materialized local workspace could not be activated: `list_secrets` attempted
to open its vault lock before reaching its documented empty-list branch. The
existence guard now precedes lock acquisition, with a unit regression and the
workspace-switch browser scenarios covering the behavior.

The final full-suite gate also exposed a pre-existing real-home dependency in
the native-rotation capability test. The test now supplies temporary local
store and key paths and passes without touching real Crosstache state.

## Third review remediation

The final review found three contract edges. Each was first reproduced with a
failing regression, then closed at the backend boundary.

### Complete attachment namespace exclusion

Local rename now checks both the source and destination attachment namespaces
while holding the same vault lock used for the rename transaction. This blocks
renaming into a destination whose secret was deleted while its persisted
attachment namespace remains. The route regression creates that exact state
and verifies the structured `xv-attachments-block-rename` rejection, unchanged
source value, absent destination secret, and untouched source and destination
attachment namespaces.

The persisted-attachment guard no longer depends on the `file-ops` feature.
Only the file API remains feature-gated; rename always scans persisted metadata
using a minimal local metadata shape. A `--no-default-features --features ui`
regression writes persisted attachment metadata directly and proves rename is
blocked even when the file API is not compiled.

### Explicit conditional-conversion contract

Backends must now explicitly advertise atomic, no-write revision validation in
addition to conditional update support before web conversion is available.
The conversion preflight and Effective UI context use the same predicates, so
the displayed capability cannot exceed the route's actual guarantee.

An update-CAS-only backend deliberately leaves the default revision validator
in place. Its negative tests prove conversion is rejected during capability
preflight, before any source read, and that the UI does not advertise
conditional conversion.

## Verification

- `cargo test --features ui --lib` — 1,045 passed, 1 ignored.
- `cargo test --no-default-features --features ui --lib --quiet` — 980 passed,
  1 ignored.
- `cargo test --features ui --lib web::secrets::tests` — 30 passed.
- `cargo test --features ui --lib records::conversion::tests` — 30 passed.
- Rename-journal durability regressions — 2 passed.
- Opaque rename artifact privacy regression — passed.
- Attachment/rename namespace regressions — passed.
- Local file backend unit tests — 9 passed.
- CLI move unit tests — 15 passed.
- `cargo test --features ui --test e2e_record_types` — 71 passed.
- `cargo test --test cli_integration_tests` — 23 passed.
- `cargo test --test e2e_local_backend` — 89 passed.
- `cargo test --test local_backend_integration` — 12 passed.
- `npm run test:unit` — 105 passed.
- `npm run test:browser` — 23 passed.
- `cargo check --features ui --all-targets` — passed.
- `cargo clippy --features ui --all-targets -- -D warnings` — passed.
- `cargo check --no-default-features --features ui --lib --bins` — passed.
- `cargo check --all-features --all-targets` — passed.
- `cargo clippy --all-features --all-targets -- -D warnings` — passed.
- `cargo check -p xv-desktop --all-targets` — passed.
- `cargo clippy -p xv-desktop --all-targets -- -D warnings` — passed.
- `cargo fmt --all -- --check` — passed.
- `git diff --check` — passed.

The default-feature-only clippy command still reports the branch's six
pre-existing dead-code findings in context/project/workspace resolution code.
The follow-up conversion changes add no warnings to that baseline; the UI and
all-feature lint gates are clean.

The no-default-feature all-target clippy command reaches an existing ungated
`tests/file_commands_tests.rs` integration target that imports feature-gated
CLI variants. Its lib/bin form reports 16 existing feature-off dead-code
findings. The no-default UI library test and check gates pass, and the new
attachment and capability code adds no feature-off dead code.

Authenticated Azure and LocalStack tests remain ignored unless their documented
external services and credentials are enabled; both compile in the applicable
all-target feature gates.
