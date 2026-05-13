# AWS Secrets Manager Backend — Phase 3 (v0.10) Design

**Date:** 2026-05-09
**Status:** Draft — pending user review
**Owner:** Scott Zionic
**Inputs:** `docs/superpowers/specs/2026-04-29-strategic-improvements-phase-1-design.md` (Phase 1 spec); `docs/superpowers/specs/2026-05-03-backend-pluggability-phase-2-design.md` (Phase 2 spec); `docs/code-review-gpt55.md`; `docs/reviews/2026-05-03-ux-review.md`; current code under `src/`.

---

## 1. Strategic context

Phase 1 (v0.6 → v0.7) shipped the "loved features" push. Phase 2 (v0.8 → v0.9) extracted the backend trait layer and shipped a local age-encrypted backend. The Phase 2 spec explicitly deferred AWS Secrets Manager and HashiCorp Vault as Phase 2b/2c.

The strategic positioning Phase 1 articulated still holds: **no general-purpose CLI secrets manager is truly backend-agnostic.** With Azure + local shipped, the trait has been validated against maximally different shapes (cloud REST API + local filesystem). AWS is the next backend to add — not because the trait needs another stress test, but because adding a second cloud backend turns "backend-agnostic" from a claim into a demonstrated capability, and the cross-cloud migration path becomes a marquee feature no competitor offers.

Phase 3 adds AWS Secrets Manager as a third backend behind the existing `Backend` trait, paired with hardened cross-cloud migration as the headline feature. Ships as **v0.10.0**.

### 1.1 Why AWS Secrets Manager (not Parameter Store, not Vault)

- AWS Secrets Manager is the AWS-native answer to Azure Key Vault — same value proposition (managed secret store, versioning, soft-delete, RBAC via IAM).
- Parameter Store overlaps with Secrets Manager but is positioned for non-sensitive config; many teams use both. Adding both backends in one phase doubles scope; Parameter Store is a candidate for a later phase.
- HashiCorp Vault has the smallest user base of the three big cloud-secret stores; deferred to Phase 4 or later.

### 1.2 What this phase does NOT do

- No S3 file storage (`has_file_storage: false` on AWS backend).
- No IAM-based `xv share` (`has_rbac: false`).
- No CloudTrail-based `xv audit` (`has_audit: false`).
- No native Lambda rotation (`has_secret_rotation: false`; `xv rotate` does new-version semantics, same as Azure).
- No Parameter Store backend.
- No Vault (HashiCorp) backend.
- No multi-region single-backend awareness (multi-region is achieved via named backends).
- No code-review-remediation pass (open gpt-5.5 P2/P3 items deferred to v1.0 phase).

---

## 2. Phase 3 scope

### 2.1 Deliverables (in sequence order)

1. **`AwsBackend` implementation** — full `Backend` + `SecretBackend` + `VaultBackend` trait surface against AWS Secrets Manager, behind a `aws` Cargo feature flag (default off at v0.10).
2. **Named-backend config generalization** — small extension to `BackendConfig` to support multiple named instances of the same backend type (`[aws-east]`, `[aws-west]`).
3. **Capability negotiation** — wire `has_rbac=false`, `has_audit=false`, `has_secret_rotation=false` on AWS through CLI/TUI capability checks; clean error messages on `xv share`, `xv audit` against AWS.
4. **Cross-cloud `xv migrate` hardening** — conflict modes, idempotency tags, bounded concurrency, dry-run summary, perf testing on 100+ secret migrations.
5. **Hermetic + LocalStack + live AWS test coverage**.
6. **Documentation** — `docs/migration.md`, `xv init` wizard updated, README and `docs/FEATURES.md` updated.

### 2.2 Deliberately deferred

- AWS Parameter Store backend (Phase 4 or later).
- HashiCorp Vault backend (Phase 4 or later).
- AWS S3 file storage (Phase 4 or later).
- IAM-based `xv share` (separate design effort; principal model differs from Azure RBAC).
- CloudTrail-based `xv audit`.
- AWS-native Lambda rotation.
- `xv migrate --with-history` (full version-history transfer; sketched but not built).
- Code-review remediation pass (separate v1.0 milestone).

---

## 3. Capability matrix

`AwsBackend::capabilities()` returns:

