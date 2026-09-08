# CLAUDE.md

This file is the developer/agent guide for the Crosstache repository. User-facing
behavior belongs in `README.md` and `docs/`; shipped history belongs in
`CHANGELOG.md`; open work belongs in `ROADMAP.md`.

## Project overview

Crosstache is a cross-platform secrets manager written in Rust. The binary is
`xv`. The current release baseline is **v0.39.0**.

Supported backends:

- Azure Key Vault, with optional Azure Blob file storage
- AWS Secrets Manager, with optional S3 file storage
- Local age-encrypted secret and file storage

Major surfaces include secret CRUD, typed records and TOTP, process/template
injection, multi-backend workspaces, migration, encrypted attachments, rotation
and scheduling, leak scanning, local Git history/audit, a read-only TUI, an
embedded localhost web UI, and a Tauri desktop shell.

## Repository rules

- Read `AGENTS.md` before making changes.
- The project does not use bd/beads; issue tracking is out of band.
- `main` is protected. Do not commit or push unless explicitly asked.
- Preserve scripting contracts: stdout is data, stderr is human status/error
  chrome, and structured output must remain parseable.
- Never log, debug-print, cache, or serialize a plaintext secret accidentally.
- Use backend capability checks instead of assuming every provider supports an
  operation.
- Keep retained specs/plans historical. Add a shipped/superseded status banner;
  do not rewrite their bodies to describe later behavior.

## Architecture

### Entry points

- `src/main.rs` — startup, config folding, CLI dispatch
- `src/lib.rs` — shared modules and feature-gated surfaces
- `src/cli/commands.rs` — Clap command definitions
- `src/cli/*_ops.rs` — command handlers

### Backend layer

`src/backend/mod.rs` defines the backend-neutral contracts:

- `Backend` — kind, health, capability declaration, and sub-trait access
- `SecretBackend` — required CRUD/lifecycle surface
- `VaultBackend` — optional vault/namespace lifecycle and access control
- `FileBackend` — optional file/blob storage (`file-ops` feature)
- `AuditBackend` — optional provider audit history
- `BackendCapabilities` — provider guarantees, limits, and supported features

`src/backend/registry.rs` constructs the active backend and lazily materializes
workspace backends. Built-in backend adapters live under:

- `src/backend/azure/`
- `src/backend/aws/` (`aws` feature)
- `src/backend/local/`

Azure still delegates some operations to older implementation modules under
`src/secret/`, `src/vault/`, and `src/blob/`. Do not add new Azure-only business
logic there when the operation belongs on a backend trait.

### Workspace and project resolution

- `src/workspace/` — workspace entries, aliases, default write target, qualified
  addressing, union reads, and target resolution
- `src/config/project.rs` — `.xv.toml` discovery and `[env.*]` profiles
- `src/config/context.rs` — persisted context/workspace state
- `src/backend/addressing.rs` — backend-prefixed `xv://` and migration addresses

A workspace may attach vaults from several backends. Reads may span entries;
unqualified writes and default file operations target the workspace default.
When no workspace is configured, resolution synthesizes a degenerate
workspace-of-one instead of using a separate legacy secret-resolution path.

Backend selection precedence is:

1. explicit `--backend`
2. active `.xv.toml` environment profile
3. `XV_BACKEND`
4. global `xv.conf`
5. `azure` compatibility default

Vault/resource resolution also considers explicit flags, the active project
environment, persisted context/workspace state, and backend/global defaults. Use
the shared resolvers; do not reproduce precedence locally in a command handler.

### Records, secrets, and attachments

- `src/records/` — type definitions, encrypted envelopes, conversions, Keeper
  import/export
- `src/totp.rs` and `src/cli/totp_ops.rs` — RFC 6238 code generation
- `src/secret/rotation.rs` — rotation policy parsing/status
- `src/secret/attachments.rs` — per-vault age encryption for attached files

Built-in record types are `login`, `api-key`, `database`, `ssh-key`,
`payment-card`, and `secure-note`. A record has exactly one primary secret field.
Metadata fields may be backend tags; protected fields remain in the encrypted
value envelope. External tools reading a typed secret directly see that envelope.

### Cache, scanning, and scheduling

- `src/cache/` — metadata/list caches and background refresh
- `src/scan/` — exact vault-value matching plus built-in token/pattern detection
- `src/schedule/` — launchd, systemd-user, and Windows Task Scheduler adapters

