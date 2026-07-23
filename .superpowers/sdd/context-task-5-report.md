# Context/workflows Task 5 report

## Outcome

Implemented scoped conversion preview/apply and rename APIs:

- `POST /api/secrets/{name}/conversion/preview`
- `POST /api/secrets/{name}/conversion`
- `POST /api/secrets/{name}/rename`

Conversion routes resolve the exact attached workspace alias/backend/vault on
every request, reject unsupported atomic-conversion backends before reading a
secret, and delegate preview/apply to the shared Task 4 conversion service.
Preview responses are value-free and include a source version. Apply requires
that preview version, rejects stale sources with
`xv-conversion-source-changed`, and returns redacted updated properties plus a
value-free summary. Actual field loss or protected-to-metadata exposure
requires explicit confirmation and returns
`xv-conversion-confirmation-required` otherwise.

Conversion and rename bodies deny unknown fields. Conversion input bounds the
body, target/type names, supplied-field count, individual values, and total
supplied bytes. Rename accepts only `new_name`, bounds the body/name, validates
the source and destination against the exact backend's name capabilities,
rejects self-renames, preflights destination collisions with `field: "name"`,
and never returns a secret value.

Malformed envelopes, unknown types/fields, missing required fields, tag
budgets, backend capability limits, body limits, backend failures, rename
collisions, and partial rename failures use stable value-free API envelopes.
All routes retain the existing authentication, localhost-origin, no-store, and
global structured-error middleware.

## Conflict-safe rename contract

The inherited rename implementation had a destination TOCTOU: its
exists-then-`set_secret` sequence could overwrite a destination created between
the check and provider write.

`SecretBackend` now exposes a provider-enforced `create_secret_if_absent`
primitive and an explicit `supports_atomic_rename` preflight. Rename refuses to
read the source when that capability is unavailable and commits only through
the no-overwrite primitive:

- Local checks and creates under one same-name mutation lock. A deterministic
  two-instance test proves exactly one concurrent creator wins.
- AWS uses native `CreateSecret`; a `ResourceExistsException` stays a conflict
  and cannot fall through into the ordinary set/upsert path.
- Azure remains explicitly unsupported because its current `SetSecret`
  primitive is unconditional and cannot meet the no-overwrite contract.

A deterministic trait race injects the destination after the initial preflight
and proves the concurrent destination value and original source both survive.
Delete-after-create failure still maps to `xv-rename-incomplete` and
deliberately preserves both copies.

## TDD evidence

Observed RED before implementation for:

1. conversion preview/apply routes returning 405;
2. rename routes returning 405;
3. the missing atomic create-if-absent trait primitive;
4. Local rename becoming explicitly unsupported until its atomic primitive
   was implemented;
5. stale conversion preview lacking a source version; and
6. route-specific body bounds being bypassed by the outer upload body limit.

Each slice was made GREEN before the next behavior was added. Coverage includes
loss and sensitivity confirmation, supplied-value redaction, missing required
fields, unknown/unrelated fields, malformed envelopes, tag budget rejection,
unsupported conversion before read, stale source preservation, bounded
requests, backend error redaction, source missing, destination collision,
self-rename, partial rename failure, attached-workspace isolation, and
concurrent cross-tab statelessness.

## Verification

- `cargo test --features ui web:: --lib` — 130 passed.
- Hermetic `cargo test --features ui --lib` — 1021 passed, 1 ignored.
- Focused web secrets — 20 passed.
- Backend rename trait — 8 passed.
- Local rename — 2 passed.
- Local concurrent atomic create — 1 passed.
- `cargo test --features aws --lib backend::aws::capability_tests` — 1 passed.
- `cargo test --features aws --lib backend::aws::secrets::atomic_create_tests`
  — 2 passed.
- `node --test src/web/assets/*.test.js` — 105 passed.
- `cargo check --features ui --all-targets` — passed.
- `cargo clippy --features ui --all-targets -- -D warnings` — passed.
- Desktop `cargo check --all-targets` — passed.
- Desktop `cargo clippy --all-targets -- -D warnings` — passed.
- `cargo fmt --all -- --check` — passed.
- `git diff --check` — passed.

The first full library run used the real HOME and hit an existing test's
attempt to chmod the user's local store. The isolated HOME/XDG rerun passed
completely and did not read or mutate real Crosstache state.

AWS smithy operation-error integration mocks are currently unavailable on this
host: without `SSL_CERT_FILE` they fail loading native roots; with the system
certificate bundle both the new probe and the pre-existing
`set_secret_update_path_when_already_exists` test fail identically in the
smithy orchestrator phase. AWS feature compilation and deterministic
provider-decision tests pass; no failing integration test was added.