| Capability | Value | Notes |
|---|---|---|
| `has_vaults` | `true` | Prefix-based virtual vaults |
| `has_file_storage` | `false` | Deferred (S3 backend integration) |
| `has_rbac` | `false` | IAM model differs from Azure RBAC; deferred |
| `has_audit` | `false` | CloudTrail integration deferred |
| `has_versioning` | `true` | Native `VersionId` + staging labels |
| `has_soft_delete` | `true` | `DeleteSecret` recovery window 7–30 days (default 30) |
| `has_secret_rotation` | `false` | New-version semantics only; native rotation deferred |
| `has_groups` | `true` | Stored in `xv:groups` tag |
| `has_folders` | `true` | Stored in `xv:folder` tag |
| `has_notes` | `true` | Stored in `Description` |
| `has_expiry` | `true` | Stored in `xv:expires_at` tag (AWS has no native expiry) |
| `max_secret_size` | `Some(65_536)` | AWS limit: 64 KB per `SecretString`/`SecretBinary` |
| `max_name_length` | `Some(512)` | AWS Secrets Manager limit |
| `name_charset` | `AwsRelaxed` | New variant: `[a-zA-Z0-9/_+=.@-]` |

`BackendCapabilities` already exposes `has_audit` and `has_secret_rotation` (Phase 2 baked them in for forward compat). The `NameCharset` enum gains an `AwsRelaxed` variant alongside the existing `AlphanumericHyphen`, `Unrestricted`, and `Custom` variants in `src/backend/mod.rs`.

---

## 4. Architecture

### 4.1 Module layout

```
src/backend/aws/
├── mod.rs       -- AwsBackend impl Backend
├── auth.rs      -- AWS credential chain wrapper, profile selection
├── config.rs    -- AwsBackendConfig (region, profile, endpoint override)
├── secrets.rs   -- AwsSecretBackend impl SecretBackend
├── vaults.rs    -- AwsVaultBackend impl VaultBackend (marker-secret-based)
├── encoding.rs  -- Name encoding/decoding, prefix joining, marker name reservation
├── metadata.rs  -- Tag <-> SecretProperties mapping
├── errors.rs    -- AWS SDK error -> BackendError mapping
└── models.rs    -- AWS-specific response shapes used internally
```

Mirrors the structure of `src/backend/azure/`.

### 4.2 SDK choice

`aws-sdk-secretsmanager` (the official `aws-sdk-rust`). `aws-config` for the credential chain. No alternative considered — `rusoto` is deprecated, `aws-sdk-rust` is the supported path.

### 4.3 Cargo feature gating

The AWS backend lives behind a `aws` feature flag, **default off** at v0.10. Mirrors the existing `tui` feature pattern.

```toml
[features]
default = ["file-ops"]
file-ops = []
tui = []
aws = ["aws-sdk-secretsmanager", "aws-config"]
```

Distribution-channel binaries (homebrew, scoop, deb/rpm, GitHub releases) ship with `--features aws`. Users building from source without AWS get a smaller binary.

### 4.4 Config schema additions and named-backend support

**Today's shape.** `Config` is a flat struct (not an enum). Top-level Azure fields (`subscription_id`, `tenant_id`, `default_vault`, `default_resource_group`, `default_location`, `azure_credential_priority`) live directly on `Config`. Local-backend config lives in a sub-struct `Config.local: Option<LocalConfig>`. Active backend is selected by `Config.backend: Option<String>` — `None` is treated as `"azure"` for backward compatibility. `BackendKind` is a flat enum (`Azure`, `Local`), parsed from the active-backend string in `BackendRegistry::from_config`.

**Phase 3 adds:**

1. `BackendKind::Aws` variant in `src/backend/mod.rs` (with parser aliases `aws`, `secretsmanager`).
2. `Config.aws: Option<AwsConfig>` sub-struct, parallel to `Config.local`. Holds `region`, `profile`, `endpoint_url`, `default_vault`.
3. `Config.named_backends: HashMap<String, NamedBackendEntry>` — a new map keyed by user-chosen names (`"aws-east"`, `"aws-west"`). Each entry has an explicit `type: BackendKind` plus a backend-specific config payload. Empty when no named backends are configured.
4. `BackendRegistry::from_config` resolution: if `Config.backend` matches a built-in name (`"azure"`, `"local"`, `"aws"`), use the corresponding top-level config block; otherwise look up `Config.named_backends[name]` and use the embedded payload.