The cache is not a general plaintext secret-value cache. Scanner hook mode is
fail-closed when requested vault or file coverage is incomplete. Scheduled jobs
carry no credentials; review `ROADMAP.md` for current target-pinning limitations.

### User interfaces

- `src/tui/` — Ratatui read-only browser (`tui` feature)
- `src/web/` and `src/web/assets/` — Axum localhost API and dependency-free web
  client (`ui` feature)
- `desktop/src-tauri/` — Tauri host for the same embedded UI

The web server binds loopback only and uses a per-process bearer token, Host and
Origin validation, request-size limits, and `Cache-Control: no-store`. The UI can
switch among resolved workspace entries, but does not provide CLI-style union
views across every entry. TOTP is currently CLI-only.

## Build and validation

```bash
# Fast compile checks
cargo check
cargo check --all-features

# Formatting and linting
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings

# Rust tests
cargo test
cargo test --all-features --workspace

# Web unit tests
npm run test:unit

# Browser tests (requires Playwright browsers)
npm run test:browser
npm run test:a11y

# Desktop package
cargo test -p xv-desktop
```

Feature flags in `Cargo.toml`:

- `file-ops` — default, shared file/attachment surface
- `aws` — AWS Secrets Manager, CloudTrail, and S3
- `tui` — Ratatui browser
- `ui` — Axum embedded web UI

Release binaries are built with AWS, TUI, and embedded Web UI support. Source
builds must enable optional features explicitly when needed.

## Configuration

Global config normally resolves to:

```text
$XDG_CONFIG_HOME/xv/xv.conf
# or ~/.config/xv/xv.conf
```

Project config is `.xv.toml`, discovered by walking upward unless
`XV_NO_PARENT_CONFIG=1`. The project file may define `[env.*]` profiles,
workspace overlays, and `[types.*]` records. Advanced repeated provider instances
live under `[named_backends.*]` in global config; `xv backend` currently manages
one canonical instance per provider type.

Primary config areas:

- `[azure]` — subscription, tenant, resource group, location, default vault
- `[aws]` — region, profile, default/prefix vault, optional S3 bucket
- `[local]` — store/key paths, metadata encryption, opaque filenames, audit, Git
- `[blob]` — transfer tuning
- `[scan]` — non-hook scanner policy
- `[types.*]` — custom record schemas

Use `xv doctor` for bootstrap-safe global-config diagnosis and repair. Use
`xv config show --resolved`, `xv context show`, and `xv env show` when debugging
precedence.

## Provider-specific notes

### Azure

Azure authentication is under `src/backend/azure/auth.rs` and supports explicit
credential priorities including CLI, managed identity, environment credentials,
OIDC/workload identity, and the default chain. Secret operations use a mix of
Azure SDK clients and direct REST requests where provider semantics require it.
Azure vault names, resource scopes, URLs, and filesystem paths must remain
strictly validated before token-bearing calls or writes.

### AWS

AWS support is feature-gated. Secrets use Secrets Manager, audit uses CloudTrail,
native rotation delegates to the configured AWS rotation Lambda, and files use
S3 when configured. `xv file sync` is not implemented for AWS; current unified
AWS file transfers also lack the older streaming/atomic local-download path.

### Local

The local backend uses age encryption with transactional filesystem operations.
Optional hardening includes encrypted metadata, opaque filenames, a hash-chained
audit log, and Git-native ciphertext history. Existing stores retain compatibility
settings; do not silently migrate or destroy a store or its age identity.

## Current known limitations

Use `ROADMAP.md` as the authoritative backlog. Important current themes include:

- remaining cloud attachment-transfer routes (Azure destinations, AWS/Azure moves)
- scheduled-rotation target pinning
- cache invalidation on vault removal (v5 filesystem hardening shipped)
- provider compare-and-swap guarantees
- AWS file sync/streaming parity
- off-box local-audit durability
- rotation hooks and downstream rollout coordination
- workspace-wide union views in Web/Desktop and TOTP UI parity
- managed named backend lifecycle and additional providers

Historical security and UX audits are evidence, not current backlogs. Their
headers point to `ROADMAP.md` and `CHANGELOG.md` for current state.

## Shipped surface reference

Dense pointers to shipped behavior, kept here because agents need the
exact flags, exit codes, and metadata keys. User-facing narrative lives in
`README.md` and `docs/`; release history lives in `CHANGELOG.md`.

