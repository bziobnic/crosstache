# Security Review: crosstache

Date: 2026-05-28  
Scope: repository-wide review of `/Users/scottzionic/Code/crosstache` at commit `d2cbca7`.

## Scope

- In scope: Rust source under `src/`, install scripts under `scripts/`, and the Python Azure Function and installer under `xfunction/`.
- Scan mode: repository-wide security review using a generated repository threat model and subagent full-file review by runtime area.
- Explicit exclusions: tests, docs, generated build artifacts, vendored caches, and runtime validation against live Azure/AWS services.
- Artifact bundle: `/tmp/codex-security-scans/crosstache/d2cbca7_20260528T213909Z`.

## Scan Summary

| Field | Value |
|---|---|
| Scan mode | Repository-wide Codex Security scan |
| Primary runtime surfaces | Rust `xv` CLI, local/Azure/AWS backends, blob/file operations, leak scanner, install/upgrade flows, Python Azure Function RBAC helper |
| Worklist coverage | 135 source-like rows generated into `/tmp/codex-security-scans/crosstache/d2cbca7_20260528T213909Z/artifacts/02_discovery/deep_review_input.csv` |
| Validation mode | Static source-to-sink tracing with subagent full-file receipts; cloud/release-runtime issues were not dynamically exercised |
| Reportable findings | 10 total: 1 critical, 4 high, 4 medium, 1 low |
| Main artifact bundle | `/tmp/codex-security-scans/crosstache/d2cbca7_20260528T213909Z` |

## Threat Model

crosstache is a cross-platform command-line secrets manager (`xv`) with local age-encrypted storage, Azure Key Vault/Blob Storage support, optional AWS Secrets Manager support, a leak scanner, self-update/install paths, and an `xfunction/` Azure Function that grants Key Vault and storage RBAC. The most important assets are secret values, local age private keys, cloud identities and authorization context, vault/blob contents, release artifacts, generated config, and process environments created by `xv run`.

The key trust boundaries are local operator input, project `.xv.toml` files, local filesystem paths, cloud metadata and RBAC APIs, remote release assets, terminal output, child-process environments, and HTTP requests to the Azure Function. High-impact failure modes include secret exfiltration, privilege escalation in Azure/AWS, local arbitrary file writes, supply-chain binary replacement, and accidental leakage through logs, stdout, clipboard, or child environments.

## Findings