**Single-instance form** (most users):

```toml
backend = "aws"

[aws]
region = "us-east-1"
profile = "default"
endpoint_url = ""              # optional, for LocalStack
default_vault = "myproj-kv"
```

**Multi-region form** (named backends):

```toml
backend = "aws-east"

[named_backends.aws-east]
type = "aws"
region = "us-east-1"
profile = "prod"
default_vault = "myproj-kv"

[named_backends.aws-west]
type = "aws"
region = "us-west-2"
profile = "prod"
default_vault = "myproj-kv"
```

**Backward compatibility.** Existing config files with no `backend` key, no `[aws]` block, and no `[named_backends.*]` section keep working unchanged — `Config.backend = None` resolves to Azure. Adding `backend = "aws"` plus an `[aws]` block opts in. Existing Azure users see no behavior change unless they explicitly opt into AWS.

### 4.5 CLI flags

- `--backend <name>` — already exists; no change. Now resolves named entries in addition to type-implicit aliases.
- `--aws-profile <name>` — per-invocation override of the AWS profile. Honored only when active backend is AWS; warn otherwise.
- `--region <name>` — per-invocation override of the AWS region. Same semantics.

---

## 5. Naming, encoding, and the vault marker

### 5.1 Name encoding

AWS Secrets Manager allows `[a-zA-Z0-9/_+=.@-]` and 512-char names. That's broad enough that most user-facing names pass through unchanged.

**Encoding rule:** `aws_name = format!("{vault_prefix}/{secret_name}")`.

If `secret_name` contains characters outside the AWS charset, those bytes are percent-encoded. The `original_name` tag (already used by Azure backend) is the authoritative source for round-tripping back to user-facing display.

Vault names themselves are constrained to `[a-zA-Z0-9-]{3,50}` (matching Azure vault name constraints) so the prefix never needs encoding and round-trips cleanly between backends.

### 5.2 Vault marker secret

Each virtual vault on AWS is represented by a **marker secret** at `<vault>/.xv-vault`:

- `SecretString = "{}"` (the value is unused; we need a value to create the secret).
- `Description` = vault description from `xv vault create`.
- Tags include:
  - `xv:type=vault-marker`
  - `xv:created_at=<iso8601>`
  - `xv:vault_name=<name>`
  - User-supplied vault tags (passed through with original keys)

### 5.3 Vault operations

- `create_vault` → `CreateSecret` for the marker.
- `get_vault` → `DescribeSecret` on the marker (no value fetch).
- `list_vaults` → `ListSecrets` with filter `[{Key: tag-key, Values: ["xv:type"]}, {Key: tag-value, Values: ["vault-marker"]}]`. One paginated call.
- `delete_vault` → list secrets under the prefix; refuse if any non-marker secret exists unless `--force`; then `DeleteSecret` on the marker. With `--force`, all non-marker secrets are deleted first (with the configured recovery window unless `--purge` also supplied).
- `update_vault` → `UpdateSecret` description + `TagResource`/`UntagResource` on the marker.

### 5.4 Reserved namespace

Any secret name starting with `.xv-` is reserved for xv-internal use. `set_secret` rejects user attempts to write `.xv-*` names with `xv-reserved-name` error.

---

## 6. Per-method mapping (`SecretBackend` trait → AWS API)