- **Output Formats**: JSON, YAML, CSV, plain, raw, and `template` (with field substitution, shipped v0.5.2) all working.
- **Pagination**: Secret listing follows Azure `nextLink` for large result sets; list-style pagination across `xv list` / `vault list` / `file list` / `share` shipped v0.6.0-rc.2.
- **Configurable Clipboard Timeout**: `clipboard_timeout` config key (default 30s, 0 to disable).
- **Config Editing**: `xv config edit` opens the resolved config in `$VISUAL`, then `$EDITOR`, then a platform default; missing configs are seeded with valid defaults.
- **Secret Write Metadata**: `xv set` and `xv gen --save` share write-time flags through `SecretWriteArgs` (`--group`, `--note`, `--folder`, `--expires`, `--not-before`).
- **File Sync** (`xv file sync`): Implemented (`--direction` up/down/both, `--dry-run`, `--delete`); see `src/blob/sync.rs` and `execute_file_sync` in `src/cli/file_ops.rs`.
- **Vault Sharing**: Implemented via Azure RBAC (`xv share grant|revoke|list`).
- **Backends**: Azure Key Vault (default), AWS Secrets Manager (`--features aws`, shipped v0.10.0), Local (age-encrypted on disk). AWS now includes share-policy hints, CloudTrail audit, native rotation, and S3 file storage; `xv file sync` remains unsupported on AWS.
- **Local Backend Hardening**: `[local].encrypt_metadata` encrypts metadata content with `xv local encrypt-metadata`; `[local].opaque_filenames` stores active secrets, versions, and trash under keyed-hash stems with `xv local migrate`.
- **v0.14 Hardening**: context files use private 0600 writes, `xv run` masking is bounded streaming, Azure `az` auth subprocesses are bounded and JWT claim shapes validated, scanner reads are bounded/fail-loud, cache locks use atomic create, and secret-list detail fetches use bounded concurrency.
- **TUI**: Read-only browser (`xv tui`), shipped v0.7.0-rc.2.
- **Web UI**: Embedded localhost browser UI (`xv ui`, `--features ui`) — secret CRUD, folder/group metadata, rename/move, file upload/download (including Files-tab ZIP bulk download), customizable color themes/palettes, vault switching; loopback-only with a per-session bearer token. See `docs/web-ui.md`.
- **Config recovery**: `xv doctor` diagnoses/repairs global `xv.conf` before normal config load (timestamped backup, exit 3 when manual steps remain). See `docs/doctor.md`.
- **Leak Scanner**: `xv scan` pre-commit scanner, shipped v0.7.0-rc.1.
- **Self-update**: `xv upgrade`, shipped v0.5.1.
- **Secret File Attachments**: `xv attach`/`xv attachments`/`xv detach` plus `xv file upload --encrypt` — client-side age encryption with per-vault key custody in the vault's secret store (`xv-attachment-key`); V2 key ring plus `xv attachment-key status|inventory|keys|initialize|upgrade|recover|export|restore|rotate|rewrap|retire`. See `docs/attachments.md`.
- **Attachment transfers**: `xv transfer` previews by default and applies with `--apply --offline` (resume with `--resume ID`); generic `copy`/`move`/`mv`/`migrate --with-attachments --offline` share the engine. Same-vault rename preserves ciphertext; cross-vault re-encrypts to `--to-key-id`. Azure destinations and AWS/Azure source moves are refused. See `docs/attachments.md` and `docs/migration.md`.
- **Rotation policies (all backends)**: `xv:rotate_every` + `xv:rotated_at` tags, `xv update --rotate-every`, `xv rotate --every/--due/--check` (exit 51 `xv-rotation-due`). AWS `--native` is still the only *server-side* rotation. See `src/secret/rotation.rs`, `docs/rotation.md`.
- **Automatic rotation scheduling**: `xv schedule install|status|uninstall` manages a per-user job in the OS scheduler (launchd / systemd user timer / Task Scheduler) running `xv rotate --due --force`. No daemon, nothing system-wide. `--print` renders without installing. Units carry no credentials; `HOME`/`XDG_CONFIG_HOME` are pinned so the scheduled run resolves the same config. Lifecycle logic is tested against a fake `CommandRunner` — no test registers a real job. See `src/schedule/mod.rs`, `src/cli/schedule_ops.rs`.
- **Local audit trail** (`[local].audit`): hash-chained append-only JSONL, `xv audit --verify` (exit 52 `xv-audit-chain-broken`). Fail-closed appends; `has_audit` reflects the flag. Tamper-*evident* only — the age-identity holder can rewrite it. Records **failures as well as successes**, with status tokens from a closed set keyed off the error variant (`DecryptionFailed`, `NotFound`, …) — never from error messages. `BackendError::Decryption` exists to make failed decryption its own status. See `src/backend/local/audit.rs`, `docs/git-versioning.md`.
- **Git-native versioning** (`[local].git`, local backend only): store is a real git repo, auto-commit per mutation, `xv git init/log/status/diff/push/pull`. Age identity protected by a managed `.gitignore` **and** a pre-commit staged-path refusal. Azure/AWS excluded by design (would create a permanent second copy of every cloud secret). See `src/backend/local/git.rs`.
- **Keeper JSON import/export**: `xv vault import|export --fmt keeper` reads/writes the Keeper Security import format. Keeper logins become typed `login` records (`f.username`/`f.url` tags, password in the envelope); `$oneTimeCode` becomes the `one-time-code` envelope field, never a tag. Folder nesting maps `\` ↔ `/`. Per-record refusals (nothing storable, sanitized-name collision, unusable folder path, backend tag-cap overflow) are reported with reasons and exit non-zero; Keeper shared-folder ACLs have no xv equivalent and are reported, not applied. Pure conversion lives in `src/records/keeper.rs`; CLI wiring in `src/cli/vault_ops.rs`. See `docs/keeper.md`.
- **`vault export`/`vault import` are backend-agnostic**: both need only `SecretBackend`, so `secrets_only_verb` in `src/cli/vault_ops.rs` routes them past the vault-trait shim that used to refuse them on local/AWS.
- **`xv doctor`**: bootstrap-safe recovery for global `xv.conf` (dispatched in
  `src/main.rs` before normal config load). Repairs missing top-level /
  `[blob_config]` scalars from defaults, writes `xv.conf.backup-<UTC>`, exits
  `3` when a person is still required. Does **not** repair `.xv.toml` or guess
  invalid types. Azure semantic checks still read top-level credential fields,
  not `azure_settings()` — see `docs/doctor.md`.
- **Backend lifecycle** (`xv backend ls|add|rm`): configure and remove backends without disturbing which one is active — `xv init` still bootstraps and switches to a single backend, but now preserves any others already configured instead of discarding them. One instance per type (`local`/`azure`/`aws`); `named_backends` remains the separate multi-instance mechanism. `rm` is config-only by default (drops the backend's block and any workspace entries pointing at it) and refuses in four cases: backend not configured, `--purge` on a non-local backend, removing the active backend while others remain, or stranding the workspace's default vault. `--purge` (local only) additionally deletes the store, age key, and recipients file — unrecoverable, since the key is gone too — guarded by store/key shape checks and a non-TTY refusal without `--yes`; it also refuses inside a directory governed by an active `.xv.toml` `[env.X].vaults` overlay, same as `xv cx rm`. New `[azure]` config block (`Config.azure`) with the top-level Azure fields kept as the legacy fallback, resolved via `azure_settings()`. `Config::validate()` resolves through `azure_settings()` too, so an `[azure]`-only config is a supported hand-authored form (in v0.37.0 it passed `xv backend ls` but failed `xv list`, because validate read the top-level fields directly). Precedence is **whole-block**: a present `[azure]` block shadows the top-level fields entirely, so a partial block that omits `subscription_id`/`tenant_id` is rejected at validation rather than resolving to `None` at the call site. `xv`-generated configs always mirror both fields to the top level. See `docs/backends.md`, `src/cli/backend_ops.rs`, `src/config/backend_ops.rs`.
- **First-party CI/CD**: root `action.yml` composite GitHub Action (per-OS release archive, fail-closed SHA-256, tool cache, masked secret export to `GITHUB_ENV`) plus OIDC-native Azure auth (`AZURE_CREDENTIAL_PRIORITY=oidc`) federating a GitHub OIDC token as a `client_assertion` — no stored secret, no `azure/login`. See `src/backend/azure/oidc.rs`, `docs/ci-cd.md`, `.github/workflows/action-test.yml`.