| # | Severity | Confidence | Finding |
|---|---|---|---|
| 1 | Critical | High | [Missing `CreatedByID` tag allows self-service Key Vault Owner assignment](#1-missing-createdbyid-tag-allows-self-service-key-vault-owner-assignment) |
| 2 | High | High | [`xv upgrade` installs unsigned releases when `.minisig` is missing](#2-xv-upgrade-installs-unsigned-releases-when-minisig-is-missing) |
| 3 | High | High | [Install scripts continue after checksum verification is unavailable or fails open](#3-install-scripts-continue-after-checksum-verification-is-unavailable-or-fails-open) |
| 4 | High | High | [Storage RBAC fallback grants roles on unrelated storage accounts](#4-storage-rbac-fallback-grants-roles-on-unrelated-storage-accounts) |
| 5 | High | Medium | [JWT audience validation is disabled when `EXPECTED_AUDIENCE` is unset](#5-jwt-audience-validation-is-disabled-when-expected_audience-is-unset) |
| 6 | Medium | High | [Recursive blob download can write outside the output directory for absolute blob names](#6-recursive-blob-download-can-write-outside-the-output-directory-for-absolute-blob-names) |
| 7 | Medium | Medium | [`xv run` reintroduces parent environment variables after `env_clear`](#7-xv-run-reintroduces-parent-environment-variables-after-env_clear) |
| 8 | Medium | Medium | [Existing local age key files are accepted without permission or symlink checks](#8-existing-local-age-key-files-are-accepted-without-permission-or-symlink-checks) |
| 9 | Medium | Medium | [Setup script prints the Azure app registration client secret](#9-setup-script-prints-the-azure-app-registration-client-secret) |
| 10 | Low | Medium | [Remote blob names and metadata can inject terminal control sequences](#10-remote-blob-names-and-metadata-can-inject-terminal-control-sequences) |

### [1] Missing `CreatedByID` tag allows self-service Key Vault Owner assignment

| Field | Value |
|---|---|
| Severity | Critical |
| Confidence | High |
| Confidence rationale | Direct code trace shows the missing-tag branch continues into privileged role assignment with no countervailing guard. |
| Category | Authorization bypass / cloud privilege escalation |
| CWE | CWE-862 Missing Authorization |
| Affected lines | `xfunction/function_app.py:202`, `xfunction/function_app.py:337`, `xfunction/function_app.py:368`, `xfunction/function_app.py:374`, `xfunction/VaultRbacProcessor/vault_role_manager.py:178` |

#### Summary

The anonymous HTTP route accepts a bearer token and caller-supplied `resourceUri`, but if the target vault lacks a `CreatedByID` tag the creator check only logs a warning and continues. The same request then assigns Owner and Key Vault Administrator to the authenticated user.

#### Validation

Validation: source tracing shows the missing-tag branch at `function_app.py:337-340` does not return, while mismatch does return 403. The role assignment calls at `function_app.py:368-375` reach `role_assignments.create` in `vault_role_manager.py:178`. No repository counterevidence requires the tag before role assignment.

#### Dataflow

HTTP request `resourceUri` and bearer token -> JWT validation -> `get_vault_info(resource_uri)` -> missing `CreatedByID` warning branch -> `assign_role_to_user` -> Azure `role_assignments.create`.

#### Reachability

Reachability: any tenant-authenticated caller who can reach the Function endpoint and name a Key Vault lacking the tag can ask the function app's privileged identity to grant them roles at that vault scope. Impact depends on the deployed service principal scope, but the code path is a direct control-plane privilege grant.

#### Severity

Critical because the vulnerable path grants cloud control-plane roles across a security boundary with only a valid tenant token and a missing metadata tag. Evidence that deployed service-principal scope is tightly limited to newly created vaults would lower severity.

#### Remediation

Remediation: fail closed when `CreatedByID` is absent or malformed; verify the vault subscription/resource group against an allowed scope; add tests for missing tag, mismatched tag, and matching tag.

### [2] `xv upgrade` installs unsigned releases when `.minisig` is missing

| Field | Value |
|---|---|
| Severity | High |
| Confidence | High |
| Confidence rationale | Direct code trace shows unsigned release assets continue to checksum, extraction, and binary replacement. |
| Category | Supply-chain signature verification bypass |
| CWE | CWE-347 Improper Verification of Cryptographic Signature |
| Affected lines | `src/cli/upgrade_ops.rs:103`, `src/cli/upgrade_ops.rs:123`, `src/cli/upgrade_ops.rs:128`, `src/cli/upgrade_ops.rs:144` |

#### Summary

`xv upgrade` looks for a minisign signature, but treats a missing `.minisig` asset as a warning and continues to checksum verification and binary replacement. The checksum is downloaded from the same release channel, so it does not provide independent authenticity if the release asset set is compromised.

#### Validation

Validation: the `sig_asset` is optional at `upgrade_ops.rs:105`; the missing-signature branch only warns at `128-130`; `replace_binary` is still reached at `144`. Existing controls are useful only when the signature is present.

#### Dataflow

GitHub release metadata -> optional `.minisig` lookup -> warning-only missing signature branch -> same-channel checksum verification -> archive extraction -> `replace_binary`.

#### Reachability

Any user running `xv upgrade` reaches this flow. An attacker needs compromise of the release channel or ability to influence the asset set, which is exactly the supply-chain boundary signature verification is meant to defend.

#### Severity

High because bypassing release authenticity can install attacker-controlled code for users who invoke the upgrade workflow. Mandatory signatures on every release asset would lower this to a rejected finding.

#### Remediation

Remediation: require `.minisig` for all upgrade installs; fail closed on missing or invalid signatures before checksum/extraction; add tests that unsigned release metadata aborts.

### [3] Install scripts continue after checksum verification is unavailable or fails open

| Field | Value |
|---|---|
| Severity | High |
| Confidence | High |
| Confidence rationale | Both install scripts have explicit warning/catch paths that continue to install after unavailable verification. |
| Category | Supply-chain artifact verification fail-open |
| CWE | CWE-494 Download of Code Without Integrity Check |
| Affected lines | `scripts/install.sh:201`, `scripts/install.sh:216`, `scripts/install.sh:226`, `scripts/install.sh:248`, `scripts/install.ps1:346`, `scripts/install.ps1:350`, `scripts/install.ps1:362`, `scripts/install.ps1:374` |

#### Summary

The shell and PowerShell installers both allow release installation to continue when checksum retrieval or verification is unavailable. The shell script skips verification for empty checksum files or missing checksum utilities, then extracts and installs. The PowerShell script catches checksum download/verification errors and continues to expand and copy the binary.

#### Validation

Validation: both scripts clearly continue after warning paths. The scripts use HTTPS and compare checksums when available, but the verification mechanism is fail-open and not signature-backed.

#### Dataflow

Release archive download -> checksum download/parse/utility failure -> warning or caught exception -> archive extraction -> binary copy/install -> installed binary execution/version check.

#### Reachability

Any first-time installer user reaches this flow. Attackers need release-channel, network, mirror, or asset tampering capability; the scripts are the bootstrap trust boundary for new installations.

#### Severity

High because bootstrap installer verification fail-open can lead to execution of a malicious `xv` binary. Requiring successful signature verification before extraction would lower or eliminate the issue.

#### Remediation

Remediation: fail closed unless checksum verification succeeds, or preferably verify minisign signatures with an embedded public key; never run the installed binary for verification until authenticity is established.

### [4] Storage RBAC fallback grants roles on unrelated storage accounts

| Field | Value |
|---|---|
| Severity | High |
| Confidence | High |
| Confidence rationale | Direct code trace shows no-match storage discovery broadens to every resource-group storage account before role assignment. |
| Category | Authorization scope expansion |
| CWE | CWE-266 Incorrect Privilege Assignment |
| Affected lines | `xfunction/function_app.py:380`, `xfunction/StorageRoleManager/storage_role_manager.py:92`, `xfunction/StorageRoleManager/storage_role_manager.py:141`, `xfunction/StorageRoleManager/storage_role_manager.py:246` |

#### Summary

After vault role assignment, the function discovers storage accounts in the vault resource group. If no explicit tag or naming association is found, it falls back to all storage accounts in that resource group, then grants Storage Account Contributor and Storage Blob Data Owner for Owner-equivalent vault grants.

#### Validation

Validation: the broad fallback is explicit at `storage_role_manager.py:92-95`; role mapping and assignment sinks are at `141-146` and `246-253`. The tag/naming checks are not sufficient because no-match broadens rather than denies.

#### Dataflow

Request-selected vault resource ID -> resource group parsed from vault ID -> list storage accounts in group -> no explicit association -> all accounts selected -> storage RBAC assignments created for caller.

#### Reachability

A caller who passes the vault authorization check for one vault in a shared resource group can receive storage roles on peer accounts in that group.

#### Severity

High because the impact is unauthorized cloud storage management/data access beyond the verified vault relationship. Evidence that deployments use one storage account per isolated resource group would lower severity.

#### Remediation

Remediation: remove the all-storage-accounts fallback; require an explicit `AssociatedVault` tag or authoritative mapping; log and skip storage role assignment when no association is found.

### [5] JWT audience validation is disabled when `EXPECTED_AUDIENCE` is unset

| Field | Value |
|---|---|
| Severity | High |
| Confidence | Medium |
| Confidence rationale | Runtime validation is definitely fail-open when unset, but the Python installer sets the value for its deployment path. |
| Category | Authentication validation fail-open |
| CWE | CWE-287 Improper Authentication |
| Affected lines | `xfunction/function_app.py:112`, `xfunction/function_app.py:140`, `xfunction/scripts/setup-app-registration.ps1:35`, `xfunction/installer/steps/function_app.py:38` |

#### Summary

The Azure Function sets `verify_aud` to `bool(expected_audience)`, so audience validation is disabled when `EXPECTED_AUDIENCE` is absent. The Python installer sets this value, but the PowerShell app-registration setup script configures tenant/client/secret without setting `EXPECTED_AUDIENCE`.

#### Validation

Validation: source tracing confirms `jwt.decode` only receives an audience when the environment value exists. Confidence is medium because the current Python installer mitigates normal deployments, but the runtime code and PowerShell setup remain fail-open.

#### Dataflow

Bearer token -> unverified header key lookup -> `EXPECTED_AUDIENCE` env read -> `verify_aud` set from truthiness -> `jwt.decode` without audience when unset -> RBAC request handling continues.

#### Reachability

Deployments missing `EXPECTED_AUDIENCE` can accept valid tenant tokens minted for other audiences. This materially expands who can reach the RBAC assignment logic.

#### Severity

High because it weakens authentication on a privileged role-granting endpoint, though confidence is medium due the Python installer mitigation. Deployment evidence proving the setting is always enforced would lower severity.

#### Remediation

Remediation: require `EXPECTED_AUDIENCE` at startup and fail requests if missing; set it in every deployment path; add tests that tokens for another audience are rejected.

### [6] Recursive blob download can write outside the output directory for absolute blob names

| Field | Value |
|---|---|
| Severity | Medium |
| Confidence | High |
| Confidence rationale | Direct code trace shows recursive download misses the absolute-path rejection used by safer sibling paths. |
| Category | Path traversal / arbitrary local file write |
| CWE | CWE-22 Path Traversal |
| Affected lines | `src/cli/file_ops.rs:1381`, `src/cli/file_ops.rs:1391`, `src/cli/file_ops.rs:1399`, `src/cli/file_ops.rs:1473`, `src/utils/helpers.rs:216` |

#### Summary

Recursive download uses `output_path.join(blob_name)` and only rejects `..` components. On Unix, joining an absolute blob name discards the base path. Single and multi-file downloads use `safe_join`, which rejects absolute paths, but recursive download does not.

#### Validation

Validation: the source-to-sink path is blob listing -> `blob_name` -> `output_path.join(blob_name)` -> `std::fs::write`. The nearest control checks only `ParentDir`; `safe_join` demonstrates the intended missing absolute-path control.

#### Dataflow

Cloud blob name from listing -> recursive download local path construction -> parent-directory-only traversal check -> parent directory creation -> `std::fs::write(local_path, content)`.

#### Reachability

A blob writer can create a blob with an absolute-looking name; an operator later running recursive download writes it outside the chosen output tree if the destination does not exist or `--force` is used.

#### Severity

Medium because it is a local arbitrary write triggered through operator download of attacker-controlled blob names, without a proven automatic code-execution path. Evidence of common recursive downloads into sensitive locations would raise severity.

#### Remediation

Remediation: use `safe_join` for recursive downloads before creating parents or writing; reject root/prefix components and Windows absolute paths; add tests for `/tmp/x`, `C:\x`, and `../x` blob names.

### [7] `xv run` reintroduces parent environment variables after `env_clear`

| Field | Value |
|---|---|
| Severity | Medium |
| Confidence | Medium |
| Confidence rationale | Source order proves the bypass, but exploitation requires attacker influence over the parent environment. |
| Category | Environment injection / isolation bypass |
| CWE | CWE-15 External Control of System or Configuration Setting |
| Affected lines | `src/cli/secret_ops.rs:2126`, `src/cli/secret_ops.rs:2276`, `src/cli/secret_ops.rs:2283`, `src/cli/secret_ops.rs:2293` |

#### Summary

When `inherit_env` is false, `xv run` calls `cmd.env_clear()`. It then iterates the parent environment and re-adds any variable whose value changes after resolving an `xv://...` reference. This lets parent-controlled variables such as loader/runtime settings re-enter the child despite clean-environment mode.

#### Validation

Validation: the bypass is explicit in the order of operations: collect URI refs from all parent env vars, clear the child env, then set changed parent env vars. Exploitability requires attacker influence over the environment that launches `xv`.

#### Dataflow

Parent environment variable containing `xv://...` -> URI reference collection -> secret resolution -> `cmd.env_clear()` -> changed parent variable re-added with secret value.

#### Reachability

An attacker who can influence the shell, wrapper script, CI job, or service environment that invokes `xv run` can reintroduce sensitive child-process variables despite clean mode.

#### Severity

Medium because this breaks an isolation expectation and can affect process behavior, but requires parent-environment influence. Restricting resolution to inherited-env mode would lower the issue to no finding.

#### Remediation

Remediation: only resolve URI references from inherited environment variables when `inherit_env` is true, or require an allowlist of variable names to resolve in clean mode.

### [8] Existing local age key files are accepted without permission or symlink checks

| Field | Value |
|---|---|
| Severity | Medium |
| Confidence | Medium |
| Confidence rationale | Existing-key load paths lack permission checks, while generated-key paths have stronger controls. |
| Category | Insecure key file handling |
| CWE | CWE-732 Incorrect Permission Assignment, CWE-59 Link Following |
| Affected lines | `src/backend/local/crypto.rs:139`, `src/backend/local/crypto.rs:149`, `src/backend/local/mod.rs:155`, `src/backend/local/mod.rs:167` |

#### Summary

Generated local keys use private writes, but loading an existing key file only checks size and then reads it. The loader does not reject symlinks or group/world-readable private keys.

#### Validation

Validation: no owner/mode/symlink checks were found in the load path. Severity is medium because exploitation generally requires local filesystem access or configuration influence, but the key protects all local backend secrets.

#### Dataflow

Configured key path or `AGE_KEY_FILE` -> local backend initialization -> `load_identity` metadata size check -> `fs::read_to_string` -> identity used for decrypting local secrets and files.

#### Reachability

The relevant attacker is a local user, shared-filesystem actor, or configuration tamperer who can expose or redirect the key file before `xv` loads it.

#### Severity

Medium because compromise of the age identity compromises local backend confidentiality, but the preconditions are local. Enforcing owner-only non-symlink keys would eliminate the issue.

#### Remediation

Remediation: use `symlink_metadata`, reject symlinks, require owner-only permissions on Unix, warn/fail on unexpected owner, and provide a repair command to chmod/chown existing keys.

### [9] Setup script prints the Azure app registration client secret

| Field | Value |
|---|---|
| Severity | Medium |
| Confidence | Medium |
| Confidence rationale | The script directly prints the generated secret, but the affected path is deployment tooling rather than steady-state runtime. |
| Category | Secret disclosure in deployment output |
| CWE | CWE-532 Insertion of Sensitive Information into Log File |
| Affected lines | `xfunction/scripts/setup-app-registration.ps1:35`, `xfunction/scripts/setup-app-registration.ps1:42` |

#### Summary

The PowerShell setup script writes the generated Azure app registration client secret to Function App settings, then prints the raw secret to the console. This can leak into terminal scrollback, transcripts, CI logs, or shared support output.

#### Validation

Validation: the disclosure is explicit at line 42. Confidence is medium because it is a deployment script rather than the main runtime path, but the printed value is a real credential.

#### Dataflow

`az ad app credential reset` result -> `$secret.password` -> Function App app setting -> `Write-Host Client Secret`.

#### Reachability

Anyone with access to the operator terminal, transcript, CI logs, or copied setup output can recover the client secret and use the app registration within its granted permissions.

#### Severity

Medium because the value is a real cloud credential, but exposure depends on deployment logging practices. Proving the script is only run locally without transcripts would lower severity.

#### Remediation

Remediation: remove the secret from console output; print only secret identifier/expiration; document where to rotate it; prefer secure vault storage.

### [10] Remote blob names and metadata can inject terminal control sequences

| Field | Value |
|---|---|
| Severity | Low |
| Confidence | Medium |
| Confidence rationale | Source tracing shows raw terminal output, but impact depends on terminal behavior and operator interaction. |
| Category | Terminal escape injection |
| CWE | CWE-150 Improper Neutralization of Escape Sequences |
| Affected lines | `src/cli/file_ops.rs:624`, `src/utils/format.rs:141`, `src/utils/pager.rs:43` |

#### Summary

Remote blob names and metadata are rendered into tables and pager output without control-character neutralization. A malicious blob name containing ANSI or OSC sequences could spoof terminal output or trigger terminal-specific behavior when an operator lists files interactively.

#### Validation

Validation: source tracing shows remote fields copied into display rows, formatted by table rendering, and written verbatim by the pager. Non-TTY JSON output reduces impact, so this is low severity.

#### Dataflow

Remote blob metadata -> display row construction -> table formatting -> pager/stdout raw write to TTY.

#### Reachability

A blob writer can choose names or metadata that an operator later displays in a terminal. The outcome is mostly operator deception or terminal feature abuse, not direct secret compromise.

#### Severity

Low because it affects terminal presentation and requires interactive display of hostile names. Evidence of terminals enabling dangerous OSC handlers by default would raise severity.

#### Remediation

Remediation: sanitize or visibly escape control characters before TTY/table/pager output while preserving raw JSON for scripts.

## Reviewed Surfaces

| Surface | Risk Area | Outcome | Notes |
|---|---|---|---|
| Rust cloud backends (`src/backend/azure`, `src/backend/aws`, `src/auth`) | Cloud identity, object naming, URL construction, destructive operations | No issue found | Subagent review found typed backend selection, Azure path/OData escaping, fixed hosts, no shell execution, and recovery/force gates for destructive operations. |
| Rust local backend (`src/backend/local`) | Key storage, symlink handling, local path traversal | Reported | Key loading and local file download symlink gaps survived; vault and secret path traversal were rejected by validation and encoding controls. |
| Rust CLI file/blob operations | Path traversal, local overwrite, terminal output | Reported | Recursive download absolute-path handling survived; single/multi download and sync path controls were rejected due `safe_join`/`sync_assert_safe_local_path`. |
| `xv run` / secret injection | Secret leakage, child environment isolation | Reported | Output masking is present; URI resolution after `env_clear` survived as an isolation bypass. |
| Leak scanner (`src/scan`) | Secret exposure, regex DoS, hook install | No issue found | Findings do not print matched values; Rust regex avoids backtracking ReDoS; hook body is constant and unmanaged hooks are refused unless forced. |
| Upgrade/install flows | Signed update, checksum, archive extraction | Reported | Rust archive extraction reads binary bytes by basename, but update/install authenticity checks are fail-open; shell tar extraction remains a secondary file-write concern under the same supply-chain trust break. |
| Python Azure Function (`xfunction`) | Authn/authz, RBAC assignment, storage role mapping | Reported | Missing creator-tag fail-open, optional audience validation, and broad storage fallback survived. |
| Python installer Azure CLI wrapper | Command injection, secret redaction | No issue found | `subprocess.run` uses argument arrays with no shell; verbose command output redacts known secret flags/settings. |

## Open Questions And Follow Up

- Confirm deployed Azure Function app settings: whether `EXPECTED_AUDIENCE` is always present, and what subscription/resource-group scope the function app service principal has.
- Decide whether install scripts should require minisign signatures or be removed in favor of `xv upgrade` once signature enforcement is fail-closed.
- Add focused regression tests for recursive blob download absolute paths, `xv run` clean environment URI resolution, missing `CreatedByID`, and missing `EXPECTED_AUDIENCE`.