| Trait method | AWS API | Notes |
|---|---|---|
| `set_secret` (create) | `CreateSecret` | Tags from request → AWS tags. Description = note. Returns `VersionId` as version string. |
| `set_secret` (update) | `PutSecretValue` + (if metadata changed) `UpdateSecret` + `TagResource`/`UntagResource` | 1–3 API calls depending on what changed. |
| `get_secret` (no value) | `DescribeSecret` | Metadata only. Cheap. |
| `get_secret` (with value) | `GetSecretValue` + `DescribeSecret`, parallelized via `tokio::join!` | AWS doesn't return tags from `GetSecretValue`. |
| `get_secret_version` | `GetSecretValue` with explicit `VersionId` | Direct lookup. |
| `list_secrets` | `ListSecrets` (paginated, follows `NextToken`) + filter by name prefix + filter out marker | Group filter applied client-side from tags. |
| `delete_secret` | `DeleteSecret` with `RecoveryWindowInDays = 30` | Or `ForceDeleteWithoutRecovery: true` if `--purge`. |
| `update_secret` (metadata) | `UpdateSecret` for description + `TagResource`/`UntagResource` for tags | No value change. |
| `list_versions` | `ListSecretVersionIds` | Returns full history. |
| `rollback` | `UpdateSecretVersionStage` (move `AWSCURRENT` to target version) | Native AWS rollback, atomic. |
| `restore_secret` | `RestoreSecret` | Cancels deletion within recovery window. |
| `purge_secret` | `DeleteSecret` with `ForceDeleteWithoutRecovery: true` | Bypasses recovery window. |
| `list_deleted_secrets` | `ListSecrets` with `IncludeDeleted: true` | |
| `secret_exists` | `DescribeSecret`; `ResourceNotFoundException` → `Ok(false)` | |
| `backup_secret` / `restore_from_backup` | `BackendError::Unsupported` | Same posture as Azure today. |

### 6.1 Tag schema for metadata

AWS allows 50 tags per secret (vs. Azure's 15). Comfortable budget.

| Tag key | Source field |
|---|---|
| `xv:original_name` | user-facing name before encoding |
| `xv:groups` | comma-separated groups |
| `xv:folder` | folder |
| `xv:created_by` | created_by |
| `xv:expires_at` | ISO 8601 expiry (AWS has no native expiry attribute) |
| `xv:content_type` | content type |
| `xv:type=vault-marker` | only on vault marker secrets |
| User-supplied tags | passed through with their original keys (no `xv:` prefix), up to the AWS 50-tag budget |

### 6.2 Expiry handling

AWS Secrets Manager has no native expiry attribute. The `xv:expires_at` tag carries an ISO 8601 timestamp; the backend checks expiry on `get_secret` and surfaces a warning (not an error — the secret is still readable). `xv list --expiring` filters via the tag client-side, mirroring the Azure path.

---

## 7. Authentication & config

### 7.1 Credential chain

The AWS SDK default credential chain is used unmodified. No xv-specific priority abstraction (unlike Azure's `--credential-priority`). The chain is:

1. Environment variables (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`)
2. Shared credentials file (`~/.aws/credentials`)
3. SSO via `aws sso login`
4. EC2 instance metadata
5. ECS task role
6. (Other SDK-defined sources)

This matches AWS user mental models. xv exposes only profile and region selection — it does not try to model "credential priority" the way Azure does.

### 7.2 Config schema

Top-level (single AWS backend):

```toml
backend = "aws"

[aws]
region = "us-east-1"
profile = "default"           # optional; falls through to AWS_PROFILE
endpoint_url = ""             # optional, for LocalStack/testing
default_vault = "myproj-kv"
```

Multi-region (named backends):

```toml
backend = "aws-east"

[aws-east]
type = "aws"
region = "us-east-1"
profile = "prod"
default_vault = "myproj-kv"

[aws-west]
type = "aws"
region = "us-west-2"
profile = "prod"
default_vault = "myproj-kv"
```

### 7.3 `xv init` updates

The init wizard gains a backend-selection step:
- `azure` (existing default for Azure users)
- `local` (existing local age backend)
- `aws` (new)

For AWS: prompt for region (with sensible default from `AWS_REGION` env), prompt for profile (with default), test connectivity via `DescribeOrganization` or `STS GetCallerIdentity` (lightweight; no IAM permission required), confirm and write config.

---

## 8. `xv migrate` hardening (the headline)

The migrate command exists from Phase 2. Phase 3 hardens it for cross-cloud as the v0.10 marquee feature.

### 8.1 Command surface

```bash
xv migrate --from azure:myproj-kv --to aws:myproj-kv \
           --on-conflict skip|replace|fail   # default: skip
           --filter "db-*"                   # glob filter
           --concurrency 8                   # bounded parallelism
           --dry-run                         # preview only
           --force-replace                   # ignore migration tags, overwrite
```

`--from` and `--to` accept `<backend-name>:<vault>` syntax. If `<vault>` is omitted, the backend's default vault is used. Source and target are resolved through the registry — no special-casing for any backend pair.

### 8.2 Pre-flight diff

Before any writes, migrate enumerates source and target, computes the diff, and prints a summary table:

```
Source: azure:myproj-kv (47 secrets)
Target: aws:myproj-kv (3 secrets)

  to migrate:    44 secrets
  conflict:       3 secrets (target has same name)
  skip:           0 secrets (none filtered out)

On conflict: skip
Dry run? yes
```

In dry-run mode, the run stops here. In real-run mode, the user sees the same summary then transfers begin.

### 8.3 Idempotency

Each migrated secret records on the target:
- `xv:migrated_from=<source-backend>:<vault>:<source-version-id>`
- `xv:migrated_at=<iso8601>`

Re-running migrate detects these tags and skips entries where the source version matches (default `--on-conflict skip` semantics). `--force-replace` overwrites regardless.

A run interrupted mid-stream (Ctrl-C) leaves no partial-state damage: each secret transfer is its own atomic unit. The next run picks up where the previous left off via the migration tags.

### 8.4 Concurrency, throttling, retry

- Bounded parallelism: default 8 concurrent transfers, configurable via `--concurrency`.
- Backoff on `ThrottlingException` (AWS) / `429 TooManyRequests` (Azure): exponential with jitter, max 30 s, 5 retries.
- Progress reporting via the existing `MultiProgressContext` from Phase 1 — per-secret log line + overall counter bar.

### 8.5 Metadata mapping (cross-backend)

| Source field | Azure → AWS | AWS → Azure |
|---|---|---|
| `groups` | tag `xv:groups` (comma-joined) | tag `groups` (existing scheme) |
| `note` | `Description` field | tag `note` |
| `folder` | tag `xv:folder` | tag `folder` |
| `expiry` | tag `xv:expires_at` | native attribute |
| `original_name` | tag `xv:original_name` | tag `original_name` |
| `created_by` | tag `xv:created_by` | tag `created_by` |
| `content_type` | tag `xv:content_type` | native attribute |
| `version_history` | current value only by default | current value only by default |

### 8.6 Performance budget

100-secret migration completes in <60 s on a warm credential cache (8 concurrent, no throttling).

### 8.7 Documentation

- `docs/migration.md` — prerequisites, IAM policy required on target, walk-through of Azure→AWS and AWS→Azure flows, conflict-mode reference, troubleshooting.
- README — short example.
- `docs/FEATURES.md` — migration entry updated.
- Asciinema demo embedded in README and migration docs.

---

## 9. Capability negotiation in CLI/TUI

When a command targets a feature the AWS backend does not support, the tool fails gracefully with a clear message:

```
$ xv --backend aws share myproj-kv/db-password user@example.com
Error [xv-unsupported]: The aws backend does not support access sharing.
Hint: Use AWS IAM resource policies to grant access. See `aws iam` documentation.

$ xv --backend aws audit myproj-kv/db-password
Error [xv-unsupported]: The aws backend does not support audit logs.
Hint: AWS CloudTrail captures audit events. See `aws cloudtrail lookup-events`.
```

Implementation pattern (already established in Phase 2): CLI handlers check `backend.capabilities()` before executing, return `CrosstacheError::Unsupported` with `backend`, `feature`, and `hint` fields.

TUI capability gating: panes/overlays for unsupported features (audit overlay on AWS) are hidden via the existing `BackendCapabilities`-driven render path.

---

## 10. Error model additions

No new `BackendError` variants required — the existing set covers what we need:

- AWS `ResourceNotFoundException` → `BackendError::NotFound`
- AWS `AccessDeniedException` / `UnauthorizedOperation` → `BackendError::PermissionDenied`
- AWS `ThrottlingException` → `BackendError::RateLimited` with retry-after if AWS provides one
- AWS `InvalidParameterException` / `ValidationException` → `BackendError::Internal` with message
- AWS `ResourceExistsException` (on create) → `BackendError::Conflict`
- AWS authentication failures → `BackendError::AuthenticationFailed`
- Network errors → `BackendError::Network`
- Any other AWS SDK error → `BackendError::Other(anyhow!(...))`

The `From<aws_sdk_secretsmanager::Error>` conversion lives in `src/backend/aws/errors.rs` and is exhaustive over the SDK's error variants we observe.

---

## 11. Testing strategy

### 11.1 Unit tests (`src/backend/aws/`)

- Encoding/decoding round-trip on edge-case names (special characters, max length, prefix collision).
- Tag ↔ metadata mapping round-trip.
- Vault marker construction and detection.
- AWS SDK error → `BackendError` mapping (every variant we map explicitly).

### 11.2 Hermetic backend tests

Use `aws-smithy-mocks-experimental` (the official mocking layer in `aws-sdk-rust`) to stub `secretsmanager` API responses. The full backend trait surface is exercised deterministically in CI without any AWS credentials.

A parametrized test matrix runs the same scenarios as the existing local-backend test suite, against AWS-backed mocks, ensuring trait conformance.

### 11.3 LocalStack integration tests (gated)

`AWS_INTEGRATION_TESTS=1` + LocalStack via Docker — full E2E exercise of the AWS backend against a real Secrets Manager API surface. Skipped silently when LocalStack is unavailable; CI runs them; local dev opt-in.

### 11.4 Live AWS integration tests (gated)

`AWS_LIVE_TESTS=1` runs against a real AWS account in a dedicated test region. Used for release validation, not every CI run. Test infrastructure provisioned via Terraform (committed to `tests/terraform/aws/`), torn down at end-of-test.

### 11.5 Migration tests

- Round-trip parametrized: create N secrets in source, migrate, verify all secrets and metadata in target. Run for Azure→AWS, AWS→Azure, AWS→Local, Local→AWS combinations.
- Idempotency: run migrate twice, verify no duplicates and tag-based skip works.
- Conflict modes: create same name with different value on target, verify each `--on-conflict` mode behaves correctly.
- Interruption: kill mid-migration, re-run, verify completion.
- Perf: 100-secret migration under 60 s.

### 11.6 Existing test suites

Azure backend tests, local backend tests, CLI integration tests run unchanged. AWS adds to the matrix.

---

## 12. Sequencing & milestones

### 12.1 Week-by-week (~3-week target, ~0.5-week buffer)

| Week | Theme | Deliverables |
|---|---|---|
| 1 | AWS foundation | `aws-sdk-secretsmanager` integration, `AwsBackendConfig`, named-backend config generalization, auth wrapper, encoding module. Compiles and registers as a backend; no operations yet. |
| 1 | Trait pass 1 | `set_secret`, `get_secret`, `list_secrets`, `delete_secret`, `secret_exists`. Hermetic mocked tests for each. |
| 2 | Trait pass 2 | `update_secret`, version operations, soft-delete/restore/purge, vault operations (marker scheme). LocalStack tests added. |
| 2 | Capability degradation polish | Wire `has_rbac=false`, `has_audit=false`, `has_secret_rotation=false` through CLI/TUI checks. Verify `xv share` / `xv audit` produce clean error messages on AWS. |
| 3 | Migrate hardening | `--on-conflict` modes, idempotency tags, concurrency/backoff, dry-run summary, parametrized round-trip migration tests. |
| 3 | Docs + release | `docs/migration.md`, README updates, asciinema demo, release notes, `docs/FEATURES.md` updates, `xv init` wizard gains AWS choice. |
| 3.5 | Buffer | Live AWS validation, perf testing, bug fixes, v0.10.0 cut. |

### 12.2 Release milestones

- **v0.10.0-rc.1** (end of week 2): AWS backend feature-complete behind `aws` feature flag, default off. Migration not yet hardened.
- **v0.10.0-rc.2** (end of week 3): Migration hardening complete; docs published; release notes drafted.
- **v0.10.0** (end of week 3.5): Public release with `aws` feature flag enabled in distribution-channel binaries.

### 12.3 Quality gates

Same as Phase 1 + Phase 2:
1. `cargo test` (all green) and `cargo test -- --test-threads=1` (no flake).
2. `cargo clippy --all-targets -- -W clippy::all` with no new warnings against baseline.
3. `cargo audit` (no new advisories).
4. CLI integration smoke tests on Linux + macOS + Windows.
5. LocalStack-backed E2E tests pass in CI.
6. Live AWS integration tests pass against a dedicated test account (manually triggered for the rc cut).

Plus:
7. **Binary size budget:** +1.5 MB ceiling for AWS SDK dependency. Alert on PRs that bust the budget.

### 12.4 Risk register

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| AWS SDK binary size bust | Medium | Medium | Feature-gate; `--no-default-features` available. Restrict to `aws-sdk-secretsmanager` only (not s3, ec2, etc.). |
| Throttling on large migrations | Medium | Low | Bounded concurrency + jittered backoff. Document AWS rate limits. |
| LocalStack ↔ live API drift | Low | Low | LocalStack covers behavior; live tests catch real auth/IAM gaps. |
| Vault marker collision with user secret named `.xv-vault` | Low | Medium | Reserved-namespace check at `set_secret` boundary. |
| Cross-cloud migration metadata loss | Medium | Medium | Documented mapping table (§8.5); `--with-history` deferred but sketched. |
| Named-backend config generalization breaks existing users | Low | High | Backward compat: `[azure]` and `[local]` blocks remain valid type-implicit aliases. Roundtrip tests on existing config files. |
| AWS SDK API instability | Low | Low | `aws-sdk-rust` is stable; pin minor versions; revisit at v0.10.0 release. |

---

## 13. Success criteria

Phase 3 is complete when:

1. `xv --backend aws` supports: `set`, `get`, `list`, `delete`, `update`, `versions`, `rollback`, `restore`, `purge`, `find`, `scan`, `inject`, `run`, `tui`, `vault {create,get,list,delete,update}`.
2. `xv share`, `xv audit`, and any other unsupported command on AWS produce `xv-unsupported` errors with actionable hints.
3. `xv migrate --from azure:X --to aws:Y` and the reverse complete a 100-secret migration in <60 s, idempotent on re-run.
4. All existing tests pass; new AWS test suite green in CI; LocalStack tests green in CI; live AWS tests pass on the rc cut.
5. Documentation shipped: `docs/migration.md`, AWS section in README, `xv init` wizard updated with AWS choice, `docs/FEATURES.md` updated.
6. Binary size delta within +1.5 MB budget.
7. No regressions on Azure or local backend paths.

---

## 14. Open questions

1. **Distribution channel for the `aws`-featured binary.** Should homebrew/scoop/deb/rpm ship one binary with all features (`tui`, `aws`, `file-ops`), or split — a "lite" binary without AWS for users who don't need it? Recommendation: one binary with all features, mirroring how most CLIs ship. Confirm before release.
2. **Live-test AWS account.** Need a dedicated AWS test account or sandbox. Who provisions and pays for it? Estimated cost: <$5/month at our test volume.
3. **LocalStack version pin.** LocalStack has free and pro tiers; some Secrets Manager features are pro-only. Confirm the free-tier feature set covers our trait surface (versioning, soft-delete, tagging, resource policies-not-needed since we don't use them).
4. **`xv migrate --with-history`** — punted from Phase 3, but worth a sketched-but-not-built design appendix here so Phase 4 has a head-start? Decision: keep it as a stretch goal note in §8.5; full design when scheduled.
5. **Phase 4 sequencing.** After v0.10, the natural Phase 4 is either (a) AWS Parameter Store + S3 file storage, (b) HashiCorp Vault backend, (c) v1.0 quality pass on remaining gpt-5.5 review items, or (d) TUI write mode. Decision deferred to a separate brainstorming session at the v0.10 retrospective.

---

## 15. Phase 4 preview (not in scope, captured for context)

After v0.10 ships, Phase 4 starts with a separate brainstorming session. Candidates:

- **Phase 4a — AWS S3 file storage + AWS Parameter Store.** Completes the AWS surface; introduces the second-service-on-one-cloud pattern (Secrets Manager + S3 share auth but are separate services); validates that `FileBackend` works on a non-Azure cloud.
- **Phase 4b — HashiCorp Vault backend.** Closes the "three big cloud-secret stores" cohort; appeals to enterprise/self-hosted users.
- **Phase 4c — v1.0 hardening pass.** Clears the remaining gpt-5.5 P2/P3 items (transactional local writes, scanner zeroization end-to-end, streaming blob I/O, capability/CLI alignment for backup/restore stubs) and ships v1.0 with a polished announcement.
- **Phase 4d — TUI write mode.** Completes the v0.8-deferred TUI feature: create/edit/delete/rename via the `c`/`d`/`e`/`r` reserved keys.

Selection happens at the v0.10 retrospective.
